//! Phase 1: connect to the Cloud PC through the AVD WebSocket gateway with Entra SSO (RDSAAD)
//! and save a screenshot. Connection setup mirrors IronRDP's screenshot example, except that
//! X.224 is sent by hand because IronRDP's connector does not advertise RDSAAD itself.

use crate::{aad, auth, feed, gateway, rdpfile, wst};
use anyhow::{Context, Result};
use ironrdp::cliprdr::Cliprdr;
use ironrdp::cliprdr::backend::CliprdrBackend;
use ironrdp::connector::connection_activation::ConnectionActivationFactory;
use ironrdp::connector::{self, ClientConnector, ClientConnectorState, ConnectionResult, Credentials};
use ironrdp::core::{WriteBuf, encode_buf};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp::pdu::nego::{self, SecurityProtocol};
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::capability_sets::client_codecs_capabilities;
use ironrdp::pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp::pdu::x224::X224;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_blocking::Framed;
use ironrdp_egfx::client::{GraphicsPipelineClient, GraphicsPipelineHandler};
use ironrdp_egfx::decode::{H264Decoder, OpenH264Decoder};
use ironrdp_egfx::pdu::CapabilitySet;
use sspi::network_client::reqwest_network_client::ReqwestNetworkClient;
use std::{
    io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const WIDTH: u16 = 1280;
const HEIGHT: u16 = 800;

pub type TlsOverWs = rustls::StreamOwned<rustls::ClientConnection, wst::WsStream>;

/// Graphics mode from `SCRY_GRAPHICS`: `rfx` (default, sharpest text), `egfx` (pipeline without
/// H.264), or `h264` (AVC420 — smoother motion, much less bandwidth, slightly softer text).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Graphics {
    Rfx,
    Egfx,
    H264,
}

fn graphics_mode() -> Graphics {
    match std::env::var("SCRY_GRAPHICS").as_deref() {
        Ok("egfx") => Graphics::Egfx,
        Ok("h264") => Graphics::H264,
        _ => Graphics::Rfx,
    }
}

fn egfx_enabled() -> bool {
    graphics_mode() != Graphics::Rfx
}

/// Logs what the server negotiated; the pipeline's compositor does the actual work.
struct EgfxLog;

impl GraphicsPipelineHandler for EgfxLog {
    fn on_capabilities_confirmed(&mut self, caps: &CapabilitySet) {
        eprintln!("graphics: pipeline active, server confirmed {caps:?}");
    }

    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        eprintln!("graphics: reset to {width}x{height}");
    }
}

/// A fresh pipeline per channel open (Windows may close and reopen it). Without a decoder the
/// pipeline doesn't advertise H.264 and the server sticks to its other codecs.
fn graphics_pipeline() -> GraphicsPipelineClient {
    let decoder = match graphics_mode() {
        Graphics::H264 => match OpenH264Decoder::new() {
            Ok(d) => Some(Box::new(d) as Box<dyn H264Decoder>),
            Err(e) => {
                eprintln!("graphics: H.264 decoder unavailable ({e}); continuing without H.264");
                None
            }
        },
        _ => None,
    };
    GraphicsPipelineClient::new(Box::new(EgfxLog), decoder)
}

fn build_config() -> connector::Config {
    connector::Config {
        credentials: Credentials::UsernamePassword {
            username: String::new(),
            password: String::new(),
        },
        domain: None,
        enable_tls: true,
        enable_credssp: false,
        enable_standard_rdp_security: false,
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: connector::DesktopSize {
            width: WIDTH,
            height: HEIGHT,
        },
        monitor_layout: None,
        // RemoteFX instead of raw bitmaps: far fewer bytes through the gateway.
        bitmap: Some(connector::BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs: client_codecs_capabilities(&[]).expect("default codec list"),
        }),
        client_build: 0,
        client_name: "scry".to_owned(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        #[cfg(windows)]
        platform: MajorPlatformType::WINDOWS,
        #[cfg(target_os = "macos")]
        platform: MajorPlatformType::MACINTOSH,
        #[cfg(target_os = "linux")]
        platform: MajorPlatformType::UNIX,
        enable_server_pointer: true,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        enable_audio_capture: false,
        // Bulk compression, same default as IronRDP's client (MPPC 64K).
        compression_type: Some(CompressionType::K64),
        pointer_software_rendering: false,
        multitransport_flags: None,
        // Graphics pipeline (EGFX) when H.264 is requested; RemoteFX bitmaps otherwise.
        support_dyn_vc_gfx_protocol: egfx_enabled(),
        performance_flags: PerformanceFlags::default(),
        desktop_scale_factor: 0,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: ironrdp::pdu::rdp::capability_sets::RailSupportLevel::empty(),
    }
}

/// TLS to the session host inside the WebSocket. The gateway hop is already TLS-verified;
/// the inner certificate is not checked yet (TODO: pin `redirectedServerCert` from the ARM reply).
fn tls_upgrade(stream: wst::WsStream, server_name: &str) -> Result<(TlsOverWs, Vec<u8>)> {
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(danger::NoCertificateVerification))
        .with_no_client_auth();
    config.resumption = rustls::client::Resumption::disabled();
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name.to_string().try_into()?)?;
    let mut tls = rustls::StreamOwned::new(conn, stream);
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock)?;
    }
    let cert = tls
        .conn
        .peer_certificates()
        .and_then(|c| c.first())
        .context("no peer certificate")?;
    use x509_cert::der::Decode as _;
    let cert = x509_cert::Certificate::from_der(cert)?;
    let public_key = cert
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .context("unaligned public key")?
        .to_vec();
    Ok((tls, public_key))
}

/// Connects to feed resource `index` and returns the session ready for the active stage.
/// `clipboard` enables the CLIPRDR channel with that backend.
pub fn open(index: usize, clipboard: Option<Box<dyn CliprdrBackend>>) -> Result<(ConnectionResult, Framed<TlsOverWs>)> {
    let path = feed::feed_dir()?.join(format!("{index}.rdp"));
    let rdp = rdpfile::parse(&rdpfile::decode(
        &std::fs::read(&path).context("run `scry list` first")?,
    ));

    println!("1. Tokens and gateway connection...");
    let token = auth::avd_access_token()?;
    let arm = gateway::arm_connect(&rdp, &token)?;
    let host_short = arm
        .server_name
        .split('.')
        .next()
        .unwrap_or(&arm.server_name)
        .to_string();
    let key = aad::PopKey::generate()?;
    let host_token = auth::rdsaad_token(&host_short, &key.kid)?;
    let aad_nonce = auth::srv_nonce()?;
    println!("   session host: {host_short}");

    println!("2. WebSocket to gateway...");
    let ws = wst::connect(&arm.ws_url, &token)?;

    println!("3. X.224 negotiation (RDSAAD)...");
    let mut framed = Framed::new(ws);
    let mut connector = ClientConnector::new(build_config(), "0.0.0.0:0".parse()?);
    // Display Control channel: lets the viewer change the remote resolution when it is resized.
    // Registered as a factory because Windows closes and reopens it during logon; a one-shot
    // channel would be refused on reopen and resizing would silently stop working.
    let mut drdynvc = DrdynvcClient::new().with_dynamic_channel_factory(ironrdp::displaycontrol::CHANNEL_NAME, || {
        DisplayControlClient::new(|_| Ok(Vec::new()))
    });
    if egfx_enabled() {
        drdynvc = drdynvc.with_dynamic_channel_factory(ironrdp_egfx::CHANNEL_NAME, graphics_pipeline);
    }
    connector.attach_static_channel(drdynvc);
    if let Some(backend) = clipboard {
        connector.attach_static_channel(Cliprdr::new(backend));
    }
    let request = nego::ConnectionRequestWithOpaqueRoutingToken {
        request: nego::ConnectionRequest {
            nego_data: None,
            flags: nego::RequestFlags::empty(),
            protocol: SecurityProtocol::RDSAAD,
            correlation_info: None,
        },
        routing_token: nego::OpaqueRoutingToken(rdp.get("loadbalanceinfo").cloned().unwrap_or_default()),
    };
    let mut buf = WriteBuf::new();
    encode_buf(&X224(request), &mut buf).map_err(|e| anyhow::anyhow!("encode X.224: {e}"))?;
    framed.write_all(buf.filled())?;
    connector.state = ClientConnectorState::ConnectionInitiationWaitConfirm {
        requested_protocol: SecurityProtocol::RDSAAD,
    };
    let should_upgrade = ironrdp_blocking::connect_begin(&mut framed, &mut connector).context("X.224 negotiation")?;

    println!("4. TLS to session host + Entra SSO...");
    let (mut tls, public_key) = tls_upgrade(framed.into_inner_no_leftover(), &arm.server_name)?;
    aad::handshake(&mut tls, &key, &host_token, &host_short, &aad_nonce)?;
    let upgraded = ironrdp_blocking::mark_as_upgraded(should_upgrade, &mut connector);

    println!("5. RDP connection sequence...");
    let mut framed = Framed::new(tls);
    let result = ironrdp_blocking::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut ReqwestNetworkClient,
        arm.server_name.clone().into(),
        public_key,
        None,
    )
    .context("RDP connection sequence")?;
    println!(
        "   connected, desktop {}x{} (graphics: {:?})",
        result.desktop_size.width,
        result.desktop_size.height,
        graphics_mode()
    );
    Ok((result, framed))
}

pub fn connect(index: usize, output: &Path, seconds: u64) -> Result<()> {
    let (result, framed) = open(index, None)?;
    println!("6. Receiving screen updates for {seconds}s...");
    let image = receive_frames(result, framed, Duration::from_secs(seconds))?;
    let png: image::RgbaImage = image::ImageBuffer::from_raw(
        u32::from(image.width()),
        u32::from(image.height()),
        image.data().to_vec(),
    )
    .context("invalid image")?;
    png.save(output)?;
    println!("Saved {}", output.display());
    Ok(())
}

/// Framebuffer, active stage and reactivation factory (for resolution changes) of a new session.
pub fn active_stage(result: ConnectionResult) -> (DecodedImage, ActiveStage, ConnectionActivationFactory) {
    let image = DecodedImage::new(
        ironrdp::graphics::image_processing::PixelFormat::RgbA32,
        result.desktop_size.width,
        result.desktop_size.height,
    );
    let stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    (image, stage, result.activation_factory)
}

fn receive_frames(result: ConnectionResult, mut framed: Framed<TlsOverWs>, duration: Duration) -> Result<DecodedImage> {
    let (mut image, mut stage, _) = active_stage(result);

    // Short read timeout so the loop can stop at the deadline even when the screen is idle.
    framed
        .get_inner()
        .0
        .sock
        .set_read_timeout(Some(Duration::from_millis(500)))?;
    let deadline = Instant::now() + duration;
    let mut frames = 0usize;
    while Instant::now() < deadline {
        let (action, payload) = match framed.read_pdu() {
            Ok(pdu) => pdu,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(anyhow::Error::new(e).context("read frame")),
        };
        frames += 1;
        for out in stage.process(&mut image, action, &payload, framed.last_read_at())? {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => framed.write_all(&frame)?,
                ActiveStageOutput::Terminate(reason) => {
                    println!("   server ended the session: {reason:?}");
                    return Ok(image);
                }
                _ => {}
            }
        }
    }
    println!("   {frames} frames received");
    Ok(image)
}

mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme, pki_types};

    #[derive(Debug)]
    pub(super) struct NoCertificateVerification;

    impl ServerCertVerifier for NoCertificateVerification {
        fn verify_server_cert(
            &self,
            _: &pki_types::CertificateDer<'_>,
            _: &[pki_types::CertificateDer<'_>],
            _: &pki_types::ServerName<'_>,
            _: &[u8],
            _: pki_types::UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &pki_types::CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &pki_types::CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}

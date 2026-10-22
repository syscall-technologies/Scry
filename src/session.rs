//! Long-running session: one thread owns the RDP connection, keeps the framebuffer current and
//! executes commands sent through a [`Handle`].

use crate::agent::AgentControl;
use crate::clipboard::{self, Clipboard};
use crate::connect::{self, TlsOverWs};
use crate::keys;
use anyhow::{Context, Result, anyhow, bail};
use ironrdp::cliprdr::CliprdrClient;
use ironrdp::cliprdr::backend::ClipboardMessage;
use ironrdp::connector::connection_activation::{ConnectionActivationFactory, ConnectionActivationState};
use ironrdp::connector::{ConnectionResult, Sequence};
use ironrdp::core::WriteBuf;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageOutput};
use ironrdp_blocking::Framed;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::{io, thread, time::Duration};

/// Pause between input steps so the remote side sees distinct presses.
const STEP: Duration = Duration::from_millis(15);
/// Extra pause between typed characters: Windows drops or mixes up Unicode key events
/// (VK_PACKET) that arrive faster than this; ~55 ms per character measured reliable in Notepad.
const CHAR_GAP: Duration = Duration::from_millis(25);
/// Minimum interval between framebuffer copies published for the viewer (~60 fps).
const PUBLISH_EVERY: Duration = Duration::from_millis(16);

/// Latest framebuffer (RGBA) for the viewer; `seq` is bumped on every publish.
#[derive(Default)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    pub seq: u64,
    /// Set when the session has ended, with the reason.
    pub ended: Option<String>,
}

/// The remote pointer, drawn by the viewer as an OS cursor.
pub enum Cursor {
    Hidden,
    Default,
    Shape {
        rgba: Vec<u8>,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    },
}

/// Latest remote pointer; `seq` bumps on every change so the viewer applies it once.
#[derive(Default)]
pub struct CursorState {
    pub seq: u64,
    pub cursor: Option<Cursor>,
}

enum Command {
    /// Raw framebuffer: width, height, RGBA.
    Screenshot(Sender<Result<(u16, u16, Vec<u8>)>>),
    Input(Vec<Operation>, Sender<Result<()>>),
    /// Ask the server for a new desktop size (Display Control channel).
    Resize(u32, u32),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
    frame: Arc<Mutex<Frame>>,
    cursor: Arc<Mutex<CursorState>>,
    agent: Arc<AgentControl>,
    clip_state: clipboard::Shared,
    clip_tx: Sender<ClipboardMessage>,
}

/// Starts the session thread; `clipboard` must be the one whose backend was given to `open`.
pub fn spawn(result: ConnectionResult, framed: Framed<TlsOverWs>, clipboard: Clipboard) -> Handle {
    let (tx, rx) = mpsc::channel();
    let frame = Arc::new(Mutex::new(Frame::default()));
    let shared = Arc::clone(&frame);
    let cursor = Arc::new(Mutex::new(CursorState::default()));
    let cursor_shared = Arc::clone(&cursor);
    clipboard.watch_local();
    let (clip_state, clip_tx) = (Arc::clone(&clipboard.state), clipboard.tx.clone());
    thread::spawn(move || {
        let reason = match run(result, framed, rx, &shared, &cursor_shared, &clipboard.rx) {
            Ok(()) => "the server ended the session".to_owned(),
            Err(e) => format!("{e:#}"),
        };
        eprintln!("session ended: {reason}");
        shared.lock().expect("frame lock").ended = Some(reason);
    });
    Handle {
        tx,
        frame,
        cursor,
        agent: Arc::default(),
        clip_state,
        clip_tx,
    }
}

fn run(
    result: ConnectionResult,
    mut framed: Framed<TlsOverWs>,
    rx: Receiver<Command>,
    frame: &Mutex<Frame>,
    cursor: &Mutex<CursorState>,
    clip_rx: &Receiver<ClipboardMessage>,
) -> Result<()> {
    let (mut image, mut stage, activation) = connect::active_stage(result);
    let mut input = Database::new();
    // Short read timeout so commands are picked up promptly while the screen is idle.
    framed
        .get_inner()
        .0
        .sock
        .set_read_timeout(Some(Duration::from_millis(20)))?;
    // Region changed since the last publish (inclusive l, t, r, b); `full` after a resolution change.
    let mut damage: Option<(u16, u16, u16, u16)> = None;
    let mut full = true;
    let mut published = Instant::now();
    let mut stats = Stats::from_env();
    // Latest requested resolution, sent once the Display Control channel is ready.
    let mut pending_resize: Option<(u32, u32)> = None;
    let mut resize_waiting_logged = false;
    let started = Instant::now();
    let mut display_control_ready = false;

    loop {
        let mut outputs = Vec::new();
        match framed.read_pdu() {
            Ok((action, payload)) => {
                let started = Instant::now();
                outputs = match stage.process(&mut image, action, &payload, framed.last_read_at()) {
                    Ok(outputs) => outputs,
                    // IronRDP rejects Set Error Info PDUs with codes it doesn't know. They are
                    // informational (a real disconnect closes the connection), so log and go on.
                    Err(e) if e.to_string().contains("ServerSetErrorInfoPdu") => {
                        eprintln!("warning: server sent an unknown error info code ({e}); continuing");
                        Vec::new()
                    }
                    Err(e) => return Err(e.into()),
                };
                stats.decoded(payload.len(), started.elapsed());
            }
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(anyhow::Error::new(e).context("read frame")),
        }

        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Command::Screenshot(reply) => {
                    let _ = reply.send(Ok((image.width(), image.height(), image.data().to_vec())));
                }
                Command::Input(ops, reply) => {
                    let events = input.apply(ops);
                    outputs.extend(stage.process_fastpath_input(&mut image, &events)?);
                    let _ = reply.send(Ok(()));
                }
                Command::Resize(width, height) => {
                    eprintln!("resize: requested {width}x{height}");
                    pending_resize = Some((width, height));
                }
            }
        }

        while let Ok(message) = clip_rx.try_recv() {
            outputs.extend(apply_clipboard_message(&mut stage, message)?);
        }

        let ready = stage.display_control_capabilities().is_some();
        if ready != display_control_ready {
            display_control_ready = ready;
            eprintln!(
                "resize: Display Control channel {} after {:.1}s ({:?})",
                if ready { "ready" } else { "gone" },
                started.elapsed().as_secs_f32(),
                stage.display_control_capabilities()
            );
        }

        if let Some((width, height)) = pending_resize {
            match stage.encode_resize(width, height, None, None) {
                Some(Ok(pdu)) => {
                    framed.write_all(&pdu)?;
                    eprintln!("resize: sent {width}x{height} over Display Control");
                    pending_resize = None;
                    resize_waiting_logged = false;
                }
                Some(Err(e)) => {
                    eprintln!("resize to {width}x{height} failed: {e}");
                    pending_resize = None;
                }
                None if !resize_waiting_logged => {
                    // Display Control channel not ready yet; retried every loop.
                    eprintln!(
                        "resize: waiting for the Display Control channel (server caps: {:?})",
                        stage.display_control_capabilities()
                    );
                    resize_waiting_logged = true;
                }
                None => {}
            }
        }

        for out in &outputs {
            match out {
                ActiveStageOutput::PointerBitmap(ptr) => set_cursor(
                    cursor,
                    Cursor::Shape {
                        rgba: ptr.bitmap_data.clone(),
                        width: ptr.width,
                        height: ptr.height,
                        hotspot_x: ptr.hotspot_x,
                        hotspot_y: ptr.hotspot_y,
                    },
                ),
                ActiveStageOutput::PointerHidden => set_cursor(cursor, Cursor::Hidden),
                ActiveStageOutput::PointerDefault => set_cursor(cursor, Cursor::Default),
                _ => {}
            }
            if let ActiveStageOutput::GraphicsUpdate(r) = out {
                damage = Some(match damage {
                    None => (r.left, r.top, r.right, r.bottom),
                    Some((l, t, rr, b)) => (l.min(r.left), t.min(r.top), rr.max(r.right), b.max(r.bottom)),
                });
            }
        }

        match send_outputs(&mut framed, outputs)? {
            Flow::Continue => {}
            Flow::Terminated => return Ok(()),
            Flow::Reactivate => {
                eprintln!("resize: server reactivating the display...");
                reactivate(&mut framed, &activation, &mut image, &mut stage)?;
                eprintln!("resize: desktop is now {}x{}", image.width(), image.height());
                full = true;
            }
        }

        // Publish for the viewer, copying only the changed region. `try_lock` so the network
        // loop never waits on the viewer drawing; the damage is kept for the next round.
        if (full || damage.is_some())
            && published.elapsed() >= PUBLISH_EVERY
            && let Ok(mut f) = frame.try_lock()
        {
            let started = Instant::now();
            let (w, h) = (image.width(), image.height());
            if full || f.width != w || f.height != h || f.rgba.len() != image.data().len() {
                f.rgba.clear();
                f.rgba.extend_from_slice(image.data());
            } else if let Some((l, t, r, b)) = damage {
                let stride = usize::from(w) * 4;
                let (x0, x1) = (usize::from(l) * 4, (usize::from(r.min(w - 1)) + 1) * 4);
                for y in usize::from(t)..=usize::from(b.min(h - 1)) {
                    let row = y * stride;
                    f.rgba[row + x0..row + x1].copy_from_slice(&image.data()[row + x0..row + x1]);
                }
            }
            f.width = w;
            f.height = h;
            f.seq += 1;
            drop(f);
            stats.published(started.elapsed());
            full = false;
            damage = None;
            published = Instant::now();
        }
        if stats.due() {
            stats.report();
        }
    }
}

/// Throughput/cost counters printed every 5 s when `SCRY_STATS=1`.
struct Stats {
    enabled: bool,
    since: Instant,
    pdus: u32,
    bytes: usize,
    decode: Duration,
    publishes: u32,
    publish: Duration,
}

impl Stats {
    fn from_env() -> Self {
        Self {
            enabled: std::env::var("SCRY_STATS").is_ok_and(|v| v == "1"),
            since: Instant::now(),
            pdus: 0,
            bytes: 0,
            decode: Duration::ZERO,
            publishes: 0,
            publish: Duration::ZERO,
        }
    }

    fn decoded(&mut self, bytes: usize, took: Duration) {
        self.pdus += 1;
        self.bytes += bytes;
        self.decode += took;
    }

    fn published(&mut self, took: Duration) {
        self.publishes += 1;
        self.publish += took;
    }

    fn due(&self) -> bool {
        self.enabled && self.since.elapsed() >= Duration::from_secs(5)
    }

    fn report(&mut self) {
        let secs = self.since.elapsed().as_secs_f64();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "stats: {:.0} PDUs/s, {:.2} MB/s, decode {:.1} ms/s ({:.0}% busy), publish {:.0}/s avg {:.2} ms",
            f64::from(self.pdus) / secs,
            self.bytes as f64 / secs / 1e6,
            ms(self.decode) / secs,
            ms(self.decode) / secs / 10.0,
            f64::from(self.publishes) / secs,
            if self.publishes == 0 {
                0.0
            } else {
                ms(self.publish) / f64::from(self.publishes)
            },
        );
        *self = Self {
            enabled: true,
            ..Self::from_env()
        };
    }
}

/// Applies a clipboard backend/watcher message to the CLIPRDR channel (as IronRDP's client does).
fn apply_clipboard_message(stage: &mut ActiveStage, message: ClipboardMessage) -> Result<Vec<ActiveStageOutput>> {
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
        return Ok(Vec::new());
    };
    let messages = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response),
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
        ClipboardMessage::Error(e) => {
            eprintln!("clipboard: {e}");
            return Ok(Vec::new());
        }
        _ => return Ok(Vec::new()), // file transfer is not supported
    }
    .map_err(|e| anyhow!("clipboard: {e}"))?;
    let frame = stage.process_svc_processor_messages(messages)?;
    Ok(vec![ActiveStageOutput::ResponseFrame(frame)])
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

enum Flow {
    Continue,
    Terminated,
    /// The server started a Deactivation-Reactivation sequence (e.g. after a resolution change).
    Reactivate,
}

/// Writes response frames and reports what the session loop must do next.
fn set_cursor(cursor: &Mutex<CursorState>, new: Cursor) {
    let mut c = cursor.lock().expect("cursor lock");
    c.seq += 1;
    c.cursor = Some(new);
}

fn send_outputs(framed: &mut Framed<TlsOverWs>, outputs: Vec<ActiveStageOutput>) -> Result<Flow> {
    let mut flow = Flow::Continue;
    for out in outputs {
        match out {
            ActiveStageOutput::ResponseFrame(frame) => framed.write_all(&frame)?,
            ActiveStageOutput::DeactivateAll => flow = Flow::Reactivate,
            ActiveStageOutput::Terminate(reason) => {
                eprintln!("server ended the session: {reason:?}");
                return Ok(Flow::Terminated);
            }
            _ => {}
        }
    }
    Ok(flow)
}

/// Runs the Deactivation-Reactivation sequence and resumes at the new desktop size
/// (mirrors IronRDP's own client).
fn reactivate(
    framed: &mut Framed<TlsOverWs>,
    activation: &ConnectionActivationFactory,
    image: &mut DecodedImage,
    stage: &mut ActiveStage,
) -> Result<()> {
    let mut sequence = activation.create();
    let mut buf = WriteBuf::new();
    loop {
        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
            static_channel_chunk_size,
            ..
        } = sequence.connection_activation_state()
        {
            *image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
            if !stage.reactivate(
                sequence.io_channel_id(),
                sequence.user_channel_id(),
                share_id,
                enable_server_pointer,
                pointer_software_rendering,
                static_channel_chunk_size,
            ) {
                bail!("invalid static channel chunk size after reactivation");
            }
            return Ok(());
        }

        buf.clear();
        let written = match sequence.next_pdu_hint() {
            Some(hint) => {
                let pdu = loop {
                    match framed.read_by_hint(hint) {
                        Ok(pdu) => break pdu,
                        Err(e) if is_timeout(&e) => {}
                        Err(e) => return Err(anyhow::Error::new(e).context("read reactivation PDU")),
                    }
                };
                sequence.step(&pdu, framed.last_read_at(), &mut buf)?
            }
            None => sequence.step_no_input(&mut buf)?,
        };
        if let Some(len) = written.size() {
            framed.write_all(&buf[..len])?;
        }
    }
}

/// Agent screenshots are at most this wide; agent coordinates refer to that scaled image.
const AGENT_MAX_WIDTH: u32 = 1280;

/// Size of the agent's view of a `width`x`height` desktop.
fn agent_size(width: u32, height: u32) -> (u32, u32) {
    if width <= AGENT_MAX_WIDTH {
        (width, height)
    } else {
        (AGENT_MAX_WIDTH, (height * AGENT_MAX_WIDTH).div_ceil(width))
    }
}

/// Maps a point in the agent's scaled view of a `rw`x`rh` desktop to desktop pixels.
fn agent_to_remote(x: u16, y: u16, rw: u32, rh: u32) -> (u16, u16) {
    let (aw, ah) = agent_size(rw, rh);
    let map = |v: u16, a: u32, r: u32| ((u32::from(v) * r / a).min(r - 1)) as u16;
    (map(x, aw, rw), map(y, ah, rh))
}

fn encode_png(width: u16, height: u16, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let image = image::RgbaImage::from_raw(u32::from(width), u32::from(height), rgba).context("invalid frame")?;
    let (w, h) = agent_size(image.width(), image.height());
    let image = if w == image.width() {
        image
    } else {
        image::imageops::resize(&image, w, h, image::imageops::FilterType::Triangle)
    };
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        image.as_raw(),
        w,
        h,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}

fn button(name: &str) -> Result<MouseButton> {
    match name {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(anyhow!("unknown mouse button {other:?}")),
    }
}

impl Handle {
    fn call<T>(&self, make: impl FnOnce(Sender<Result<T>>) -> Command) -> Result<T> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(make(tx)).map_err(|_| anyhow!("session is closed"))?;
        rx.recv().context("session is closed")?
    }

    fn input(&self, ops: Vec<Operation>) -> Result<()> {
        self.call(|reply| Command::Input(ops, reply))?;
        thread::sleep(STEP);
        Ok(())
    }

    /// Sends input without waiting or pacing (for live human input from the viewer).
    pub fn send_input(&self, ops: Vec<Operation>) {
        let (reply, _) = mpsc::channel();
        let _ = self.tx.send(Command::Input(ops, reply));
    }

    pub fn frame(&self) -> &Mutex<Frame> {
        &self.frame
    }

    pub fn cursor(&self) -> &Mutex<CursorState> {
        &self.cursor
    }

    pub fn agent(&self) -> &AgentControl {
        &self.agent
    }

    /// PNG of the screen, scaled down to at most 1280 px wide for the agent.
    pub fn screenshot(&self) -> Result<Vec<u8>> {
        let (width, height, rgba) = self.call(Command::Screenshot)?;
        encode_png(width, height, rgba)
    }

    /// Requests a new remote desktop size (even width, 200..=8192 per MS-RDPEDISP).
    pub fn resize(&self, width: u32, height: u32) {
        let (width, height) = (width.clamp(200, 8192) & !1, height.clamp(200, 8192));
        let current = {
            let f = self.frame.lock().expect("frame lock");
            (u32::from(f.width), u32::from(f.height))
        };
        if (width, height) != current {
            let _ = self.tx.send(Command::Resize(width, height));
        }
    }

    /// Agent coordinates (in its scaled screenshot) -> remote desktop coordinates.
    fn to_remote(&self, x: u16, y: u16) -> (u16, u16) {
        let (rw, rh) = {
            let f = self.frame.lock().expect("frame lock");
            (u32::from(f.width), u32::from(f.height))
        };
        if rw == 0 {
            return (x, y);
        }
        agent_to_remote(x, y, rw, rh)
    }

    // Agent actions below report progress to the viewer and honour pause/stop. The gate is checked
    // only between whole units (character, click, combo) so no key or button is left held down.

    pub fn move_to(&self, x: u16, y: u16) -> Result<()> {
        let _activity = self.agent.begin(format!("move to {x},{y}"), 1);
        self.agent.wait_turn()?;
        self.raw_move(x, y)
    }

    pub fn click(&self, x: u16, y: u16, button_name: &str, double: bool) -> Result<()> {
        let b = button(button_name)?;
        let kind = if double { "double-click" } else { "click" };
        let _activity = self.agent.begin(format!("{kind} at {x},{y}"), 1);
        self.agent.wait_turn()?;
        self.raw_move(x, y)?;
        for _ in 0..if double { 2 } else { 1 } {
            self.input(vec![Operation::MouseButtonPressed(b)])?;
            self.input(vec![Operation::MouseButtonReleased(b)])?;
        }
        Ok(())
    }

    /// `dy` in wheel notches, positive scrolls down.
    pub fn scroll(&self, x: u16, y: u16, dy: i16) -> Result<()> {
        let units = dy.checked_mul(-120).context("scroll amount too large")?;
        let _activity = self.agent.begin(format!("scroll {dy} at {x},{y}"), 1);
        self.agent.wait_turn()?;
        self.raw_move(x, y)?;
        self.input(vec![Operation::WheelRotations(WheelRotations {
            is_vertical: true,
            rotation_units: units,
        })])
    }

    pub fn type_text(&self, text: &str) -> Result<()> {
        let total = text.chars().filter(|&c| c != '\r').count();
        let preview: String = text.chars().take(32).map(|c| if c == '\n' { ' ' } else { c }).collect();
        let more = if total > 32 { "..." } else { "" };
        let activity = self.agent.begin(format!("typing \"{preview}{more}\""), total);
        let enter = keys::parse_combo("enter")?;
        for c in text.chars() {
            if c == '\r' {
                continue; // "\r\n" line endings: Enter is sent for the '\n'
            }
            self.agent.wait_turn()?;
            if c == '\n' {
                self.raw_combo(&enter)?;
            } else {
                self.input(vec![Operation::UnicodeKeyPressed(c)])?;
                self.input(vec![Operation::UnicodeKeyReleased(c)])?;
            }
            activity.advance();
            thread::sleep(CHAR_GAP);
        }
        Ok(())
    }

    /// Presses the keys in order and releases them in reverse, e.g. `win+r`.
    pub fn key_combo(&self, combo: &str) -> Result<()> {
        let codes = keys::parse_combo(combo)?;
        let _activity = self.agent.begin(format!("key {combo}"), 1);
        self.agent.wait_turn()?;
        self.raw_combo(&codes)
    }

    /// Puts `text` on the Cloud PC clipboard and pastes it with Ctrl+V (instant, any length).
    /// The local clipboard is left untouched.
    pub fn paste_text(&self, text: &str) -> Result<()> {
        if !self.clip_state.lock().expect("clipboard lock").ready {
            // Clipboard redirection disabled on this Cloud PC: type it instead.
            return self.type_text(text);
        }
        let _activity = self.agent.begin(format!("pasting {} chars", text.chars().count()), 1);
        self.agent.wait_turn()?;
        self.clip_state.lock().expect("clipboard lock").outgoing = Some(text.to_owned());
        self.clip_tx
            .send(clipboard::offer_text())
            .map_err(|_| anyhow!("session is closed"))?;
        // Let the Cloud PC take ownership of the new clipboard content before pasting.
        thread::sleep(Duration::from_millis(300));
        self.raw_combo(&keys::parse_combo("ctrl+v")?)
    }

    /// Last text copied on the Cloud PC, if any.
    pub fn remote_clipboard(&self) -> Result<Option<String>> {
        let state = self.clip_state.lock().expect("clipboard lock");
        if !state.ready {
            bail!("clipboard redirection is disabled on this Cloud PC (no clipboard channel)");
        }
        Ok(state.remote.clone())
    }

    fn raw_move(&self, x: u16, y: u16) -> Result<()> {
        let (x, y) = self.to_remote(x, y);
        self.input(vec![Operation::MouseMove(MousePosition { x, y })])
    }

    fn raw_combo(&self, codes: &[Scancode]) -> Result<()> {
        for &code in codes {
            self.input(vec![Operation::KeyPressed(code)])?;
        }
        for &code in codes.iter().rev() {
            self.input(vec![Operation::KeyReleased(code)])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_view_caps_width_and_keeps_aspect() {
        assert_eq!(agent_size(1280, 800), (1280, 800));
        assert_eq!(agent_size(1024, 768), (1024, 768));
        assert_eq!(agent_size(2560, 1600), (1280, 800));
        assert_eq!(agent_size(1728, 1117), (1280, 828));
    }

    #[test]
    fn agent_coordinates_map_back_to_desktop() {
        assert_eq!(agent_to_remote(100, 50, 1280, 800), (100, 50));
        assert_eq!(agent_to_remote(1240, 684, 2560, 1440), (2480, 1368));
        assert_eq!(agent_to_remote(1279, 719, 2560, 1440), (2558, 1438));
        assert_eq!(
            agent_to_remote(5000, 5000, 2560, 1440),
            (2559, 1439),
            "clamped to the desktop"
        );
    }
}

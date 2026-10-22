//! RDS AAD Auth (PROTOCOL_RDSAAD) handshake run right after TLS (FreeRDP libfreerdp/core/aad.c).
//!
//! Server sends `{"ts_nonce": ...}`, client answers `{"rdp_assertion": <JWS>}` signed with the
//! proof-of-possession key the access token is bound to, server replies `{"authentication_result": 0}`.
//! PDUs have no length prefix and end with a NUL byte.

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use rsa::{
    RsaPrivateKey,
    pkcs1v15::SigningKey,
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub struct PopKey {
    key: RsaPrivateKey,
    /// `req_cnf` for the token request and `kid` of the JWS: b64url({"kid": b64url(sha256(jwk))}).
    pub kid: String,
}

impl PopKey {
    pub fn generate() -> Result<Self> {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048)?;
        let (e, n) = jwk_params(&key);
        let jwk = format!(r#"{{"e":"{e}","kty":"RSA","n":"{n}"}}"#);
        let hash = B64.encode(Sha256::digest(jwk.as_bytes()));
        let kid = B64.encode(format!(r#"{{"kid":"{hash}"}}"#));
        Ok(Self { key, kid })
    }
}

fn jwk_params(key: &RsaPrivateKey) -> (String, String) {
    (B64.encode(key.e().to_bytes_be()), B64.encode(key.n().to_bytes_be()))
}

fn read_pdu(stream: &mut impl Read) -> Result<serde_json::Value> {
    let mut pdu = Vec::new();
    let mut byte = [0u8];
    loop {
        stream.read_exact(&mut byte).context("read RDSAAD PDU")?;
        if byte[0] == 0 {
            break;
        }
        pdu.push(byte[0]);
    }
    Ok(serde_json::from_slice(&pdu)?)
}

pub fn handshake<S: Read + Write>(
    stream: &mut S,
    key: &PopKey,
    access_token: &str,
    host_short: &str,
    aad_nonce: &str,
) -> Result<()> {
    let msg = read_pdu(stream)?;
    let ts_nonce = msg["ts_nonce"].as_str().context("server sent no ts_nonce")?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let (e, n) = jwk_params(&key.key);
    let header = B64.encode(format!(r#"{{"alg":"RS256","kid":"{}"}}"#, key.kid));
    let payload = B64.encode(format!(
        r#"{{"ts":"{ts}","at":"{access_token}","u":"ms-device-service://termsrv.wvd.microsoft.com/name/{host_short}","nonce":"{ts_nonce}","cnf":{{"jwk":{{"kty":"RSA","e":"{e}","n":"{n}"}}}},"client_claims":"{{\"aad_nonce\":\"{aad_nonce}\"}}"}}"#
    ));
    let signing_input = format!("{header}.{payload}");
    let signature = SigningKey::<Sha256>::new(key.key.clone()).sign(signing_input.as_bytes());
    let assertion = format!("{signing_input}.{}", B64.encode(signature.to_bytes()));

    let mut pdu = format!(r#"{{"rdp_assertion":"{assertion}"}}"#).into_bytes();
    pdu.push(0);
    stream.write_all(&pdu)?;
    stream.flush()?;

    let result = read_pdu(stream)?;
    match result["authentication_result"].as_u64() {
        Some(0) => Ok(()),
        Some(code) => bail!("session host rejected Entra sign-in: 0x{code:08x}"),
        None => bail!("unexpected RDSAAD reply: {result}"),
    }
}

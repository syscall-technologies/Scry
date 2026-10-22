//! AVD WebSocket transport (FreeRDP gateway/wst.c): raw RDP bytes in binary WebSocket frames.
//!
//! A plain GET first collects the ARRAffinity load-balancer cookie; the WebSocket upgrade then
//! goes to the same host with that cookie and the token as `ClmTk=Bearer%20<token>`.

use crate::auth;
use anyhow::{Context, Result};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::Arc,
    time::Duration,
};
use tungstenite::{Message, WebSocket, client::IntoClientRequest};
use url::Url;

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

pub struct WsStream {
    ws: WebSocket<Tls>,
    buf: Vec<u8>,
    pos: usize,
}

fn affinity_cookies(url: &Url, token: &str) -> Result<String> {
    let resp = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .get(url.as_str())
        .bearer_auth(token)
        .header("X-MS-User-Agent", auth::MS_USER_AGENT)
        .send()?;
    let cookies: Vec<String> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .filter(|kv| kv.starts_with("ARRAffinity"))
        .map(String::from)
        .collect();
    println!("   affinity probe: {} ({} cookie(s))", resp.status(), cookies.len());
    Ok(cookies.join("; "))
}

fn tls_connect(host: &str) -> Result<Tls> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let conn = rustls::ClientConnection::new(Arc::new(config), host.to_string().try_into()?)?;
    let sock = TcpStream::connect((host, 443))?;
    Ok(rustls::StreamOwned::new(conn, sock))
}

pub fn connect(gateway_url: &str, token: &str) -> Result<WsStream> {
    let mut url = Url::parse(gateway_url)?;
    url.set_scheme("https").ok();
    let host = url.host_str().context("gateway URL has no host")?.to_string();
    crate::feed::ensure_trusted(url.as_str())?;

    let cookies = affinity_cookies(&url, token)?;

    url.set_scheme("wss").ok();
    // Like FreeRDP: after the affinity round-trip the token moves into the URL (header size limits).
    let ws_url = if cookies.is_empty() {
        url.to_string()
    } else {
        let sep = if url.query().is_some() { '&' } else { '?' };
        format!("{url}{sep}ClmTk=Bearer%20{token}")
    };
    let mut request = ws_url.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("X-MS-User-Agent", auth::MS_USER_AGENT.parse()?);
    if cookies.is_empty() {
        headers.insert("Authorization", format!("Bearer {token}").parse()?);
    } else {
        headers.insert("Cookie", cookies.parse()?);
    }

    let (ws, resp) = tungstenite::client::client(request, tls_connect(&host)?)
        .map_err(|e| anyhow::anyhow!("WebSocket upgrade failed: {e}"))?;
    println!("   websocket: {}", resp.status());
    Ok(WsStream {
        ws,
        buf: Vec::new(),
        pos: 0,
    })
}

impl WsStream {
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.ws.get_ref().sock.set_read_timeout(timeout)
    }
}

impl Read for WsStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            match self.ws.read() {
                Ok(Message::Binary(data)) => {
                    self.buf = data.to_vec();
                    self.pos = 0;
                }
                Ok(Message::Close(_)) => return Ok(0),
                Ok(_) => {} // ping/pong/text
                Err(tungstenite::Error::Io(e)) => return Err(e),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(0),
                Err(e) => return Err(io::Error::other(e)),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for WsStream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.ws.send(Message::binary(data.to_vec())).map_err(io::Error::other)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.ws.flush().map_err(io::Error::other)
    }
}

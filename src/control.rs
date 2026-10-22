//! Local HTTP control API for a running session (127.0.0.1 only).
//!
//!   GET  /screenshot                         -> image/png
//!   POST /click   {"x","y","button"?,"double"?}
//!   POST /move    {"x","y"}
//!   POST /scroll  {"x","y","dy"}             (notches, positive = down)
//!   POST /type    {"text"}
//!   POST /key     {"combo"}                  (e.g. "win+r", "ctrl+shift+esc")
//!   POST /paste   {"text"}                   (via the Cloud PC clipboard + Ctrl+V)
//!   GET  /clipboard                          -> text last copied on the Cloud PC

use crate::session::Handle;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tiny_http::{Header, Method, Request, Response, Server};

#[derive(Deserialize)]
struct Point {
    x: u16,
    y: u16,
    #[serde(default = "left")]
    button: String,
    #[serde(default)]
    double: bool,
    #[serde(default)]
    dy: i16,
}

fn left() -> String {
    "left".into()
}

#[derive(Deserialize)]
struct Text {
    text: String,
}

#[derive(Deserialize)]
struct Combo {
    combo: String,
}

pub fn serve(handle: Handle, port: u16) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("bind 127.0.0.1:{port}: {e}"))?;
    println!("Control API on http://127.0.0.1:{port} (Ctrl+C to disconnect)");
    for mut request in server.incoming_requests() {
        let response = match handle_request(&handle, &mut request) {
            Ok(Some((body, content_type))) => Response::from_data(body)
                .with_header(Header::from_bytes("Content-Type", content_type).expect("static header")),
            Ok(None) => Response::from_string("ok\n"),
            Err(e) => Response::from_string(format!("{e:#}\n")).with_status_code(400),
        };
        let _ = request.respond(response);
    }
    Ok(())
}

fn body<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T> {
    serde_json::from_reader(request.as_reader()).context("invalid JSON body")
}

/// Returns a body and its content type for GET endpoints, `None` for actions.
fn handle_request(handle: &Handle, request: &mut Request) -> Result<Option<(Vec<u8>, &'static str)>> {
    match (request.method(), request.url()) {
        (Method::Get, "/screenshot") => return Ok(Some((handle.screenshot()?, "image/png"))),
        (Method::Post, "/click") => {
            let p: Point = body(request)?;
            handle.click(p.x, p.y, &p.button, p.double)?;
        }
        (Method::Post, "/move") => {
            let p: Point = body(request)?;
            handle.move_to(p.x, p.y)?;
        }
        (Method::Post, "/scroll") => {
            let p: Point = body(request)?;
            handle.scroll(p.x, p.y, p.dy)?;
        }
        (Method::Post, "/type") => handle.type_text(&body::<Text>(request)?.text)?,
        (Method::Post, "/key") => handle.key_combo(&body::<Combo>(request)?.combo)?,
        (Method::Post, "/paste") => handle.paste_text(&body::<Text>(request)?.text)?,
        (Method::Get, "/clipboard") => {
            let text = handle.remote_clipboard()?.unwrap_or_default();
            return Ok(Some((text.into_bytes(), "text/plain; charset=utf-8")));
        }
        (method, url) => bail!("unknown endpoint {method} {url}"),
    }
    Ok(None)
}

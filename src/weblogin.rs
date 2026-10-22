//! Entra sign-in in an embedded web view instead of the default browser (and its profile).
//!
//! The window runs in a child process (`scry webview-login <url>`) because an event loop is
//! per-process and the viewer needs its own. The child intercepts the navigation to the redirect
//! URI, prints it on stdout and exits, so no copy/paste is needed.
//!
//! Uses `tao` (wry's companion windowing library) rather than winit: winit's macOS window
//! delegate crashes (use-after-free in `window_did_resign_key`) when a wry WebView window loses
//! focus. tao does not have that bug. The viewer keeps winit (no WebView, never affected).

use crate::auth::REDIRECT;
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Parent side: opens the sign-in window and returns the redirect URL carrying the code.
pub fn sign_in(url: &str) -> Result<String> {
    let output = Command::new(std::env::current_exe()?)
        .args(["webview-login", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .context("start sign-in window")?;
    if !output.status.success() {
        bail!("sign-in window was closed before completing");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Child side: the `webview-login` subcommand. `event_loop.run` diverges, so the window and
/// webview created here live for the process lifetime.
pub fn run_window(url: String) -> Result<()> {
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("scry - Sign in to Microsoft")
        .with_inner_size(LogicalSize::new(520.0, 720.0))
        .build(&event_loop)
        .context("create sign-in window")?;
    crate::platform::set_dock_icon();

    let _webview = WebViewBuilder::new()
        .with_url(&url)
        .with_navigation_handler(move |target| {
            if target.starts_with(REDIRECT) {
                let _ = proxy.send_event(target);
                return false; // don't load the blank redirect page; exit cleanly instead
            }
            true
        })
        .build(&window)
        .context("create sign-in webview")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // Redirect intercepted: hand the URL to the parent on stdout, then exit.
            Event::UserEvent(redirect) => {
                println!("{redirect}");
                let _ = std::io::stdout().flush();
                *control_flow = ControlFlow::ExitWithCode(0);
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::ExitWithCode(1);
            }
            _ => {}
        }
    })
}

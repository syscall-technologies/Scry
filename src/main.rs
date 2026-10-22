mod aad;
mod agent;
mod auth;
mod clipboard;
mod connect;
mod control;
mod feed;
mod gateway;
mod icon;
mod keymap;
mod keys;
mod launcher;
mod mcp;
mod overlay;
mod platform;
mod probe;
mod rdpfile;
mod session;
mod viewer;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod weblogin;
mod wst;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Cross-platform Windows 365 / AVD Cloud PC client: CLI, agent control (MCP) and a live viewer.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in with Entra ID and store a refresh token.
    Login,
    /// Write the app icon as a PNG (used by packaging).
    #[command(hide = true)]
    Icon { out: PathBuf },
    /// List entitled Cloud PCs / desktops and download their .rdp files.
    List,
    /// Ask the AVD gateway for a connection and request the session-host SSO token.
    Probe {
        /// Resource index printed by `list`.
        index: usize,
    },
    /// Connect once and save a screenshot of the Cloud PC to a PNG.
    Connect {
        /// Resource index printed by `list`.
        index: usize,
        /// PNG output path.
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
        /// How long to receive screen updates before saving.
        #[arg(short, long, default_value_t = 20)]
        seconds: u64,
    },
    /// Internal: sign-in window used by `login` (prints the redirect URL).
    #[command(hide = true)]
    WebviewLogin { url: String },
    /// Connect and stay connected: live viewer, agent control (MCP) and a local HTTP API.
    Session {
        /// Resource index printed by `list`.
        index: usize,
        /// Port of the control API on 127.0.0.1.
        #[arg(short, long, default_value_t = 7878)]
        port: u16,
        /// Port of the MCP server (http://127.0.0.1:<port>/mcp).
        #[arg(long, default_value_t = 7879)]
        mcp_port: u16,
        /// Open a window showing the session; local mouse/keyboard input goes to the Cloud PC.
        #[arg(long)]
        viewer: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    // No subcommand (e.g. launched from the desktop): open the GUI launcher.
    match Cli::parse().cmd {
        None => launcher::run(),
        Some(cmd) => run(cmd),
    }
}

fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Login => auth::login(),
        Cmd::Icon { out } => icon::write(&out),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Cmd::WebviewLogin { url } => weblogin::run_window(url),
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Cmd::WebviewLogin { .. } => anyhow::bail!("embedded sign-in is not available on this platform"),
        Cmd::List => feed::list(),
        Cmd::Probe { index } => probe::probe(index),
        Cmd::Connect { index, output, seconds } => connect::connect(index, &output, seconds),
        Cmd::Session {
            index,
            port,
            mcp_port,
            viewer,
        } => {
            let clipboard = clipboard::Clipboard::new();
            let (result, framed) = connect::open(index, Some(clipboard.backend()))?;
            let (width, height) = (result.desktop_size.width, result.desktop_size.height);
            let handle = session::spawn(result, framed, clipboard);
            let control_handle = handle.clone();
            std::thread::spawn(move || control::serve(control_handle, port));
            if viewer {
                // The window needs the main thread; closing it ends the session.
                let mcp_handle = handle.clone();
                std::thread::spawn(move || mcp::serve(mcp_handle, mcp_port));
                viewer::run(handle, width, height)
            } else {
                mcp::serve(handle, mcp_port)
            }
        }
    }
}

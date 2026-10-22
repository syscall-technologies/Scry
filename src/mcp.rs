//! MCP server (streamable HTTP on 127.0.0.1) exposing the session to AI agents.

use crate::session::Handle;
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, model::*, schemars, tool, tool_handler,
    tool_router,
};
use serde::Deserialize;

const INSTRUCTIONS: &str = "Controls a remote Windows 365 Cloud PC. Call `screenshot` to see the screen \
(at most 1280 px wide; the size can change when the user resizes their viewer). All x/y coordinates \
are pixels in the most recent screenshot. After each action, take a new screenshot to check the result \
before continuing.";

#[derive(Deserialize, schemars::JsonSchema)]
struct ClickArgs {
    /// X in screenshot pixels.
    x: u16,
    /// Y in screenshot pixels.
    y: u16,
    /// "left" (default), "right" or "middle".
    button: Option<String>,
    /// Double-click instead of a single click.
    #[serde(default)]
    double: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PointArgs {
    /// X in screenshot pixels.
    x: u16,
    /// Y in screenshot pixels.
    y: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ScrollArgs {
    /// X in screenshot pixels where the wheel is turned.
    x: u16,
    /// Y in screenshot pixels where the wheel is turned.
    y: u16,
    /// Wheel notches; positive scrolls down, negative scrolls up.
    dy: i16,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TypeArgs {
    /// Text to type; "\n" presses Enter. About 18 characters per second.
    text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PasteArgs {
    /// Text to paste; inserted instantly via the clipboard and Ctrl+V.
    text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct KeyArgs {
    /// Key or combo joined with "+", e.g. "enter", "win+r", "ctrl+shift+esc", "alt+f4", "pagedown".
    combo: String,
}

#[derive(Clone)]
struct Tools {
    handle: Handle,
}

/// Session calls block (they pace input), so run them off the async executor.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}

fn outcome(result: Result<()>) -> Result<CallToolResult, McpError> {
    Ok(match result {
        Ok(()) => CallToolResult::success(vec![ContentBlock::text("ok")]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
    })
}

#[tool_router]
impl Tools {
    fn new(handle: Handle) -> Self {
        Self { handle }
    }

    #[tool(description = "Capture the current Cloud PC screen as a PNG (at most 1280 px wide).")]
    async fn screenshot(&self) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        Ok(match blocking(move || h.screenshot()).await {
            Ok(png) => CallToolResult::success(vec![ContentBlock::image(STANDARD.encode(png), "image/png")]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
        })
    }

    #[tool(description = "Move the mouse to (x, y) and click.")]
    async fn click(&self, Parameters(a): Parameters<ClickArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.click(a.x, a.y, a.button.as_deref().unwrap_or("left"), a.double)).await)
    }

    #[tool(description = "Move the mouse to (x, y) without clicking (e.g. to hover).")]
    async fn move_mouse(&self, Parameters(a): Parameters<PointArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.move_to(a.x, a.y)).await)
    }

    #[tool(description = "Turn the mouse wheel at (x, y).")]
    async fn scroll(&self, Parameters(a): Parameters<ScrollArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.scroll(a.x, a.y, a.dy)).await)
    }

    #[tool(description = "Type text into the focused window.")]
    async fn type_text(&self, Parameters(a): Parameters<TypeArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.type_text(&a.text)).await)
    }

    #[tool(
        description = "Paste text into the focused window via the Cloud PC clipboard (instant; prefer this \
over type_text for long text). Replaces the Cloud PC clipboard. Falls back to typing when clipboard \
redirection is disabled on the Cloud PC."
    )]
    async fn paste_text(&self, Parameters(a): Parameters<PasteArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.paste_text(&a.text)).await)
    }

    #[tool(description = "Read the text last copied on the Cloud PC (e.g. after selecting and pressing ctrl+c).")]
    async fn get_clipboard(&self) -> Result<CallToolResult, McpError> {
        Ok(match self.handle.remote_clipboard() {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text.unwrap_or_default())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
        })
    }

    #[tool(description = "Press a key or key combination.")]
    async fn key(&self, Parameters(a): Parameters<KeyArgs>) -> Result<CallToolResult, McpError> {
        let h = self.handle.clone();
        outcome(blocking(move || h.key_combo(&a.combo)).await)
    }
}

#[tool_handler]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

pub fn serve(handle: Handle, port: u16) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let service = StreamableHttpService::new(
            move || Ok(Tools::new(handle.clone())),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        println!("MCP server on http://127.0.0.1:{port}/mcp");
        axum::serve(listener, router).await?;
        Ok(())
    })
}

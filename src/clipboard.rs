//! Text clipboard over CLIPRDR: the local (Mac) clipboard and the Cloud PC's stay in sync, and the
//! agent can paste text instantly or read what was copied remotely.
//!
//! The CLIPRDR backend runs inside the session thread; it and the local watcher thread talk to the
//! session loop through [`ClipboardMessage`]s, which the loop applies to the CLIPRDR channel.

use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp::core::impl_as_any;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

/// How often the local clipboard is checked for new text.
const WATCH_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct ClipState {
    /// Text offered to the Cloud PC when it pastes (local clipboard or an agent paste).
    pub outgoing: Option<String>,
    /// Last text copied on the Cloud PC.
    pub remote: Option<String>,
    /// Last local clipboard text we saw or wrote, so remote text isn't echoed back.
    local_seen: Option<String>,
    /// The Cloud PC opened the clipboard channel (it doesn't when an admin policy disables it).
    pub ready: bool,
}

pub type Shared = Arc<Mutex<ClipState>>;

/// Clipboard plumbing shared by the CLIPRDR backend, the local watcher and the session.
pub struct Clipboard {
    pub state: Shared,
    pub tx: Sender<ClipboardMessage>,
    pub rx: Receiver<ClipboardMessage>,
}

impl Clipboard {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: Shared::default(),
            tx,
            rx,
        }
    }

    pub fn backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(Backend {
            state: Arc::clone(&self.state),
            tx: self.tx.clone(),
        })
    }

    /// Watches the local clipboard and offers new text to the Cloud PC.
    pub fn watch_local(&self) {
        let (state, tx) = (Arc::clone(&self.state), self.tx.clone());
        thread::spawn(move || {
            loop {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    let mut s = state.lock().expect("clipboard lock");
                    if s.local_seen.as_ref() != Some(&text) {
                        s.local_seen = Some(text.clone());
                        debug!(
                            chars = text.chars().count(),
                            "clipboard: local text changed, offering it"
                        );
                        s.outgoing = Some(text);
                        drop(s);
                        if tx.send(offer_text()).is_err() {
                            return; // session ended
                        }
                    }
                }
                thread::sleep(WATCH_EVERY);
            }
        });
    }
}

/// Announces that text is available, without sending it yet (CLIPRDR delayed rendering).
pub fn offer_text() -> ClipboardMessage {
    ClipboardMessage::SendInitiateCopy(vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
}

#[derive(Debug)]
struct Backend {
    state: Shared,
    tx: Sender<ClipboardMessage>,
}

impl_as_any!(Backend);

impl Backend {
    fn send(&self, message: ClipboardMessage) {
        let _ = self.tx.send(message);
    }
}

impl CliprdrBackend for Backend {
    fn temporary_directory(&self) -> &str {
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        info!("clipboard: channel ready");
        self.state.lock().expect("clipboard lock").ready = true;
    }

    fn on_request_format_list(&mut self) {
        debug!("clipboard: server asked for our format list");
        let has_text = self.state.lock().expect("clipboard lock").outgoing.is_some();
        self.send(if has_text {
            offer_text()
        } else {
            ClipboardMessage::SendInitiateCopy(Vec::new())
        });
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        debug!(?capabilities, "clipboard: capabilities negotiated");
    }

    fn on_format_list_response(&mut self, ok: bool) {
        debug!(ok, "clipboard: server answered our format list");
    }

    /// Something was copied on the Cloud PC: fetch it right away if it is text.
    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        debug!(formats = ?available_formats.iter().map(|f| f.id).collect::<Vec<_>>(), "clipboard: copied on the Cloud PC");
        if available_formats
            .iter()
            .any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT)
        {
            self.send(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
        }
    }

    /// The Cloud PC is pasting: hand over our text.
    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        debug!(format = ?request.format, "clipboard: Cloud PC is pasting, sending our text");
        let text = self.state.lock().expect("clipboard lock").outgoing.clone();
        let response = match text {
            Some(text) if request.format == ClipboardFormatId::CF_UNICODETEXT => {
                OwnedFormatDataResponse::new_unicode_string(&text)
            }
            _ => OwnedFormatDataResponse::new_error(),
        };
        self.send(ClipboardMessage::SendFormatData(response));
    }

    /// Text copied on the Cloud PC arrived: keep it for the agent and put it on the local clipboard.
    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            warn!("clipboard: Cloud PC returned an error instead of the copied text");
            return;
        }
        let Ok(text) = response.to_unicode_string() else { return };
        let text = text.trim_end_matches('\0').to_owned();
        debug!(
            chars = text.chars().count(),
            "clipboard: received text from the Cloud PC"
        );
        {
            let mut s = self.state.lock().expect("clipboard lock");
            s.remote = Some(text.clone());
            s.local_seen = Some(text.clone());
        }
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            warn!("clipboard: could not set local clipboard: {e}");
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

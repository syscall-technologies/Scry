//! Desktop launcher (all platforms): a small native window that signs in, lists the Cloud PCs and
//! opens the viewer. Shown when `scry` runs with no subcommand.
//!
//! Built on egui/eframe (wgpu) rather than a WebView, so it needs no GTK/WebKit and works the same
//! on macOS, Windows and Linux. It shells out to `scry session <i> --viewer` as a child process,
//! so the viewer keeps its own event loop.

use crate::feed;
use anyhow::{Result, anyhow};
use eframe::egui;
use egui::{Color32, Margin, RichText, Stroke, vec2};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};

// Deep-slate dark palette (developer-tool identity), green CTA.
const BG: Color32 = Color32::from_rgb(0x0f, 0x17, 0x2a);
const CARD: Color32 = Color32::from_rgb(0x1b, 0x23, 0x36);
const BORDER: Color32 = Color32::from_rgb(0x2b, 0x37, 0x4f);
const FG: Color32 = Color32::from_rgb(0xf8, 0xfa, 0xfc);
const MUTED: Color32 = Color32::from_rgb(0x94, 0xa3, 0xb8);
const ACCENT: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
const ON_ACCENT: Color32 = Color32::from_rgb(0x07, 0x1b, 0x12);
const AMBER: Color32 = Color32::from_rgb(0xf0, 0xa0, 0x30);
const RED: Color32 = Color32::from_rgb(0xef, 0x44, 0x44);

/// Graphics mode offered in the launcher.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphicsPref {
    /// RemoteFX — sharpest text (default).
    Sharp,
    /// H.264 — smoother motion, much less bandwidth, slightly softer text.
    Fast,
}

impl GraphicsPref {
    fn env(self) -> &'static str {
        match self {
            GraphicsPref::Sharp => "rfx",
            GraphicsPref::Fast => "h264",
        }
    }
}

/// How an agent adds scry's MCP server (live while a Cloud PC viewer is open).
const MCP_URL: &str = "http://127.0.0.1:7879/mcp";
const MCP_ADD_CMD: &str = "claude mcp add --transport http scry http://127.0.0.1:7879/mcp";

/// Results from the sign-in/list worker thread.
enum Msg {
    Status(String),
    /// (index, title) per Cloud PC.
    Loaded(Vec<(usize, String)>),
    Error(String),
}

pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("scry")
            .with_inner_size([440.0, 500.0])
            .with_min_inner_size([360.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "scry",
        options,
        Box::new(|cc| {
            apply_style(&cc.egui_ctx);
            crate::platform::set_dock_icon();
            let app = Launcher::new(cc.egui_ctx.clone());
            if !app.signed_out {
                app.start_load();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow!("launcher: {e}"))
}

struct Launcher {
    ctx: egui::Context,
    msg_tx: Sender<Msg>,
    msg_rx: Receiver<Msg>,
    status: String,
    pcs: Vec<(usize, String)>,
    error: Option<String>,
    /// Cloud PCs whose viewer is currently open.
    open: HashSet<usize>,
    /// Viewer-closed notifications (carrying the Cloud PC index).
    closed_tx: Sender<usize>,
    closed_rx: Receiver<usize>,
    /// No sign-in stored: show a Sign in button instead of auto-connecting.
    signed_out: bool,
    /// Graphics mode passed to the viewer on Connect.
    graphics: GraphicsPref,
    /// Transient "Copied" confirmation for the MCP command.
    copied: bool,
}

impl Launcher {
    fn new(ctx: egui::Context) -> Self {
        let (msg_tx, msg_rx) = channel();
        let (closed_tx, closed_rx) = channel();
        Self {
            ctx,
            msg_tx,
            msg_rx,
            status: "Starting…".into(),
            pcs: Vec::new(),
            error: None,
            open: HashSet::new(),
            closed_tx,
            closed_rx,
            signed_out: !crate::auth::is_signed_in(),
            graphics: GraphicsPref::Sharp,
            copied: false,
        }
    }

    /// Signs in and lists Cloud PCs on a worker thread, waking the UI as it progresses.
    fn start_load(&self) {
        let (tx, ctx) = (self.msg_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Status("Signing in…".into()));
            ctx.request_repaint();
            let msg = match feed::collect() {
                Ok(pcs) => Msg::Loaded(pcs.into_iter().map(|p| (p.index, p.title)).collect()),
                Err(e) => Msg::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                Msg::Status(s) => self.status = s,
                Msg::Loaded(pcs) => {
                    self.error = None;
                    self.status = if pcs.len() == 1 {
                        "Ready.".into()
                    } else {
                        format!("{} Cloud PCs", pcs.len())
                    };
                    self.pcs = pcs;
                }
                Msg::Error(e) => {
                    self.status = "Something went wrong.".into();
                    self.error = Some(e);
                }
            }
        }
        while let Ok(idx) = self.closed_rx.try_recv() {
            self.open.remove(&idx);
        }
    }
}

impl eframe::App for Launcher {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();

        egui::Frame::new().inner_margin(Margin::same(26)).show(ui, |ui| {
            // Wordmark + subtitle.
            ui.label(RichText::new("scry").monospace().size(34.0).strong().color(FG));
            ui.add_space(2.0);
            ui.label(RichText::new("Windows 365 · AVD Cloud PC").size(13.0).color(MUTED));

            ui.add_space(22.0);

            if self.signed_out {
                self.signed_out_body(ui);
            } else {
                self.connected_body(ui);
            }

            // Footer pinned to the bottom: hint on the left, Sign out on the right.
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("The viewer opens in its own window.")
                            .size(11.0)
                            .color(MUTED),
                    );
                    if !self.signed_out {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = egui::Button::new(RichText::new("Sign out").size(12.0).color(MUTED))
                                .fill(CARD)
                                .stroke(Stroke::new(1.0, BORDER))
                                .corner_radius(8.0);
                            if ui.add(btn).clicked() {
                                let _ = crate::auth::logout();
                                self.pcs.clear();
                                self.open.clear();
                                self.error = None;
                                self.status.clear();
                                self.signed_out = true;
                            }
                        });
                    }
                });
            });
        });

        // Keep polling the worker/viewer channels even when idle.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
}

impl Launcher {
    /// Signed-out screen: a prompt and a Sign in button.
    fn signed_out_body(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Sign in to your Windows 365 account to continue.")
                .size(14.0)
                .color(MUTED),
        );
        ui.add_space(16.0);
        if accent_button(ui, "Sign in with Microsoft", false).clicked() {
            self.signed_out = false;
            self.error = None;
            self.status = "Signing in…".into();
            self.start_load();
        }
    }

    /// Signed-in screen: status, any error, and the Cloud PC list.
    fn connected_body(&mut self, ui: &mut egui::Ui) {
        self.status_row(ui);
        ui.add_space(18.0);

        if let Some(error) = self.error.clone() {
            error_card(ui, &error);
            if accent_button(ui, "Try again", false).clicked() {
                self.error = None;
                self.pcs.clear();
                self.status = "Signing in…".into();
                self.start_load();
            }
            ui.add_space(12.0);
        }

        for (index, title) in self.pcs.clone() {
            self.pc_card(ui, index, &title);
            ui.add_space(10.0);
        }

        if !self.pcs.is_empty() {
            ui.add_space(6.0);
            self.graphics_toggle(ui);
            ui.add_space(10.0);
            self.mcp_card(ui);
        }
    }

    /// Sharp (RemoteFX) vs Fast (H.264) selector.
    fn graphics_toggle(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Graphics").size(13.0).color(MUTED));
            ui.selectable_value(&mut self.graphics, GraphicsPref::Sharp, "Sharp");
            ui.selectable_value(&mut self.graphics, GraphicsPref::Fast, "Fast · H.264");
        });
        let hint = match self.graphics {
            GraphicsPref::Sharp => "Lossless — sharpest text, more bandwidth.",
            GraphicsPref::Fast => "H.264 — smoother motion, far less bandwidth, slightly softer text.",
        };
        ui.label(RichText::new(hint).size(11.0).color(MUTED));
    }

    /// Copiable MCP endpoint so an AI (e.g. Claude Code) can drive the Cloud PC.
    fn mcp_card(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Agent access (MCP)").size(13.0).color(FG).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.copied { "Copied ✓" } else { "Copy" };
                        let btn = egui::Button::new(RichText::new(label).size(12.0).color(ON_ACCENT).strong())
                            .fill(ACCENT)
                            .corner_radius(8.0);
                        if ui.add(btn).clicked() {
                            self.copied = arboard::Clipboard::new()
                                .and_then(|mut c| c.set_text(MCP_ADD_CMD))
                                .is_ok();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(RichText::new(MCP_URL).size(12.0).monospace().color(MUTED));
                ui.label(
                    RichText::new("Live while a Cloud PC viewer is open. Copy adds it to Claude Code.")
                        .size(11.0)
                        .color(MUTED),
                );
            });
    }

    /// Status dot + text; the dot colour reflects the phase.
    fn status_row(&self, ui: &mut egui::Ui) {
        let dot = if self.error.is_some() {
            RED
        } else if self.pcs.is_empty() {
            AMBER
        } else {
            ACCENT
        };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, dot);
            ui.add_space(2.0);
            ui.label(RichText::new(&self.status).size(14.0).color(MUTED));
        });
    }

    /// A Cloud PC row: name + specs on the left, Connect on the right.
    fn pc_card(&mut self, ui: &mut egui::Ui, index: usize, title: &str) {
        let is_open = self.open.contains(&index);
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (name, specs) = split_title(title);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).size(15.0).color(FG).strong());
                        if let Some(specs) = specs {
                            ui.add_space(1.0);
                            ui.label(RichText::new(specs).size(12.0).color(MUTED));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if is_open { "Open" } else { "Connect" };
                        if accent_button(ui, label, is_open).clicked() && !is_open {
                            match open_viewer(index, self.closed_tx.clone(), self.graphics) {
                                Ok(()) => {
                                    self.open.insert(index);
                                    self.status = "Viewer open — close it to reconnect.".into();
                                }
                                Err(e) => self.error = Some(format!("{e:#}")),
                            }
                        }
                    });
                });
            });
    }
}

/// Sets the global dark style: fills, spacing and button padding.
fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = CARD;
        v.override_text_color = Some(FG);
        style.visuals = v;
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.button_padding = vec2(14.0, 8.0);
    });
}

/// A filled pill button in the accent colour (muted when `disabled`).
fn accent_button(ui: &mut egui::Ui, label: &str, disabled: bool) -> egui::Response {
    let (fill, text) = if disabled { (CARD, MUTED) } else { (ACCENT, ON_ACCENT) };
    let button = egui::Button::new(RichText::new(label).color(text).strong())
        .fill(fill)
        .stroke(Stroke::new(1.0, if disabled { BORDER } else { ACCENT }))
        .corner_radius(8.0)
        .min_size(vec2(96.0, 32.0));
    ui.add_enabled(!disabled, button)
}

fn error_card(ui: &mut egui::Ui, error: &str) {
    egui::Frame::new()
        .fill(Color32::from_rgb(0x2a, 0x17, 0x17))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0x5a, 0x24, 0x24)))
        .corner_radius(10.0)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.label(
                RichText::new(error)
                    .size(13.0)
                    .color(Color32::from_rgb(0xf0, 0xa8, 0xa8)),
            );
        });
    ui.add_space(10.0);
}

/// Splits a title with a trailing specs blob (e.g. "… 4vCPU/16GB/256GB") into name and specs.
fn split_title(title: &str) -> (&str, Option<String>) {
    if let Some(i) = title.find(|c: char| c.is_ascii_digit())
        && title[i..].contains("GB")
    {
        let name = title[..i].trim_end();
        let specs = title[i..].replace('/', " · ");
        if !name.is_empty() {
            return (name, Some(specs));
        }
    }
    (title, None)
}

/// Opens the viewer for Cloud PC `index` in a child process, notifying `closed` when it exits.
fn open_viewer(index: usize, closed: Sender<usize>, graphics: GraphicsPref) -> Result<()> {
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["session", &index.to_string(), "--viewer"])
        .env("SCRY_GRAPHICS", graphics.env())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = closed.send(index);
    });
    Ok(())
}

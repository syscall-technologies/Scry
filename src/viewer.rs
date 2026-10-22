//! Viewer window: shows the session framebuffer and forwards local mouse/keyboard input.
//! Runs on the main thread (required by macOS); the agent keeps driving the same session.
//! The frame is uploaded at the Cloud PC's resolution and scaled to the window on the GPU
//! (`pixels`/wgpu); the agent banner is drawn into the frame before upload.

use crate::agent::Mode;
use crate::keymap::map_key_code;
use crate::overlay::{self, Button, Overlay, Rect};
use crate::session::{Cursor, Handle};
use anyhow::Result;
use ironrdp::input::{MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use pixels::{Pixels, ScalingMode, SurfaceTexture};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, CustomCursor, Window, WindowId};

/// Banner clicks are ignored this long after its buttons change, so a double-click on Stop
/// can't land on the "Allow agent" button that replaces it.
const BUTTON_COOLDOWN: Duration = Duration::from_millis(600);
/// The remote resolution follows the window once it has stopped changing for this long.
const RESIZE_SETTLE: Duration = Duration::from_millis(300);
/// Trackpad pixels per wheel notch.
const PIXELS_PER_NOTCH: f64 = 40.0;

struct App {
    handle: Handle,
    window: Option<(Arc<Window>, Pixels<'static>)>,
    /// Size of the `pixels` buffer, which follows the Cloud PC resolution.
    buffer_size: (u32, u32),
    drawn_seq: u64,
    pressed: HashSet<Scancode>,
    scroll_acc: f64,
    cursor: PhysicalPosition<f64>,
    /// Agent banner buttons from the last draw, and what that banner showed.
    overlay: Overlay,
    buttons: Vec<(Button, Rect)>,
    drawn_overlay: String,
    /// Swallow the release of a click that landed on the banner.
    swallow_release: bool,
    last_banner_click: Instant,
    /// Window size (physical) waiting to settle before the remote resolution follows it.
    pending_resize: Option<(PhysicalSize<u32>, Instant)>,
    /// Last remote-cursor sequence applied to the window.
    cursor_seq: u64,
    stats: RedrawStats,
}

pub fn run(handle: Handle, width: u16, height: u16) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        handle,
        window: None,
        buffer_size: (u32::from(width), u32::from(height)),
        drawn_seq: 0,
        pressed: HashSet::new(),
        scroll_acc: 0.0,
        cursor: PhysicalPosition::new(0.0, 0.0),
        overlay: Overlay::new(),
        buttons: Vec::new(),
        drawn_overlay: String::new(),
        swallow_release: false,
        last_banner_click: Instant::now(),
        pending_resize: None,
        cursor_seq: 0,
        stats: RedrawStats::from_env(),
    };
    let size = LogicalSize::new(f64::from(width), f64::from(height));
    event_loop.run_app(&mut AppWithSize { app: &mut app, size })?;
    Ok(())
}

/// Carries the initial window size into `resumed`.
struct AppWithSize<'a> {
    app: &'a mut App,
    size: LogicalSize<f64>,
}

impl ApplicationHandler for AppWithSize<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("scry - Cloud PC")
            .with_inner_size(self.size)
            .with_resizable(true);
        let (bw, bh) = self.app.buffer_size;
        let created = event_loop
            .create_window(attrs)
            .map_err(anyhow::Error::from)
            .and_then(|w| {
                let window = Arc::new(w);
                let size = window.inner_size();
                let surface = SurfaceTexture::new(size.width.max(1), size.height.max(1), Arc::clone(&window));
                let mut pixels = Pixels::new(bw, bh, surface)?;
                // Fill the window (keeping aspect) while the Cloud PC resolution catches up after a resize.
                pixels.set_scaling_mode(ScalingMode::Fill);
                Ok((window, pixels))
            });
        match created {
            Ok(w) => {
                crate::platform::set_dock_icon();
                self.app.window = Some(w);
            }
            Err(e) => {
                eprintln!("viewer: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Close instead of showing a frozen picture once the session is gone.
        if let Some(reason) = self.app.handle.frame().lock().expect("frame lock").ended.clone() {
            eprintln!("viewer: closing, session ended: {reason}");
            event_loop.exit();
            return;
        }
        self.app.apply_settled_resize();
        self.app.apply_cursor(event_loop);
        // Poll the shared framebuffer and agent state ~120 times a second; redraw on change.
        if let Some((window, _)) = &self.app.window
            && (self.app.handle.frame().lock().expect("frame lock").seq != self.app.drawn_seq
                || self.app.overlay_signature() != self.app.drawn_overlay)
        {
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(8)));
    }
}

impl App {
    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.pending_resize = Some((size, Instant::now()));
                if let Some((window, pixels)) = &mut self.window
                    && size.width > 0
                    && size.height > 0
                {
                    if let Err(e) = pixels.resize_surface(size.width, size.height) {
                        eprintln!("viewer: resize failed: {e}");
                    }
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    eprintln!("viewer: redraw failed: {e:#}");
                }
            }
            WindowEvent::Focused(false) => self.release_keys(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.handle.agent().human_input();
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                if event.state == ElementState::Pressed && code == KeyCode::KeyV && self.cmd_shift_held() {
                    self.type_local_clipboard();
                    return;
                }
                let Some(scancode) = map_key_code(code) else { return };
                let op = if event.state == ElementState::Pressed {
                    self.pressed.insert(scancode);
                    Operation::KeyPressed(scancode)
                } else {
                    self.pressed.remove(&scancode);
                    Operation::KeyReleased(scancode)
                };
                self.handle.send_input(vec![op]);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.on_banner() {
                    return;
                }
                if let Some(pos) = self.to_remote(position) {
                    self.handle.send_input(vec![Operation::MouseMove(pos)]);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed && self.on_banner() {
                    self.swallow_release = true;
                    self.press_banner();
                    return;
                }
                if state == ElementState::Released && self.swallow_release {
                    self.swallow_release = false;
                    return;
                }
                if state == ElementState::Pressed {
                    self.handle.agent().human_input();
                }
                let button = match button {
                    WinitButton::Left => MouseButton::Left,
                    WinitButton::Right => MouseButton::Right,
                    WinitButton::Middle => MouseButton::Middle,
                    _ => return,
                };
                let op = match state {
                    ElementState::Pressed => Operation::MouseButtonPressed(button),
                    ElementState::Released => Operation::MouseButtonReleased(button),
                };
                self.handle.send_input(vec![op]);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle.agent().human_input();
                // Positive = scroll up in both winit and RDP.
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(p) => {
                        self.scroll_acc += p.y / PIXELS_PER_NOTCH;
                        let whole = self.scroll_acc.trunc();
                        self.scroll_acc -= whole;
                        whole
                    }
                };
                if notches != 0.0 {
                    let units = (notches * 120.0).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
                    self.handle.send_input(vec![Operation::WheelRotations(WheelRotations {
                        is_vertical: true,
                        rotation_units: units,
                    })]);
                }
            }
            _ => {}
        }
    }

    /// Once the window size has settled, asks the Cloud PC for a matching resolution
    /// (in points, so a Retina window doesn't quadruple the pixels to stream).
    fn apply_settled_resize(&mut self) {
        let Some((size, at)) = self.pending_resize else { return };
        let Some((window, _)) = &self.window else { return };
        if at.elapsed() < RESIZE_SETTLE {
            return;
        }
        self.pending_resize = None;
        let logical = size.to_logical::<f64>(window.scale_factor());
        self.handle
            .resize(logical.width.round() as u32, logical.height.round() as u32);
    }

    /// Applies the latest remote pointer as the window's cursor (shape, hidden or default).
    fn apply_cursor(&mut self, event_loop: &ActiveEventLoop) {
        let Some((window, _)) = &self.window else { return };
        enum Apply {
            Hide,
            Shape(winit::window::CustomCursorSource),
            Default,
        }
        let apply = {
            let state = self.handle.cursor().lock().expect("cursor lock");
            if state.seq == self.cursor_seq {
                return;
            }
            self.cursor_seq = state.seq;
            match &state.cursor {
                Some(Cursor::Hidden) => Apply::Hide,
                Some(Cursor::Shape {
                    rgba,
                    width,
                    height,
                    hotspot_x,
                    hotspot_y,
                }) => match CustomCursor::from_rgba(rgba.clone(), *width, *height, *hotspot_x, *hotspot_y) {
                    Ok(source) => Apply::Shape(source),
                    Err(_) => Apply::Default,
                },
                Some(Cursor::Default) | None => Apply::Default,
            }
        };
        match apply {
            Apply::Hide => window.set_cursor_visible(false),
            Apply::Shape(source) => {
                window.set_cursor(event_loop.create_custom_cursor(source));
                window.set_cursor_visible(true);
            }
            Apply::Default => {
                window.set_cursor(CursorIcon::Default);
                window.set_cursor_visible(true);
            }
        }
    }

    /// What the agent banner should show now; empty when hidden.
    fn overlay_signature(&self) -> String {
        let s = self.handle.agent().snapshot();
        if !overlay::visible(&s) {
            return String::new();
        }
        let (label, done, total) = s
            .activity
            .as_ref()
            .map_or(("", 0, 0), |a| (a.label.as_str(), a.done, a.total));
        format!("{:?}|{label}|{done}|{total}", s.mode)
    }

    /// Cursor position in frame (Cloud PC) pixels, clamped to the frame.
    fn cursor_in_frame(&self) -> Option<(usize, usize)> {
        let (_, pixels) = self.window.as_ref()?;
        let pos = (self.cursor.x as f32, self.cursor.y as f32);
        Some(
            pixels
                .window_pos_to_pixel(pos)
                .unwrap_or_else(|p| pixels.clamp_pixel_pos(p)),
        )
    }

    /// Frame pixels per logical window point (1 when the Cloud PC follows the window size).
    fn overlay_scale(&self) -> usize {
        let Some((window, _)) = &self.window else { return 1 };
        let logical_w = window.inner_size().to_logical::<f64>(window.scale_factor()).width;
        (f64::from(self.buffer_size.0) / logical_w.max(1.0)).round().max(1.0) as usize
    }

    fn on_banner(&self) -> bool {
        !self.drawn_overlay.is_empty()
            && self
                .cursor_in_frame()
                .is_some_and(|(_, y)| y < overlay::height(self.overlay_scale()))
    }

    fn press_banner(&mut self) {
        if self.last_banner_click.elapsed() < BUTTON_COOLDOWN {
            return;
        }
        self.last_banner_click = Instant::now();
        let Some((x, y)) = self.cursor_in_frame() else { return };
        let hit = self.buttons.iter().find(|(_, r)| r.contains(x as f64, y as f64));
        let agent = self.handle.agent();
        match hit.map(|(b, _)| *b) {
            Some(Button::Pause) => agent.set_mode(Mode::Paused),
            Some(Button::Resume | Button::Allow) => agent.set_mode(Mode::Running),
            Some(Button::Stop) => agent.set_mode(Mode::Stopped),
            None => {}
        }
    }

    /// ⌘ (sent as the Windows key) and ⇧ are both held.
    fn cmd_shift_held(&self) -> bool {
        let held = |codes: [KeyCode; 2]| {
            codes
                .iter()
                .filter_map(|&c| map_key_code(c))
                .any(|s| self.pressed.contains(&s))
        };
        held([KeyCode::SuperLeft, KeyCode::SuperRight]) && held([KeyCode::ShiftLeft, KeyCode::ShiftRight])
    }

    /// ⌘⇧V: types the local clipboard text into the Cloud PC as key presses (local -> Cloud PC
    /// only), for Cloud PCs where clipboard redirection is disabled.
    fn type_local_clipboard(&mut self) {
        // ⌘⇧ are already down on the Cloud PC: release them first, or every typed character would
        // become a Win+Shift shortcut. Tapping Ctrl first stops the Win release from opening Start.
        let ctrl = map_key_code(KeyCode::ControlLeft).expect("ctrl is mapped");
        let mut ops = vec![Operation::KeyPressed(ctrl), Operation::KeyReleased(ctrl)];
        ops.extend(self.pressed.drain().map(Operation::KeyReleased));
        self.handle.send_input(ops);

        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(text) if !text.is_empty() => {
                let handle = self.handle.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle.type_text(&text) {
                        eprintln!("viewer: typing the clipboard stopped: {e:#}");
                    }
                });
            }
            _ => eprintln!("viewer: the local clipboard has no text to type"),
        }
    }

    /// Releases keys still held when the window loses focus (e.g. Cmd+Tab away).
    fn release_keys(&mut self) {
        let ops = self.pressed.drain().map(Operation::KeyReleased).collect::<Vec<_>>();
        if !ops.is_empty() {
            self.handle.send_input(ops);
        }
    }

    /// Window pixel position -> remote desktop coordinates.
    fn to_remote(&self, position: PhysicalPosition<f64>) -> Option<MousePosition> {
        let (_, pixels) = self.window.as_ref()?;
        let pos = (position.x as f32, position.y as f32);
        let (x, y) = pixels
            .window_pos_to_pixel(pos)
            .unwrap_or_else(|p| pixels.clamp_pixel_pos(p));
        Some(MousePosition {
            x: u16::try_from(x).ok()?,
            y: u16::try_from(y).ok()?,
        })
    }

    /// Copies the latest frame into the GPU buffer, draws the banner on top and presents.
    fn redraw(&mut self) -> Result<()> {
        let overlay_scale = self.overlay_scale();
        let Some((_, pixels)) = self.window.as_mut() else {
            return Ok(());
        };
        let started = Instant::now();
        {
            let frame = self.handle.frame().lock().expect("frame lock");
            if frame.width == 0 {
                return Ok(());
            }
            let size = (u32::from(frame.width), u32::from(frame.height));
            if size != self.buffer_size {
                pixels.resize_buffer(size.0, size.1)?;
                self.buffer_size = size;
            }
            pixels.frame_mut().copy_from_slice(&frame.rgba);
            self.drawn_seq = frame.seq;
        }

        let agent = self.handle.agent().snapshot();
        let (w, h) = (self.buffer_size.0 as usize, self.buffer_size.1 as usize);
        self.buttons = if overlay::visible(&agent) {
            self.overlay.draw(pixels.frame_mut(), w, h, overlay_scale, &agent)
        } else {
            Vec::new()
        };
        pixels.render()?;
        self.drawn_overlay = self.overlay_signature();
        self.stats.redrawn(started.elapsed());
        Ok(())
    }
}

/// Redraw counters printed every 5 s when `SCRY_STATS=1`.
struct RedrawStats {
    enabled: bool,
    since: Instant,
    count: u32,
    total: Duration,
}

impl RedrawStats {
    fn from_env() -> Self {
        let enabled = std::env::var("SCRY_STATS").is_ok_and(|v| v == "1");
        Self {
            enabled,
            since: Instant::now(),
            count: 0,
            total: Duration::ZERO,
        }
    }

    fn redrawn(&mut self, took: Duration) {
        self.count += 1;
        self.total += took;
        let secs = self.since.elapsed().as_secs_f64();
        if self.enabled && secs >= 5.0 {
            eprintln!(
                "stats: viewer {:.0} redraws/s, avg {:.2} ms",
                f64::from(self.count) / secs,
                self.total.as_secs_f64() * 1000.0 / f64::from(self.count)
            );
            *self = Self::from_env();
        }
    }
}

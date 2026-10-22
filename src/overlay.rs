//! Agent banner drawn over the viewer: status dot, current action, Pause/Stop buttons and a thin
//! progress line. Rendered straight into the RGBA frame; text uses the bundled Ubuntu Light
//! font (from egui's `epaint_default_fonts`) rasterized with `fontdue`.

use crate::agent::{Mode, Snapshot};
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;
use std::time::Duration;

/// How long the banner stays up after the agent's last action.
const LINGER: Duration = Duration::from_millis(1500);
/// Sizes in logical pixels; multiplied by the window scale factor.
const BAR_H: usize = 24;
const TEXT_PX: f32 = 13.0;
const BUTTON_H: usize = 18;
const PAD: usize = 10;

const BAR_BG: u32 = 0x141414;
const BAR_ALPHA: u32 = 215;
const TEXT: u32 = 0xeeeeee;
const MUTED: u32 = 0xa0a0a0;
const NEUTRAL_BTN: u32 = 0x555555;
const STOP_BTN: u32 = 0xb3362b;
const GO_BTN: u32 = 0x2f7d4f;
const PROGRESS: u32 = 0x4caf50;
const DOT_RUNNING: u32 = 0x4caf50;
const DOT_PAUSED: u32 = 0xf0a030;
const DOT_STOPPED: u32 = 0xe04a3c;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Pause,
    Resume,
    Stop,
    Allow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Icon {
    Pause,
    Play,
    Stop,
}

#[derive(Clone, Copy)]
pub struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x as f64 && py >= self.y as f64 && px < (self.x + self.w) as f64 && py < (self.y + self.h) as f64
    }
}

pub fn visible(s: &Snapshot) -> bool {
    s.mode != Mode::Running
        || s.activity
            .as_ref()
            .is_some_and(|a| a.finished.is_none_or(|t| t.elapsed() < LINGER))
}

/// Banner height in physical pixels for the window scale factor.
pub fn height(scale: usize) -> usize {
    BAR_H * scale
}

/// Blends 0xRRGGBB `color` over one RGBA pixel.
fn blend(dst: &mut [u8], color: u32, alpha: u32) {
    for (channel, shift) in dst.iter_mut().zip([16, 8, 0]) {
        let src = color >> shift & 0xff;
        *channel = ((src * alpha + u32::from(*channel) * (255 - alpha)) / 255) as u8;
    }
}

pub struct Overlay {
    font: Font,
    glyphs: HashMap<(char, u32), (Metrics, Vec<u8>)>,
}

struct Canvas<'a> {
    /// RGBA bytes.
    buf: &'a mut [u8],
    w: usize,
    h: usize,
}

impl Canvas<'_> {
    fn blend_px(&mut self, x: usize, y: usize, color: u32, alpha: u32) {
        if x < self.w && y < self.h && alpha > 0 {
            let i = (y * self.w + x) * 4;
            blend(&mut self.buf[i..i + 3], color, alpha);
        }
    }

    /// Rectangle with anti-aliased rounded corners of radius `r`.
    fn round_rect(&mut self, rect: Rect, r: f32, color: u32, alpha: u32) {
        let (x0, y0, x1, y1) = (
            rect.x as f32,
            rect.y as f32,
            (rect.x + rect.w) as f32,
            (rect.y + rect.h) as f32,
        );
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                // Distance outside the rounded corner, if this pixel is in a corner square.
                let dx = (x0 + r - cx).max(cx - (x1 - r)).max(0.0);
                let dy = (y0 + r - cy).max(cy - (y1 - r)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt();
                let coverage = if d == 0.0 { 1.0 } else { (r + 0.5 - d).clamp(0.0, 1.0) };
                self.blend_px(x, y, color, (alpha as f32 * coverage) as u32);
            }
        }
    }

    fn icon(&mut self, icon: Icon, x: usize, y: usize, size: usize, color: u32) {
        match icon {
            Icon::Pause => {
                let bar = (size / 3).max(1);
                self.round_rect(Rect { x, y, w: bar, h: size }, 0.0, color, 255);
                self.round_rect(
                    Rect {
                        x: x + size - bar,
                        y,
                        w: bar,
                        h: size,
                    },
                    0.0,
                    color,
                    255,
                );
            }
            Icon::Stop => self.round_rect(Rect { x, y, w: size, h: size }, size as f32 / 6.0, color, 255),
            Icon::Play => {
                // Right-pointing triangle, anti-aliased along the slanted edges.
                for row in 0..size {
                    let half = size as f32 / 2.0;
                    let width = (1.0 - ((row as f32 + 0.5) - half).abs() / half) * size as f32;
                    for col in 0..size {
                        let coverage = (width - col as f32).clamp(0.0, 1.0);
                        self.blend_px(x + col, y + row, color, (255.0 * coverage) as u32);
                    }
                }
            }
        }
    }
}

impl Overlay {
    pub fn new() -> Self {
        let font = Font::from_bytes(epaint_default_fonts::UBUNTU_LIGHT, FontSettings::default())
            .expect("bundled font is valid");
        Self {
            font,
            glyphs: HashMap::new(),
        }
    }

    fn glyph(&mut self, c: char, px: f32) -> &(Metrics, Vec<u8>) {
        self.glyphs
            .entry((c, px.to_bits()))
            .or_insert_with(|| self.font.rasterize(c, px))
    }

    fn text_width(&mut self, text: &str, px: f32) -> usize {
        text.chars()
            .map(|c| self.glyph(c, px).0.advance_width)
            .sum::<f32>()
            .ceil() as usize
    }

    /// Draws `text` with its baseline at `baseline`; returns the width used.
    fn text(&mut self, canvas: &mut Canvas, x: usize, baseline: usize, text: &str, px: f32, color: u32) -> usize {
        let mut pen = x as f32;
        for c in text.chars() {
            let (m, coverage) = self.glyph(c, px).clone();
            let gx = (pen + m.xmin as f32).round() as isize;
            let gy = baseline as isize - m.ymin as isize - m.height as isize;
            for row in 0..m.height {
                for col in 0..m.width {
                    let (tx, ty) = (gx + col as isize, gy + row as isize);
                    if tx >= 0 && ty >= 0 {
                        canvas.blend_px(
                            tx as usize,
                            ty as usize,
                            color,
                            u32::from(coverage[row * m.width + col]),
                        );
                    }
                }
            }
            pen += m.advance_width;
        }
        (pen - x as f32).ceil() as usize
    }

    /// Longest prefix of `text` that fits in `max` pixels, with an ellipsis if cut.
    fn fit(&mut self, text: &str, px: f32, max: usize) -> String {
        if self.text_width(text, px) <= max {
            return text.to_owned();
        }
        let mut out = String::new();
        for c in text.chars() {
            if self.text_width(&format!("{out}{c}…"), px) > max {
                break;
            }
            out.push(c);
        }
        format!("{}…", out.trim_end())
    }

    /// Draws the banner across the top of a `w`x`h` RGBA buffer; returns the clickable buttons.
    pub fn draw(&mut self, buf: &mut [u8], w: usize, h: usize, scale: usize, s: &Snapshot) -> Vec<(Button, Rect)> {
        let scale = scale.max(1);
        let mut canvas = Canvas { buf, w, h };
        let bar_h = height(scale);
        let px = TEXT_PX * scale as f32;
        let pad = PAD * scale;
        canvas.round_rect(
            Rect {
                x: 0,
                y: 0,
                w,
                h: bar_h,
            },
            0.0,
            BAR_BG,
            BAR_ALPHA,
        );

        // Buttons, right-aligned: rounded pills with an icon and a label.
        let buttons: &[(Button, Icon, &str, u32)] = match s.mode {
            Mode::Running => &[
                (Button::Pause, Icon::Pause, "Pause", NEUTRAL_BTN),
                (Button::Stop, Icon::Stop, "Stop", STOP_BTN),
            ],
            Mode::Paused => &[
                (Button::Resume, Icon::Play, "Resume", GO_BTN),
                (Button::Stop, Icon::Stop, "Stop", STOP_BTN),
            ],
            Mode::Stopped => &[(Button::Allow, Icon::Play, "Allow agent", GO_BTN)],
        };
        let button_h = BUTTON_H * scale;
        let icon = 8 * scale;
        let baseline = |top: usize, height: usize| top + (height + (px * 0.7) as usize) / 2;
        let mut rects = Vec::new();
        let mut right = w.saturating_sub(pad / 2);
        for &(button, kind, label, color) in buttons.iter().rev() {
            let bw = pad + icon + pad / 2 + self.text_width(label, px) + pad;
            let r = Rect {
                x: right.saturating_sub(bw),
                y: (bar_h - button_h) / 2,
                w: bw,
                h: button_h,
            };
            canvas.round_rect(r, button_h as f32 / 2.0, color, 255);
            canvas.icon(kind, r.x + pad, r.y + (button_h - icon) / 2, icon, TEXT);
            self.text(
                &mut canvas,
                r.x + pad + icon + pad / 2,
                baseline(r.y, button_h),
                label,
                px,
                TEXT,
            );
            rects.push((button, r));
            right = r.x.saturating_sub(pad / 2);
        }

        // Status dot, then "Agent · <action>", truncated before the buttons.
        let (dot_color, state) = match s.mode {
            Mode::Running => (DOT_RUNNING, "Agent"),
            Mode::Paused => (DOT_PAUSED, "Agent paused"),
            Mode::Stopped => (DOT_STOPPED, "Agent stopped"),
        };
        let dot = 8 * scale;
        canvas.round_rect(
            Rect {
                x: pad,
                y: (bar_h - dot) / 2,
                w: dot,
                h: dot,
            },
            dot as f32 / 2.0,
            dot_color,
            255,
        );
        let mut x = pad + dot + pad;
        let base = baseline(0, bar_h);
        x += self.text(&mut canvas, x, base, state, px, TEXT);
        let detail = match (s.mode, &s.activity) {
            (Mode::Stopped, _) => " · its actions are refused".to_owned(),
            (_, Some(a)) => format!(" · {}", a.label),
            (_, None) => String::new(),
        };
        let detail = self.fit(&detail, px, right.saturating_sub(x + pad));
        self.text(&mut canvas, x, base, &detail, px, MUTED);

        // Thin progress line along the bottom edge for multi-step actions.
        if let Some(a) = s.activity.as_ref().filter(|a| a.total > 1 && s.mode != Mode::Stopped) {
            let line = (2 * scale).max(2);
            canvas.round_rect(
                Rect {
                    x: 0,
                    y: bar_h - line,
                    w: w * a.done.min(a.total) / a.total,
                    h: line,
                },
                0.0,
                PROGRESS,
                255,
            );
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_mixes_channels() {
        let mut px = [0, 0, 0, 255];
        blend(&mut px[..3], 0xffffff, 255);
        assert_eq!(px, [255, 255, 255, 255]);
        let mut px = [0, 0, 0, 255];
        blend(&mut px[..3], 0x00ff00, 128);
        assert_eq!(px, [0, 128, 0, 255]);
    }

    #[test]
    fn square_rect_is_opaque() {
        let mut buf = vec![0u8; 2 * 2 * 4];
        Canvas {
            buf: &mut buf,
            w: 2,
            h: 2,
        }
        .round_rect(Rect { x: 0, y: 0, w: 2, h: 2 }, 0.0, 0xffffff, 255);
        assert!(buf.chunks(4).all(|px| px[..3] == [255, 255, 255]));
    }

    #[test]
    fn buttons_follow_mode() {
        let (w, h) = (1280, 800);
        let mut buf = vec![0u8; w * h * 4];
        let mut overlay = Overlay::new();
        let mut buttons = |mode| -> Vec<Button> {
            let s = Snapshot { mode, activity: None };
            overlay
                .draw(&mut buf, w, h, 1, &s)
                .into_iter()
                .map(|(b, _)| b)
                .collect()
        };
        assert_eq!(buttons(Mode::Running), vec![Button::Stop, Button::Pause]);
        assert_eq!(buttons(Mode::Paused), vec![Button::Stop, Button::Resume]);
        assert_eq!(buttons(Mode::Stopped), vec![Button::Allow]);
    }
}

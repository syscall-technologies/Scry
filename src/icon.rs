//! Generates the app icon (a rounded hexagon badge with a violet corner glow and a white "s")
//! as a PNG, so packaging scripts don't need any art assets. `scry icon <out.png>`.

use anyhow::Result;
use fontdue::{Font, FontSettings};
use std::path::Path;

const SIZE: u32 = 1024;
const FILL: [f32; 3] = [0x14 as f32, 0x1b as f32, 0x2e as f32]; // deep navy
const GLOW: [f32; 3] = [0x7c as f32, 0x5c as f32, 0xff as f32]; // violet
const FG: [f32; 3] = [255.0, 255.0, 255.0];

pub fn write(out: &Path) -> Result<()> {
    render().save(out)?;
    Ok(())
}

/// The icon as PNG bytes (used to set the macOS Dock icon at runtime).
#[cfg(target_os = "macos")]
pub fn png_bytes() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let _ = render().write_to(&mut buf, image::ImageFormat::Png);
    buf.into_inner()
}

/// Renders the full icon into an RGBA image.
pub fn render() -> image::RgbaImage {
    let n = SIZE as f32;
    let center = n / 2.0;
    let radius = n * 0.46; // hexagon circumradius
    let corner = n * 0.07; // rounded corners
    let mut img = image::RgbaImage::new(SIZE, SIZE);

    for (x, y, px) in img.enumerate_pixels_mut() {
        let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
        // Signed distance to the rounded hexagon; <0 inside.
        let d = hexagon_sdf(dx, dy, radius - corner) - corner;
        let coverage = (0.5 - d).clamp(0.0, 1.0);
        if coverage <= 0.0 {
            continue;
        }
        // Base navy fill, with a violet glow fading in from the top-left.
        let glow = (1.0 - ((x as f32 + y as f32) / (n * 0.9))).clamp(0.0, 1.0).powf(1.6);
        let color = mix(FILL, GLOW, glow * 0.85);
        *px = image::Rgba([color[0] as u8, color[1] as u8, color[2] as u8, (coverage * 255.0) as u8]);
    }

    draw_glyph(&mut img, center);
    img
}

/// Flat-top regular hexagon SDF (after Inigo Quilez). Negative inside.
fn hexagon_sdf(px: f32, py: f32, r: f32) -> f32 {
    let (kx, ky, kz) = (-0.866_025_4_f32, 0.5_f32, 0.577_350_26_f32);
    let (mut x, mut y) = (px.abs(), py.abs());
    let dot = (kx * x + ky * y).min(0.0);
    x -= 2.0 * dot * kx;
    y -= 2.0 * dot * ky;
    x -= x.clamp(-kz * r, kz * r);
    y -= r;
    (x * x + y * y).sqrt() * y.signum()
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Draws a white lowercase "s" centred in the badge.
fn draw_glyph(img: &mut image::RgbaImage, center: f32) {
    let font = Font::from_bytes(epaint_default_fonts::UBUNTU_LIGHT, FontSettings::default()).expect("bundled font");
    let (metrics, bitmap) = font.rasterize('s', 560.0);
    let ox = (center - metrics.width as f32 / 2.0).round() as i64;
    let oy = (center - metrics.height as f32 / 2.0).round() as i64;
    for gy in 0..metrics.height {
        for gx in 0..metrics.width {
            let a = bitmap[gy * metrics.width + gx];
            if a == 0 {
                continue;
            }
            let (px, py) = (ox + gx as i64, oy + gy as i64);
            if px < 0 || py < 0 || px >= SIZE as i64 || py >= SIZE as i64 {
                continue;
            }
            let bg = img.get_pixel(px as u32, py as u32).0;
            let blend = |b: u8, f: f32| ((f * a as f32 + b as f32 * (255 - a) as f32) / 255.0) as u8;
            img.put_pixel(
                px as u32,
                py as u32,
                image::Rgba([
                    blend(bg[0], FG[0]),
                    blend(bg[1], FG[1]),
                    blend(bg[2], FG[2]),
                    bg[3].max(a),
                ]),
            );
        }
    }
}

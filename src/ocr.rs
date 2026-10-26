//! Local OCR (pure Rust, `ocrs`): reads the text in a region of the Cloud PC screen, so text can be
//! copied out when clipboard redirection is disabled. The two models (~12 MB) are downloaded into
//! the data dir on first use.
//!
//! Text is laid out row by row: words are grouped into rows by position, and table columns (split
//! by grid lines or wide gaps) become tab-separated cells, which paste into Excel as real cells.

use anyhow::{Context, Result};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use rten::Model;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MODELS_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const DETECTION_MODEL: &str = "text-detection.onnx";
const RECOGNITION_MODEL: &str = "text-recognition.onnx";

/// Table grid lines at least this long (in pixels) are erased before OCR; shorter ones could be
/// the stems of a single line of text.
const MIN_GRID_LINE: usize = 40;
/// Pixels within this summed RGB difference count as the same color (RemoteFX is slightly lossy).
const SAME_COLOR: u32 = 30;

/// Loaded once; a failed load (e.g. offline on first use) is retried on the next call.
static ENGINE: Mutex<Option<Arc<OcrEngine>>> = Mutex::new(None);

/// The text in an RGBA image: one line per row, table columns separated by tabs.
pub fn read_text(rgba: &[u8], width: u32, height: u32) -> Result<String> {
    let engine = engine()?;
    let mut rgba = rgba.to_vec();
    let column_lines = erase_grid_lines(&mut rgba, width as usize, height as usize);
    let input = engine.prepare_input(ImageSource::from_bytes(&rgba, (width, height))?)?;
    let words = engine.detect_words(&input)?;
    let lines = engine.find_text_lines(&input, &words);
    let words = engine
        .recognize_text(&input, &lines)?
        .iter()
        .flatten()
        .flat_map(|line| {
            line.words()
                .map(|w| {
                    let r = w.bounding_rect();
                    Word {
                        text: w.to_string(),
                        left: r.left(),
                        right: r.right(),
                        top: r.top(),
                        bottom: r.bottom(),
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(layout(words, &column_lines))
}

/// A recognized word and its box, in image pixels.
struct Word {
    text: String,
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

/// Lays words out as rows of text. Words split by a vertical grid line (x in `column_lines`) or a
/// gap of a line height or more are separate cells; cells line up into tab-separated columns.
fn layout(mut words: Vec<Word>, column_lines: &[i32]) -> String {
    if words.is_empty() {
        return String::new();
    }
    // The typical line height is the unit for every tolerance below.
    let mut heights: Vec<i32> = words.iter().map(|w| w.bottom - w.top).collect();
    heights.sort_unstable();
    let line_h = heights[heights.len() / 2].max(1);

    // Rows: words whose vertical centers are within half a line of the row's first word.
    words.sort_by_key(|w| w.top + w.bottom);
    let mut rows: Vec<Vec<Word>> = Vec::new();
    for w in words {
        match rows.last_mut() {
            Some(row) if (w.top + w.bottom) - (row[0].top + row[0].bottom) <= line_h => row.push(w),
            _ => rows.push(vec![w]),
        }
    }

    // Cells: words in a row belong together unless a grid line or a wide gap separates them.
    let rows: Vec<Vec<Word>> = rows
        .into_iter()
        .map(|mut row| {
            row.sort_by_key(|w| w.left);
            let mut cells: Vec<Word> = Vec::new();
            for w in row {
                match cells.last_mut() {
                    Some(c)
                        if w.left - c.right < line_h
                            && !column_lines.iter().any(|&x| c.right <= x + 1 && x <= w.left + 1) =>
                    {
                        c.text = format!("{} {}", c.text, w.text);
                        c.right = c.right.max(w.right);
                    }
                    _ => cells.push(w),
                }
            }
            cells
        })
        .collect();

    // Columns: the horizontal spans covered by cells of any row, merged where they overlap.
    let mut spans: Vec<(i32, i32)> = rows.iter().flatten().map(|c| (c.left, c.right)).collect();
    spans.sort_unstable();
    let mut columns: Vec<(i32, i32)> = Vec::new();
    for (left, right) in spans {
        match columns.last_mut() {
            Some(col) if left <= col.1 => col.1 = col.1.max(right),
            _ => columns.push((left, right)),
        }
    }

    rows.iter()
        .map(|cells| {
            let mut out = vec![String::new(); columns.len()];
            for c in cells {
                let i = columns
                    .iter()
                    .position(|col| c.left <= col.1)
                    .unwrap_or(columns.len() - 1);
                if !out[i].is_empty() {
                    out[i].push(' ');
                }
                out[i].push_str(&c.text);
            }
            out.join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Paints over table grid lines (1-2 px lines crossing most of the image, like Excel cell
/// borders), which OCR would otherwise read as `|` and glue to the neighboring cells' text.
/// Returns the x of each vertical line, which separate table columns.
fn erase_grid_lines(rgba: &mut [u8], width: usize, height: usize) -> Vec<i32> {
    // Horizontal lines: one per y, running along x.
    erase_lines(rgba, height, width, |y, x| (y * width + x) * 4);
    // Vertical lines: one per x, running along y.
    erase_lines(rgba, width, height, |x, y| (y * width + x) * 4)
        .into_iter()
        .map(|x| x as i32)
        .collect()
}

/// Erases lines at positions `0..count`, each `len` pixels long; `at(pos, i)` is the byte offset
/// of pixel `i` of the line at `pos`. Returns the positions of the erased lines.
fn erase_lines(rgba: &mut [u8], count: usize, len: usize, at: impl Fn(usize, usize) -> usize) -> Vec<usize> {
    if len < MIN_GRID_LINE {
        return Vec::new();
    }
    let same = |a: usize, b: usize| -> bool {
        let diff: u32 = (0..3).map(|k| u32::from(rgba[a + k].abs_diff(rgba[b + k]))).sum();
        diff <= SAME_COLOR
    };
    let mut erase = Vec::new();
    for thickness in 1..=2 {
        for pos in 1..count.saturating_sub(thickness) {
            // A line pixel differs from the matching background on both sides of the line.
            let on_line: Vec<usize> = (0..len)
                .filter(|&i| {
                    let (before, after) = (at(pos - 1, i), at(pos + thickness, i));
                    same(before, after) && (0..thickness).all(|k| !same(at(pos + k, i), before))
                })
                .collect();
            if on_line.len() * 4 >= len * 3 {
                erase.push((pos, thickness, on_line));
            }
        }
    }
    let positions = erase.iter().map(|(pos, _, _)| *pos).collect();
    for (pos, thickness, on_line) in erase {
        for i in on_line {
            let bg = at(pos - 1, i);
            for k in 0..thickness {
                let px = at(pos + k, i);
                rgba.copy_within(bg..bg + 3, px);
            }
        }
    }
    positions
}

/// Copies the `w`x`h` region at (`x`, `y`) out of an RGBA frame `frame_w` pixels wide.
pub fn crop(rgba: &[u8], frame_w: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = (row * frame_w + x) * 4;
        out.extend_from_slice(&rgba[start..start + w * 4]);
    }
    out
}

fn engine() -> Result<Arc<OcrEngine>> {
    let mut engine = ENGINE.lock().expect("ocr engine lock");
    if let Some(e) = engine.as_ref() {
        return Ok(Arc::clone(e));
    }
    let loaded = Arc::new(OcrEngine::new(OcrEngineParams {
        detection_model: Some(load_model(DETECTION_MODEL)?),
        recognition_model: Some(load_model(RECOGNITION_MODEL)?),
        ..Default::default()
    })?);
    *engine = Some(Arc::clone(&loaded));
    Ok(loaded)
}

/// Loads a model from the data dir, downloading it first if needed.
fn load_model(name: &str) -> Result<Model> {
    let dir = crate::auth::data_dir()?.join("ocr-models");
    let path = dir.join(name);
    if !path.exists() {
        fs::create_dir_all(&dir)?;
        download(name, &path)?;
    }
    Model::load_file(&path).with_context(|| format!("loading OCR model {}", path.display()))
}

fn download(name: &str, path: &PathBuf) -> Result<()> {
    eprintln!("ocr: downloading {name} (one-time)");
    let bytes = reqwest::blocking::get(format!("{MODELS_URL}/{name}"))
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .with_context(|| format!("downloading OCR model {name}"))?;
    // Write then rename, so an interrupted download isn't mistaken for a model.
    let partial = path.with_extension("part");
    fs::write(&partial, &bytes)?;
    fs::rename(&partial, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Word, crop, erase_grid_lines, layout};

    fn word(text: &str, left: i32, top: i32) -> Word {
        let right = left + 8 * text.len() as i32;
        Word {
            text: text.into(),
            left,
            right,
            top,
            bottom: top + 14,
        }
    }

    #[test]
    fn crops_rows_of_the_region() {
        // 3x2 frame; pixel value = its index.
        let frame: Vec<u8> = (0..6).flat_map(|i| [i; 4]).collect();
        let out = crop(&frame, 3, 1, 0, 2, 2);
        assert_eq!(out, [1, 2, 4, 5].iter().flat_map(|&i| [i; 4]).collect::<Vec<u8>>());
    }

    #[test]
    fn lays_out_rows_top_to_bottom_as_plain_lines() {
        let words = vec![
            word("world", 60, 31),
            word("second", 0, 30),
            word("hello", 0, 1),
            word("there", 50, 0),
        ];
        assert_eq!(layout(words, &[]), "hello there\nsecond world");
    }

    #[test]
    fn splits_cells_at_grid_lines_and_keeps_empty_cells() {
        // Columns split by grid lines at x=45 and x=95; the second row's middle cell is empty.
        let words = vec![
            word("a1", 0, 0),
            word("b1", 50, 0),
            word("c", 100, 0),
            word("one", 120, 0),
            word("a2", 0, 20),
            word("c2", 100, 20),
        ];
        assert_eq!(layout(words, &[45, 95]), "a1\tb1\tc one\na2\t\tc2");
    }

    #[test]
    fn erases_long_grid_lines_but_not_text_stems() {
        let (w, h) = (60, 60);
        let mut img = vec![255u8; w * h * 4];
        let mut paint = |x: usize, y: usize, v: u8| img[(y * w + x) * 4..][..3].fill(v);
        for y in 0..h {
            paint(20, y, 200); // vertical grid line
        }
        for x in 0..w {
            paint(x, 30, 200); // horizontal grid line
        }
        for y in 5..20 {
            paint(40, y, 0); // a letter's stem
        }
        assert_eq!(erase_grid_lines(&mut img, w, h), [20]);
        let at = |x: usize, y: usize| img[(y * w + x) * 4];
        assert_eq!((at(20, 10), at(10, 30)), (255, 255));
        assert_eq!(at(40, 10), 0);
    }
}

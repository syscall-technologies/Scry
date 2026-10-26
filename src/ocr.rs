//! Local OCR (pure Rust, `ocrs`): reads the text in a region of the Cloud PC screen, so text can be
//! copied out when clipboard redirection is disabled. The two models (~12 MB) are downloaded into
//! the data dir on first use.

use anyhow::{Context, Result};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MODELS_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const DETECTION_MODEL: &str = "text-detection.onnx";
const RECOGNITION_MODEL: &str = "text-recognition.onnx";

/// Loaded once; a failed load (e.g. offline on first use) is retried on the next call.
static ENGINE: Mutex<Option<Arc<OcrEngine>>> = Mutex::new(None);

/// The text in an RGBA image, one line per text line.
pub fn read_text(rgba: &[u8], width: u32, height: u32) -> Result<String> {
    let engine = engine()?;
    let input = engine.prepare_input(ImageSource::from_bytes(rgba, (width, height))?)?;
    Ok(engine.get_text(&input)?.trim().to_owned())
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
    use super::crop;

    #[test]
    fn crops_rows_of_the_region() {
        // 3x2 frame; pixel value = its index.
        let frame: Vec<u8> = (0..6).flat_map(|i| [i; 4]).collect();
        let out = crop(&frame, 3, 1, 0, 2, 2);
        assert_eq!(out, [1, 2, 4, 5].iter().flat_map(|&i| [i; 4]).collect::<Vec<u8>>());
    }
}

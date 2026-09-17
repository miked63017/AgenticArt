use crate::document::{Layer, SelectionRect};
use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{Rgba, RgbaImage};
use ort::session::Session;
use std::sync::{Mutex, OnceLock};

/// AI-native ops sit behind local ONNX inference today; a cloud backend
/// can slot in later without changing the MCP tool contract. This is the
/// first one: subject
/// segmentation via a small U2Net variant (u2netp), used to cut a layer's
/// foreground subject out into its own layer - the AI-assisted analogue of
/// manually selecting-and-copying, not a generative replacement for it.
const MODEL_BYTES: &[u8] = include_bytes!("../assets/u2netp.onnx");
const INPUT_SIZE: u32 = 320;
/// ImageNet normalization stats, matching the model's training preprocessing.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

fn session() -> Result<&'static Mutex<Session>> {
    static SESSION: OnceLock<Result<Mutex<Session>, ort::Error>> = OnceLock::new();
    SESSION
        .get_or_init(|| Ok(Mutex::new(Session::builder()?.commit_from_memory(MODEL_BYTES)?)))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("failed to load segmentation model: {e}"))
}

/// Runs subject segmentation on `pixels` and returns a same-size soft mask
/// (single channel, 0-255) where higher means "more likely foreground".
fn segment_mask(pixels: &RgbaImage) -> Result<image::GrayImage> {
    let session_lock = session()?;
    let mut session = session_lock.lock().map_err(|_| anyhow::anyhow!("segmentation model mutex poisoned"))?;
    let (orig_w, orig_h) = pixels.dimensions();

    let resized = image::imageops::resize(pixels, INPUT_SIZE, INPUT_SIZE, FilterType::Triangle);
    let mut input = vec![0f32; (3 * INPUT_SIZE * INPUT_SIZE) as usize];
    let plane = (INPUT_SIZE * INPUT_SIZE) as usize;
    for y in 0..INPUT_SIZE {
        for x in 0..INPUT_SIZE {
            let p = resized.get_pixel(x, y);
            let idx = (y * INPUT_SIZE + x) as usize;
            for c in 0..3 {
                let v = p[c] as f32 / 255.0;
                input[c * plane + idx] = (v - MEAN[c]) / STD[c];
            }
        }
    }

    let input_value = ort::value::Value::from_array(([1usize, 3, INPUT_SIZE as usize, INPUT_SIZE as usize], input))
        .context("failed to build input tensor")?;
    let input_name = session.inputs()[0].name().to_string();
    let output_name = session.outputs()[0].name().to_string();
    let outputs = session.run(ort::inputs![input_name => input_value]).context("segmentation inference failed")?;
    let (shape, data) = outputs[output_name]
        .try_extract_tensor::<f32>()
        .context("failed to read model output")?;
    let _ = shape;

    let (mut min, mut max) = (f32::MAX, f32::MIN);
    for &v in data {
        min = min.min(v);
        max = max.max(v);
    }
    let range = (max - min).max(1e-6);

    let mut mask_small = image::GrayImage::new(INPUT_SIZE, INPUT_SIZE);
    for y in 0..INPUT_SIZE {
        for x in 0..INPUT_SIZE {
            let idx = (y * INPUT_SIZE + x) as usize;
            let normalized = ((data[idx] - min) / range).clamp(0.0, 1.0);
            mask_small.put_pixel(x, y, image::Luma([(normalized * 255.0).round() as u8]));
        }
    }

    Ok(image::imageops::resize(&mask_small, orig_w, orig_h, FilterType::Triangle))
}

/// Cuts the foreground subject of `layer` out into a new layer (original
/// RGB, alpha = original alpha * segmentation mask). The source layer is
/// left untouched.
pub fn cutout_subject(layer: &Layer) -> Result<Layer> {
    let mask = segment_mask(&layer.pixels)?;
    let (w, h) = layer.pixels.dimensions();
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let src = layer.pixels.get_pixel(x, y);
            let m = mask.get_pixel(x, y)[0] as f32 / 255.0;
            let a = (src[3] as f32 * m).round() as u8;
            out.put_pixel(x, y, Rgba([src[0], src[1], src[2], a]));
        }
    }
    let mut cutout = Layer::new_transparent(format!("{} (subject)", layer.name), w, h);
    cutout.pixels = out;
    Ok(cutout)
}

/// Runs subject segmentation and returns the bounding box of pixels above
/// `threshold` (0-255), for setting the document's rectangular selection
/// to "roughly where the subject is". This engine's selection model is a
/// single rectangle (see document.rs), not an arbitrary mask, so this is a
/// deliberate approximation - a true soft-mask selection is a larger data-
/// model change (SelectionRect -> a Rect|Mask enum) noted as future work
/// rather than done here, to avoid touching every selection-clipping call
/// site (paint/adjustments/filters/project) in the same change as a first
/// AI op.
pub fn subject_bounding_box(layer: &Layer, threshold: u8) -> Result<Option<SelectionRect>> {
    let mask = segment_mask(&layer.pixels)?;
    let (w, h) = mask.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0i64, 0i64);
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] >= threshold {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x as i64);
                max_y = max_y.max(y as i64);
            }
        }
    }
    if !found {
        return Ok(None);
    }
    Ok(Some(SelectionRect {
        x: min_x as i64,
        y: min_y as i64,
        width: (max_x - min_x as i64 + 1) as u32,
        height: (max_y - min_y as i64 + 1) as u32,
    }))
}

use crate::document::{Document, Layer};
use anyhow::{Context, Result};
use image::RgbaImage;
use std::path::Path;

/// Opens a .psd file as a new multi-layer Document. See `psd_export` for
/// the write side (which does exist, and does write real PSD layer
/// groups) - this reader is the asymmetric half: it imports every layer
/// flat, it does not reconstruct PSD layer groups into
/// `Layer::parent_group` (the `psd` crate exposes `Psd::groups()`
/// separately from `Psd::layers()`, and wiring that into our group model
/// is future work, not done here). Effects/adjustment layers/smart
/// objects in the source file are not preserved as anything other than
/// their flattened layer pixels.
pub fn document_from_psd(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let psd = psd::Psd::from_bytes(&bytes).map_err(|e| anyhow::anyhow!("failed to parse PSD: {e}"))?;

    let width = psd.width();
    let height = psd.height();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled").to_string();

    let mut doc = Document::new(name, width, height);
    doc.layers.clear();

    for psd_layer in psd.layers() {
        let rgba = psd_layer.rgba();
        let pixels = RgbaImage::from_raw(width, height, rgba).context("PSD layer pixel buffer size mismatch")?;
        let mut layer = Layer::new_transparent(psd_layer.name().to_string(), width, height);
        layer.pixels = pixels;
        layer.opacity = psd_layer.opacity() as f32 / 255.0;
        layer.visible = psd_layer.visible();
        doc.layers.push(layer);
    }

    if doc.layers.is_empty() {
        // Flat/single-layer PSDs (or ones the layer parser didn't expose)
        // fall back to the flattened composite as one layer.
        let rgba = psd.rgba();
        let pixels = RgbaImage::from_raw(width, height, rgba).context("PSD composite pixel buffer size mismatch")?;
        let mut layer = Layer::new_transparent("Background", width, height);
        layer.pixels = pixels;
        doc.layers.push(layer);
    }

    doc.active_layer = doc.layers.len() - 1;
    Ok(doc)
}

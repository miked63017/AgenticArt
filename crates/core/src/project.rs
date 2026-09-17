use crate::document::{BlendMode, Document, Layer, LayerComp, LayerStyle, SelectionRect, SmartFilter, TextLayerData};
use anyhow::{Context, Result};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// AgenticArt's native project format: a zip file with one manifest.json
/// (document/layer metadata) plus one lossless PNG per layer. This is what
/// "Save"/"Open" round-trip through — distinct from `io::export_to_file`
/// (flattened PNG/JPEG export) and `io::document_from_file` (opening a
/// flat image as a new single-layer document).
const MANIFEST_NAME: &str = "manifest.json";

#[derive(Serialize, Deserialize)]
struct LayerManifest {
    id: Uuid,
    name: String,
    opacity: f32,
    visible: bool,
    blend_mode: BlendMode,
    #[serde(default)]
    clip_to_below: bool,
    #[serde(default)]
    style: LayerStyle,
    #[serde(default)]
    smart_filters: Vec<SmartFilter>,
    #[serde(default)]
    is_group: bool,
    #[serde(default)]
    parent_group: Option<Uuid>,
    file: String,
    #[serde(default)]
    mask_file: Option<String>,
    #[serde(default)]
    text: Option<TextLayerData>,
}

#[derive(Serialize, Deserialize)]
struct SelectionManifest {
    x: i64,
    y: i64,
    width: u32,
    height: u32,
}

#[derive(Serialize, Deserialize)]
struct DocumentManifest {
    id: Uuid,
    name: String,
    width: u32,
    height: u32,
    active_layer: usize,
    selection: Option<SelectionManifest>,
    layers: Vec<LayerManifest>,
    #[serde(default)]
    layer_comps: Vec<LayerComp>,
}

pub fn save_project(doc: &Document, path: &Path) -> Result<()> {
    let file = File::create(path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut layer_manifests = Vec::with_capacity(doc.layers.len());
    for (i, layer) in doc.layers.iter().enumerate() {
        let file_name = format!("layer_{i}_{}.png", layer.id);
        zip.start_file(&file_name, options)?;
        let mut bytes = Vec::new();
        layer.pixels.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
        zip.write_all(&bytes)?;

        let mask_file = if let Some(mask) = &layer.mask {
            let mask_name = format!("layer_{i}_{}_mask.png", layer.id);
            zip.start_file(&mask_name, options)?;
            let mut mask_bytes = Vec::new();
            mask.write_to(&mut Cursor::new(&mut mask_bytes), ImageFormat::Png)?;
            zip.write_all(&mask_bytes)?;
            Some(mask_name)
        } else {
            None
        };

        layer_manifests.push(LayerManifest {
            id: layer.id,
            name: layer.name.clone(),
            opacity: layer.opacity,
            visible: layer.visible,
            blend_mode: layer.blend_mode,
            clip_to_below: layer.clip_to_below,
            style: layer.style.clone(),
            smart_filters: layer.smart_filters.clone(),
            is_group: layer.is_group,
            parent_group: layer.parent_group,
            file: file_name,
            mask_file,
            text: layer.text.clone(),
        });
    }

    let manifest = DocumentManifest {
        id: doc.id,
        name: doc.name.clone(),
        width: doc.width,
        height: doc.height,
        active_layer: doc.active_layer,
        selection: doc.selection.map(|s| SelectionManifest { x: s.x, y: s.y, width: s.width, height: s.height }),
        layers: layer_manifests,
        layer_comps: doc.layer_comps.clone(),
    };
    zip.start_file(MANIFEST_NAME, options)?;
    zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    zip.finish()?;
    Ok(())
}

pub fn load_project(path: &Path) -> Result<Document> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;

    let manifest: DocumentManifest = {
        let mut entry = archive.by_name(MANIFEST_NAME).context("project file is missing manifest.json")?;
        let mut contents = String::new();
        entry.read_to_string(&mut contents)?;
        serde_json::from_str(&contents)?
    };

    let mut layers = Vec::with_capacity(manifest.layers.len());
    for lm in &manifest.layers {
        let pixels = {
            let mut entry = archive.by_name(&lm.file).with_context(|| format!("project file is missing layer image '{}'", lm.file))?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            image::load_from_memory_with_format(&bytes, ImageFormat::Png)?.to_rgba8()
        };

        let mask = match &lm.mask_file {
            Some(mask_name) => {
                let mut mask_entry = archive.by_name(mask_name).with_context(|| format!("project file is missing mask image '{mask_name}'"))?;
                let mut mask_bytes = Vec::new();
                mask_entry.read_to_end(&mut mask_bytes)?;
                Some(image::load_from_memory_with_format(&mask_bytes, ImageFormat::Png)?.to_luma8())
            }
            None => None,
        };

        layers.push(Layer {
            id: lm.id,
            name: lm.name.clone(),
            pixels,
            opacity: lm.opacity,
            visible: lm.visible,
            blend_mode: lm.blend_mode,
            clip_to_below: lm.clip_to_below,
            style: lm.style.clone(),
            smart_filters: lm.smart_filters.clone(),
            mask,
            text: lm.text.clone(),
            is_group: lm.is_group,
            parent_group: lm.parent_group,
        });
    }

    Ok(Document {
        id: manifest.id,
        name: manifest.name,
        width: manifest.width,
        height: manifest.height,
        layers,
        active_layer: manifest.active_layer,
        selection: manifest.selection.map(|s| SelectionRect { x: s.x, y: s.y, width: s.width, height: s.height }),
        layer_comps: manifest.layer_comps,
    })
}

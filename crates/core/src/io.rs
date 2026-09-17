use crate::compositor;
use crate::document::{Document, Layer};
use anyhow::Result;
use image::RgbaImage;
use std::io::Cursor;
use std::path::Path;

/// Renders and saves the document to a path; format is inferred from the
/// extension (png, jpg/jpeg, etc. via the `image` crate).
pub fn export_to_file(doc: &Document, path: &Path) -> Result<()> {
    let flat = compositor::render(doc);
    flat.save(path)?;
    Ok(())
}

/// Renders the document and returns encoded bytes (e.g. PNG) without
/// touching disk — used by `canvas.render` over MCP so an agent can pull
/// the current pixels back without a round trip through the filesystem.
pub fn render_to_bytes(doc: &Document, format: image::ImageFormat) -> Result<Vec<u8>> {
    let flat = compositor::render(doc);
    let mut bytes: Vec<u8> = Vec::new();
    flat.write_to(&mut Cursor::new(&mut bytes), format)?;
    Ok(bytes)
}

/// `render_to_bytes`, cropped to one region - see
/// `compositor::render_region` for what this does and doesn't solve.
pub fn render_region_to_bytes(doc: &Document, region: &crate::document::SelectionRect, format: image::ImageFormat) -> Result<Vec<u8>> {
    let tile = compositor::render_region(doc, region);
    let mut bytes: Vec<u8> = Vec::new();
    tile.write_to(&mut Cursor::new(&mut bytes), format)?;
    Ok(bytes)
}

/// Loads any format the `image` crate supports (PNG/JPEG/TIFF/WebP/GIF/BMP)
/// and returns it as an RGBA8 buffer.
pub fn load_image(path: &Path) -> Result<RgbaImage> {
    Ok(image::open(path)?.to_rgba8())
}

/// Opens an image file as a brand-new document sized to the image, with a
/// single layer holding its pixels — the "File > Open" action.
pub fn document_from_file(path: &Path) -> Result<Document> {
    let image = load_image(path)?;
    let (width, height) = (image.width(), image.height());
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled").to_string();
    let mut doc = Document::new(name, width, height);
    doc.layers.clear();
    let mut layer = Layer::new_transparent("Background", width, height);
    layer.pixels = image;
    doc.layers.push(layer);
    Ok(doc)
}

/// Composites an image file onto an existing layer at (x, y) in canvas
/// space, clipping to canvas bounds. Used both by a UI "Place Image" action
/// and by agents that want to drop a reference/source image onto a layer
/// rather than painting it stroke by stroke.
pub fn place_image_on_layer(layer: &mut Layer, path: &Path, x: i64, y: i64) -> Result<()> {
    let src = load_image(path)?;
    let (canvas_w, canvas_h) = (layer.pixels.width() as i64, layer.pixels.height() as i64);
    for sy in 0..src.height() {
        let dy = y + sy as i64;
        if dy < 0 || dy >= canvas_h {
            continue;
        }
        for sx in 0..src.width() {
            let dx = x + sx as i64;
            if dx < 0 || dx >= canvas_w {
                continue;
            }
            let src_px = *src.get_pixel(sx, sy);
            let dst_px = layer.pixels.get_pixel_mut(dx as u32, dy as u32);
            let src_a = src_px[3] as f32 / 255.0;
            let dst_a = dst_px[3] as f32 / 255.0;
            let out_a = src_a + dst_a * (1.0 - src_a);
            if out_a <= 0.0 {
                *dst_px = image::Rgba([0, 0, 0, 0]);
                continue;
            }
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let mixed = src_px[c] as f32 / 255.0 * src_a + dst_px[c] as f32 / 255.0 * dst_a * (1.0 - src_a);
                rgb[c] = ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            *dst_px = image::Rgba([rgb[0], rgb[1], rgb[2], (out_a * 255.0).round() as u8]);
        }
    }
    Ok(())
}

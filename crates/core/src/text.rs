use crate::document::Layer;
use ab_glyph::{FontRef, PxScale};
use anyhow::{Context, Result};
use image::Rgba;
use imageproc::drawing::draw_text_mut;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// This is a direct raster stamp onto a layer (see `document::Layer::text`
/// for the separate live, re-editable text layer that doesn't bake into
/// pixels). Defaults to a system font since we don't bundle one; callers
/// can point at any .ttf/.otf.
fn default_font_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        for candidate in ["C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\arial.ttf"] {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return p;
            }
        }
        PathBuf::from("C:\\Windows\\Fonts\\arial.ttf")
    })
}

/// Draws `text` onto `layer` with its top-left at (x, y) in canvas pixels.
pub fn draw_text(layer: &mut Layer, text: &str, x: i32, y: i32, size: f32, color: Rgba<u8>, font_path: Option<&Path>) -> Result<()> {
    let path = font_path.map(Path::to_path_buf).unwrap_or_else(|| default_font_path().to_path_buf());
    let font_bytes = std::fs::read(&path).with_context(|| format!("failed to read font at {}", path.display()))?;
    let font = FontRef::try_from_slice(&font_bytes).context("failed to parse font file")?;
    draw_text_mut(&mut layer.pixels, color, x, y, PxScale::from(size), &font, text);
    Ok(())
}

/// Renders a live text layer's content into a fresh `width` x `height`
/// (fully transparent otherwise) buffer - the non-destructive counterpart
/// to `draw_text`, used by `compositor::text_source_pixels` to re-render a
/// `TextLayerData` at composite time instead of baking it into `pixels`.
pub fn render_text_layer(width: u32, height: u32, data: &crate::document::TextLayerData, font_path: Option<&Path>) -> Result<image::RgbaImage> {
    let path = font_path.map(Path::to_path_buf).unwrap_or_else(|| default_font_path().to_path_buf());
    let font_bytes = std::fs::read(&path).with_context(|| format!("failed to read font at {}", path.display()))?;
    let font = FontRef::try_from_slice(&font_bytes).context("failed to parse font file")?;
    let mut img = image::RgbaImage::new(width, height);
    draw_text_mut(&mut img, Rgba(data.color), data.x, data.y, PxScale::from(data.size), &font, &data.text);
    Ok(img)
}

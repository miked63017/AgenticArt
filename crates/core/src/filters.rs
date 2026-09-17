use crate::document::{Layer, SelectionRect};
use image::RgbaImage;

/// Writes `result` back onto `layer`, restricted to `clip` when present
/// (so filters honor the active selection the same way paint/adjustments
/// do) or wholesale otherwise.
fn apply_clipped(layer: &mut Layer, result: RgbaImage, clip: Option<SelectionRect>) {
    match clip {
        None => layer.pixels = result,
        Some(sel) => {
            for y in 0..layer.pixels.height() {
                for x in 0..layer.pixels.width() {
                    if sel.contains(x as i64, y as i64) {
                        layer.pixels.put_pixel(x, y, *result.get_pixel(x, y));
                    }
                }
            }
        }
    }
}

/// Gaussian blur; `sigma` is the blur radius in pixels.
pub fn gaussian_blur(layer: &mut Layer, sigma: f32, clip: Option<SelectionRect>) {
    let result = image::imageops::blur(&layer.pixels, sigma);
    apply_clipped(layer, result, clip);
}

/// Unsharp-mask sharpen; `sigma` controls the blur radius used to build the
/// mask, `threshold` is the minimum brightness change (0-255) to sharpen.
pub fn sharpen(layer: &mut Layer, sigma: f32, threshold: i32, clip: Option<SelectionRect>) {
    let result = image::imageops::unsharpen(&layer.pixels, sigma, threshold);
    apply_clipped(layer, result, clip);
}

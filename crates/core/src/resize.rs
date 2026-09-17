use crate::document::Document;
use image::{imageops::FilterType, GrayImage, Luma, RgbaImage};

/// Places `src` at `(offset_x, offset_y)` in a new `new_width` x
/// `new_height` white-filled (fully visible) canvas, cropping/padding like
/// `resize_canvas` does for RGBA layer pixels. Shared by `resize_canvas`
/// so a layer's mask is repositioned in lockstep with its pixels instead
/// of silently going stale (which would otherwise misalign it, or panic
/// `compositor::apply_mask` on an out-of-bounds read).
fn reposition_mask(src: &GrayImage, new_width: u32, new_height: u32, offset_x: i64, offset_y: i64) -> GrayImage {
    let mut out = GrayImage::from_pixel(new_width, new_height, Luma([255]));
    for y in 0..src.height() {
        let dy = y as i64 + offset_y;
        if dy < 0 || dy >= new_height as i64 {
            continue;
        }
        for x in 0..src.width() {
            let dx = x as i64 + offset_x;
            if dx < 0 || dx >= new_width as i64 {
                continue;
            }
            out.put_pixel(dx as u32, dy as u32, *src.get_pixel(x, y));
        }
    }
    out
}

/// Photoshop's "Image Size": scales the canvas and every layer's pixels to
/// new dimensions (high-quality Lanczos3 resampling). The document's
/// selection is cleared since its coordinates no longer make sense at the
/// new size.
pub fn resize_document(doc: &mut Document, new_width: u32, new_height: u32) {
    for layer in &mut doc.layers {
        layer.pixels = image::imageops::resize(&layer.pixels, new_width, new_height, FilterType::Lanczos3);
        if let Some(mask) = &layer.mask {
            layer.mask = Some(image::imageops::resize(mask, new_width, new_height, FilterType::Lanczos3));
        }
    }
    doc.width = new_width;
    doc.height = new_height;
    doc.selection = None;
}

/// Photoshop's "Canvas Size": changes canvas dimensions *without* scaling
/// content - existing pixels are placed at `(offset_x, offset_y)` in the
/// new canvas (crop where they fall outside it, pad with transparency
/// where the new canvas is larger).
pub fn resize_canvas(doc: &mut Document, new_width: u32, new_height: u32, offset_x: i64, offset_y: i64) {
    for layer in &mut doc.layers {
        let mut new_pixels = RgbaImage::new(new_width, new_height);
        for y in 0..layer.pixels.height() {
            let dy = y as i64 + offset_y;
            if dy < 0 || dy >= new_height as i64 {
                continue;
            }
            for x in 0..layer.pixels.width() {
                let dx = x as i64 + offset_x;
                if dx < 0 || dx >= new_width as i64 {
                    continue;
                }
                new_pixels.put_pixel(dx as u32, dy as u32, *layer.pixels.get_pixel(x, y));
            }
        }
        layer.pixels = new_pixels;
        if let Some(mask) = &layer.mask {
            layer.mask = Some(reposition_mask(mask, new_width, new_height, offset_x, offset_y));
        }
    }
    doc.width = new_width;
    doc.height = new_height;
    doc.selection = None;
}


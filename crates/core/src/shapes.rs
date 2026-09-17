use crate::document::{Layer, SelectionRect};
use image::Rgba;

pub(crate) fn blend_pixel_in_place(layer: &mut Layer, x: u32, y: u32, color: Rgba<u8>) {
    let alpha = color[3] as f32 / 255.0;
    if alpha <= 0.0 {
        return;
    }
    let existing = *layer.pixels.get_pixel(x, y);
    let out_a = alpha + (existing[3] as f32 / 255.0) * (1.0 - alpha);
    let mut rgb = [0u8; 3];
    for c in 0..3 {
        let src = color[c] as f32 / 255.0;
        let dst = existing[c] as f32 / 255.0;
        let mixed = src * alpha + dst * (existing[3] as f32 / 255.0) * (1.0 - alpha);
        rgb[c] = if out_a > 0.0 { ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
    }
    layer.pixels.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], (out_a.clamp(0.0, 1.0) * 255.0).round() as u8]));
}

/// Fills an axis-aligned rectangle with a solid color, clipped to the
/// layer bounds and (optionally) the active selection. Hard edges, same
/// as Photoshop's default shape-tool fill (no feathering).
pub fn fill_rect(layer: &mut Layer, x: i64, y: i64, width: u32, height: u32, color: Rgba<u8>, clip: Option<SelectionRect>) {
    let (canvas_w, canvas_h) = (layer.pixels.width() as i64, layer.pixels.height() as i64);
    let min_x = x.max(0);
    let min_y = y.max(0);
    let max_x = (x + width as i64).min(canvas_w);
    let max_y = (y + height as i64).min(canvas_h);
    for py in min_y..max_y {
        for px in min_x..max_x {
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            blend_pixel_in_place(layer, px as u32, py as u32, color);
        }
    }
}

/// Fills an ellipse centered at (cx, cy) with radii (rx, ry), clipped to
/// the layer bounds and (optionally) the active selection.
pub fn fill_ellipse(layer: &mut Layer, cx: f32, cy: f32, rx: f32, ry: f32, color: Rgba<u8>, clip: Option<SelectionRect>) {
    if rx <= 0.0 || ry <= 0.0 {
        return;
    }
    let (canvas_w, canvas_h) = (layer.pixels.width() as i64, layer.pixels.height() as i64);
    let min_x = ((cx - rx).floor() as i64).max(0);
    let max_x = ((cx + rx).ceil() as i64).min(canvas_w - 1);
    let min_y = ((cy - ry).floor() as i64).max(0);
    let max_y = ((cy + ry).ceil() as i64).min(canvas_h - 1);
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let nx = (px as f32 + 0.5 - cx) / rx;
            let ny = (py as f32 + 0.5 - cy) / ry;
            if nx * nx + ny * ny > 1.0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            blend_pixel_in_place(layer, px as u32, py as u32, color);
        }
    }
}

fn color_distance(a: Rgba<u8>, b: Rgba<u8>) -> u32 {
    (0..4).map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs()).max().unwrap_or(0)
}

/// Photoshop's Paint Bucket: flood-fills the contiguous region of pixels
/// starting at `(x, y)` whose color is within `tolerance` of the seed
/// pixel's own color (compared per-channel including alpha, so filling a
/// transparent area works the same way as filling an opaque one).
/// "Contiguous" means 4-connected and stops at any pixel outside
/// tolerance or outside `clip` - it never jumps across a boundary the way
/// a "fill all similar pixels" (non-contiguous) mode would. Tolerance is
/// always checked against each pixel's *original* color (read before it's
/// painted), not the just-painted fill color, so a high tolerance can't
/// make the fill spill past the actual contiguous region by matching its
/// own trail.
pub fn paint_bucket(layer: &mut Layer, x: u32, y: u32, color: Rgba<u8>, tolerance: u8, clip: Option<SelectionRect>) {
    let (width, height) = layer.pixels.dimensions();
    if x >= width || y >= height {
        return;
    }
    let seed_color = *layer.pixels.get_pixel(x, y);
    if let Some(sel) = clip {
        if !sel.contains(x as i64, y as i64) {
            return;
        }
    }

    let mut visited = vec![false; (width as usize) * (height as usize)];
    let idx = |x: u32, y: u32| (y as usize) * (width as usize) + (x as usize);
    let mut stack = vec![(x, y)];
    visited[idx(x, y)] = true;

    while let Some((cx, cy)) = stack.pop() {
        blend_pixel_in_place(layer, cx, cy, color);
        for (nx, ny) in [(cx.wrapping_sub(1), cy), (cx + 1, cy), (cx, cy.wrapping_sub(1)), (cx, cy + 1)] {
            if nx >= width || ny >= height || visited[idx(nx, ny)] {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(nx as i64, ny as i64) {
                    continue;
                }
            }
            let original = *layer.pixels.get_pixel(nx, ny);
            if color_distance(original, seed_color) <= tolerance as u32 {
                visited[idx(nx, ny)] = true;
                stack.push((nx, ny));
            }
        }
    }
}

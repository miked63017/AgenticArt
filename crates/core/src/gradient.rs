//! Photoshop's Gradient tool: fill a layer (or its active selection) with
//! a linear or radial ramp between two or more color stops.

use crate::document::{Layer, SelectionRect};
use crate::shapes::blend_pixel_in_place;
use image::Rgba;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
}

/// One color stop in a gradient ramp. `position` is in `[0, 1]`, where 0
/// is the gradient's start point and 1 is its end point.
#[derive(Debug, Clone, Copy)]
pub struct GradientStop {
    pub position: f32,
    pub color: [u8; 4],
}

/// Linearly interpolates the gradient's color at `t` (`[0, 1]`, already
/// clamped by the caller) between whichever two stops bracket it. Stops
/// don't need to be pre-sorted or start at 0/end at 1 - `t` before the
/// first stop or after the last one clamps to that stop's color.
pub(crate) fn color_at(stops: &[GradientStop], t: f32) -> Rgba<u8> {
    let mut sorted: Vec<&GradientStop> = stops.iter().collect();
    sorted.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap_or(std::cmp::Ordering::Equal));

    if sorted.is_empty() {
        return Rgba([0, 0, 0, 0]);
    }
    if t <= sorted[0].position {
        return Rgba(sorted[0].color);
    }
    if t >= sorted[sorted.len() - 1].position {
        return Rgba(sorted[sorted.len() - 1].color);
    }
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t >= a.position && t <= b.position {
            let span = (b.position - a.position).max(f32::EPSILON);
            let local_t = (t - a.position) / span;
            let mut out = [0u8; 4];
            for c in 0..4 {
                out[c] = (a.color[c] as f32 + (b.color[c] as f32 - a.color[c] as f32) * local_t).round() as u8;
            }
            return Rgba(out);
        }
    }
    Rgba(sorted[sorted.len() - 1].color)
}

/// Fills `layer` (clipped to the active selection if any) with a gradient
/// ramp from `start` to `end` in canvas pixel coordinates. For a linear
/// gradient, `t` at each pixel is its projection onto the start->end
/// line, clamped to `[0, 1]` (so the fill is solid beyond either end).
/// For a radial gradient, `end`'s distance from `start` sets the radius,
/// and `t` is each pixel's distance from `start` relative to that radius.
pub fn fill_gradient(layer: &mut Layer, kind: GradientKind, start: (f32, f32), end: (f32, f32), stops: &[GradientStop], clip: Option<SelectionRect>) {
    if stops.is_empty() {
        return;
    }
    let (width, height) = layer.pixels.dimensions();
    let (sx, sy) = start;
    let (ex, ey) = end;
    let dx = ex - sx;
    let dy = ey - sy;

    for y in 0..height {
        for x in 0..width {
            if let Some(sel) = clip {
                if !sel.contains(x as i64, y as i64) {
                    continue;
                }
            }
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = match kind {
                GradientKind::Linear => {
                    let len_sq = dx * dx + dy * dy;
                    if len_sq <= f32::EPSILON {
                        0.0
                    } else {
                        (((px - sx) * dx + (py - sy) * dy) / len_sq).clamp(0.0, 1.0)
                    }
                }
                GradientKind::Radial => {
                    let radius = (dx * dx + dy * dy).sqrt();
                    if radius <= f32::EPSILON {
                        0.0
                    } else {
                        (((px - sx).powi(2) + (py - sy).powi(2)).sqrt() / radius).clamp(0.0, 1.0)
                    }
                }
            };
            let color = color_at(stops, t);
            blend_pixel_in_place(layer, x, y, color);
        }
    }
}

use crate::document::{Layer, SelectionRect};
use image::Rgba;

#[derive(Debug, Clone, Copy)]
pub struct BrushPoint {
    pub x: f32,
    pub y: f32,
    /// 0.0-1.0, scales the brush size. Agents that don't have real pressure
    /// input should just pass 1.0.
    pub pressure: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Brush {
    pub size: f32,
    pub color: Rgba<u8>,
    /// 0.0 = fully soft (feathered) edge, 1.0 = hard edge.
    pub hardness: f32,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            size: 12.0,
            color: Rgba([0, 0, 0, 255]),
            hardness: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StampMode {
    Paint,
    Erase,
}

fn stamp(layer: &mut Layer, cx: f32, cy: f32, radius: f32, brush: &Brush, mode: StampMode, clip: Option<SelectionRect>) {
    if radius <= 0.0 {
        return;
    }
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(layer.pixels.width() as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(layer.pixels.height() as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let edge_softness = (1.0 - brush.hardness).max(0.001) * radius;
            let coverage = if dist <= radius - edge_softness {
                1.0
            } else {
                1.0 - ((dist - (radius - edge_softness)) / edge_softness).clamp(0.0, 1.0)
            };
            let alpha = coverage * (brush.color[3] as f32 / 255.0);
            if alpha <= 0.0 {
                continue;
            }
            let existing = *layer.pixels.get_pixel(px as u32, py as u32);

            match mode {
                StampMode::Erase => {
                    let existing_a = existing[3] as f32 / 255.0;
                    let out_a = (existing_a * (1.0 - alpha)).clamp(0.0, 1.0);
                    layer.pixels.put_pixel(
                        px as u32,
                        py as u32,
                        Rgba([existing[0], existing[1], existing[2], (out_a * 255.0).round() as u8]),
                    );
                }
                StampMode::Paint => {
                    let out_a = alpha + (existing[3] as f32 / 255.0) * (1.0 - alpha);
                    let mut rgb = [0u8; 3];
                    for c in 0..3 {
                        let src = brush.color[c] as f32 / 255.0;
                        let dst = existing[c] as f32 / 255.0;
                        let mixed = src * alpha + dst * (existing[3] as f32 / 255.0) * (1.0 - alpha);
                        rgb[c] = if out_a > 0.0 {
                            ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8
                        } else {
                            0
                        };
                    }
                    layer.pixels.put_pixel(
                        px as u32,
                        py as u32,
                        Rgba([rgb[0], rgb[1], rgb[2], (out_a.clamp(0.0, 1.0) * 255.0).round() as u8]),
                    );
                }
            }
        }
    }
}

fn walk_path<F: FnMut(f32, f32, f32)>(points: &[BrushPoint], brush_size: f32, mut visit: F) {
    if points.is_empty() {
        return;
    }
    if points.len() == 1 {
        let p = points[0];
        visit(p.x, p.y, p.pressure.max(0.05));
        return;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let dist = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        let steps = (dist / (brush_size.max(1.0) * 0.25)).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = a.x + (b.x - a.x) * t;
            let y = a.y + (b.y - a.y) * t;
            let pressure = a.pressure + (b.pressure - a.pressure) * t;
            visit(x, y, pressure.max(0.05));
        }
    }
}

/// Applies a brush stroke along a path of points to a layer, in-place,
/// optionally clipped to a selection. This is the primitive both the UI's
/// live brush tool and the MCP `paint.strokePath` operation call — same
/// code path, same result.
pub fn stroke_path(layer: &mut Layer, points: &[BrushPoint], brush: &Brush, clip: Option<SelectionRect>) {
    walk_path(points, brush.size, |x, y, pressure| {
        stamp(layer, x, y, brush.size * 0.5 * pressure, brush, StampMode::Paint, clip);
    });
}

/// Erases (reduces alpha) along a path of points, same shape as
/// `stroke_path` but subtracting coverage instead of compositing color.
pub fn erase_path(layer: &mut Layer, points: &[BrushPoint], brush: &Brush, clip: Option<SelectionRect>) {
    walk_path(points, brush.size, |x, y, pressure| {
        stamp(layer, x, y, brush.size * 0.5 * pressure, brush, StampMode::Erase, clip);
    });
}

fn clone_dab(layer: &mut Layer, source: &image::RgbaImage, cx: f32, cy: f32, radius: f32, brush: &Brush, offset: (f32, f32), clip: Option<SelectionRect>) {
    if radius <= 0.0 {
        return;
    }
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let edge_softness = (1.0 - brush.hardness).max(0.001) * radius;
            let coverage = if dist <= radius - edge_softness {
                1.0
            } else {
                1.0 - ((dist - (radius - edge_softness)) / edge_softness).clamp(0.0, 1.0)
            };
            if coverage <= 0.0 {
                continue;
            }

            let src_x = px as f32 + offset.0;
            let src_y = py as f32 + offset.1;
            if src_x < 0.0 || src_y < 0.0 || src_x >= source.width() as f32 || src_y >= source.height() as f32 {
                continue;
            }
            let sampled = *source.get_pixel(src_x as u32, src_y as u32);
            let alpha = coverage * (sampled[3] as f32 / 255.0);
            if alpha <= 0.0 {
                continue;
            }
            let existing = *layer.pixels.get_pixel(px as u32, py as u32);
            let out_a = alpha + (existing[3] as f32 / 255.0) * (1.0 - alpha);
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let src = sampled[c] as f32 / 255.0;
                let dst = existing[c] as f32 / 255.0;
                let mixed = src * alpha + dst * (existing[3] as f32 / 255.0) * (1.0 - alpha);
                rgb[c] = if out_a > 0.0 { ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
            }
            layer.pixels.put_pixel(px as u32, py as u32, Rgba([rgb[0], rgb[1], rgb[2], (out_a.clamp(0.0, 1.0) * 255.0).round() as u8]));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMode {
    Dodge,
    Burn,
}

fn tone_dab(layer: &mut Layer, cx: f32, cy: f32, radius: f32, brush: &Brush, mode: ToneMode, strength: f32, clip: Option<SelectionRect>) {
    if radius <= 0.0 {
        return;
    }
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let edge_softness = (1.0 - brush.hardness).max(0.001) * radius;
            let coverage = if dist <= radius - edge_softness {
                1.0
            } else {
                1.0 - ((dist - (radius - edge_softness)) / edge_softness).clamp(0.0, 1.0)
            };
            if coverage <= 0.0 {
                continue;
            }

            let existing = *layer.pixels.get_pixel(px as u32, py as u32);
            let delta = coverage * strength * match mode {
                ToneMode::Dodge => 1.0,
                ToneMode::Burn => -1.0,
            };
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let v = existing[c] as f32 / 255.0;
                rgb[c] = ((v + delta).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            layer.pixels.put_pixel(px as u32, py as u32, Rgba([rgb[0], rgb[1], rgb[2], existing[3]]));
        }
    }
}

/// Photoshop's Dodge/Burn: lightens (Dodge) or darkens (Burn) pixels along
/// a brush stroke, `strength` in `[0, 1]` controlling how much each full-
/// coverage dab shifts RGB per pass (alpha is left untouched - dodge/burn
/// changes tone, not opacity). A simplified single "midtones" exposure
/// range rather than Photoshop's separate shadows/midtones/highlights
/// range selector.
pub fn dodge_burn(layer: &mut Layer, points: &[BrushPoint], brush: &Brush, mode: ToneMode, strength: f32, clip: Option<SelectionRect>) {
    walk_path(points, brush.size, |x, y, pressure| {
        tone_dab(layer, x, y, brush.size * 0.5 * pressure, brush, mode, strength, clip);
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpongeMode {
    Saturate,
    Desaturate,
}

fn sponge_dab(layer: &mut Layer, cx: f32, cy: f32, radius: f32, brush: &Brush, mode: SpongeMode, strength: f32, clip: Option<SelectionRect>) {
    if radius <= 0.0 {
        return;
    }
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let edge_softness = (1.0 - brush.hardness).max(0.001) * radius;
            let coverage = if dist <= radius - edge_softness {
                1.0
            } else {
                1.0 - ((dist - (radius - edge_softness)) / edge_softness).clamp(0.0, 1.0)
            };
            if coverage <= 0.0 {
                continue;
            }

            let existing = *layer.pixels.get_pixel(px as u32, py as u32);
            let c = [existing[0] as f32 / 255.0, existing[1] as f32 / 255.0, existing[2] as f32 / 255.0];
            let luminosity = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
            let delta = coverage * strength;
            let factor = match mode {
                SpongeMode::Saturate => 1.0 + delta,
                SpongeMode::Desaturate => (1.0 - delta).max(0.0),
            };
            let mut rgb = [0u8; 3];
            for i in 0..3 {
                rgb[i] = ((luminosity + (c[i] - luminosity) * factor).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            layer.pixels.put_pixel(px as u32, py as u32, Rgba([rgb[0], rgb[1], rgb[2], existing[3]]));
        }
    }
}

/// Photoshop's Sponge: saturates or desaturates pixels along a brush
/// stroke, `strength` in `[0, 1]` controlling how much each full-coverage
/// dab shifts saturation per pass. Works by scaling each pixel's distance
/// from its own luminosity (the same "pull toward/away from gray" idea
/// `adjustments::vibrance` uses, applied as a brush instead of over a
/// whole layer).
pub fn sponge(layer: &mut Layer, points: &[BrushPoint], brush: &Brush, mode: SpongeMode, strength: f32, clip: Option<SelectionRect>) {
    walk_path(points, brush.size, |x, y, pressure| {
        sponge_dab(layer, x, y, brush.size * 0.5 * pressure, brush, mode, strength, clip);
    });
}

fn smudge_dab(layer: &mut Layer, cx: f32, cy: f32, radius: f32, strength: f32, carried: Rgba<u8>, clip: Option<SelectionRect>) -> Rgba<u8> {
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    let center_x = (cx.round() as i64).clamp(0, w as i64 - 1) as u32;
    let center_y = (cy.round() as i64).clamp(0, h as i64 - 1) as u32;
    let sampled_before = *layer.pixels.get_pixel(center_x, center_y);

    if radius <= 0.0 {
        return sampled_before;
    }
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let coverage = 1.0 - (dist / radius);
            let alpha = (coverage * strength).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            let existing = *layer.pixels.get_pixel(px as u32, py as u32);
            let mut rgb = [0u8; 3];
            for i in 0..3 {
                rgb[i] = (existing[i] as f32 * (1.0 - alpha) + carried[i] as f32 * alpha).round() as u8;
            }
            layer.pixels.put_pixel(px as u32, py as u32, Rgba([rgb[0], rgb[1], rgb[2], existing[3]]));
        }
    }
    sampled_before
}

/// Photoshop's Smudge: drags color along a brush stroke, blending each
/// dab's destination pixels toward the color sampled at the *previous*
/// dab's center (before that dab painted) - so color "carries forward"
/// along the stroke direction, the classic smudge smear. `strength` in
/// `[0, 1]` controls how strongly the carried color overwrites the
/// destination at full brush coverage.
pub fn smudge(layer: &mut Layer, points: &[BrushPoint], brush: &Brush, strength: f32, clip: Option<SelectionRect>) {
    if points.is_empty() {
        return;
    }
    let mut carried = *layer.pixels.get_pixel(points[0].x.round().clamp(0.0, layer.pixels.width() as f32 - 1.0) as u32, points[0].y.round().clamp(0.0, layer.pixels.height() as f32 - 1.0) as u32);
    walk_path(points, brush.size, |x, y, pressure| {
        carried = smudge_dab(layer, x, y, brush.size * 0.5 * pressure, strength, carried, clip);
    });
}

/// Photoshop's Clone Stamp: paints along `dest_points` (same path shape as
/// `stroke_path`) with color sampled from `source` plus a constant offset
/// - the offset between `source` and the *first* destination point is
/// fixed for the whole stroke, matching Photoshop's default (non-"aligned
/// off") clone stamp behavior where the source moves in lockstep with the
/// brush once the stroke begins. Samples from a snapshot of the layer
/// taken before the stroke starts, not the live (being-painted) pixels -
/// avoids a self-referential feedback smear when source and destination
/// overlap, a simplification worth calling out even though the visual
/// difference from Photoshop's own same-stroke sampling is subtle.
pub fn clone_stamp(layer: &mut Layer, source: (f32, f32), dest_points: &[BrushPoint], brush: &Brush, clip: Option<SelectionRect>) {
    if dest_points.is_empty() {
        return;
    }
    let offset = (source.0 - dest_points[0].x, source.1 - dest_points[0].y);
    let snapshot = layer.pixels.clone();
    walk_path(dest_points, brush.size, |x, y, pressure| {
        clone_dab(layer, &snapshot, x, y, brush.size * 0.5 * pressure, brush, offset, clip);
    });
}

/// Average RGB (ignoring alpha) within `radius` of `(cx, cy)`, used by
/// `healing_brush` to tone-match a source patch to its destination.
/// Returns `None` if the sampled area is entirely outside the image.
fn average_color_near(image: &image::RgbaImage, cx: f32, cy: f32, radius: f32) -> Option<[f32; 3]> {
    let (w, h) = (image.width(), image.height());
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;
    let mut sum = [0f64; 3];
    let mut count = 0u64;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if x < 0 || y < 0 {
                continue;
            }
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if (dx * dx + dy * dy).sqrt() > radius {
                continue;
            }
            let p = image.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                sum[c] += p[c] as f64;
            }
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some([(sum[0] / count as f64) as f32, (sum[1] / count as f64) as f32, (sum[2] / count as f64) as f32])
}

fn heal_dab(layer: &mut Layer, source: &image::RgbaImage, cx: f32, cy: f32, radius: f32, brush: &Brush, offset: (f32, f32), correction: [f32; 3], clip: Option<SelectionRect>) {
    if radius <= 0.0 {
        return;
    }
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    let min_x = ((cx - radius).floor().max(0.0)) as i64;
    let max_x = ((cx + radius).ceil()).min(w as f32 - 1.0) as i64;
    let min_y = ((cy - radius).floor().max(0.0)) as i64;
    let max_y = ((cy + radius).ceil()).min(h as f32 - 1.0) as i64;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if px < 0 || py < 0 {
                continue;
            }
            if let Some(sel) = clip {
                if !sel.contains(px, py) {
                    continue;
                }
            }
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let edge_softness = (1.0 - brush.hardness).max(0.001) * radius;
            let coverage = if dist <= radius - edge_softness {
                1.0
            } else {
                1.0 - ((dist - (radius - edge_softness)) / edge_softness).clamp(0.0, 1.0)
            };
            if coverage <= 0.0 {
                continue;
            }

            let src_x = px as f32 + offset.0;
            let src_y = py as f32 + offset.1;
            if src_x < 0.0 || src_y < 0.0 || src_x >= source.width() as f32 || src_y >= source.height() as f32 {
                continue;
            }
            let sampled = *source.get_pixel(src_x as u32, src_y as u32);
            let alpha = coverage * (sampled[3] as f32 / 255.0);
            if alpha <= 0.0 {
                continue;
            }
            let existing = *layer.pixels.get_pixel(px as u32, py as u32);
            let out_a = alpha + (existing[3] as f32 / 255.0) * (1.0 - alpha);
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let src = ((sampled[c] as f32 + correction[c]).clamp(0.0, 255.0)) / 255.0;
                let dst = existing[c] as f32 / 255.0;
                let mixed = src * alpha + dst * (existing[3] as f32 / 255.0) * (1.0 - alpha);
                rgb[c] = if out_a > 0.0 { ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
            }
            layer.pixels.put_pixel(px as u32, py as u32, Rgba([rgb[0], rgb[1], rgb[2], (out_a.clamp(0.0, 1.0) * 255.0).round() as u8]));
        }
    }
}

/// Photoshop's Healing Brush: Clone Stamp plus a one-time tone correction,
/// so the source's *texture* transfers but its overall brightness/color
/// shifts to match the destination area it's healing into - the defining
/// difference from a plain clone stamp, simplified from Photoshop's own
/// per-pixel Poisson-blending healing algorithm to a single constant
/// per-channel offset (source patch average vs. destination patch average
/// around the stroke's starting points), computed once for the whole
/// stroke rather than continuously re-matched.
pub fn healing_brush(layer: &mut Layer, source: (f32, f32), dest_points: &[BrushPoint], brush: &Brush, clip: Option<SelectionRect>) {
    if dest_points.is_empty() {
        return;
    }
    let offset = (source.0 - dest_points[0].x, source.1 - dest_points[0].y);
    let snapshot = layer.pixels.clone();

    let sample_radius = brush.size;
    let source_avg = average_color_near(&snapshot, source.0, source.1, sample_radius);
    let dest_avg = average_color_near(&snapshot, dest_points[0].x, dest_points[0].y, sample_radius);
    let correction = match (source_avg, dest_avg) {
        (Some(s), Some(d)) => [d[0] - s[0], d[1] - s[1], d[2] - s[2]],
        _ => [0.0, 0.0, 0.0],
    };

    walk_path(dest_points, brush.size, |x, y, pressure| {
        heal_dab(layer, &snapshot, x, y, brush.size * 0.5 * pressure, brush, offset, correction, clip);
    });
}

fn average_color_rect(image: &image::RgbaImage, x: i64, y: i64, width: u32, height: u32) -> Option<[f32; 3]> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let mut sum = [0f64; 3];
    let mut count = 0u64;
    for dy in 0..height as i64 {
        for dx in 0..width as i64 {
            let (px, py) = (x + dx, y + dy);
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let p = image.get_pixel(px as u32, py as u32);
            for c in 0..3 {
                sum[c] += p[c] as f64;
            }
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some([(sum[0] / count as f64) as f32, (sum[1] / count as f64) as f32, (sum[2] / count as f64) as f32])
}

/// Photoshop's Patch Tool: replaces `dest` (the "damaged" selection) with
/// content copied from a same-sized region starting at `(source_x,
/// source_y)`, with the same one-time tone-correction idea
/// `healing_brush` uses (average color of the source region vs. `dest`'s
/// own original average, applied as a constant per-channel offset to
/// every copied pixel) so the patch blends into its surroundings instead
/// of pasting in verbatim. Reads source pixels from a snapshot taken
/// before the patch, so a source/destination overlap can't feed back into
/// itself mid-copy.
pub fn patch(layer: &mut Layer, dest: SelectionRect, source_x: i64, source_y: i64) {
    let snapshot = layer.pixels.clone();
    let (w, h) = (layer.pixels.width() as i64, layer.pixels.height() as i64);

    let source_avg = average_color_rect(&snapshot, source_x, source_y, dest.width, dest.height);
    let dest_avg = average_color_rect(&snapshot, dest.x, dest.y, dest.width, dest.height);
    let correction = match (source_avg, dest_avg) {
        (Some(s), Some(d)) => [d[0] - s[0], d[1] - s[1], d[2] - s[2]],
        _ => [0.0, 0.0, 0.0],
    };

    for dy in 0..dest.height as i64 {
        for dx in 0..dest.width as i64 {
            let (tx, ty) = (dest.x + dx, dest.y + dy);
            if tx < 0 || ty < 0 || tx >= w || ty >= h {
                continue;
            }
            let (sx, sy) = (source_x + dx, source_y + dy);
            if sx < 0 || sy < 0 || sx >= w || sy >= h {
                continue;
            }
            let sampled = *snapshot.get_pixel(sx as u32, sy as u32);
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                rgb[c] = (sampled[c] as f32 + correction[c]).clamp(0.0, 255.0).round() as u8;
            }
            layer.pixels.put_pixel(tx as u32, ty as u32, Rgba([rgb[0], rgb[1], rgb[2], sampled[3]]));
        }
    }
}

//! Classical (non-ML) content-aware fill / inpainting.
//!
//! This is deliberately scoped: it diffuses color inward from a region's
//! boundary via Jacobi iteration on the discrete Laplace equation (each
//! interior pixel converges toward the average of its neighbors) - the
//! same mathematical family as classic PDE-based inpainting (e.g. the
//! Navier-Stokes/Telea algorithms OpenCV ships). It fills flat regions and
//! smooth gradients convincingly and is fully deterministic and local
//! (no network, no model weights to bundle), but it does **not** synthesize
//! texture or repeat patterns the way Photoshop's real Content-Aware Fill
//! or a GAN/diffusion-based generative fill does - painting over a
//! patterned or textured region produces a blur, not a plausible
//! continuation of the pattern. See `generative_ml::ml_inpaint` for the
//! real ML-backed alternative (MI-GAN); this classical version stays
//! available as the fully local, no-model-weights option for flat colors
//! and smooth gradients, not a placeholder pretending to be the ML one.

use crate::document::{Document, Layer, SelectionRect};
use anyhow::{Context, Result};
use image::Rgba;
use uuid::Uuid;

/// Fills `region` on `layer` by diffusing color in from its boundary.
/// Clips `region` to the layer bounds; does nothing if the clipped region
/// is empty. `iterations` trades quality for speed - each iteration
/// propagates boundary influence one pixel further into the region, so it
/// should be at least `region`'s larger dimension for the diffusion to
/// fully reach the center; the MCP tool defaults to a generous value.
pub fn content_aware_fill(layer: &mut Layer, region: SelectionRect, iterations: usize) {
    let (w, h) = layer.pixels.dimensions();
    let x0 = region.x.clamp(0, w as i64) as u32;
    let y0 = region.y.clamp(0, h as i64) as u32;
    let x1 = (region.x + region.width as i64).clamp(0, w as i64) as u32;
    let y1 = (region.y + region.height as i64).clamp(0, h as i64) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    // Seed the region with the average of its immediate boundary pixels,
    // so the first diffusion pass starts from a reasonable estimate
    // rather than whatever content (or transparency) was there before.
    let mut boundary_points: Vec<(u32, u32)> = Vec::new();
    for x in x0..x1 {
        if y0 > 0 {
            boundary_points.push((x, y0 - 1));
        }
        if y1 < h {
            boundary_points.push((x, y1));
        }
    }
    for y in y0..y1 {
        if x0 > 0 {
            boundary_points.push((x0 - 1, y));
        }
        if x1 < w {
            boundary_points.push((x1, y));
        }
    }
    let mut sum = [0f64; 3];
    let count = boundary_points.len() as u64;
    for (x, y) in boundary_points {
        let p = layer.pixels.get_pixel(x, y);
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
    }
    let seed = if count > 0 {
        Rgba([(sum[0] / count as f64) as u8, (sum[1] / count as f64) as u8, (sum[2] / count as f64) as u8, 255])
    } else {
        // The region covers the whole layer - nothing to diffuse from.
        Rgba([128, 128, 128, 255])
    };
    for y in y0..y1 {
        for x in x0..x1 {
            layer.pixels.put_pixel(x, y, seed);
        }
    }

    for _ in 0..iterations {
        let snapshot = layer.pixels.clone();
        for y in y0..y1 {
            for x in x0..x1 {
                let mut sum = [0f32; 3];
                let mut n = 0f32;
                for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                    if nx < w && ny < h {
                        let p = snapshot.get_pixel(nx, ny);
                        for c in 0..3 {
                            sum[c] += p[c] as f32;
                        }
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    layer.pixels.put_pixel(x, y, Rgba([(sum[0] / n).round() as u8, (sum[1] / n).round() as u8, (sum[2] / n).round() as u8, 255]));
                }
            }
        }
    }
}

/// Photoshop's "Content-Aware" canvas extension (`generative.expand`,
/// minus the text prompt - this is the classical, non-ML edge): grows
/// the canvas by `top`/`bottom`/`left`/
/// `right` pixels (reusing `resize::resize_canvas`, which repositions
/// every layer's existing content in lockstep), then runs
/// `content_aware_fill` over each newly exposed border strip on
/// `layer_id` - so the layer's content appears to extend outward rather
/// than the new area staying blank. Other layers get the same
/// transparent padding `resize_canvas` always gives them; call
/// `content_aware_fill` on any of those too if they need filling as well.
pub fn expand_canvas(doc: &mut Document, layer_id: Uuid, top: u32, bottom: u32, left: u32, right: u32, iterations: usize) -> Result<()> {
    let old_width = doc.width;
    let old_height = doc.height;
    let new_width = old_width + left + right;
    let new_height = old_height + top + bottom;
    crate::resize::resize_canvas(doc, new_width, new_height, left as i64, top as i64);

    let layer = doc.find_layer_mut(layer_id).context("layer not found")?;
    if top > 0 {
        content_aware_fill(layer, SelectionRect { x: 0, y: 0, width: new_width, height: top }, iterations);
    }
    if bottom > 0 {
        content_aware_fill(layer, SelectionRect { x: 0, y: (new_height - bottom) as i64, width: new_width, height: bottom }, iterations);
    }
    if left > 0 {
        content_aware_fill(layer, SelectionRect { x: 0, y: top as i64, width: left, height: old_height }, iterations);
    }
    if right > 0 {
        content_aware_fill(layer, SelectionRect { x: (new_width - right) as i64, y: top as i64, width: right, height: old_height }, iterations);
    }
    Ok(())
}

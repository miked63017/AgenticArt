use crate::document::{Layer, SelectionRect};

/// These adjustments are destructive (applied straight to the layer's
/// pixels). The document model doesn't yet have a non-destructive
/// adjustment-layer node type - that's a data-model change for a later
/// increment; these functions are written so wiring them into a future
/// non-destructive stack only means calling them at render time instead
/// of edit time, not rewriting the math.
fn for_each_pixel(layer: &mut Layer, clip: Option<SelectionRect>, mut f: impl FnMut([f32; 4]) -> [f32; 4]) {
    let (w, h) = (layer.pixels.width(), layer.pixels.height());
    for y in 0..h {
        for x in 0..w {
            if let Some(sel) = clip {
                if !sel.contains(x as i64, y as i64) {
                    continue;
                }
            }
            let p = layer.pixels.get_pixel(x, y);
            let rgba = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0];
            let out = f(rgba);
            layer.pixels.put_pixel(
                x,
                y,
                image::Rgba([
                    (out[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (out[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (out[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (out[3].clamp(0.0, 1.0) * 255.0).round() as u8,
                ]),
            );
        }
    }
}

/// `brightness` and `contrast` are both in [-1.0, 1.0].
pub fn brightness_contrast(layer: &mut Layer, brightness: f32, contrast: f32, clip: Option<SelectionRect>) {
    let contrast_factor = (1.0 + contrast).max(0.0);
    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            c[i] = (c[i] - 0.5) * contrast_factor + 0.5 + brightness;
        }
        c
    });
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s.abs() < 1e-6 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    (hue_to_rgb(p, q, h + 1.0 / 3.0), hue_to_rgb(p, q, h), hue_to_rgb(p, q, h - 1.0 / 3.0))
}

/// `hue_shift_deg` in degrees [-180, 180]; `saturation` and `lightness` are
/// multiplicative/additive deltas in [-1.0, 1.0].
pub fn hue_saturation(layer: &mut Layer, hue_shift_deg: f32, saturation: f32, lightness: f32, clip: Option<SelectionRect>) {
    let hue_shift = hue_shift_deg / 360.0;
    for_each_pixel(layer, clip, |c| {
        let (h, s, l) = rgb_to_hsl(c[0], c[1], c[2]);
        let new_h = (h + hue_shift).rem_euclid(1.0);
        let new_s = (s * (1.0 + saturation)).clamp(0.0, 1.0);
        let new_l = (l + lightness).clamp(0.0, 1.0);
        let (r, g, b) = hsl_to_rgb(new_h, new_s, new_l);
        [r, g, b, c[3]]
    });
}

pub fn invert(layer: &mut Layer, clip: Option<SelectionRect>) {
    for_each_pixel(layer, clip, |c| [1.0 - c[0], 1.0 - c[1], 1.0 - c[2], c[3]]);
}

/// Photoshop's Levels: remaps `[in_black, in_white]` to `[0, 1]` (clamped),
/// applies `gamma` (1.0 = no change, matching the Levels dialog's middle
/// slider), then remaps `[0, 1]` to `[out_black, out_white]`.
pub fn levels(layer: &mut Layer, in_black: f32, in_white: f32, gamma: f32, out_black: f32, out_white: f32, clip: Option<SelectionRect>) {
    let in_range = (in_white - in_black).max(1e-6);
    let gamma = gamma.max(0.01);
    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            let t = ((c[i] - in_black) / in_range).clamp(0.0, 1.0);
            let g = t.powf(1.0 / gamma);
            c[i] = out_black + g * (out_white - out_black);
        }
        c
    });
}

/// Photoshop's Curves, simplified to one master RGB curve (not separate
/// per-channel curves): `points` are `(input, output)` pairs in `[0, 1]`,
/// unsorted-order-tolerant, piecewise-linearly interpolated between
/// whichever two bracket a given input value (same bracketing approach as
/// `gradient::color_at`). Requires at least 2 points.
pub fn curves(layer: &mut Layer, points: &[(f32, f32)], clip: Option<SelectionRect>) {
    if points.len() < 2 {
        return;
    }
    let mut sorted: Vec<(f32, f32)> = points.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let eval = |x: f32| -> f32 {
        if x <= sorted[0].0 {
            return sorted[0].1;
        }
        if x >= sorted[sorted.len() - 1].0 {
            return sorted[sorted.len() - 1].1;
        }
        for pair in sorted.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if x >= a.0 && x <= b.0 {
                let span = (b.0 - a.0).max(f32::EPSILON);
                let t = (x - a.0) / span;
                return a.1 + (b.1 - a.1) * t;
            }
        }
        sorted[sorted.len() - 1].1
    };

    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            c[i] = eval(c[i]);
        }
        c
    });
}

/// Photoshop's Exposure: `exposure` in stops (each +1.0 doubles linear
/// brightness), `offset` is an additive shift applied before the exposure
/// scale (lifts/lowers shadows), `gamma` is the correction applied last
/// (1.0 = no change).
pub fn exposure(layer: &mut Layer, exposure_stops: f32, offset: f32, gamma: f32, clip: Option<SelectionRect>) {
    let scale = 2f32.powf(exposure_stops);
    let gamma = gamma.max(0.01);
    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            c[i] = ((c[i] + offset) * scale).max(0.0).powf(1.0 / gamma);
        }
        c
    });
}

/// Photoshop's Color Balance, simplified to midtones only (not separate
/// shadows/midtones/highlights ranges): `cyan_red`/`magenta_green`/
/// `yellow_blue` are additive shifts in `[-1, 1]` to the red/green/blue
/// channels respectively, weighted down near black/white so pure shadows
/// and highlights shift less than midtones (a rough approximation of
/// Photoshop's "preserve luminosity" tonal weighting).
pub fn color_balance(layer: &mut Layer, cyan_red: f32, magenta_green: f32, yellow_blue: f32, clip: Option<SelectionRect>) {
    let shifts = [cyan_red, magenta_green, yellow_blue];
    for_each_pixel(layer, clip, |mut c| {
        let luminosity = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
        let midtone_weight = 1.0 - (2.0 * luminosity - 1.0).abs();
        for i in 0..3 {
            c[i] = (c[i] + shifts[i] * midtone_weight).clamp(0.0, 1.0);
        }
        c
    });
}

/// Photoshop's Black & White: converts to grayscale using per-channel
/// weights (Photoshop's default preset is close to 40/40/20 red/green/
/// blue; pass whatever mix is wanted - they need not sum to 1.0, though
/// they usually should to avoid clipping).
pub fn black_and_white(layer: &mut Layer, red_weight: f32, green_weight: f32, blue_weight: f32, clip: Option<SelectionRect>) {
    for_each_pixel(layer, clip, |c| {
        let gray = (c[0] * red_weight + c[1] * green_weight + c[2] * blue_weight).clamp(0.0, 1.0);
        [gray, gray, gray, c[3]]
    });
}

/// Photoshop's Vibrance: boosts saturation more for already-low-saturation
/// pixels and less for already-saturated ones (unlike a flat Hue/
/// Saturation boost), so it reads as "smarter" saturation that resists
/// blowing out already-vivid colors. `amount` in `[-1, 1]`.
pub fn vibrance(layer: &mut Layer, amount: f32, clip: Option<SelectionRect>) {
    for_each_pixel(layer, clip, |c| {
        let (h, s, l) = rgb_to_hsl(c[0], c[1], c[2]);
        let boost = amount * (1.0 - s);
        let new_s = (s + boost).clamp(0.0, 1.0);
        let (r, g, b) = hsl_to_rgb(h, new_s, l);
        [r, g, b, c[3]]
    });
}

/// Photoshop's Photo Filter: tints the image toward `color` by `density`
/// (`[0, 1]`) - a straight lerp toward the filter color, the same
/// mathematical shape as an actual colored-glass camera filter's effect
/// on a linear-light image.
pub fn photo_filter(layer: &mut Layer, color: [u8; 3], density: f32, clip: Option<SelectionRect>) {
    let density = density.clamp(0.0, 1.0);
    let filter = [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0];
    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            c[i] = c[i] + (filter[i] - c[i]) * density;
        }
        c
    });
}

/// Photoshop's Channel Mixer: each output channel is a linear combination
/// of the source R/G/B channels plus a constant, via a 3x3 matrix (rows
/// are output R/G/B, columns are source R/G/B weights) and a per-channel
/// constant offset in `[-1, 1]`.
pub fn channel_mixer(layer: &mut Layer, matrix: [[f32; 3]; 3], constants: [f32; 3], clip: Option<SelectionRect>) {
    for_each_pixel(layer, clip, |c| {
        let src = [c[0], c[1], c[2]];
        let mut out = [0f32; 3];
        for row in 0..3 {
            out[row] = constants[row] + (0..3).map(|col| matrix[row][col] * src[col]).sum::<f32>();
        }
        [out[0], out[1], out[2], c[3]]
    });
}

/// Photoshop's Gradient Map: maps each pixel's luminosity to a color from
/// a gradient ramp (reusing `gradient::color_at`'s stop-bracketing/
/// interpolation), the standard way Gradient Map adjustment layers work -
/// shadows to one end of the gradient, highlights to the other.
pub fn gradient_map(layer: &mut Layer, stops: &[crate::gradient::GradientStop], clip: Option<SelectionRect>) {
    if stops.is_empty() {
        return;
    }
    for_each_pixel(layer, clip, |c| {
        let luminosity = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
        let mapped = crate::gradient::color_at(stops, luminosity.clamp(0.0, 1.0));
        [mapped[0] as f32 / 255.0, mapped[1] as f32 / 255.0, mapped[2] as f32 / 255.0, c[3]]
    });
}

/// Photoshop's Posterize: reduces each channel to `levels` discrete steps
/// (`levels >= 2`; Photoshop's dialog allows 2-255).
pub fn posterize(layer: &mut Layer, levels: u32, clip: Option<SelectionRect>) {
    let levels = levels.max(2) as f32;
    for_each_pixel(layer, clip, |mut c| {
        for i in 0..3 {
            c[i] = (c[i] * (levels - 1.0)).round() / (levels - 1.0);
        }
        c
    });
}

/// Photoshop's Threshold: every pixel becomes pure black or pure white
/// based on whether its luminosity is above or below `cutoff` (`[0, 1]`).
pub fn threshold(layer: &mut Layer, cutoff: f32, clip: Option<SelectionRect>) {
    for_each_pixel(layer, clip, |c| {
        let luminosity = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
        let v = if luminosity >= cutoff { 1.0 } else { 0.0 };
        [v, v, v, c[3]]
    });
}

/// Photoshop's Shadows/Highlights: `shadows` (`[0, 1]`) lifts dark tones,
/// `highlights` (`[0, 1]`) pulls down bright ones, each weighted by how
/// far a pixel's luminosity actually is into that tonal range - so
/// midtones are left close to untouched, the same "targeted" shape as the
/// real dialog (simplified: no separate radius/detail/color-correction
/// controls).
pub fn shadows_highlights(layer: &mut Layer, shadows: f32, highlights: f32, clip: Option<SelectionRect>) {
    let shadows = shadows.clamp(0.0, 1.0);
    let highlights = highlights.clamp(0.0, 1.0);
    for_each_pixel(layer, clip, |mut c| {
        let luminosity = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
        let shadow_weight = (1.0 - luminosity).powi(2) * shadows;
        let highlight_weight = luminosity.powi(2) * highlights;
        for i in 0..3 {
            c[i] = (c[i] + shadow_weight * (1.0 - c[i]) - highlight_weight * c[i]).clamp(0.0, 1.0);
        }
        c
    });
}

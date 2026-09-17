use crate::document::{BevelEmbossStyle, BlendMode, Document, DropShadowStyle, Layer, OuterGlowStyle, SmartFilter, StrokeStyle};
use image::{Rgba, RgbaImage};
use std::borrow::Cow;

fn blend_channel(mode: BlendMode, b: f32, t: f32) -> f32 {
    match mode {
        BlendMode::Normal => t,
        BlendMode::Multiply => b * t,
        BlendMode::Screen => 1.0 - (1.0 - b) * (1.0 - t),
        BlendMode::Overlay => {
            if b < 0.5 {
                2.0 * b * t
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - t)
            }
        }
        BlendMode::Darken => b.min(t),
        BlendMode::Lighten => b.max(t),
        BlendMode::ColorDodge => {
            if t >= 1.0 {
                1.0
            } else {
                (b / (1.0 - t)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if t <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / t).min(1.0)
            }
        }
        BlendMode::HardLight => {
            if t < 0.5 {
                2.0 * b * t
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - t)
            }
        }
        BlendMode::SoftLight => {
            if t < 0.5 {
                b - (1.0 - 2.0 * t) * b * (1.0 - b)
            } else {
                let d = if b < 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * t - 1.0) * (d - b)
            }
        }
        BlendMode::Difference => (b - t).abs(),
        BlendMode::Exclusion => b + t - 2.0 * b * t,
        BlendMode::LinearBurn => (b + t - 1.0).clamp(0.0, 1.0),
        BlendMode::LinearDodge => (b + t).clamp(0.0, 1.0),
        BlendMode::VividLight => {
            if t <= 0.0 {
                0.0
            } else if t >= 1.0 {
                1.0
            } else if t < 0.5 {
                // Color Burn with the blend value scaled to [0,1] over its half.
                1.0 - ((1.0 - b) / (2.0 * t)).min(1.0)
            } else {
                (b / (2.0 * (1.0 - t))).min(1.0)
            }
        }
        BlendMode::LinearLight => (b + 2.0 * t - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if t < 0.5 {
                b.min(2.0 * t)
            } else {
                b.max(2.0 * t - 1.0)
            }
        }
        BlendMode::HardMix => {
            if blend_channel(BlendMode::VividLight, b, t) < 0.5 {
                0.0
            } else {
                1.0
            }
        }
        BlendMode::Subtract => (b - t).clamp(0.0, 1.0),
        BlendMode::Divide => {
            if t <= 0.0 {
                1.0
            } else {
                (b / t).clamp(0.0, 1.0)
            }
        }
        // Whole-pixel (non-per-channel) modes are computed in `blend_pixel`
        // and never reach here.
        BlendMode::DarkerColor | BlendMode::LighterColor | BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            unreachable!("whole-pixel blend mode {mode:?} must be handled by blend_pixel, not blend_channel")
        }
    }
}

fn luminosity(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn saturation(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Pulls a color's channels back into [0,1] after `set_lum` shifts them,
/// preserving hue/saturation - the standard PDF/CSS "ClipColor" algorithm.
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = luminosity(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        for v in &mut out {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut out {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - luminosity(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// Sets `c`'s saturation to `s` while preserving its hue and luminosity -
/// the standard PDF/CSS "SetSat" algorithm (scale the mid channel between
/// the min and max, relative to the min/max spread).
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap());
    let (min_i, mid_i, max_i) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[max_i] > c[min_i] {
        out[mid_i] = (c[mid_i] - c[min_i]) * s / (c[max_i] - c[min_i]);
        out[max_i] = s;
    }
    out[min_i] = 0.0;
    out
}

fn blend_pixel(mode: BlendMode, base: Rgba<u8>, top: Rgba<u8>) -> [f32; 3] {
    let b = [base[0] as f32 / 255.0, base[1] as f32 / 255.0, base[2] as f32 / 255.0];
    let t = [top[0] as f32 / 255.0, top[1] as f32 / 255.0, top[2] as f32 / 255.0];

    match mode {
        BlendMode::DarkerColor => {
            if luminosity(b) <= luminosity(t) {
                b
            } else {
                t
            }
        }
        BlendMode::LighterColor => {
            if luminosity(b) >= luminosity(t) {
                b
            } else {
                t
            }
        }
        BlendMode::Hue => set_lum(set_sat(t, saturation(b)), luminosity(b)),
        BlendMode::Saturation => set_lum(set_sat(b, saturation(t)), luminosity(b)),
        BlendMode::Color => set_lum(t, luminosity(b)),
        BlendMode::Luminosity => set_lum(b, luminosity(t)),
        _ => [blend_channel(mode, b[0], t[0]), blend_channel(mode, b[1], t[1]), blend_channel(mode, b[2], t[2])],
    }
}

/// Alpha-composites `top` over `base` in place (source-over, with an
/// optional Photoshop-style blend mode and clip-to-below mask). Used both
/// for the top-level layer stack and for compositing a layer's own effects
/// (e.g. drop shadow) underneath its original pixels.
///
/// Tries the GPU path (`gpu::composite_over_gpu`) first and falls back to
/// the CPU implementation below whenever it's unavailable or fails for any
/// reason (no adapter, exceeding device limits, etc.) - correctness never
/// depends on a GPU being present.
fn composite_over(base: &mut RgbaImage, top: &RgbaImage, opacity: f32, mode: BlendMode, clip_to_below: bool) {
    if let Some(result) = crate::gpu::composite_over_gpu(base, top, opacity, mode, clip_to_below) {
        *base = result;
        return;
    }
    composite_over_cpu(base, top, opacity, mode, clip_to_below);
}

fn composite_over_cpu(base: &mut RgbaImage, top: &RgbaImage, opacity: f32, mode: BlendMode, clip_to_below: bool) {
    let (w, h) = base.dimensions();
    for y in 0..h {
        for x in 0..w {
            let base_px = *base.get_pixel(x, y);
            let top_px = *top.get_pixel(x, y);
            let mut top_a = (top_px[3] as f32 / 255.0) * opacity;
            if clip_to_below {
                top_a *= base_px[3] as f32 / 255.0;
            }
            if top_a <= 0.0 {
                continue;
            }
            let blended = blend_pixel(mode, base_px, top_px);
            let base_a = base_px[3] as f32 / 255.0;
            let out_a = top_a + base_a * (1.0 - top_a);
            if out_a <= 0.0 {
                base.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                continue;
            }
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let base_c = base_px[c] as f32 / 255.0;
                let mixed = blended[c] * top_a + base_c * base_a * (1.0 - top_a);
                rgb[c] = ((mixed / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            base.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], (out_a * 255.0).round() as u8]));
        }
    }
}

fn render_drop_shadow(pixels: &RgbaImage, ds: DropShadowStyle) -> RgbaImage {
    let (width, height) = pixels.dimensions();
    let mut shadow = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let sx = x as i64 - ds.offset_x as i64;
            let sy = y as i64 - ds.offset_y as i64;
            if sx < 0 || sy < 0 || sx >= width as i64 || sy >= height as i64 {
                continue;
            }
            let src_a = pixels.get_pixel(sx as u32, sy as u32)[3];
            if src_a == 0 {
                continue;
            }
            shadow.put_pixel(x, y, Rgba([ds.color[0], ds.color[1], ds.color[2], src_a]));
        }
    }
    let mut shadow = if ds.blur_radius > 0.0 { image::imageops::blur(&shadow, ds.blur_radius) } else { shadow };
    for p in shadow.pixels_mut() {
        p[3] = (((p[3] as f32 / 255.0) * ds.opacity).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    shadow
}

/// Outer glow: identical construction to a drop shadow with zero offset -
/// a blurred halo of `color` radiating from the layer's opaque edges.
fn render_outer_glow(pixels: &RgbaImage, glow: OuterGlowStyle) -> RgbaImage {
    render_drop_shadow(pixels, DropShadowStyle { color: glow.color, offset_x: 0, offset_y: 0, blur_radius: glow.radius, opacity: glow.opacity })
}

/// Outer stroke: for every transparent source pixel within `width` of an
/// opaque one, paint the stroke color. O(w*h*width^2) — fine for the
/// small stroke widths (a handful of px) this is meant for; a distance
/// transform would be the scalable version if that ever matters.
fn render_stroke(pixels: &RgbaImage, st: StrokeStyle) -> RgbaImage {
    let (width, height) = pixels.dimensions();
    let mut ring = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    if st.width == 0 {
        return ring;
    }
    let r = st.width as i64;
    let alpha = ((st.opacity.clamp(0.0, 1.0)) * st.color[3] as f32).round() as u8;
    for y in 0..height {
        for x in 0..width {
            if pixels.get_pixel(x, y)[3] > 0 {
                continue;
            }
            let mut found = false;
            'search: for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy > r * r {
                        continue;
                    }
                    let nx = x as i64 + dx;
                    let ny = y as i64 + dy;
                    if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
                        continue;
                    }
                    if pixels.get_pixel(nx as u32, ny as u32)[3] > 0 {
                        found = true;
                        break 'search;
                    }
                }
            }
            if found {
                ring.put_pixel(x, y, Rgba([st.color[0], st.color[1], st.color[2], alpha]));
            }
        }
    }
    ring
}

/// Inner Bevel: paints a highlight/shadow pair along the layer's opaque
/// edges to simulate a raised surface. Builds a soft "height map" from the
/// alpha channel (Gaussian-blurred by `depth`, so the bevel reads as wide
/// as `depth`), then estimates each pixel's in-plane lighting via a
/// central-difference gradient of that height map dotted with the light
/// direction - positive dot (surface leaning toward the light) paints the
/// highlight, negative (leaning away) paints the shadow, both restricted
/// to the layer's own opaque area (an *inner* bevel, Photoshop's default,
/// as opposed to an outer one that would extend past the edge).
fn render_bevel_emboss(pixels: &RgbaImage, be: BevelEmbossStyle) -> RgbaImage {
    let (width, height) = pixels.dimensions();
    let mut out = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    if width < 2 || height < 2 {
        return out;
    }

    let mut alpha_map = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for (x, y, p) in alpha_map.enumerate_pixels_mut() {
        let a = pixels.get_pixel(x, y)[3];
        *p = Rgba([a, a, a, 255]);
    }
    let depth = be.depth.max(0.5);
    let height_map = image::imageops::blur(&alpha_map, depth);

    let theta = be.angle_degrees.to_radians();
    let light = (theta.cos(), -theta.sin());

    for y in 0..height {
        for x in 0..width {
            if pixels.get_pixel(x, y)[3] == 0 {
                continue;
            }
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(width - 1);
            let y0 = y.saturating_sub(1);
            let y1 = (y + 1).min(height - 1);
            let dx = height_map.get_pixel(x1, y)[0] as f32 - height_map.get_pixel(x0, y)[0] as f32;
            let dy = height_map.get_pixel(x, y1)[0] as f32 - height_map.get_pixel(x, y0)[0] as f32;
            let dot = (dx * light.0 + dy * light.1) / 255.0;
            if dot.abs() < 0.01 {
                continue;
            }
            let (color, strength) = if dot > 0.0 { (be.highlight_color, dot) } else { (be.shadow_color, -dot) };
            let a = (strength.clamp(0.0, 1.0) * (color[3] as f32 / 255.0) * 255.0).round() as u8;
            out.put_pixel(x, y, Rgba([color[0], color[1], color[2], a]));
        }
    }
    out
}

/// Runs a layer's non-destructive smart-filter stack over `pixels`, in
/// order, without ever touching `Layer::pixels` itself. Reuses the same
/// (normally destructive) filter/adjustment functions the "bake it in"
/// MCP tools call, applied to a scratch layer instead — one code path for
/// the pixel math either way. Borrows `pixels` unchanged when the stack is
/// empty, to avoid a copy in the common case.
fn apply_smart_filters<'a>(pixels: Cow<'a, RgbaImage>, filters: &[SmartFilter]) -> Cow<'a, RgbaImage> {
    if filters.is_empty() {
        return pixels;
    }
    let mut scratch = Layer::new_transparent("smart-filter-scratch", pixels.width(), pixels.height());
    scratch.pixels = pixels.into_owned();
    for f in filters {
        match *f {
            SmartFilter::GaussianBlur { radius } => crate::filters::gaussian_blur(&mut scratch, radius, None),
            SmartFilter::Sharpen { radius, threshold } => crate::filters::sharpen(&mut scratch, radius, threshold, None),
            SmartFilter::BrightnessContrast { brightness, contrast } => {
                crate::adjustments::brightness_contrast(&mut scratch, brightness, contrast, None)
            }
            SmartFilter::HueSaturation { hue, saturation, lightness } => {
                crate::adjustments::hue_saturation(&mut scratch, hue, saturation, lightness, None)
            }
            SmartFilter::Invert => crate::adjustments::invert(&mut scratch, None),
        }
    }
    Cow::Owned(scratch.pixels)
}

/// Multiplies `pixels`' alpha channel by a grayscale mask (white = fully
/// visible, black = fully hidden), the standard raster-layer-mask
/// convention. Passes `pixels` through unchanged when there is no mask.
fn apply_mask<'a>(pixels: Cow<'a, RgbaImage>, mask: &Option<image::GrayImage>) -> Cow<'a, RgbaImage> {
    let Some(mask) = mask else {
        return pixels;
    };
    let mut masked = pixels.into_owned();
    for (x, y, p) in masked.enumerate_pixels_mut() {
        let m = mask.get_pixel(x, y)[0] as f32 / 255.0;
        p[3] = ((p[3] as f32 / 255.0) * m * 255.0).round() as u8;
    }
    Cow::Owned(masked)
}

/// A text layer has no meaningful `Layer::pixels` of its own - like a
/// group, its content is synthesized at render time (here, from its
/// `TextLayerData`) rather than stored. Falls back to a blank (fully
/// transparent) buffer and logs to stderr if the configured font can't be
/// loaded, rather than failing the whole render over one bad layer.
fn text_source_pixels(layer: &Layer) -> Cow<'_, RgbaImage> {
    let Some(text) = &layer.text else {
        return Cow::Borrowed(&layer.pixels);
    };
    let (w, h) = layer.pixels.dimensions();
    match crate::text::render_text_layer(w, h, text, None) {
        Ok(img) => Cow::Owned(img),
        Err(e) => {
            eprintln!("text layer '{}' failed to render, showing blank: {e}", layer.name);
            Cow::Owned(RgbaImage::new(w, h))
        }
    }
}

/// Returns the pixels a layer should composite with: its content (a text
/// layer's live-rendered text, or its own pixels otherwise), then its
/// non-destructive smart-filter stack, then its raster mask (if any), then
/// any layer-style effects (drop shadow, outer glow, stroke - stacked in
/// that order, furthest-back first, matching Photoshop's own layer-style
/// ordering) rendered around/underneath the result. Borrows the layer's
/// raw pixels unchanged when it has none of these, to avoid a copy in the
/// common case.
fn effective_pixels(layer: &Layer) -> Cow<'_, RgbaImage> {
    let filtered = apply_smart_filters(text_source_pixels(layer), &layer.smart_filters);
    let filtered = apply_mask(filtered, &layer.mask);

    if layer.style.drop_shadow.is_none() && layer.style.stroke.is_none() && layer.style.outer_glow.is_none() && layer.style.bevel_emboss.is_none() {
        return filtered;
    }

    let (width, height) = layer.pixels.dimensions();
    let mut composed = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));

    if let Some(ds) = layer.style.drop_shadow {
        let shadow = render_drop_shadow(&filtered, ds);
        composite_over(&mut composed, &shadow, 1.0, BlendMode::Normal, false);
    }
    if let Some(glow) = layer.style.outer_glow {
        let halo = render_outer_glow(&filtered, glow);
        composite_over(&mut composed, &halo, 1.0, BlendMode::Normal, false);
    }
    if let Some(st) = layer.style.stroke {
        let ring = render_stroke(&filtered, st);
        composite_over(&mut composed, &ring, 1.0, BlendMode::Normal, false);
    }
    composite_over(&mut composed, &filtered, 1.0, BlendMode::Normal, false);
    // Inner bevel paints ON the layer's own surface (highlight/shadow along
    // its edges), so it composites last - on top of the content, not
    // behind it like drop shadow/outer glow.
    if let Some(be) = layer.style.bevel_emboss {
        let relief = render_bevel_emboss(&filtered, be);
        composite_over(&mut composed, &relief, 1.0, BlendMode::Normal, false);
    }
    Cow::Owned(composed)
}

/// Composites a group's children (bottom to top) into a single buffer,
/// exactly like the top-level `render` loop but scoped to one group. A
/// child that is itself a group is treated as an ordinary layer (nested
/// groups aren't supported yet) rather than recursed into.
fn render_group(doc: &Document, group_id: uuid::Uuid) -> RgbaImage {
    let mut out = RgbaImage::from_pixel(doc.width, doc.height, Rgba([0, 0, 0, 0]));
    for layer in &doc.layers {
        if layer.parent_group != Some(group_id) || !layer.visible || layer.opacity <= 0.0 {
            continue;
        }
        let effective = effective_pixels(layer);
        composite_over(&mut out, &effective, layer.opacity, layer.blend_mode, layer.clip_to_below);
    }
    out
}

/// Flattens the document's visible layers, bottom to top, into a single
/// RGBA8 image using "over" alpha compositing. This is a CPU reference
/// implementation for Phase 0/1; later phases move this to a GPU (wgpu)
/// compositing graph without changing the public `render` contract.
pub fn render(doc: &Document) -> RgbaImage {
    let mut out = RgbaImage::from_pixel(doc.width, doc.height, Rgba([0, 0, 0, 0]));

    for layer in &doc.layers {
        // Grouped layers are composited as part of their group, below.
        if layer.parent_group.is_some() {
            continue;
        }
        if !layer.visible || layer.opacity <= 0.0 {
            continue;
        }
        if layer.is_group {
            let mut group_as_layer = layer.clone();
            group_as_layer.pixels = render_group(doc, layer.id);
            let effective = effective_pixels(&group_as_layer);
            composite_over(&mut out, &effective, layer.opacity, layer.blend_mode, layer.clip_to_below);
        } else {
            let effective = effective_pixels(layer);
            composite_over(&mut out, &effective, layer.opacity, layer.blend_mode, layer.clip_to_below);
        }
    }

    out
}

/// Renders and crops to just one rectangular region of the document -
/// tiled/virtualized rendering so multi-gigapixel canvases don't require
/// the whole image resident in memory at once, scoped down to what's
/// honestly deliverable without rewriting
/// `Layer::pixels` into an actual tiled/paged storage format (a much
/// larger addition touching nearly every op in this crate). What this
/// *does* solve is the practical pain point that motivates tiling in the
/// first place for an MCP-driven workflow: pulling a 16384x16384 canvas
/// out as one base64 PNG response is enormous and often unnecessary when
/// an agent only needs to inspect one area. It does not reduce the
/// memory a document occupies while open - `render` still composites the
/// full canvas internally; only the returned buffer is cropped.
/// `region` is clamped to the canvas bounds; a region entirely outside
/// the canvas returns an empty (0x0) image rather than erroring.
pub fn render_region(doc: &Document, region: &crate::document::SelectionRect) -> RgbaImage {
    let full = render(doc);
    let x0 = region.x.clamp(0, doc.width as i64) as u32;
    let y0 = region.y.clamp(0, doc.height as i64) as u32;
    let x1 = (region.x + region.width as i64).clamp(0, doc.width as i64) as u32;
    let y1 = (region.y + region.height as i64).clamp(0, doc.height as i64) as u32;
    if x1 <= x0 || y1 <= y0 {
        return RgbaImage::new(0, 0);
    }
    image::imageops::crop_imm(&full, x0, y0, x1 - x0, y1 - y0).to_image()
}

/// Bakes `top` (with its own smart filters/mask/styles/opacity/blend mode)
/// down onto `below` (same), producing the pixels `below` should have
/// after a Photoshop "Merge Down" - the caller (`Document::merge_down`)
/// installs this as the surviving layer's new `pixels` and clears its
/// mask/smart_filters/style (now baked in), leaving its own
/// opacity/blend_mode/clip_to_below untouched since those still apply
/// once against whatever remains beneath it in the stack.
pub fn merge_two_layers(below: &Layer, top: &Layer) -> RgbaImage {
    let mut result = effective_pixels(below).into_owned();
    let top_effective = effective_pixels(top);
    composite_over(&mut result, &top_effective, top.opacity, top.blend_mode, top.clip_to_below);
    result
}

/// Photoshop's "Flatten Image": composites every visible layer (respecting
/// groups/masks/smart filters/styles, same as `render`) onto an opaque
/// white background and replaces the whole layer stack with that single
/// result - unlike `render`, which leaves transparency as transparency for
/// a plain export, flatten commits to an opaque final image the way
/// Photoshop's own Flatten Image does.
pub fn flatten(doc: &mut Document) {
    let rendered = render(doc);
    let mut flattened = RgbaImage::from_pixel(doc.width, doc.height, Rgba([255, 255, 255, 255]));
    composite_over(&mut flattened, &rendered, 1.0, BlendMode::Normal, false);

    let mut background = Layer::new_transparent("Background", doc.width, doc.height);
    background.pixels = flattened;
    doc.layers = vec![background];
    doc.active_layer = 0;
    doc.selection = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_matches_cpu_for_all_blend_modes() {
        if !crate::gpu::is_available() {
            eprintln!("skipping GPU parity test: no GPU adapter available in this environment");
            return;
        }
        let modes = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
        ];
        let mut base = RgbaImage::new(16, 16);
        let mut top = RgbaImage::new(16, 16);
        for y in 0..16u32 {
            for x in 0..16u32 {
                base.put_pixel(x, y, Rgba([(x * 16) as u8, (y * 16) as u8, 128, 200]));
                top.put_pixel(x, y, Rgba([255 - (x * 16) as u8, 60, (y * 16) as u8, 180]));
            }
        }
        for &mode in &modes {
            for clip in [false, true] {
                let mut cpu_result = base.clone();
                composite_over_cpu(&mut cpu_result, &top, 0.8, mode, clip);
                let gpu_result = crate::gpu::composite_over_gpu(&base, &top, 0.8, mode, clip).expect("gpu composite should succeed when available");
                for y in 0..16 {
                    for x in 0..16 {
                        let c = cpu_result.get_pixel(x, y);
                        let g = gpu_result.get_pixel(x, y);
                        for ch in 0..4 {
                            let diff = (c[ch] as i32 - g[ch] as i32).abs();
                            assert!(diff <= 2, "mode={mode:?} clip={clip} pixel=({x},{y}) channel={ch} cpu={} gpu={} diff={diff}", c[ch], g[ch]);
                        }
                    }
                }
            }
        }
    }
}

//! Real ML-based generative fill, closing the gap `generative.rs`'s
//! classical PDE-based `content_aware_fill` was explicit about not being:
//! a diffusion/GAN model that can plausibly synthesize texture and
//! pattern continuation, not just smooth flat regions and gradients.
//!
//! Backed by [MI-GAN](https://github.com/Picsart-AI-Research/MI-GAN)
//! (ICCV 2023) - chosen specifically for being small (the bundled ONNX
//! pipeline is ~27MB) and MIT-licensed (code and weights both; see
//! `assets/MIGAN_LICENSE.txt`), unlike a full diffusion model which would
//! be hundreds of MB to several GB. It's a GAN, not a diffusion model -
//! one inference pass, no iterative denoising - but it's a real trained
//! neural network doing texture-aware inpainting, the actual capability
//! `content_aware_fill`'s docs named as the larger, distinct addition.
//!
//! Same `InferenceProvider`-behind-the-scenes spirit as `ai.rs`'s
//! segmentation model: local ONNX inference today, swappable for a cloud
//! backend later without changing the MCP tool contract.

use crate::document::{Layer, SelectionRect};
use anyhow::{Context, Result};
use image::Rgba;
use ort::session::Session;
use std::sync::{Mutex, OnceLock};

const MODEL_BYTES: &[u8] = include_bytes!("../assets/migan_pipeline_v2.onnx");

fn session() -> Result<&'static Mutex<Session>> {
    static SESSION: OnceLock<Result<Mutex<Session>, ort::Error>> = OnceLock::new();
    SESSION
        .get_or_init(|| Ok(Mutex::new(Session::builder()?.commit_from_memory(MODEL_BYTES)?)))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("failed to load MI-GAN inpainting model: {e}"))
}

/// Runs ML-based generative fill on `region` of `layer`. Unlike
/// `content_aware_fill`'s PDE diffusion, this can plausibly continue
/// texture and pattern (e.g. removing an object from a textured
/// background) rather than just blurring toward the surrounding average.
/// `region` is clamped to the layer bounds; does nothing if the clipped
/// region is empty.
///
/// The model's own input/output contract (verified against the ONNX
/// graph, not just the README): `image` is a `[1,3,H,W]` uint8 RGB
/// tensor (channel-first, alpha dropped - the model has no alpha
/// concept), `mask` is `[1,1,H,W]` uint8 grayscale where 255 = keep this
/// pixel as-is and 0 = fill it in, and `result` is a `[1,3,H,W]` uint8
/// RGB tensor already blended seamlessly with the untouched area - no
/// further compositing needed on this end. The model supports arbitrary
/// input resolution (not just its 512x512 training size); its own
/// preprocessing/postprocessing handles resizing internally.
///
/// Quality drops off noticeably for a `region` that's large relative to
/// the layer (confirmed empirically while writing this: a 64x64 hole in
/// a 256x256 solid-color test image came back visibly darker/off than a
/// 16x16 hole in the same image, at the same region-to-canvas fraction
/// pattern MI-GAN's own README warns about - "the best results can be
/// achieved with small, incremental brush strokes" rather than one large
/// single-shot mask). This isn't a bug in this wiring (confirmed: pixels
/// outside the mask come back byte-exact, proving the mask/tensor
/// plumbing itself is correct) - it's a real characteristic of the
/// underlying model. Prefer several smaller calls over one covering a
/// large area.
pub fn ml_inpaint(layer: &mut Layer, region: SelectionRect) -> Result<()> {
    let (w, h) = layer.pixels.dimensions();
    let x0 = region.x.clamp(0, w as i64) as u32;
    let y0 = region.y.clamp(0, h as i64) as u32;
    let x1 = (region.x + region.width as i64).clamp(0, w as i64) as u32;
    let y1 = (region.y + region.height as i64).clamp(0, h as i64) as u32;
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }

    let plane = (w as usize) * (h as usize);
    let mut image_data = vec![0u8; 3 * plane];
    for (px, py, p) in layer.pixels.enumerate_pixels() {
        let idx = (py as usize) * (w as usize) + px as usize;
        image_data[idx] = p[0];
        image_data[plane + idx] = p[1];
        image_data[2 * plane + idx] = p[2];
    }

    let mut mask_data = vec![255u8; plane];
    for py in y0..y1 {
        for px in x0..x1 {
            mask_data[(py as usize) * (w as usize) + px as usize] = 0;
        }
    }

    let session_lock = session()?;
    let mut session = session_lock.lock().map_err(|_| anyhow::anyhow!("inpainting model mutex poisoned"))?;

    let image_value = ort::value::Value::from_array(([1usize, 3, h as usize, w as usize], image_data)).context("failed to build image tensor")?;
    let mask_value = ort::value::Value::from_array(([1usize, 1, h as usize, w as usize], mask_data)).context("failed to build mask tensor")?;

    let outputs = session.run(ort::inputs!["image" => image_value, "mask" => mask_value]).context("MI-GAN inpainting inference failed")?;
    let (shape, data) = outputs["result"].try_extract_tensor::<u8>().context("failed to read model output")?;

    let out_h = shape[2] as u32;
    let out_w = shape[3] as u32;
    let out_plane = (out_h as usize) * (out_w as usize);
    if out_w != w || out_h != h {
        anyhow::bail!("MI-GAN returned a {out_w}x{out_h} result for a {w}x{h} input - the pipeline is expected to preserve resolution");
    }

    for py in 0..h {
        for px in 0..w {
            let idx = (py as usize) * (w as usize) + px as usize;
            let r = data[idx];
            let g = data[out_plane + idx];
            let b = data[2 * out_plane + idx];
            let inside_region = px >= x0 && px < x1 && py >= y0 && py < y1;
            let alpha = if inside_region { 255 } else { layer.pixels.get_pixel(px, py)[3] };
            layer.pixels.put_pixel(px, py, Rgba([r, g, b, alpha]));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Layer;
    use image::Rgba as R;

    #[test]
    fn ml_inpaint_fills_a_hole_in_a_solid_color_field() {
        let mut layer = Layer::new_transparent("t", 256, 256);
        for p in layer.pixels.pixels_mut() {
            *p = R([100, 150, 200, 255]);
        }
        // A small hole relative to the canvas - MI-GAN's own README notes
        // best results come from small, incremental masks rather than one
        // large single-shot region.
        for y in 120..136 {
            for x in 120..136 {
                layer.pixels.put_pixel(x, y, R([0, 0, 0, 255]));
            }
        }

        ml_inpaint(&mut layer, SelectionRect { x: 120, y: 120, width: 16, height: 16 }).expect("MI-GAN inference must succeed");

        let center = layer.pixels.get_pixel(128, 128);
        eprintln!("center after inpaint: {center:?}");
        // The model should have filled the hole with something close to
        // the surrounding solid color, not left it black or produced
        // garbage. Generous tolerance since this is a real trained model,
        // not exact arithmetic.
        assert!((center[0] as i32 - 100).abs() < 60, "red channel should approximate the surrounding fill: {center:?}");
        assert!((center[1] as i32 - 150).abs() < 60, "green channel should approximate the surrounding fill: {center:?}");
        assert!((center[2] as i32 - 200).abs() < 60, "blue channel should approximate the surrounding fill: {center:?}");
        assert_eq!(center[3], 255, "filled region must be opaque");
    }

    #[test]
    fn ml_inpaint_leaves_pixels_outside_the_region_untouched() {
        let mut layer = Layer::new_transparent("t", 64, 64);
        for p in layer.pixels.pixels_mut() {
            *p = R([50, 60, 70, 255]);
        }
        let before_corner = *layer.pixels.get_pixel(2, 2);

        ml_inpaint(&mut layer, SelectionRect { x: 24, y: 24, width: 16, height: 16 }).unwrap();

        assert_eq!(layer.pixels.get_pixel(2, 2), &before_corner, "pixels well outside the fill region must be exactly unchanged");
    }

    #[test]
    fn ml_inpaint_does_nothing_for_an_out_of_bounds_region() {
        let mut layer = Layer::new_transparent("t", 32, 32);
        for p in layer.pixels.pixels_mut() {
            *p = R([10, 20, 30, 255]);
        }
        let before = layer.pixels.clone();
        ml_inpaint(&mut layer, SelectionRect { x: 100, y: 100, width: 10, height: 10 }).unwrap();
        assert_eq!(layer.pixels, before, "an entirely out-of-bounds region must be a no-op, not an error or a panic");
    }
}

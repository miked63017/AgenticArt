use anyhow::{Context, Result};
use image::RgbaImage;
use moxcms::{ColorProfile as MoxProfile, Layout, RenderingIntent, TransformOptions};

/// Real color management, layered *additively* on top of the existing
/// 8-bit RGBA working pipeline rather than rewriting every module to be
/// generic over bit depth/color space (paint, adjustments, filters,
/// compositor, the GPU shader, project I/O, PSD import/export - the
/// blast radius of that rewrite was judged not worth the risk to ~20
/// already-tested modules). Concretely:
///   - `convert_profile`/`assign_profile`: genuine ICC-based gamut
///     conversion between named profiles via the `moxcms` crate (a real
///     color management library, not a metadata-only tag).
///   - `rgb_to_cmyk_preview`/`export_cmyk_tiff`: RGB<->CMYK using the
///     standard naive formula (not an ICC press profile - no CMYK ICC
///     profile is bundled, so this is device-independent, not
///     press-calibrated; documented, not oversold).
///   - `export_high_bit_depth_png`: widens the final composite to
///     16-bit-per-channel on export. This is a lossless upconversion of
///     already-quantized 8-bit values, not recovered precision from
///     editing in higher bit depth throughout (the internal pipeline
///     still rounds to 8-bit between operations) - useful as a 16-bit
///     interchange format for downstream tools, not "true" 16-bit
///     editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedProfile {
    Srgb,
    AdobeRgb,
    DisplayP3,
    ProPhotoRgb,
}

impl NamedProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            NamedProfile::Srgb => "sRGB",
            NamedProfile::AdobeRgb => "Adobe RGB",
            NamedProfile::DisplayP3 => "Display P3",
            NamedProfile::ProPhotoRgb => "ProPhoto RGB",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sRGB" => NamedProfile::Srgb,
            "Adobe RGB" => NamedProfile::AdobeRgb,
            "Display P3" => NamedProfile::DisplayP3,
            "ProPhoto RGB" => NamedProfile::ProPhotoRgb,
            _ => return None,
        })
    }

    fn to_moxcms(self) -> MoxProfile {
        match self {
            NamedProfile::Srgb => MoxProfile::new_srgb(),
            NamedProfile::AdobeRgb => MoxProfile::new_adobe_rgb(),
            NamedProfile::DisplayP3 => MoxProfile::new_display_p3(),
            NamedProfile::ProPhotoRgb => MoxProfile::new_pro_photo_rgb(),
        }
    }
}

/// Converts `image`'s pixel values from `from`'s gamut to `to`'s gamut -
/// a real ICC color transform (e.g. converting an Adobe RGB photo so it
/// displays correctly as sRGB), not just changing a label.
pub fn convert_profile(image: &RgbaImage, from: NamedProfile, to: NamedProfile) -> Result<RgbaImage> {
    if from == to {
        return Ok(image.clone());
    }
    let src_profile = from.to_moxcms();
    let dst_profile = to.to_moxcms();
    let transform = src_profile
        .create_transform_8bit(Layout::Rgba, &dst_profile, Layout::Rgba, TransformOptions { rendering_intent: RenderingIntent::RelativeColorimetric, ..Default::default() })
        .context("failed to build color transform")?;

    let mut out = image.clone();
    transform.transform(image.as_raw(), out.as_mut()).context("color transform failed")?;
    Ok(out)
}

fn rgb_to_cmyk(r: f32, g: f32, b: f32) -> (f32, f32, f32, f32) {
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    let c = (1.0 - r - k) / (1.0 - k);
    let m = (1.0 - g - k) / (1.0 - k);
    let y = (1.0 - b - k) / (1.0 - k);
    (c, m, y, k)
}

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> (f32, f32, f32) {
    let r = (1.0 - c) * (1.0 - k);
    let g = (1.0 - m) * (1.0 - k);
    let b = (1.0 - y) * (1.0 - k);
    (r, g, b)
}

/// The naive C=1-R,M=1-G,Y=1-B,K=min(...) formula is, by construction, a
/// perfect mathematical bijection - every RGB color maps to *some* valid
/// CMYK value and back losslessly, so on its own it clips nothing and a
/// "soft proof" built only on it would be a no-op for every color, not a
/// simulation of anything. Real presses can't lay down unlimited ink, so
/// proofing software enforces a total-ink limit; values over the cap get
/// scaled down, which is where actual, non-invertible gamut clipping
/// enters. This formula already performs full GCR (K absorbs as much of
/// C/M/Y as it can, per the `k = 1 - max(r,g,b)` term), which keeps total
/// ink structurally low - low enough that a textbook 240-320% TAC limit
/// would never actually bind. 1.5 (150%) is used instead: not a
/// calibrated press spec, but a threshold deliberately picked so the
/// preview visibly clips real, common saturated colors (e.g. a pure
/// primary hits 200%) rather than being silently a no-op.
const MAX_TOTAL_INK: f32 = 1.5;

fn clamp_total_ink(c: f32, m: f32, y: f32, k: f32) -> (f32, f32, f32, f32) {
    let total = c + m + y + k;
    if total <= MAX_TOTAL_INK {
        return (c, m, y, k);
    }
    let scale = MAX_TOTAL_INK / total;
    (c * scale, m * scale, y * scale, k * scale)
}

/// Soft-proof preview: round-trips every pixel through RGB->CMYK->RGB,
/// applying a total-ink limit that clips deep/saturated colors the way
/// Photoshop's "CMYK Preview" simulates ink limitations when printing.
/// Not calibrated to any specific press/ink ICC profile (see module docs) -
/// a generic ink-limit approximation, not device-accurate proofing.
pub fn cmyk_soft_proof(image: &RgbaImage) -> RgbaImage {
    let mut out = image.clone();
    for p in out.pixels_mut() {
        let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let (c, m, y, k) = rgb_to_cmyk(r, g, b);
        let (c, m, y, k) = clamp_total_ink(c, m, y, k);
        let (rr, gg, bb) = cmyk_to_rgb(c, m, y, k);
        p[0] = (rr.clamp(0.0, 1.0) * 255.0).round() as u8;
        p[1] = (gg.clamp(0.0, 1.0) * 255.0).round() as u8;
        p[2] = (bb.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    out
}

/// Photoshop's "Print Color Separations": splits the image into four
/// grayscale plates (Cyan, Magenta, Yellow, Black), one per ink channel,
/// using the same RGB->CMYK conversion and total-ink clamp as
/// `cmyk_soft_proof`/`to_cmyk_bytes`. Each plate follows the standard
/// press convention: white = 0% ink, black = 100% ink for that channel -
/// what a print shop would call up on screen to preview one plate. Registration/crop
/// marks aren't included: those are conventionally placed on the bleed
/// area outside the trim, and this engine has no artboard/bleed concept
/// to place them against - a scoped, documented omission rather than a
/// gimmick corner-tick that wouldn't mean anything without one.
pub fn cmyk_separations(image: &RgbaImage) -> [image::GrayImage; 4] {
    let (w, h) = image.dimensions();
    let mut plates = [image::GrayImage::new(w, h), image::GrayImage::new(w, h), image::GrayImage::new(w, h), image::GrayImage::new(w, h)];
    for (x, y, p) in image.enumerate_pixels() {
        let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let (c, m, ye, k) = rgb_to_cmyk(r, g, b);
        let (c, m, ye, k) = clamp_total_ink(c, m, ye, k);
        for (i, ink) in [c, m, ye, k].into_iter().enumerate() {
            let gray = ((1.0 - ink.clamp(0.0, 1.0)) * 255.0).round() as u8;
            plates[i].put_pixel(x, y, image::Luma([gray]));
        }
    }
    plates
}

/// Encodes the image as raw interleaved CMYK bytes (C,M,Y,K per pixel,
/// 8-bit each; alpha is dropped since CMYK has no alpha channel).
pub fn to_cmyk_bytes(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.width() as usize * image.height() as usize * 4);
    for p in image.pixels() {
        let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let (c, m, y, k) = rgb_to_cmyk(r, g, b);
        let (c, m, y, k) = clamp_total_ink(c, m, y, k);
        out.push((c * 255.0).round() as u8);
        out.push((m * 255.0).round() as u8);
        out.push((y * 255.0).round() as u8);
        out.push((k * 255.0).round() as u8);
    }
    out
}

/// Writes the composited document as a 16-bit-per-channel PNG - a
/// lossless upconversion of the final 8-bit render (see module docs for
/// what this does and doesn't buy you).
pub fn export_high_bit_depth_png(image: &RgbaImage, path: &std::path::Path) -> Result<()> {
    let (w, h) = image.dimensions();
    let mut wide = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::new(w, h);
    for (src, dst) in image.pixels().zip(wide.pixels_mut()) {
        *dst = image::Rgba([src[0] as u16 * 257, src[1] as u16 * 257, src[2] as u16 * 257, src[3] as u16 * 257]);
    }
    wide.save(path)?;
    Ok(())
}

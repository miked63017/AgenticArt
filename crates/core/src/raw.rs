//! Camera RAW processing: reconstructs a full-color image from Bayer
//! color-filter-array (CFA) sensor mosaic data - the actual algorithmic
//! core of "RAW image processing," independent of any particular camera
//! manufacturer's proprietary container format. A camera sensor has only
//! one color filter per photosite (arranged in a repeating 2x2 tile), so
//! every RAW workflow's first real step is demosaicing: reconstructing
//! the two missing color channels at each pixel from its same-channel
//! neighbors.
//!
//! Unwrapping a specific manufacturer's proprietary container (CR2, NEF,
//! ARW, ...) - their own compression and metadata encoding - is a
//! distinct, per-format addition this doesn't attempt (and couldn't be
//! responsibly verified here without real camera sample files, which
//! aren't available in this environment). What's implemented is the
//! actual demosaicing math, operating on already-unwrapped mosaic data
//! (e.g. a plain grayscale image representing the raw sensor readout,
//! which is exactly how DNG - the one open, documented RAW container -
//! stores it, modulo DNG's own TIFF-tag metadata that isn't parsed here
//! either).

use crate::document::{Document, Layer};
use anyhow::{Context, Result};
use image::{GrayImage, Rgba, RgbaImage};
use std::path::Path;

/// Bayer CFA layout: which color filter sits at each of the four
/// positions in the repeating 2x2 tile, starting from `(0, 0)`. RGGB is
/// by far the most common arrangement in consumer cameras.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BayerPattern {
    Rggb,
    Bggr,
    Grbg,
    Gbrg,
}

impl BayerPattern {
    /// Which channel (0=R, 1=G, 2=B) a sensor photosite at `(x, y)`
    /// measured, given this CFA layout.
    fn channel_at(self, x: u32, y: u32) -> usize {
        let (ex, ey) = (x % 2, y % 2);
        match (self, ex, ey) {
            (BayerPattern::Rggb, 0, 0) => 0,
            (BayerPattern::Rggb, 1, 0) => 1,
            (BayerPattern::Rggb, 0, 1) => 1,
            (BayerPattern::Rggb, 1, 1) => 2,
            (BayerPattern::Bggr, 0, 0) => 2,
            (BayerPattern::Bggr, 1, 0) => 1,
            (BayerPattern::Bggr, 0, 1) => 1,
            (BayerPattern::Bggr, 1, 1) => 0,
            (BayerPattern::Grbg, 0, 0) => 1,
            (BayerPattern::Grbg, 1, 0) => 0,
            (BayerPattern::Grbg, 0, 1) => 2,
            (BayerPattern::Grbg, 1, 1) => 1,
            (BayerPattern::Gbrg, 0, 0) => 1,
            (BayerPattern::Gbrg, 1, 0) => 2,
            (BayerPattern::Gbrg, 0, 1) => 0,
            (BayerPattern::Gbrg, 1, 1) => 1,
            _ => unreachable!("x % 2 and y % 2 are always 0 or 1"),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "RGGB" => BayerPattern::Rggb,
            "BGGR" => BayerPattern::Bggr,
            "GRBG" => BayerPattern::Grbg,
            "GBRG" => BayerPattern::Gbrg,
            _ => return None,
        })
    }
}

/// Demosaics a single-channel Bayer-mosaic sensor image into full RGB.
/// Each pixel keeps its own directly-measured channel unchanged, and gets
/// its other two channels filled in by averaging whichever same-channel
/// neighbors exist in its surrounding 3x3 neighborhood - the classic
/// bilinear/nearest-neighbor-average demosaic. Real RAW converters use
/// more sophisticated edge-aware algorithms (to avoid the color fringing
/// this simpler method produces at sharp edges), but this is a
/// legitimate, correct reconstruction for anything without fine detail
/// right at the Nyquist limit - a documented simplification, not a
/// placeholder.
pub fn demosaic_bayer(mosaic: &GrayImage, pattern: BayerPattern) -> RgbaImage {
    let (w, h) = mosaic.dimensions();
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let own_channel = pattern.channel_at(x, y);
            let mut rgb = [0u8; 3];
            rgb[own_channel] = mosaic.get_pixel(x, y)[0];

            for c in 0..3 {
                if c == own_channel {
                    continue;
                }
                let mut sum = 0u32;
                let mut count = 0u32;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                            continue;
                        }
                        if pattern.channel_at(nx as u32, ny as u32) == c {
                            sum += mosaic.get_pixel(nx as u32, ny as u32)[0] as u32;
                            count += 1;
                        }
                    }
                }
                // No same-channel neighbor exists only at the very corner
                // pixels of a 2x2-or-smaller image; fall back to the
                // pixel's own measured value rather than leaving it 0.
                rgb[c] = if count > 0 { (sum / count) as u8 } else { rgb[own_channel] };
            }

            out.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }
    out
}

/// Opens a grayscale image file as raw Bayer sensor mosaic data and
/// demosaics it into a new single-layer document - the "File > Open"
/// entry point for RAW processing once mosaic data is already in hand
/// (e.g. extracted from a DNG's image data by another tool, or supplied
/// directly). See the module docs for what this does and doesn't cover.
pub fn document_from_bayer_mosaic(path: &Path, pattern: BayerPattern) -> Result<Document> {
    let mosaic = image::open(path).with_context(|| format!("failed to read mosaic image at {}", path.display()))?.to_luma8();
    let debayered = demosaic_bayer(&mosaic, pattern);
    let (width, height) = debayered.dimensions();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled").to_string();

    let mut doc = Document::new(name, width, height);
    doc.layers.clear();
    let mut layer = Layer::new_transparent("Background", width, height);
    layer.pixels = debayered;
    doc.layers.push(layer);
    Ok(doc)
}

use crate::document::{Layer, SelectionRect};
use crate::paint::{stroke_path as brush_stroke, Brush, BrushPoint};
use crate::shapes::blend_pixel_in_place;
use image::{GrayImage, Luma, Rgba};

/// A 2D point in canvas pixel coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// One anchor point of a vector path, with optional Bezier control handles
/// (Photoshop's pen-tool model). `control_out` shapes the curve leaving
/// this anchor toward the next one; `control_in` shapes the curve arriving
/// at this anchor from the previous one. Either or both absent means the
/// adjacent segment(s) are straight lines.
#[derive(Debug, Clone, Copy)]
pub struct PathNode {
    pub anchor: Point,
    pub control_in: Option<Point>,
    pub control_out: Option<Point>,
}

/// An ordered sequence of anchor points, optionally closed into a loop.
/// This is a direct data model, not a live-editable path layer yet -
/// `fill`/`stroke` rasterize it immediately onto a layer, same tier as
/// the other paint/shape ops.
pub struct Path {
    pub nodes: Vec<PathNode>,
    pub closed: bool,
}

const CURVE_SEGMENTS: usize = 24;

fn cubic_bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f32) -> Point {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let d = t * t * t;
    Point { x: a * p0.x + b * p1.x + c * p2.x + d * p3.x, y: a * p0.y + b * p1.y + c * p2.y + d * p3.y }
}

impl Path {
    /// Samples the path into a flat polyline. Straight segments contribute
    /// their two endpoints; segments with either control handle set are
    /// flattened into `CURVE_SEGMENTS` steps of a cubic Bezier (missing
    /// handles default to the adjacent anchor itself, matching Photoshop's
    /// behavior for a "half-curved" segment).
    pub fn flatten(&self) -> Vec<Point> {
        let n = self.nodes.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![self.nodes[0].anchor];
        }
        let mut out = Vec::new();
        let pair_count = if self.closed { n } else { n - 1 };
        for i in 0..pair_count {
            let a = &self.nodes[i];
            let b = &self.nodes[(i + 1) % n];
            out.push(a.anchor);
            if a.control_out.is_some() || b.control_in.is_some() {
                let c1 = a.control_out.unwrap_or(a.anchor);
                let c2 = b.control_in.unwrap_or(b.anchor);
                for step in 1..CURVE_SEGMENTS {
                    let t = step as f32 / CURVE_SEGMENTS as f32;
                    out.push(cubic_bezier(a.anchor, c1, c2, b.anchor, t));
                }
            }
        }
        if !self.closed {
            out.push(self.nodes[n - 1].anchor);
        }
        out
    }
}

/// Returns the horizontal fill spans `[x_start, x_end)` for scanline `y`,
/// clipped to `[0, w)`, using an even-odd rule over the closed polyline
/// `points` - correctly handles self-intersecting shapes (e.g. a 5-point
/// star), unlike a naive winding-number fill. Shared by `fill_path`
/// (paints a layer) and `rasterize_mask` (rasterizes a vector mask), so
/// both use exactly the same fill geometry.
fn even_odd_spans(points: &[Point], y: i64, w: u32) -> Vec<(i64, i64)> {
    let yf = y as f32 + 0.5;
    let n = points.len();
    let mut xs: Vec<f32> = Vec::new();
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        if (a.y <= yf && b.y > yf) || (b.y <= yf && a.y > yf) {
            let t = (yf - a.y) / (b.y - a.y);
            xs.push(a.x + t * (b.x - a.x));
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut spans = Vec::new();
    let mut i = 0;
    while i + 1 < xs.len() {
        let x_start = (xs[i].round() as i64).max(0);
        let x_end = (xs[i + 1].round() as i64).min(w as i64);
        if x_start < x_end {
            spans.push((x_start, x_end));
        }
        i += 2;
    }
    spans
}

/// Fills the path's interior with a solid color using an even-odd scanline
/// fill, clipped to the layer bounds and (optionally) the active
/// selection. Requires a closed path with at least 3 flattened points.
pub fn fill_path(layer: &mut Layer, path: &Path, color: Rgba<u8>, clip: Option<SelectionRect>) {
    let points = path.flatten();
    if points.len() < 3 {
        return;
    }
    let (w, h) = layer.pixels.dimensions();
    let min_y = points.iter().map(|p| p.y).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
    let max_y = points.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil().min(h as f32) as i64;

    for y in min_y..max_y {
        for (x_start, x_end) in even_odd_spans(&points, y, w) {
            for x in x_start..x_end {
                if let Some(sel) = clip {
                    if !sel.contains(x, y) {
                        continue;
                    }
                }
                blend_pixel_in_place(layer, x as u32, y as u32, color);
            }
        }
    }
}

/// Rasterizes a closed path into a vector mask: a canvas-sized grayscale
/// buffer, white (255) inside the path's even-odd-filled interior and
/// black (0) outside - directly usable as `Layer::mask`. Requires a closed
/// path with at least 3 flattened points; returns an all-black (fully
/// hidden) mask otherwise, same "nothing to fill" convention as
/// `fill_path`'s no-op.
pub fn rasterize_mask(path: &Path, width: u32, height: u32) -> GrayImage {
    let mut mask = GrayImage::from_pixel(width, height, Luma([0]));
    let points = path.flatten();
    if points.len() < 3 {
        return mask;
    }
    let min_y = points.iter().map(|p| p.y).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
    let max_y = points.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil().min(height as f32) as i64;

    for y in min_y..max_y {
        for (x_start, x_end) in even_odd_spans(&points, y, width) {
            for x in x_start..x_end {
                mask.put_pixel(x as u32, y as u32, Luma([255]));
            }
        }
    }
    mask
}

/// Strokes the path's outline with a brush, reusing the exact same
/// stroke-stamping primitive the freehand brush tool uses
/// (`paint::stroke_path`) - a vector path and a hand-drawn stroke are the
/// same thing once flattened to points.
pub fn stroke_path_shape(layer: &mut Layer, path: &Path, brush: &Brush, clip: Option<SelectionRect>) {
    let mut points = path.flatten();
    if path.closed {
        if let Some(&first) = points.first() {
            points.push(first);
        }
    }
    let brush_points: Vec<BrushPoint> = points.iter().map(|p| BrushPoint { x: p.x, y: p.y, pressure: 1.0 }).collect();
    brush_stroke(layer, &brush_points, brush, clip);
}

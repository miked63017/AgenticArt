//! Photoshop's "Free Transform": move/rotate/scale a layer's content in
//! place on its canvas-sized buffer (the canvas itself doesn't change size
//! - content moved past the edge is clipped, exactly like Photoshop's
//! transform tool). Destructive (bakes into `Layer::pixels`), same tier as
//! the other paint/shape/filter ops - non-destructive transforms are a
//! later addition (Smart Objects), same as Photoshop's own history here.
//!
//! Built on `imageproc::geometric_transformations::warp`, which internally
//! inverts the given `Projection` and samples the source at each output
//! pixel's pre-image - so a `Projection` here reads as "move content from
//! here to there" in the intuitive forward direction, not its inverse.

use crate::document::Layer;
use anyhow::{bail, Context, Result};
use image::{GrayImage, Luma, Rgba, RgbaImage};
use imageproc::geometric_transformations::{warp, Interpolation, Projection};

fn warp_rgba(pixels: &RgbaImage, projection: &Projection) -> RgbaImage {
    warp(pixels, projection, Interpolation::Bilinear, Rgba([0, 0, 0, 0]))
}

fn warp_gray(pixels: &GrayImage, projection: &Projection) -> GrayImage {
    // A mask has no natural "outside" color the way transparent RGBA does;
    // black (0 = fully hidden) is the conservative choice - content
    // rotated/moved past the mask's original coverage becomes hidden
    // rather than spuriously revealed.
    warp(pixels, projection, Interpolation::Bilinear, Luma([0]))
}

/// Applies `projection` to a layer's pixels and (if present) its mask, in
/// place - the shared tail end of every transform op below.
fn apply_projection(layer: &mut Layer, projection: &Projection) {
    layer.pixels = warp_rgba(&layer.pixels, projection);
    if let Some(mask) = &layer.mask {
        layer.mask = Some(warp_gray(mask, projection));
    }
}

/// A projection centered on the layer's own dimensions: wraps `inner` in
/// translate-to-center / untranslate, so `inner` itself only needs to
/// describe the transform about the origin (rotate/scale/skew all read
/// far more naturally that way than pre-composed with an off-center
/// pivot).
fn about_center(layer: &Layer, inner: Projection) -> Projection {
    let (w, h) = layer.pixels.dimensions();
    let center = (w as f32 / 2.0, h as f32 / 2.0);
    Projection::translate(center.0, center.1) * inner * Projection::translate(-center.0, -center.1)
}

/// Moves a layer's content by `(dx, dy)` pixels. Content that moves off
/// the canvas is clipped; the vacated area becomes transparent.
pub fn translate(layer: &mut Layer, dx: f32, dy: f32) {
    let projection = Projection::translate(dx, dy);
    apply_projection(layer, &projection);
}

/// Rotates a layer's content clockwise by `degrees` about its own center.
pub fn rotate(layer: &mut Layer, degrees: f32) {
    let projection = about_center(layer, Projection::rotate(degrees.to_radians()));
    apply_projection(layer, &projection);
}

/// Scales a layer's content by `(sx, sy)` about its own center (1.0 = no
/// change). Scaling up clips content past the canvas edge, same as
/// Photoshop's Free Transform; scaling down leaves the vacated area
/// transparent.
pub fn scale(layer: &mut Layer, sx: f32, sy: f32) {
    let projection = about_center(layer, Projection::scale(sx, sy));
    apply_projection(layer, &projection);
}

/// Skews (shears) a layer's content about its own center: `shear_x` tilts
/// content sideways as a function of its y position, `shear_y` tilts it
/// vertically as a function of x (both are the standard affine shear
/// factors - e.g. shear_x = 0.5 shifts a pixel `0.5 * (y - center_y)`
/// pixels horizontally).
pub fn skew(layer: &mut Layer, shear_x: f32, shear_y: f32) {
    // Row-major 3x3 affine matrix: [a b c; d e f; g h i] maps
    // (x,y) -> (a*x + b*y + c, d*x + e*y + f).
    let matrix = [1.0, shear_x, 0.0, shear_y, 1.0, 0.0, 0.0, 0.0, 1.0];
    let shear = Projection::from_matrix(matrix).expect("a pure shear matrix is always invertible");
    let projection = about_center(layer, shear);
    apply_projection(layer, &projection);
}

/// Solves for the 3x3 projective (homography) matrix `H` mapping each
/// `src[i]` exactly onto `dst[i]` (row-major, `H[8]` normalized to 1) -
/// the standard 4-point Direct Linear Transform. Each correspondence
/// contributes two linear equations in the 8 unknowns `h11..h32`
/// (`h33` fixed at 1); solved by Gauss-Jordan elimination with partial
/// pivoting. Returns `None` if the 8x8 system is singular (the 4 points
/// are degenerate - e.g. three or more collinear).
fn solve_homography(src: [(f32, f32); 4], dst: [(f32, f32); 4]) -> Option<[f32; 9]> {
    let mut a = [[0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (src[i].0 as f64, src[i].1 as f64);
        let (xp, yp) = (dst[i].0 as f64, dst[i].1 as f64);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * xp, -y * xp, xp];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * yp, -y * yp, yp];
    }

    for col in 0..8 {
        let pivot = (col..8).max_by(|&r1, &r2| a[r1][col].abs().partial_cmp(&a[r2][col].abs()).unwrap())?;
        if a[pivot][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, pivot);
        let pivot_val = a[col][col];
        for k in col..9 {
            a[col][k] /= pivot_val;
        }
        for r in 0..8 {
            if r != col {
                let factor = a[r][col];
                for k in col..9 {
                    a[r][k] -= factor * a[col][k];
                }
            }
        }
    }

    let mut h = [0f32; 9];
    for (r, slot) in h.iter_mut().enumerate().take(8) {
        *slot = a[r][8] as f32;
    }
    h[8] = 1.0;
    Some(h)
}

/// Photoshop's Free Transform "Perspective" (and the general case of
/// "Distort"): warps a layer so its four corners - `(0,0)`, `(width,0)`,
/// `(width,height)`, `(0,height)` in that order - land exactly on
/// `top_left`/`top_right`/`bottom_right`/`bottom_left`, via a true 4-point
/// projective homography (not an approximation composed from
/// translate/rotate/scale/skew - a real vanishing-point perspective warp).
/// Errors if the four destination points are degenerate (e.g. three or
/// more collinear, which has no unique homography).
pub fn perspective(layer: &mut Layer, top_left: (f32, f32), top_right: (f32, f32), bottom_right: (f32, f32), bottom_left: (f32, f32)) -> Result<()> {
    let (w, h) = layer.pixels.dimensions();
    let src = [(0.0, 0.0), (w as f32, 0.0), (w as f32, h as f32), (0.0, h as f32)];
    let dst = [top_left, top_right, bottom_right, bottom_left];

    let matrix = solve_homography(src, dst).context("the four destination points are degenerate (e.g. collinear) - no unique perspective warp exists")?;
    let Some(projection) = Projection::from_matrix(matrix) else {
        bail!("the resulting perspective transform is not invertible");
    };
    apply_projection(layer, &projection);
    Ok(())
}

/// A minimal, honest "3D layer": treats a layer as a flat rectangular
/// plane floating in 3D space, rotates it by `pitch_degrees` (tilt around
/// the horizontal axis) and `yaw_degrees` (turn around the vertical
/// axis), then projects it back onto the 2D canvas through a simple
/// pinhole camera placed `camera_distance` pixels back along the z axis,
/// looking at the plane's center - and finally warps the layer to match
/// that projection via `perspective`. This is not a 3D renderer (no
/// mesh geometry beyond a single flat plane, no lighting/shading, no
/// z-buffering against other layers) - "3D layers" was always the
/// lowest-priority, droppable item on this project's roadmap, and full 3D
/// rendering is a different scale of project entirely. What this *does* give
/// genuinely: placing a flat image onto a simulated tilted/turned
/// surface via real 3D math, not a canned "fake perspective" preset -
/// the same category of thing Photoshop's "3D > New Mesh from Layer >
/// Plane" does for a single flat plane, at a scope this session can
/// actually build and verify.
pub fn place_as_3d_plane(layer: &mut Layer, pitch_degrees: f32, yaw_degrees: f32, camera_distance: f32) -> Result<()> {
    let (w, h) = layer.pixels.dimensions();
    let (half_w, half_h) = (w as f32 / 2.0, h as f32 / 2.0);
    let corners_3d = [(-half_w, -half_h, 0.0f32), (half_w, -half_h, 0.0), (half_w, half_h, 0.0), (-half_w, half_h, 0.0)];

    let pitch = pitch_degrees.to_radians();
    let yaw = yaw_degrees.to_radians();
    let (cp, sp) = (pitch.cos(), pitch.sin());
    let (cy, sy) = (yaw.cos(), yaw.sin());

    let mut projected = [(0.0f32, 0.0f32); 4];
    for (i, &(x, y, z)) in corners_3d.iter().enumerate() {
        // Rotate around the vertical (y) axis first (yaw), then the
        // horizontal (x) axis (pitch) - standard intrinsic rotation order
        // for "turn, then tilt".
        let (x1, z1) = (x * cy + z * sy, -x * sy + z * cy);
        let (y2, z2) = (y * cp - z1 * sp, y * sp + z1 * cp);

        if camera_distance - z2 <= 1.0 {
            bail!("the plane's rotation brings a corner too close to (or past) the camera - reduce pitch/yaw or increase cameraDistance");
        }
        let scale = camera_distance / (camera_distance - z2);
        projected[i] = (x1 * scale + half_w, y2 * scale + half_h);
    }

    perspective(layer, projected[0], projected[1], projected[2], projected[3])
}

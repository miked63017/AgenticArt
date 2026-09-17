// GPU mirror of compositor::blend_channel / composite_over in blend.rs
// (Rust CPU implementation) — keep these in sync; the parity test in
// gpu.rs checks GPU and CPU output match within tolerance.

struct Params {
    opacity: f32,
    mode: u32,
    clip_to_below: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> base_in: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> top_in: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> out_buf: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

fn blend_channel(mode: u32, b: f32, t: f32) -> f32 {
    switch mode {
        case 0u: { return t; }
        case 1u: { return b * t; }
        case 2u: { return 1.0 - (1.0 - b) * (1.0 - t); }
        case 3u: {
            if (b < 0.5) { return 2.0 * b * t; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - t);
        }
        case 4u: { return min(b, t); }
        case 5u: { return max(b, t); }
        case 6u: {
            if (t >= 1.0) { return 1.0; }
            return min(b / (1.0 - t), 1.0);
        }
        case 7u: {
            if (t <= 0.0) { return 0.0; }
            return 1.0 - min((1.0 - b) / t, 1.0);
        }
        case 8u: {
            if (t < 0.5) { return 2.0 * b * t; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - t);
        }
        case 9u: {
            var d: f32;
            if (b < 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; } else { d = sqrt(b); }
            if (t < 0.5) { return b - (1.0 - 2.0 * t) * b * (1.0 - b); }
            return b + (2.0 * t - 1.0) * (d - b);
        }
        case 10u: { return abs(b - t); }
        case 11u: { return b + t - 2.0 * b * t; }
        default: { return t; }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= arrayLength(&out_buf)) {
        return;
    }
    let base_px = base_in[idx];
    let top_px = top_in[idx];

    var top_a = top_px.a * params.opacity;
    if (params.clip_to_below == 1u) {
        top_a = top_a * base_px.a;
    }
    if (top_a <= 0.0) {
        out_buf[idx] = base_px;
        return;
    }

    let br = blend_channel(params.mode, base_px.r, top_px.r);
    let bg = blend_channel(params.mode, base_px.g, top_px.g);
    let bb = blend_channel(params.mode, base_px.b, top_px.b);

    let base_a = base_px.a;
    let out_a = top_a + base_a * (1.0 - top_a);
    if (out_a <= 0.0) {
        out_buf[idx] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }

    let out_r = (br * top_a + base_px.r * base_a * (1.0 - top_a)) / out_a;
    let out_g = (bg * top_a + base_px.g * base_a * (1.0 - top_a)) / out_a;
    let out_b = (bb * top_a + base_px.b * base_a * (1.0 - top_a)) / out_a;

    out_buf[idx] = vec4<f32>(clamp(out_r, 0.0, 1.0), clamp(out_g, 0.0, 1.0), clamp(out_b, 0.0, 1.0), clamp(out_a, 0.0, 1.0));
}

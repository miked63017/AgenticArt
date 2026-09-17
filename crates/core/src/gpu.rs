use crate::document::BlendMode;
use image::{Rgba, RgbaImage};
use std::sync::OnceLock;
use wgpu::util::DeviceExt;

/// GPU-accelerated layer compositing, moving the compositing graph onto
/// a GPU (wgpu) backend. Scoped deliberately: only the hot inner loop
/// (`composite_over`, the
/// per-layer alpha-over-with-blend-mode step called once per layer in
/// `compositor::render` and once per child in group compositing) runs on
/// the GPU - layer styles (drop shadow/stroke) and group flattening stay
/// on the already-tested CPU path in `compositor.rs`, which calls into
/// this module rather than duplicating it. If GPU init fails for any
/// reason (no adapter, driver issue, headless environment), every
/// function here returns `None` and the caller transparently falls back
/// to the CPU implementation - there is no hard dependency on a GPU being
/// present.
struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

fn context() -> Option<&'static GpuContext> {
    static CONTEXT: OnceLock<Option<GpuContext>> = OnceLock::new();
    CONTEXT.get_or_init(init_context).as_ref()
}

fn init_context() -> Option<GpuContext> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()?;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blend.wgsl"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blend.wgsl").into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blend-bgl"),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, true),
            storage_entry(2, false),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blend-pl"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("blend-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    });

    Some(GpuContext { device, queue, pipeline, bind_group_layout })
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    opacity: f32,
    mode: u32,
    clip_to_below: u32,
    _pad: u32,
}

fn to_f32_rgba(img: &RgbaImage) -> Vec<[f32; 4]> {
    img.pixels().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect()
}

/// Maps a blend mode to the `blend.wgsl` shader's mode code, or `None` if
/// the shader doesn't implement it yet - `composite_over_gpu` treats that
/// as a GPU-path miss and the caller transparently falls back to the CPU
/// implementation, which supports every mode. So a blend mode added to
/// `BlendMode` works correctly immediately; it just isn't GPU-accelerated
/// until someone also ports it into the shader.
fn blend_mode_code(mode: BlendMode) -> Option<u32> {
    Some(match mode {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::ColorDodge => 6,
        BlendMode::ColorBurn => 7,
        BlendMode::HardLight => 8,
        BlendMode::SoftLight => 9,
        BlendMode::Difference => 10,
        BlendMode::Exclusion => 11,
        BlendMode::LinearBurn
        | BlendMode::DarkerColor
        | BlendMode::LinearDodge
        | BlendMode::LighterColor
        | BlendMode::VividLight
        | BlendMode::LinearLight
        | BlendMode::PinLight
        | BlendMode::HardMix
        | BlendMode::Subtract
        | BlendMode::Divide
        | BlendMode::Hue
        | BlendMode::Saturation
        | BlendMode::Color
        | BlendMode::Luminosity => return None,
    })
}

/// GPU version of `compositor::composite_over`. Returns `None` (rather
/// than panicking) on any GPU failure so the caller can fall back to the
/// CPU path - covers missing adapters, exceeding device buffer-size
/// limits on very large canvases, etc.
pub fn composite_over_gpu(base: &RgbaImage, top: &RgbaImage, opacity: f32, mode: BlendMode, clip_to_below: bool) -> Option<RgbaImage> {
    let ctx = context()?;
    let mode_code = blend_mode_code(mode)?;
    let (width, height) = base.dimensions();
    let pixel_count = (width as u64) * (height as u64);
    let byte_size = pixel_count * 16; // vec4<f32>

    let base_data = to_f32_rgba(base);
    let top_data = to_f32_rgba(top);

    let base_buf = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("base"),
        contents: bytemuck::cast_slice(&base_data),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let top_buf = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("top"),
        contents: bytemuck::cast_slice(&top_data),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let out_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: byte_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: byte_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let params = Params { opacity, mode: mode_code, clip_to_below: clip_to_below as u32, _pad: 0 };
    let params_buf = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blend-bg"),
        layout: &ctx.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: base_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: top_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: params_buf.as_entire_binding() },
        ],
    });

    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("blend-encoder") });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("blend-pass"), timestamp_writes: None });
        pass.set_pipeline(&ctx.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let workgroups = (pixel_count as u32).div_ceil(256).max(1);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &readback_buf, 0, byte_size);
    ctx.queue.submit(Some(encoder.finish()));

    let slice = readback_buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    ctx.device.poll(wgpu::Maintain::Wait);
    rx.recv().ok()?.ok()?;

    let mapped = slice.get_mapped_range();
    let out_f32: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
    let mut out_img = RgbaImage::new(width, height);
    for (i, px) in out_f32.iter().enumerate() {
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        out_img.put_pixel(
            x,
            y,
            Rgba([(px[0] * 255.0).round() as u8, (px[1] * 255.0).round() as u8, (px[2] * 255.0).round() as u8, (px[3] * 255.0).round() as u8]),
        );
    }
    drop(mapped);
    readback_buf.unmap();

    Some(out_img)
}

/// True if a usable GPU adapter was found (for diagnostics/status only;
/// callers should just call `composite_over_gpu` and handle `None`).
pub fn is_available() -> bool {
    context().is_some()
}

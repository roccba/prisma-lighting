//! S0 bring-up probe: render a known constant to an `Rgba16Float` target and
//! read it back to CPU `f16`.
//!
//! This exercises the whole M5 plumbing at its thinnest — shader module compile,
//! render pipeline, a vertex-less fullscreen draw, an `Rgba16Float` color target
//! (the exact format the canonical master buffer is sampled from, §5.2), and the
//! texture→buffer copy + async map + readback ([`crate::readback`]) — **without
//! any effect, zone or compositor logic**. Green on both adapters means the GPU
//! path is trustworthy and effects (S1) can build on it.

use crate::device::Gpu;
use crate::readback::{create_render_target, read_rgba16f, Rgba16};

/// Render `color` (linear RGBA) as a full-screen constant into a `width`×`height`
/// `Rgba16Float` texture and read it back, row-major, tightly packed.
///
/// The color is baked into the fragment shader, so this needs no bind group — it
/// is the minimum that still proves the pipeline path.
pub fn render_solid_and_readback(
    gpu: &Gpu,
    width: u32,
    height: u32,
    color: [f32; 4],
) -> Vec<Rgba16> {
    let device = &gpu.device;

    let texture = create_render_target(gpu, width, height);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("s0-solid"),
        source: wgpu::ShaderSource::Wgsl(solid_shader(color).into()),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("s0-pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("s0-enc"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("s0-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1); // vertex-less fullscreen triangle
    }
    gpu.queue.submit([encoder.finish()]);

    read_rgba16f(gpu, &texture, width, height)
}

/// A shader whose fragment stage emits a compile-time-constant color, drawn over
/// a vertex-less fullscreen triangle (positions derived from `vertex_index`).
fn solid_shader(color: [f32; 4]) -> String {
    format!(
        "@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {{
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}}

@fragment
fn fs() -> @location(0) vec4<f32> {{
    return vec4<f32>({:?}, {:?}, {:?}, {:?});
}}",
        color[0], color[1], color[2], color[3]
    )
}

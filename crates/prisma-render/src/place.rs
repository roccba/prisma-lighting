//! The zone placement pass (CLAUDE.md M8, docs/M8-PARTA.md §2).
//!
//! [`ZonePlacer`] takes a zone's `mapping_size` texture and **places** it into a
//! world-sized texture at the zone's footprint, nearest (integer) fetch. That
//! placed texture is what the **unchanged** M5 compositor ([`crate::blend`])
//! consumes as a [`ZoneComposite`](crate::blend::ZoneComposite) input — the
//! compositor, the overlap counter and the gather all keep operating on the one
//! global (world) framebuffer, exactly as in M5.
//!
//! This is the only new GPU stage M8 adds to the render pipeline (M8-PARTA §1).
//! The shader is `shaders/place.wgsl`; read its header for the placement maths and
//! why the identity case (`mapping_size == footprint`, identity transform) is a
//! byte-exact copy — the no-regression gate against M5.

/// A zone footprint in **world cells**: where the zone's local mapping space lands
/// on the world framebuffer. `origin` is the lower corner, `size` the extent.
///
/// The **identity** footprint (`origin = [0, 0]`, `size = [world_w, world_h]`) with
/// a `mapping_size` equal to `size` makes placement a 1:1 copy — the no-regression
/// gate (M8-PARTA §2.3). The zone→footprint derivation from `world_transform` /
/// `Zone::bounds` layers on top of this primitive; the pass itself only ever sees
/// a footprint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Footprint {
    pub origin: [f32; 2],
    pub size: [f32; 2],
}

impl Footprint {
    /// The whole world framebuffer — identity placement. `mapping_size == size`
    /// makes this a byte-exact copy of the source.
    pub fn identity(world: (u32, u32)) -> Footprint {
        Footprint {
            origin: [0.0, 0.0],
            size: [world.0 as f32, world.1 as f32],
        }
    }
}

/// CPU mirror of `PlaceParams` in `shaders/place.wgsl`. 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlaceParams {
    foot_origin: [f32; 2],
    foot_size: [f32; 2],
}

/// Places per-zone `mapping_size` textures into the world framebuffer. Holds the
/// pipeline and a reusable uniform buffer; bind groups are rebuilt per call because
/// each references a different source texture.
pub struct ZonePlacer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
}

impl ZonePlacer {
    pub fn new(gpu: &crate::Gpu) -> ZonePlacer {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("place"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../../shaders/place.wgsl").into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("place-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Non-filterable + integer `textureLoad` → no sampler, exact fetch.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("place-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("place"),
            layout: Some(&layout),
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
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("place-uniform"),
            size: std::mem::size_of::<PlaceParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ZonePlacer {
            pipeline,
            bind_group_layout,
            uniform,
        }
    }

    /// Place `src` (a zone's `mapping_size` texture) into `out` (a world-sized
    /// `Rgba16Float` view) at `footprint`, nearest fetch. Records and submits its
    /// own command buffer. `out` must be distinct from `src`.
    pub fn place(
        &self,
        gpu: &crate::Gpu,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        footprint: Footprint,
    ) {
        gpu.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&PlaceParams {
                foot_origin: footprint.origin,
                foot_size: footprint.size,
            }),
        );
        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("place-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("place-enc"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("place-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: out,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

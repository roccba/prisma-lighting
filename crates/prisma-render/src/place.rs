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

    /// Whether the footprint covers no cells (a zone that occupies nothing). Such a
    /// placement writes all black, so the zone contributes nothing.
    pub fn is_empty(&self) -> bool {
        self.size[0] <= 0.0 || self.size[1] <= 0.0
    }
}

/// Derive a zone's footprint from its **coverage mask** — the integer bounding box
/// of the world cells the mask marks set.
///
/// `mask` is `crate::overlap::zone_cell_coverage`'s per-zone mask: `mask[y*w + x] !=
/// 0` iff a member pixel of the zone lands in cell `(x, y)`. Deriving the footprint
/// from **this** array (not from `Zone::bounds`, a cached float `Rect` that can be
/// `None` or stale) is what guarantees the load-bearing invariant **placement ⊇
/// coverage** (M8-PARTA §2.3): the footprint is the bbox of exactly the cells the
/// compositor's coverage confines the zone to, computed from the same `cell_of`
/// source the gather and the overlap counter use, so the two can never disagree and
/// a covered cell can never fall outside the placed region (which would leave a
/// hole — a pixel gathering black).
///
/// An all-zero mask (a zone covering nothing) yields an empty footprint
/// ([`Footprint::is_empty`]). `matrix_w` is the world matrix width the mask is laid
/// out in row-major (`cell = y * matrix_w + x`).
pub fn footprint_from_coverage(mask: &[u32], matrix_w: u32) -> Footprint {
    let w = matrix_w as usize;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut any = false;
    for (cell, &set) in mask.iter().enumerate() {
        if set == 0 {
            continue;
        }
        any = true;
        let x = (cell % w) as u32;
        let y = (cell / w) as u32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    if !any {
        return Footprint {
            origin: [0.0, 0.0],
            size: [0.0, 0.0],
        };
    }
    Footprint {
        origin: [min_x as f32, min_y as f32],
        size: [(max_x - min_x + 1) as f32, (max_y - min_y + 1) as f32],
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
    /// Pipeline + bind-group layout, shared via the [`crate::cache`] across recompiles.
    cached: std::sync::Arc<crate::cache::CachedRender>,
    uniform: wgpu::Buffer,
}

impl ZonePlacer {
    pub fn new(gpu: &crate::Gpu) -> ZonePlacer {
        let device = &gpu.device;
        // The place pipeline + layout depend only on the shader and target format, so
        // they are cached and reused across recompiles (M8 live-apply optimisation).
        let cached = gpu.cache.render("place", || {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("place"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../shaders/place.wgsl").into(),
                ),
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            crate::cache::CachedRender {
                pipeline,
                bind_group_layout,
            }
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("place-uniform"),
            size: std::mem::size_of::<PlaceParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ZonePlacer { cached, uniform }
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
            layout: &self.cached.bind_group_layout,
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
            pass.set_pipeline(&self.cached.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

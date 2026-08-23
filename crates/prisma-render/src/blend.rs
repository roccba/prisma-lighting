//! The blend/compositor pass (CLAUDE.md §4.2, §5.2 step 3).
//!
//! One [`Compositor`] blends a source layer onto an accumulator below it, in
//! **linear light**, using the effect's [`BlendMode`]. The formulas live in
//! `shaders/blend.wgsl` and are documented there against their mathematical
//! definitions; [`blend_code`] is the single mapping from the `BlendMode` enum to
//! the shader's `mode` codes.
//!
//! This is deliberately the same machinery §4.1 reuses for zone-over-zone
//! compositing in S4: intra-zone layers and inter-zone stacking blend identically,
//! which is what lets an `Add`/`Max` global strobe zone brighten the zones under
//! it rather than occlude them.

use prisma_patch::BlendMode;

use crate::readback::create_render_target;

/// Map a [`BlendMode`] to the shader's `mode` code. Explicit rather than an `as`
/// cast so the shader contract does not silently depend on the enum's layout.
/// Must match the `switch` in `shaders/blend.wgsl`.
#[inline]
pub fn blend_code(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Add => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        // PROVISIONAL: Mask is a luminance key (see shaders/blend.wgsl). Revisit at
        // M10 when alpha-carrying media exists — operators will expect alpha gating.
        BlendMode::Mask => 4,
        BlendMode::Max => 5,
    }
}

/// CPU mirror of the `BlendParams` uniform (`shaders/blend.wgsl`). 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendParams {
    mode: u32,
    opacity: f32,
    use_mask: u32,
    _pad0: u32,
    zone_scale: [f32; 4],
}

/// One source layer to fold into a stack: its rendered texture, how it blends onto
/// what is below, and its opacity.
pub struct Layer<'a> {
    pub texture: &'a wgpu::Texture,
    pub mode: BlendMode,
    pub opacity: f32,
}

/// One zone to fold into the master framebuffer (§5.2 step 4). Beyond a layer it
/// carries the zone's flat parameters, which this pass applies **only on cells the
/// frame's snapshot marks overlapped** — elsewhere they are deferred to the output
/// stage (§5.2.1).
pub struct ZoneComposite<'a> {
    pub texture: &'a wgpu::Texture,
    pub mode: BlendMode,
    pub opacity: f32,
    /// The zone's `dimmer × color_filter`, in linear light.
    pub zone_scale: [f32; 3],
    /// Per-cell mask of the cells this zone's member pixels occupy. Outside it the
    /// zone leaves the accumulator untouched, which is what keeps zones
    /// independent. Build with [`crate::overlap::zone_cell_coverage`].
    pub coverage: &'a wgpu::Buffer,
}

/// Upload a snapshot's per-cell overlap mask for the zone composite to consult.
/// A zero-length buffer is invalid, so an empty mask becomes one zeroed cell.
pub fn create_cell_mask(gpu: &crate::Gpu, mask: &[u32]) -> wgpu::Buffer {
    let mut bytes: Vec<u8> = bytemuck::cast_slice(mask).to_vec();
    if bytes.is_empty() {
        bytes.resize(4, 0);
    }
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell-overlap-mask"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: true,
    });
    {
        let mut view = buffer
            .slice(..)
            .get_mapped_range_mut()
            .expect("freshly mapped buffer");
        view.copy_from_slice(&bytes);
    }
    buffer.unmap();
    buffer
}

/// Blends layers in linear space. Holds the pipeline and a reusable uniform
/// buffer; bind groups are (re)built per blend because they reference the
/// specific input textures (which ping-pong during a fold).
pub struct Compositor {
    /// Pipeline + bind-group layout, shared via the [`crate::cache`] across recompiles.
    cached: std::sync::Arc<crate::cache::CachedRender>,
    uniform: wgpu::Buffer,
    /// Bound at binding 3 on the layer path, which never reads the mask. The
    /// binding must still be satisfied, so one zeroed cell stands in.
    dummy_mask: wgpu::Buffer,
}

impl Compositor {
    pub fn new(gpu: &crate::Gpu) -> Compositor {
        let device = &gpu.device;
        // The blend pipeline + layout depend only on the shader and target format, so
        // they are cached and reused across recompiles (M8 live-apply optimisation).
        let cached = gpu.cache.render("blend", || {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("blend"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../shaders/blend.wgsl").into(),
                ),
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("blend-bgl"),
                    entries: &[
                        tex_entry(0),
                        tex_entry(1),
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 4,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("blend-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("blend"),
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
            label: Some("blend-uniform"),
            size: std::mem::size_of::<BlendParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummy_mask = create_cell_mask(gpu, &[]);
        Compositor {
            cached,
            uniform,
            dummy_mask,
        }
    }

    /// Blend `src` onto `dst` (both same-size `Rgba16Float`) with `mode`/`opacity`
    /// and write the result into `out`. `out` must be distinct from `dst` and
    /// `src` (a render pass cannot read its own attachment).
    pub fn blend(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
    ) {
        self.blend_inner(
            gpu,
            dst,
            src,
            out,
            mode,
            opacity,
            None,
            &self.dummy_mask,
            &self.dummy_mask,
        );
    }

    /// Blend one **zone** onto the accumulator, applying its `zone_scale`
    /// (dimmer x colour filter) on the cells `cell_mask` marks overlapped. See
    /// §5.2 step 4 and §5.2.1.
    #[allow(clippy::too_many_arguments)]
    pub fn blend_zone(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        zone_scale: [f32; 3],
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
    ) {
        self.blend_inner(
            gpu,
            dst,
            src,
            out,
            mode,
            opacity,
            Some(zone_scale),
            cell_mask,
            coverage,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn blend_inner(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        zone_scale: Option<[f32; 3]>,
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
    ) {
        let scale = zone_scale.unwrap_or([1.0, 1.0, 1.0]);
        gpu.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&BlendParams {
                mode: blend_code(mode),
                opacity,
                use_mask: u32::from(zone_scale.is_some()),
                _pad0: 0,
                zone_scale: [scale[0], scale[1], scale[2], 1.0],
            }),
        );
        let dst_view = dst.create_view(&wgpu::TextureViewDescriptor::default());
        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend-bg"),
            layout: &self.cached.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dst_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: cell_mask.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: coverage.as_entire_binding(),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blend-enc"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blend-pass"),
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

    /// Fold an ordered layer stack (bottom-to-top) over a black base and return
    /// the composited texture (§5.2 step 3). Ping-pongs two scratch targets; the
    /// bottom layer therefore blends over black (Normal/Add/Screen/Max recover the
    /// layer; Multiply/Mask against a black base are legitimately zero). Allocates
    /// its scratch per call — fine for the S2 stack; the real loop (S6) pools.
    pub fn composite_over_black(
        &self,
        gpu: &crate::Gpu,
        layers: &[Layer<'_>],
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let ping = create_render_target(gpu, width, height);
        let pong = create_render_target(gpu, width, height);
        clear_to_black(gpu, &ping);

        // acc holds the current accumulator; other is the free scratch target.
        let mut acc = ping;
        let mut other = pong;
        for layer in layers {
            let out_view = other.create_view(&wgpu::TextureViewDescriptor::default());
            self.blend(
                gpu,
                &acc,
                layer.texture,
                &out_view,
                layer.mode,
                layer.opacity,
            );
            std::mem::swap(&mut acc, &mut other);
        }
        acc
    }

    /// Composite zones into the master framebuffer (§5.2 step 4).
    ///
    /// `zones` must already be in **bottom-to-top compositing order** — use
    /// `PatchIndex::composite_order()`, the single source of truth for `z_order`
    /// with its declaration-order tie-break. Each zone blends onto the zones below
    /// it with its own `blend_mode`, which is what lets an `Add`/`Max` global
    /// strobe zone *brighten* the zones underneath instead of occluding them.
    ///
    /// `cell_mask` is the frame snapshot's per-cell overlap mask: on those cells
    /// each zone's dimmer/colour is applied here, before its blend; everywhere
    /// else it is deferred to the output stage (§5.2.1).
    pub fn composite_zones(
        &self,
        gpu: &crate::Gpu,
        zones: &[ZoneComposite<'_>],
        cell_mask: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let ping = create_render_target(gpu, width, height);
        let pong = create_render_target(gpu, width, height);
        clear_to_black(gpu, &ping);

        let mut acc = ping;
        let mut other = pong;
        for zone in zones {
            let out_view = other.create_view(&wgpu::TextureViewDescriptor::default());
            self.blend_zone(
                gpu,
                &acc,
                zone.texture,
                &out_view,
                zone.mode,
                zone.opacity,
                zone.zone_scale,
                cell_mask,
                zone.coverage,
            );
            std::mem::swap(&mut acc, &mut other);
        }
        acc
    }

    /// Like [`composite_zones`](Self::composite_zones), but the final blend writes
    /// straight into `target` — the render loop's framebuffer — so no extra copy
    /// is needed to get the composite where the gather expects it.
    pub fn composite_zones_into(
        &self,
        gpu: &crate::Gpu,
        zones: &[ZoneComposite<'_>],
        cell_mask: &wgpu::Buffer,
        target: &wgpu::Texture,
        width: u32,
        height: u32,
    ) {
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let Some((last, rest)) = zones.split_last() else {
            clear_to_black(gpu, target);
            return;
        };
        let ping = create_render_target(gpu, width, height);
        let pong = create_render_target(gpu, width, height);
        clear_to_black(gpu, &ping);
        let mut acc = ping;
        let mut other = pong;
        for zone in rest {
            let out_view = other.create_view(&wgpu::TextureViewDescriptor::default());
            self.blend_zone(
                gpu,
                &acc,
                zone.texture,
                &out_view,
                zone.mode,
                zone.opacity,
                zone.zone_scale,
                cell_mask,
                zone.coverage,
            );
            std::mem::swap(&mut acc, &mut other);
        }
        self.blend_zone(
            gpu,
            &acc,
            last.texture,
            &target_view,
            last.mode,
            last.opacity,
            last.zone_scale,
            cell_mask,
            last.coverage,
        );
    }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Clear a texture to opaque black (the compositor's base).
fn clear_to_black(gpu: &crate::Gpu, texture: &wgpu::Texture) {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear-pass"),
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
    gpu.queue.submit([encoder.finish()]);
}

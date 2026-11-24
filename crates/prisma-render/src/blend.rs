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
        // Mask gates what is below by the source's ALPHA (LIVE-MODEL-SPEC 2b); the
        // luminance key is its own mode, LumaKey.
        BlendMode::Mask => 4,
        BlendMode::Max => 5,
        BlendMode::Subtract => 6,
        BlendMode::Min => 7,
        BlendMode::LumaKey => 8,
    }
}

/// The shader's internal crossfade code (not a [`BlendMode`]): a linear mix of both
/// premultiplied pictures, for clip transitions. See `shaders/blend.wgsl`.
const CROSSFADE_CODE: u32 = 9;

/// The default luminance key, `[threshold, softness]` in linear luminance.
pub const DEFAULT_KEY: [f32; 2] = [0.05, 0.1];

/// CPU mirror of the `BlendParams` uniform (`shaders/blend.wgsl`). 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendParams {
    mode: u32,
    opacity: f32,
    use_mask: u32,
    /// 1 = read the source through the pixel → texel table (binding 5).
    src_indirect: u32,
    /// rgb = zone dimmer × colour filter, a = zone dimmer (its opacity).
    zone_scale: [f32; 4],
    /// LumaKey: x = threshold, y = softness.
    key: [f32; 4],
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
    /// The zone's dimmer: on overlapped cells it is an OPACITY (scales alpha too),
    /// so a lowered upper zone reveals the one below (LIVE-MODEL-SPEC 2b).
    pub zone_opacity: f32,
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
    /// The blend parameters of every pass, one slot each, bound with a dynamic
    /// offset. Passes recorded into one encoder (one submission per frame) each need
    /// their own slot: a single uniform rewritten per pass would leave every pass of
    /// the submission reading the last value. Grows (doubling) when a frame needs
    /// more slots than it has — only on the first frames of a show.
    ring: std::cell::RefCell<wgpu::Buffer>,
    /// Bytes between slots: `BlendParams` rounded up to the device's uniform offset
    /// alignment.
    stride: u64,
    capacity: std::cell::Cell<u32>,
    next: std::cell::Cell<u32>,
    /// Between [`begin_frame`](Self::begin_frame) and [`end_frame`](Self::end_frame):
    /// slots are not reused (the ring grows instead).
    in_frame: std::cell::Cell<bool>,
    /// Bound at binding 3 on the layer path, which never reads the mask. The
    /// binding must still be satisfied, so one zeroed cell stands in.
    dummy_mask: wgpu::Buffer,
}

impl Compositor {
    pub fn new(gpu: &crate::Gpu) -> Compositor {
        let device = &gpu.device;
        // The blend pipeline + layout depend only on the shader and target format, so
        // they are cached and reused across recompiles (M8 live-apply optimisation).
        let cached =
            gpu.cache.render("blend", || {
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
                                    has_dynamic_offset: true,
                                    min_binding_size: std::num::NonZeroU64::new(
                                        std::mem::size_of::<BlendParams>() as u64,
                                    ),
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
                            wgpu::BindGroupLayoutEntry {
                                binding: 5,
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
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1);
        let stride = (std::mem::size_of::<BlendParams>() as u64).div_ceil(align) * align;
        const SLOTS: u32 = 64;
        let dummy_mask = create_cell_mask(gpu, &[]);
        Compositor {
            cached,
            ring: std::cell::RefCell::new(ring_buffer(gpu, stride, SLOTS)),
            stride,
            capacity: std::cell::Cell::new(SLOTS),
            next: std::cell::Cell::new(0),
            in_frame: std::cell::Cell::new(false),
            dummy_mask,
        }
    }

    /// Start recording a frame into one encoder: every pass from here to
    /// [`end_frame`](Self::end_frame) gets its own uniform slot.
    pub fn begin_frame(&self) {
        self.in_frame.set(true);
        self.next.set(0);
    }

    /// The frame's passes are recorded (the encoder can be submitted).
    pub fn end_frame(&self) {
        self.in_frame.set(false);
    }

    /// The next free uniform slot's byte offset. Inside a frame a full ring grows
    /// (a new, larger buffer; passes already recorded keep the old one alive); outside
    /// one, every pass is submitted on its own, so the ring wraps.
    fn take_slot(&self, gpu: &crate::Gpu) -> u64 {
        let mut slot = self.next.get();
        if slot >= self.capacity.get() {
            if self.in_frame.get() {
                let grown = self.capacity.get() * 2;
                *self.ring.borrow_mut() = ring_buffer(gpu, self.stride, grown);
                self.capacity.set(grown);
            } else {
                slot = 0;
            }
        }
        self.next.set(slot + 1);
        u64::from(slot) * self.stride
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
        self.blend_keyed(gpu, dst, src, out, mode, opacity, DEFAULT_KEY);
    }

    /// [`blend`](Self::blend) with an explicit luminance key `[threshold, softness]`
    /// (read only by [`BlendMode::LumaKey`]).
    #[allow(clippy::too_many_arguments)]
    pub fn blend_keyed(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        key: [f32; 2],
    ) {
        self.blend_inner(
            gpu,
            dst,
            src,
            out,
            blend_code(mode),
            opacity,
            key,
            None,
            &self.dummy_mask,
            &self.dummy_mask,
        );
    }

    /// Blend a zone-texture `src` onto a **pixel-space** accumulator `dst` (LIVE-MODEL-SPEC
    /// §8.6): each output pixel reads its source texel through `src_index` (one `u32` per
    /// pixel texel, `u32::MAX` = not in the zone → transparent). With `coverage` (one
    /// `u32` per pixel texel, non-zero = inside) the blend is confined to it: outside,
    /// `dst` passes through untouched — a track that reaches the zone through groups
    /// (exact per pixel).
    #[allow(clippy::too_many_arguments)]
    pub fn blend_from(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        key: [f32; 2],
        src_index: &wgpu::Buffer,
        coverage: Option<&wgpu::Buffer>,
    ) {
        self.blend_inner_with(
            gpu,
            dst,
            src,
            out,
            blend_code(mode),
            opacity,
            key,
            if coverage.is_some() { 2 } else { 0 },
            [1.0; 4],
            &self.dummy_mask,
            coverage.unwrap_or(&self.dummy_mask),
            Some(src_index),
            None,
        );
    }

    /// A clip transition: `out = mix(dst, src, t)` on all four premultiplied channels.
    /// Unlike `Normal` at opacity `t`, transparent areas of `src` still fade `dst` out.
    pub fn crossfade(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        t: f32,
    ) {
        self.blend_inner(
            gpu,
            dst,
            src,
            out,
            CROSSFADE_CODE,
            t,
            DEFAULT_KEY,
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
        zone_opacity: f32,
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
    ) {
        self.blend_inner(
            gpu,
            dst,
            src,
            out,
            blend_code(mode),
            opacity,
            DEFAULT_KEY,
            Some([zone_scale[0], zone_scale[1], zone_scale[2], zone_opacity]),
            cell_mask,
            coverage,
        );
    }

    /// [`crossfade`](Self::crossfade), recorded into the frame's `encoder`.
    pub fn crossfade_in(
        &self,
        gpu: &crate::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        t: f32,
    ) {
        self.blend_inner_with(
            gpu,
            dst,
            src,
            out,
            CROSSFADE_CODE,
            t,
            DEFAULT_KEY,
            0,
            [1.0; 4],
            &self.dummy_mask,
            &self.dummy_mask,
            None,
            Some(encoder),
        );
    }

    /// [`blend_from`](Self::blend_from), recorded into the frame's `encoder`.
    #[allow(clippy::too_many_arguments)]
    pub fn blend_from_in(
        &self,
        gpu: &crate::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        key: [f32; 2],
        src_index: &wgpu::Buffer,
        coverage: Option<&wgpu::Buffer>,
    ) {
        self.blend_inner_with(
            gpu,
            dst,
            src,
            out,
            blend_code(mode),
            opacity,
            key,
            if coverage.is_some() { 2 } else { 0 },
            [1.0; 4],
            &self.dummy_mask,
            coverage.unwrap_or(&self.dummy_mask),
            Some(src_index),
            Some(encoder),
        );
    }

    /// [`blend_zone`](Self::blend_zone), recorded into the frame's `encoder`.
    #[allow(clippy::too_many_arguments)]
    pub fn blend_zone_in(
        &self,
        gpu: &crate::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: BlendMode,
        opacity: f32,
        zone_scale: [f32; 3],
        zone_opacity: f32,
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
    ) {
        self.blend_inner_with(
            gpu,
            dst,
            src,
            out,
            blend_code(mode),
            opacity,
            DEFAULT_KEY,
            1,
            [zone_scale[0], zone_scale[1], zone_scale[2], zone_opacity],
            cell_mask,
            coverage,
            None,
            Some(encoder),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn blend_inner(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: u32,
        opacity: f32,
        key: [f32; 2],
        zone_scale: Option<[f32; 4]>,
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
    ) {
        self.blend_inner_with(
            gpu,
            dst,
            src,
            out,
            mode,
            opacity,
            key,
            u32::from(zone_scale.is_some()),
            zone_scale.unwrap_or([1.0; 4]),
            cell_mask,
            coverage,
            None,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn blend_inner_with(
        &self,
        gpu: &crate::Gpu,
        dst: &wgpu::Texture,
        src: &wgpu::Texture,
        out: &wgpu::TextureView,
        mode: u32,
        opacity: f32,
        key: [f32; 2],
        use_mask: u32,
        scale: [f32; 4],
        cell_mask: &wgpu::Buffer,
        coverage: &wgpu::Buffer,
        src_index: Option<&wgpu::Buffer>,
        encoder: Option<&mut wgpu::CommandEncoder>,
    ) {
        let offset = self.take_slot(gpu);
        let ring = self.ring.borrow();
        gpu.queue.write_buffer(
            &ring,
            offset,
            bytemuck::bytes_of(&BlendParams {
                mode,
                opacity,
                use_mask,
                src_indirect: u32::from(src_index.is_some()),
                zone_scale: scale,
                key: [key[0], key[1], 0.0, 0.0],
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
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &ring,
                        offset: 0,
                        size: std::num::NonZeroU64::new(std::mem::size_of::<BlendParams>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: cell_mask.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: coverage.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: src_index.unwrap_or(&self.dummy_mask).as_entire_binding(),
                },
            ],
        });

        drop(ring);
        let mut own = None;
        let encoder = match encoder {
            Some(e) => e,
            None => own.insert(gpu.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("blend-enc"),
                },
            )),
        };
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
            pass.set_bind_group(0, &bind_group, &[offset as u32]);
            pass.draw(0..3, 0..1);
        }
        if let Some(e) = own {
            gpu.queue.submit([e.finish()]);
        }
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
                zone.zone_opacity,
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
                zone.zone_opacity,
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
            last.zone_opacity,
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

/// A uniform ring of `slots` slots, `stride` bytes apart.
fn ring_buffer(gpu: &crate::Gpu, stride: u64, slots: u32) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("blend-uniform-ring"),
        size: stride * u64::from(slots),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

//! The effect ↔ Parameter Tree seam (CLAUDE.md §5.3, §3.1 principle 2).
//!
//! This is the join between M3 (the Parameter Tree) and M5 (rendering), and the
//! contract every future effect stands on. An effect is **data + a shader**, not
//! special-cased Rust: it declares an [`EffectManifest`] listing its parameters,
//! and that single declaration:
//!
//! 1. **registers those parameters into the tree** (so MIDI/OSC/UI can drive them
//!    and the UI builds itself by walking the tree — nothing hardcoded), and
//! 2. **fixes the uniform-buffer layout** the shader reads.
//!
//! Per frame the render side does a wait-free read of each parameter handle
//! (§3.3), maps it through its descriptor curve to a real value, and packs the
//! values — in manifest order, behind the standard globals — into the effect's
//! uniform buffer. The shader's `Uniforms` struct mirrors that layout. No string
//! path and no lock touch the hot path: paths are resolved to handles **once**, at
//! construction.

use prisma_core::{ClockSnapshot, ParamDescriptor, ParamHandle, ParameterTree};

/// A channel of a compound colour control (option (a) grouping). Three scalar
/// params tagged with the same colour group and distinct channels form one colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorChannel {
    R,
    G,
    B,
}

/// How the UI should present a parameter — its *type* (colour / direction / plain
/// scalar). This is a **presentation hint only**: the value is still a scalar `f32`
/// in the Parameter Tree and one `f32` in the uniform buffer, and the render, the
/// tree and the show model are all untouched. The hint only decides which control the
/// parameter panel draws (§3.5 / §5.3 — the panel builds itself from the manifest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamRole {
    /// A standalone scalar → a slider. The default for every parameter.
    Scalar,
    /// A colour channel belonging to colour `group`. The group id is **explicit** (not
    /// inferred from the param name) so an effect with two colours — e.g. a gradient's
    /// start and end — keeps them in separate, non-colliding groups. The UI collects a
    /// group's channels into one 255-space picker; the operator thinks in 0–255 sRGB
    /// while the engine keeps the value in linear-light `0..1` (the UI converts, §2.3).
    Color {
        group: &'static str,
        channel: ColorChannel,
    },
    /// A discrete 8-way signal direction → the rose (reusing the editor's 8 signal
    /// directions). No built-in effect declares one yet; the control is wired and
    /// lights up the day an effect's manifest asks for it.
    Direction,
    /// One channel of palette slot `index` (0-based). Like [`ParamRole::Color`] the
    /// value stays a linear-light scalar in the tree/uniform — a palette is just a
    /// fixed block of colour channels plus a [`ParamRole::PaletteCount`] param. The UI
    /// collects every `Palette` param, groups them by `index`, and draws ONE palette
    /// editor (swatches + add/remove) whose active length is the count param. The
    /// engine packs the channels flat like any other scalar; the shader reassembles the
    /// colours into a local array and gates the loop by count.
    Palette { index: u8, channel: ColorChannel },
    /// The active length of a palette (how many of the fixed slots are live). A plain
    /// scalar in the tree; tagged so the palette editor can find it and so the shader
    /// reads it to gate its slot loop.
    PaletteCount,
}

/// One parameter an effect exposes. `name` is the trailing path segment (the
/// parameter is registered at `{prefix}/param/{name}`) and also documents which
/// uniform field it feeds. `role` is a presentation hint only (see [`ParamRole`]);
/// the value model is unchanged regardless of it.
pub struct ParamSpec {
    pub name: &'static str,
    pub descriptor: ParamDescriptor,
    pub role: ParamRole,
}

impl ParamSpec {
    /// A standalone scalar parameter — the panel draws a slider.
    pub fn scalar(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Scalar,
        }
    }

    /// A colour channel. `group` names the colour (distinct groups for two colours in
    /// one effect); `channel` is the component. The panel collects a group's channels
    /// into one 255-space picker.
    pub fn color(
        name: &'static str,
        descriptor: ParamDescriptor,
        group: &'static str,
        channel: ColorChannel,
    ) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Color { group, channel },
        }
    }

    /// A discrete 8-way signal direction — the panel draws the rose. Provided for the
    /// first effect that needs it; unused by the built-ins today.
    pub fn direction(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Direction,
        }
    }

    /// One channel of palette slot `index`. The panel collects a palette's channels
    /// into a single editor (see [`ParamRole::Palette`]).
    pub fn palette(
        name: &'static str,
        descriptor: ParamDescriptor,
        index: u8,
        channel: ColorChannel,
    ) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Palette { index, channel },
        }
    }

    /// The active-length parameter of a palette (see [`ParamRole::PaletteCount`]).
    pub fn palette_count(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::PaletteCount,
        }
    }
}

/// Static description of an effect: its id, its parameters (order matters — it is
/// the uniform layout), and its WGSL source. Built-in effects each provide one
/// (see [`crate::effects`]).
pub struct EffectManifest {
    pub effect_id: &'static str,
    pub params: Vec<ParamSpec>,
    /// WGSL with a `Uniforms` block following the shared convention: `time`,
    /// `_pad0`, `resolution`, then one `f32` per param in `params` order.
    pub shader_source: &'static str,
}

/// Number of `f32` globals prepended to every effect's uniform buffer:
/// `[time, _pad0, resolution.x, resolution.y]`. Keep in lockstep with the
/// `Uniforms` struct prefix in every shader.
const GLOBALS_F32: usize = 4;

/// An effect bound into one Parameter Tree: its parameters are registered, their
/// handles resolved once, and its GPU pipeline + uniform buffer built. Reused
/// every frame; `render` allocates nothing on the GPU side.
pub struct EffectInstance {
    /// Parallel to the manifest's params: resolved once, read wait-free per frame.
    handles: Vec<ParamHandle>,
    /// The effect's pipeline + bind-group layout, shared via the [`crate::cache`] across
    /// zones running the same effect and across recompiles.
    pipeline: std::sync::Arc<crate::cache::CachedRender>,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Scratch reused each frame to pack uniforms without allocating.
    scratch: Vec<f32>,
    /// Total f32 slots in the uniform buffer (globals + params + tail padding to
    /// a 16-byte multiple).
    uniform_f32_len: usize,
}

impl EffectInstance {
    /// Register the effect's parameters under `path_prefix` in `tree`, build its
    /// pipeline, and resolve its handles. Registered paths are
    /// `{path_prefix}/param/{name}`.
    pub fn new(
        gpu: &crate::Gpu,
        tree: &ParameterTree,
        path_prefix: &str,
        manifest: &EffectManifest,
    ) -> EffectInstance {
        let device = &gpu.device;

        // 1. Register params → resolve handles once (never per frame, §4.5).
        let mut handles = Vec::with_capacity(manifest.params.len());
        for spec in &manifest.params {
            let path = format!("{path_prefix}/param/{}", spec.name);
            handles.push(tree.register(&path, spec.descriptor.clone()));
        }

        // 2. Uniform buffer sized to globals + params, padded to a 16-byte
        //    multiple (the std140 rule for a uniform struct's total size).
        let payload_f32 = GLOBALS_F32 + manifest.params.len();
        let uniform_f32_len = align_up_usize(payload_f32 * 4, 16) / 4;
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect-uniforms"),
            size: (uniform_f32_len * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 3. Pipeline: one uniform at group 0 binding 0. The pipeline + its layout
        //    depend only on the effect's shader and the fixed target format, so they are
        //    cached by `effect_id` and shared across every zone running this effect and
        //    across recompiles (M8 live-apply optimisation). Only the per-instance
        //    uniform buffer and bind group are built here each time.
        let pipeline = gpu.cache.render(manifest.effect_id, || {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(manifest.effect_id),
                source: wgpu::ShaderSource::Wgsl(manifest.shader_source.into()),
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("effect-bgl"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("effect-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(manifest.effect_id),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect-bg"),
            layout: &pipeline.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        EffectInstance {
            handles,
            pipeline,
            uniform_buf,
            bind_group,
            scratch: vec![0.0; uniform_f32_len],
            uniform_f32_len,
        }
    }

    /// Pack the current parameter values (read wait-free from the tree) plus the
    /// clock globals into the uniform buffer and render the effect into `target`.
    /// Records and submits its own command buffer.
    pub fn render(
        &mut self,
        gpu: &crate::Gpu,
        clock: &ClockSnapshot,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        // Globals, then each param mapped through its curve to a real value, in
        // manifest order. Tail slots stay zero (padding).
        debug_assert_eq!(self.scratch.len(), self.uniform_f32_len);
        for s in self.scratch.iter_mut() {
            *s = 0.0;
        }
        self.scratch[0] = clock.show_seconds() as f32;
        // scratch[1] is _pad0.
        self.scratch[2] = width as f32;
        self.scratch[3] = height as f32;
        for (i, h) in self.handles.iter().enumerate() {
            // A dead handle (param unregistered) reads its default of 0.0 — the
            // effect degrades to black rather than panicking.
            self.scratch[GLOBALS_F32 + i] = h.read_real().unwrap_or(0.0);
        }
        gpu.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&self.scratch));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("effect-enc"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effect-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

/// Round `n` up to the next multiple of `align` (a power of two).
#[inline]
fn align_up_usize(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

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
    /// A discrete choice → a dropdown. The value is a scalar INDEX in
    /// `0..=choices.len()-1`, carried like any other scalar (the descriptor's real range
    /// is `0.0 ..= (len-1)` so the real value the shader receives is the index itself,
    /// selected with `i32(round(u.<name>))`). The panel draws the labels in index order.
    Enum { choices: &'static [&'static str] },
    /// An on/off switch → a latching button (the classic "Atenuar"). The value is `0.0`
    /// (off) or `1.0` (on), carried like any other scalar; the shader tests `> 0.5`.
    Toggle,
    /// A tempo in beats per minute → a slider plus the classic "Pulsar" (tap) button, which
    /// sets the value from the operator's taps. A plain real-valued scalar otherwise.
    Bpm,
    /// A direction / movement mode picked from a grouped menu (the classic direction rose +
    /// "→ Derecha" cascading menu). Like [`ParamRole::Enum`] the value is the choice
    /// INDEX; each choice is `"Group|Label"` so the panel can build the submenus, and the
    /// rose's arrows select choices by label.
    DirectionMenu { choices: &'static [&'static str] },
    /// How the effect walks its colour table (the classic play-mode button next to
    /// "Colores": loop / ping-pong / RND). Like [`ParamRole::Enum`] the value is the
    /// choice INDEX; the panel draws it as an icon menu beside the palette's button, so
    /// only effects that declare a `palette` should carry one.
    PaletteMode { choices: &'static [&'static str] },
}

/// When a parameter only applies in some state of another one, the panel greys it out
/// (consoles: "Anchura de la cruz" only in Cross mode, displacement options only while
/// there is displacement). Presentation only — the value is still carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requires {
    /// Enabled while `param` (an enum / direction menu) has the choice labelled `choice`.
    Choice {
        param: &'static str,
        choice: &'static str,
    },
    /// Enabled while `param` is above zero.
    NonZero { param: &'static str },
}

/// One parameter an effect exposes. `name` is the trailing path segment (the
/// parameter is registered at `{prefix}/param/{name}`) and also documents which
/// uniform field it feeds. `role` is a presentation hint only (see [`ParamRole`]);
/// the value model is unchanged regardless of it.
pub struct ParamSpec {
    pub name: &'static str,
    pub descriptor: ParamDescriptor,
    pub role: ParamRole,
    /// Greyed out unless this holds (see [`Requires`]). `None` = always enabled.
    pub requires: Option<Requires>,
}

impl ParamSpec {
    /// A standalone scalar parameter — the panel draws a slider.
    pub fn scalar(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Scalar,
            requires: None,
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
            requires: None,
        }
    }

    /// A discrete 8-way signal direction — the panel draws the rose. Provided for the
    /// first effect that needs it; unused by the built-ins today.
    pub fn direction(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Direction,
            requires: None,
        }
    }

    /// An on/off switch — the panel draws a latching button. Descriptor `0..1`.
    pub fn toggle(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Toggle,
            requires: None,
        }
    }

    /// A tempo (BPM) — slider + tap button. See [`ParamRole::Bpm`].
    pub fn bpm(name: &'static str, descriptor: ParamDescriptor) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Bpm,
            requires: None,
        }
    }

    /// A grouped direction menu. The descriptor's real range must be
    /// `0.0 ..= (choices.len()-1)`, linear, so the shader receives the index.
    pub fn direction_menu(
        name: &'static str,
        descriptor: ParamDescriptor,
        choices: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::DirectionMenu { choices },
            requires: None,
        }
    }

    /// A colour-table play mode (see [`ParamRole::PaletteMode`]). The descriptor's real
    /// range must be `0.0 ..= (choices.len()-1)`, linear, so the shader receives the index.
    pub fn palette_mode(
        name: &'static str,
        descriptor: ParamDescriptor,
        choices: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::PaletteMode { choices },
            requires: None,
        }
    }

    /// Grey this parameter out unless `req` holds.
    pub fn requires(mut self, req: Requires) -> Self {
        self.requires = Some(req);
        self
    }

    pub fn enum_choice(
        name: &'static str,
        descriptor: ParamDescriptor,
        choices: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            descriptor,
            role: ParamRole::Enum { choices },
            requires: None,
        }
    }
}

/// Static description of an effect: its id, its parameters (order matters — it is
/// the uniform layout), and its WGSL source. Built-in effects each provide one
/// (see [`crate::effects`]).
pub struct EffectManifest {
    pub effect_id: &'static str,
    pub params: Vec<ParamSpec>,
    /// A colour palette the effect reads (the classic colour table), with its factory
    /// colours (linear light). `Some` gives the shader a second uniform at group 0
    /// binding 1 — `struct { len: f32, _pad: vec3<f32>, colors: array<vec4<f32>, 255> }`
    /// — filled from the cell's own colour list ([`EffectInstance::set_palette`]). A
    /// palette is a LIST stored with the cell, not one tree parameter per colour, so
    /// it can be long (up to [`PALETTE_MAX`]) without bloating the Parameter Tree.
    pub palette: Option<&'static [[f32; 3]]>,
    /// WGSL with a `Uniforms` block following the shared convention: `time`,
    /// `_pad0`, `resolution`, then one `f32` per param in `params` order.
    pub shader_source: &'static str,
}

/// Most colours a palette holds (the shader's array length).
pub const PALETTE_MAX: usize = 255;
/// Palette uniform size in f32: a vec4 header (`len`) + PALETTE_MAX vec4 colours.
const PALETTE_F32: usize = 4 + PALETTE_MAX * 4;

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
    /// The colour-list uniform (binding 1) for effects that declare a palette.
    palette_buf: Option<wgpu::Buffer>,
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
            let uniform_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            };
            // Binding 1 (the palette) only for effects that declare one. A UNIFORM, not
            // a storage buffer, so it also works on the GLES fallback backend.
            let mut entries = vec![uniform_entry(0)];
            if manifest.palette.is_some() {
                entries.push(uniform_entry(1));
            }
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("effect-bgl"),
                    entries: &entries,
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
        let palette_buf = manifest.palette.map(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effect-palette"),
                size: (PALETTE_F32 * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }];
        if let Some(buf) = &palette_buf {
            entries.push(wgpu::BindGroupEntry {
                binding: 1,
                resource: buf.as_entire_binding(),
            });
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect-bg"),
            layout: &pipeline.bind_group_layout,
            entries: &entries,
        });

        let inst = EffectInstance {
            handles,
            pipeline,
            uniform_buf,
            palette_buf,
            bind_group,
            scratch: vec![0.0; uniform_f32_len],
            uniform_f32_len,
        };
        if let Some(defaults) = manifest.palette {
            inst.set_palette(gpu, defaults);
        }
        inst
    }

    /// Whether this effect reads a colour palette (binding 1).
    pub fn has_palette(&self) -> bool {
        self.palette_buf.is_some()
    }

    /// Upload the palette (linear-light RGB, at most [`PALETTE_MAX`] colours; extra
    /// ones are ignored). A no-op for an effect without a palette. One `write_buffer`
    /// of 4 KB from a stack array — no heap allocation — so it is cheap enough to run
    /// at the render loop's frame boundary when a colour is edited live.
    pub fn set_palette(&self, gpu: &crate::Gpu, colors: &[[f32; 3]]) {
        let Some(buf) = &self.palette_buf else {
            return;
        };
        let n = colors.len().min(PALETTE_MAX);
        let mut data = [0.0f32; PALETTE_F32];
        data[0] = n as f32;
        for (i, c) in colors.iter().take(n).enumerate() {
            let o = 4 + i * 4;
            data[o..o + 3].copy_from_slice(c);
            data[o + 3] = 1.0;
        }
        gpu.queue.write_buffer(buf, 0, bytemuck::cast_slice(&data));
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

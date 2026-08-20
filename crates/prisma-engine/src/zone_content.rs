//! The per-zone compositor content (CLAUDE.md M8, docs/M8-PARTA.md §2, §3).
//!
//! [`ZoneCompositor`] is the [`FrameContent`] that replaces [`IdleContent`] once a
//! show runs. Per frame it renders **each zone's effect at that zone's own
//! `mapping_size`**, **places** each into the world framebuffer at the zone's
//! footprint (`prisma_render::ZonePlacer`), and folds the placed textures with the
//! **unchanged** M5 blend machinery (`Compositor::blend_zone`). The gather, the
//! overlap counter and the output stage downstream keep operating on the one world
//! framebuffer exactly as in M5 — this content only changes *where a zone's pixels
//! came from* (a per-`mapping_size` texture, placed), never the world coordinate.
//!
//! # Hot-path allocation: zero (M8-PARTA §2.6, guardrail)
//!
//! Everything GPU-sized is allocated **once**, at construction, from the compiled
//! layout: per-zone `mapping_size` textures and world-sized placed textures, the
//! per-zone coverage buffers, the two world-sized fold scratch textures, and the
//! per-frame cell-overlap mask buffer. Per frame the loop:
//! - writes the frame's cell mask with `queue.write_buffer` (no buffer alloc),
//! - reuses one `Vec<u32>` (`contrib`) cleared and refilled in place,
//! - folds by calling `blend_zone` directly in a ping-pong over the two scratch
//!   textures — no per-frame `Vec<ZoneComposite>` and, crucially, **no GPU buffer
//!   or texture allocation**.
//!
//! # Per-zone speed (M5 gap closed here, guardrail)
//!
//! `speed_multiplier` drives only the **effect's** show-time — a per-zone phase fed to
//! a *copy* of the clock snapshot — never the master clock, which would desync
//! everything. The phase is **integrated** from the signed speed each frame
//! ([`phase_clock`] + the accumulator in [`ZoneCompositor::render_into`]), so the
//! velocity is **bipolar**: 0 holds the animation static, a negative speed plays the
//! zone in reverse. (Scaling absolute `show_ns` — the earlier approach — cannot reverse,
//! because that clock is unsigned and monotonic.)

use std::sync::mpsc;

use prisma_core::show::{Show, Source};
use prisma_core::source::{priority, SourceId};
use prisma_core::tree::ParameterTree;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, OverlapSnapshot};
use prisma_patch::{BlendMode, Patch, PatchIndex};
use prisma_render::blend::Compositor;
use prisma_render::effect::EffectInstance;
use prisma_render::live_params::LiveParams;
use prisma_render::overlap::zone_cell_coverage;
use prisma_render::place::{footprint_from_coverage, Footprint, ZonePlacer};
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::FrameContent;
use prisma_render::{create_cell_mask, manifest_by_id, Gpu};

/// Where the engine writes the Show's initial parameter values from. A load-time
/// seed at UI priority, so a later MIDI/UI write can take over normally (§4.5).
fn show_load_source() -> SourceId {
    SourceId::new(priority::UI, 0)
}

/// Build each zone's active effect from a Show, indexed by **zone slot** (the
/// `Patch::zones` / overlap-counter coordinate), ready to hand to
/// [`ZoneCompositor::new`]. A zone with no active cell, an empty cell, or an
/// unknown `effect_id` yields `None`.
///
/// This resolves the show model's string `effect_id` against the GPU effect
/// catalogue ([`manifest_by_id`]), builds the [`EffectInstance`] (registering its
/// params in `tree` under `/zone/{id}/layer/0`), then writes the cell's stored
/// parameter values over the manifest defaults. The GPU work (pipeline compile)
/// happens here, at load — not per frame.
pub fn build_zone_effects(
    gpu: &Gpu,
    tree: &ParameterTree,
    patch: &Patch,
    index: &PatchIndex,
    show: &Show,
) -> Vec<Option<EffectInstance>> {
    let src = show_load_source();
    (0..index.zone_count())
        .map(|slot| {
            let zone = &patch.zones[slot];
            let zs = show.zone(zone.id)?;
            let cell = zs.grid.get(zs.active?)?;
            let layer = cell.layers.first()?;
            let (effect_id, params) = match &layer.source {
                Source::Shader { effect_id, params } => (effect_id, params),
            };
            let manifest = manifest_by_id(effect_id)?;
            let prefix = format!("/zone/{}/layer/0", zone.id);
            let inst = EffectInstance::new(gpu, tree, &prefix, &manifest);
            for (name, value) in params.iter() {
                tree.write_path(&format!("{prefix}/param/{name}"), value, src);
            }
            Some(inst)
        })
        .collect()
}

/// Seed the Parameter Tree with a Show's initial **live** state — master
/// dimmer/colour and each zone's dimmer/colour/speed/solo/blackout — so a loaded
/// show lights up at the values it was saved with rather than at engine defaults
/// (dimmer down = dark). Written through the same [`LiveParams`] handles the loops
/// read, at load-time UI priority.
pub fn seed_live_from_show(tree: &ParameterTree, live: &LiveParams, patch: &Patch, show: &Show) {
    let src = show_load_source();
    let master = live.master();
    tree.write_real(&master.dimmer, show.master.dimmer, src);
    for (i, h) in master.color.iter().enumerate() {
        tree.write_real(h, show.master.color[i], src);
    }
    for (slot, zone) in patch.zones.iter().enumerate() {
        let Some(zs) = show.zone(zone.id) else {
            continue;
        };
        let h = live.zone(slot);
        let l = &zs.live;
        tree.write_real(&h.dimmer, l.dimmer, src);
        for (i, ch) in h.color.iter().enumerate() {
            tree.write_real(ch, l.color_filter[i], src);
        }
        tree.write_real(&h.speed, l.speed_multiplier, src);
        tree.write_real(&h.solo, if l.solo { 1.0 } else { 0.0 }, src);
        tree.write_real(&h.blackout, if l.blackout { 1.0 } else { 0.0 }, src);
    }
}

/// A structural change to the running content, applied at the **frame boundary**
/// (M8-PARTA §3, locked decision 3). The expensive part — building an
/// [`EffectInstance`] (which compiles a GPU pipeline) — happens **off** the render
/// thread on the sender's side; the command carries the built instance and the
/// render loop's only work is the pointer swap. That is what makes the swap atomic
/// per frame: a command sent mid-frame lands in the queue and is applied at the
/// **top** of the next `render_into`, so frame N renders wholly with the old effect
/// and frame N+1 wholly with the new one — never a half-and-half frame.
pub enum ZoneCommand {
    /// Replace a zone's active effect (`None` clears it to nothing playing).
    SetEffect {
        slot: usize,
        effect: Option<EffectInstance>,
    },
}

/// The producer end of the frame-boundary command queue. Cloneable and `Send`, so
/// the UI/transport threads can drive content changes without touching the render
/// thread directly. Obtain one from [`ZoneCompositor::command_sender`].
#[derive(Clone)]
pub struct ZoneCommandSender {
    tx: mpsc::Sender<ZoneCommand>,
}

impl ZoneCommandSender {
    /// Queue an effect swap for `slot`, applied at the next frame boundary. Returns
    /// `false` if the compositor has been dropped (the render loop is gone).
    pub fn set_zone_effect(&self, slot: usize, effect: Option<EffectInstance>) -> bool {
        self.tx
            .send(ZoneCommand::SetEffect { slot, effect })
            .is_ok()
    }
}

/// One zone's GPU-side resources, all sized once at construction.
struct ZoneSlot {
    /// The zone's own local matrix resolution (`Zone::mapping_size`, X/Y).
    mapping: (u32, u32),
    /// Where the zone's local space lands on the world framebuffer — derived from
    /// the zone's coverage mask so placement is always a superset of coverage
    /// (M8-PARTA §2.3).
    footprint: Footprint,
    /// The effect rendered at `mapping`, once per frame.
    zone_tex: wgpu::Texture,
    /// The placed result at world resolution — the compositor's input for this zone.
    placed_tex: wgpu::Texture,
    /// Per-cell coverage mask (world cells), uploaded once.
    coverage: wgpu::Buffer,
    /// How this zone blends onto the zones below it.
    blend_mode: BlendMode,
    /// The active cell's effect, or `None` when the zone has nothing playing.
    effect: Option<EffectInstance>,
}

/// The per-zone compositor. Implements [`FrameContent`]; install it in place of
/// [`IdleContent`](crate::IdleContent) when a show runs.
pub struct ZoneCompositor {
    compositor: Compositor,
    placer: ZonePlacer,
    /// Bottom-to-top compositing order (`PatchIndex::composite_order`), once.
    order: Vec<u32>,
    slots: Vec<ZoneSlot>,
    /// This frame's per-cell overlap mask (world cells). Persistent; rewritten each
    /// frame from the snapshot with `write_buffer`.
    cell_mask: wgpu::Buffer,
    /// World-sized ping/pong scratch for the fold. Persistent.
    ping: wgpu::Texture,
    pong: wgpu::Texture,
    /// Reused each frame: contributing zones in compositing order. Cleared + refilled
    /// in place, so it allocates only on the first (or a larger) frame.
    contrib: Vec<u32>,
    /// Frame-boundary command queue (M8-PARTA §3). Drained at the top of every
    /// `render_into`; `commands_tx` is kept so [`command_sender`](Self::command_sender)
    /// can hand out cloned producers.
    commands_rx: mpsc::Receiver<ZoneCommand>,
    commands_tx: mpsc::Sender<ZoneCommand>,
    /// Per-zone animation phase in show-nanoseconds, integrated from signed speed each
    /// frame so a **negative** speed runs that zone's effects in reverse (bipolar
    /// velocity). Seeded to [`PHASE_BIAS_NS`] so the unsigned clock never underflows.
    zone_phase_ns: Vec<f64>,
    /// Master show-time at the previous frame, to take the delta the phase integrates.
    /// `None` before the first frame (delta 0).
    last_show_ns: Option<u64>,
}

/// Starting value for every zone's phase accumulator, in show-ns (`100` s). It exists to
/// give **reverse** playback (a negative speed) room before the phase reaches zero and
/// the unsigned per-zone clock clamps: ~100 s of reverse at 1×, ~10 s at 10× — and the
/// headroom grows as the show plays forward. It is deliberately *not* larger: the shader
/// feeds `time = show_seconds` straight into GPU `sin`/`cos`, whose hardware range
/// reduction loses accuracy for large arguments, so a big time offset would visibly
/// corrupt the trig-based effects (plasma/wave/radial/color_scroll). That accuracy
/// ceiling is a *pre-existing* property of the absolute-time model (any effect degrades
/// after a long runtime as `show_seconds` grows); 100 s keeps the bias within a normal
/// session's magnitudes. Reverse past the show origin holds the show-start frame (benign
/// clamp), not a crash. A wrap-based phase (constant trig accuracy + unbounded reverse)
/// is the eventual fix, but it must reconcile periodic vs noise-coordinate effects and is
/// out of scope here.
const PHASE_BIAS_NS: f64 = 1.0e11;

impl ZoneCompositor {
    /// Build the compositor for a compiled layout. `effects` is indexed by **zone
    /// slot** (the same coordinate as `Patch::zones` and the overlap counter); a
    /// `None` slot is a zone with no active cell. All GPU resources are allocated
    /// here — nothing below allocates GPU memory per frame.
    pub fn new(
        gpu: &Gpu,
        patch: &Patch,
        index: &PatchIndex,
        mut effects: Vec<Option<EffectInstance>>,
    ) -> ZoneCompositor {
        let world = (patch.matrix_size[0].max(1), patch.matrix_size[1].max(1));
        assert_eq!(
            effects.len(),
            index.zone_count(),
            "one effect slot per zone (None allowed)"
        );

        let coverage_masks = zone_cell_coverage(patch, index);
        let slots: Vec<ZoneSlot> = (0..index.zone_count())
            .map(|slot| {
                let zone = &patch.zones[slot];
                let mapping = (zone.mapping_size[0].max(1), zone.mapping_size[1].max(1));
                let footprint = footprint_from_coverage(&coverage_masks[slot], world.0);
                ZoneSlot {
                    mapping,
                    footprint,
                    zone_tex: create_render_target(gpu, mapping.0, mapping.1),
                    placed_tex: create_render_target(gpu, world.0, world.1),
                    coverage: create_cell_mask(gpu, &coverage_masks[slot]),
                    blend_mode: zone.blend_mode,
                    effect: effects[slot].take(),
                }
            })
            .collect();

        let cell_count = (world.0 as usize) * (world.1 as usize);
        let cell_mask = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zone-cell-overlap-mask"),
            size: (cell_count * std::mem::size_of::<u32>()).max(4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (commands_tx, commands_rx) = mpsc::channel();
        ZoneCompositor {
            compositor: Compositor::new(gpu),
            placer: ZonePlacer::new(gpu),
            order: index.composite_order().to_vec(),
            slots,
            cell_mask,
            ping: create_render_target(gpu, world.0, world.1),
            pong: create_render_target(gpu, world.0, world.1),
            contrib: Vec::new(),
            commands_rx,
            commands_tx,
            zone_phase_ns: vec![PHASE_BIAS_NS; index.zone_count()],
            last_show_ns: None,
        }
    }

    /// A producer for the frame-boundary command queue (M8-PARTA §3). Hand these to
    /// the UI/transport threads; their `set_zone_effect` calls take effect at the
    /// next frame boundary, never mid-frame.
    pub fn command_sender(&self) -> ZoneCommandSender {
        ZoneCommandSender {
            tx: self.commands_tx.clone(),
        }
    }

    /// Replace a zone's active effect directly (`None` clears it). Same swap the
    /// queue performs, for callers already on the render thread / holding `&mut`.
    /// Textures and footprint are unchanged — only the content source moves.
    pub fn set_zone_effect(&mut self, slot: usize, effect: Option<EffectInstance>) {
        self.slots[slot].effect = effect;
    }

    /// Apply every queued command. Called at the **top** of `render_into`, so all
    /// structural changes land at the frame boundary and the frame is then rendered
    /// wholly under the post-swap state — never a half-and-half frame. Non-blocking
    /// (`try_recv`); the render thread never waits on a producer.
    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.commands_rx.try_recv() {
            match cmd {
                ZoneCommand::SetEffect { slot, effect } => {
                    if slot < self.slots.len() {
                        self.slots[slot].effect = effect;
                    }
                }
            }
        }
    }
}

/// A per-zone view of the clock whose **show-time** is the zone's integrated phase —
/// never the master clock (guardrail, M8-PARTA §3). `frame`/`beat`/`bpm` are left as-is;
/// the built-in effects animate off `show_seconds`. The phase is advanced per frame by
/// [`ZoneCompositor`] so a signed speed can run the zone forward or in reverse.
fn phase_clock(clock: &ClockSnapshot, phase_ns: f64) -> ClockSnapshot {
    let mut c = *clock;
    c.show_ns = phase_ns.max(0.0) as u64;
    c
}

/// A zone's linear-light scale for the compositor: `dimmer × color_filter`.
fn zone_scale(zones: &ZoneStates, slot: usize) -> [f32; 3] {
    let z = zones.get(slot);
    let d = z.dimmer();
    [
        d * z.color_filter[0],
        d * z.color_filter[1],
        d * z.color_filter[2],
    ]
}

fn view(tex: &wgpu::Texture) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

impl FrameContent for ZoneCompositor {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        zones: &ZoneStates,
        overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        // 0. Frame boundary: apply all queued structural changes (effect swaps)
        //    BEFORE any rendering this frame. This is the atomic-swap point — the
        //    whole frame below is rendered under one consistent set of effects, so a
        //    swap can never split a frame into old-half / new-half (M8-PARTA §3).
        self.drain_commands();

        // 0b. Advance every zone's animation phase by its signed speed over the
        //     master's show-time delta. Integrating (rather than scaling absolute time)
        //     is what lets a negative speed play a zone in REVERSE; a zero speed holds
        //     the phase (static). Done for all slots, contributing or not, so a zone that
        //     comes back on resumes where its animation would have been. Frozen master
        //     clock ⇒ zero delta ⇒ every zone freezes.
        let delta_ns = match self.last_show_ns {
            Some(prev) => clock.show_ns.saturating_sub(prev) as f64,
            None => 0.0,
        };
        self.last_show_ns = Some(clock.show_ns);
        for s in 0..self.slots.len() {
            let speed = zones.get(s).speed_multiplier as f64;
            self.zone_phase_ns[s] = (self.zone_phase_ns[s] + delta_ns * speed).max(0.0);
        }

        // 1. Which zones actually contribute this frame, in compositing order. A
        //    zone contributes iff it has live state (contributes_pixels), an active
        //    effect, and a non-empty footprint. Reuses `contrib` in place.
        self.contrib.clear();
        for &slot in &self.order {
            let s = slot as usize;
            let slot_ref = &self.slots[s];
            if zones.contributes_pixels(s)
                && slot_ref.effect.is_some()
                && !slot_ref.footprint.is_empty()
            {
                self.contrib.push(slot);
            }
        }

        // 2. Render each contributing zone's effect at its OWN mapping_size (with a
        //    per-zone speed-scaled clock), then PLACE it into the world framebuffer.
        for &slot in &self.contrib {
            let s = slot as usize;
            let zclock = phase_clock(clock, self.zone_phase_ns[s]);
            {
                let slot_mut = &mut self.slots[s];
                if let Some(effect) = slot_mut.effect.as_mut() {
                    effect.render(
                        gpu,
                        &zclock,
                        &view(&slot_mut.zone_tex),
                        slot_mut.mapping.0,
                        slot_mut.mapping.1,
                    );
                }
            }
            let slot_ref = &self.slots[s];
            self.placer.place(
                gpu,
                &slot_ref.zone_tex,
                &view(&slot_ref.placed_tex),
                slot_ref.footprint,
            );
        }

        // 3. This frame's per-cell overlap mask, into the persistent buffer.
        gpu.queue.write_buffer(
            &self.cell_mask,
            0,
            bytemuck::cast_slice(overlap.cell_mask_bytes()),
        );

        // 4. Fold the placed textures into `target` with the UNCHANGED blend
        //    machinery (`blend_zone`), ping-ponging over two persistent scratch
        //    textures. No per-frame Vec<ZoneComposite>, no GPU allocation. Zone
        //    dimmer/colour is applied here only on cells the snapshot marks
        //    overlapped (via cell_mask); elsewhere it is deferred to the output
        //    stage (§5.2.1) — this content does not touch that split, it just feeds
        //    the same cell_mask the M5 compositor expects.
        let Some((&last, rest)) = self.contrib.split_last() else {
            // Nothing contributes: the world framebuffer is black this frame.
            clear_black(gpu, target);
            return;
        };

        clear_black(gpu, &self.ping);
        let mut acc: &wgpu::Texture = &self.ping;
        let mut other: &wgpu::Texture = &self.pong;
        for &slot in rest {
            let s = slot as usize;
            let zs = &self.slots[s];
            self.compositor.blend_zone(
                gpu,
                acc,
                &zs.placed_tex,
                &view(other),
                zs.blend_mode,
                1.0,
                zone_scale(zones, s),
                &self.cell_mask,
                &zs.coverage,
            );
            std::mem::swap(&mut acc, &mut other);
        }
        // The last zone blends straight into the render loop's framebuffer.
        let s = last as usize;
        let zs = &self.slots[s];
        self.compositor.blend_zone(
            gpu,
            acc,
            &zs.placed_tex,
            &view(target),
            zs.blend_mode,
            1.0,
            zone_scale(zones, s),
            &self.cell_mask,
            &zs.coverage,
        );
    }
}

/// Clear an `Rgba16Float` texture to linear-light black — the fold's base and the
/// no-content frame. A bare render pass with a clear load op; no allocation.
fn clear_black(gpu: &Gpu, texture: &wgpu::Texture) {
    let v = view(texture);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("zone-clear"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("zone-clear-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &v,
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

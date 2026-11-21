//! The per-zone compositor content (CLAUDE.md M8, docs/M8-PARTA.md §2, §3,
//! docs/LIVE-MODEL-SPEC.md §4.1).
//!
//! [`ZoneCompositor`] is the [`FrameContent`] that replaces [`IdleContent`] once a
//! show runs. Per frame, for **each zone**, it renders every track that reaches the
//! zone at that zone's own `mapping_size` (one effect instance per (track, zone)),
//! folds those track textures in track order with each track's blend mode and
//! opacity (`Compositor::blend`, the layer machinery), **places** the result into the
//! world framebuffer at the zone's footprint (`prisma_render::ZonePlacer`), and folds
//! the placed zones with the **unchanged** M5 zone machinery (`Compositor::
//! blend_zone`). Gather, overlap counter and output stage downstream are untouched —
//! this content only changes *where a zone's pixels came from*.
//!
//! # The 1:1 shortcut
//!
//! A zone whose only playing track is `Normal` at full opacity renders that track
//! **straight into the zone texture**, with no fold: blending one `Normal` layer at
//! opacity 1 over black is the identity, so the fold would only cost two passes and
//! two textures. A zone reached by a single such track (the default show: one track
//! per zone) never allocates fold textures at all. The `m8_zone_render` suite asserts
//! the shortcut and the fold produce the same frame.
//!
//! # Hot-path allocation: zero (M8-PARTA §2.6, LIVE-MODEL-SPEC §4.4)
//!
//! Everything GPU-sized is allocated **once**, at construction, from the compiled
//! layout and the show's track destinations: per-zone `mapping_size` and world-sized
//! placed textures, per-(track, zone) textures and per-zone fold scratch where a
//! zone can fold, the per-zone coverage buffers, the two world-sized fold scratch
//! textures, and the per-frame cell-overlap mask buffer. Per frame the loop writes the
//! cell mask with `queue.write_buffer`, reuses one `Vec<u32>` (`contrib`), and
//! ping-pongs over persistent textures — no GPU buffer or texture allocation. A
//! destination change goes through `recompile_show`.
//!
//! # Per-zone speed (M5 gap closed here, guardrail)
//!
//! `speed_multiplier` drives only the **effect's** show-time — a per-instance phase fed
//! to a *copy* of the clock snapshot — never the master clock, which would desync
//! everything. Each (track, zone) instance **integrates** its phase from its zone's
//! signed speed each frame ([`phase_clock`] + the accumulator in
//! [`ZoneCompositor::render_into`]), so the velocity is **bipolar** (0 holds, negative
//! reverses), and restarting one track never moves another track in the same zone.

use std::sync::mpsc;

use prisma_core::show::{Show, Source, Track};
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
use uuid::Uuid;

/// Where the engine writes the Show's initial parameter values from. A load-time
/// seed at UI priority, so a later MIDI/UI write can take over normally (§4.5).
fn show_load_source() -> SourceId {
    SourceId::new(priority::UI, 0)
}

/// The Parameter Tree prefix of a track's running effect: its params live at
/// `{prefix}/param/{name}`.
///
/// One prefix per **track**, not per (track, zone): a track reaching several zones
/// (phase 4) must share one set of params across its instances, which needs
/// `EffectInstance` to take pre-resolved handles — `register` replaces a path, so a
/// second instance would orphan the first's (LIVE-MODEL-SPEC §9).
pub fn track_param_prefix(track: Uuid) -> String {
    format!("/track/{track}/layer/0")
}

/// The zone slots (`Patch::zones` positions) a track reaches, in destination order,
/// without duplicates; zones absent from the patch are skipped. The compositor and
/// the clip controller both size a track's instances from this, so they agree.
pub fn destination_slots(patch: &Patch, track: &Track) -> Vec<usize> {
    let mut slots: Vec<usize> = Vec::with_capacity(track.destination.zones.len());
    for id in &track.destination.zones {
        if let Some(slot) = patch.zones.iter().position(|z| z.id == *id) {
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
    }
    slots
}

/// One track as the compositor builds it: which zones it reaches, how it blends
/// there, and its running effect per zone (`effects[k]` renders in `zones[k]`).
pub struct TrackContent {
    /// Zone slots, in destination order.
    pub zones: Vec<usize>,
    pub blend_mode: BlendMode,
    /// `0.0..=1.0`.
    pub opacity: f32,
    /// Parallel to `zones`; `None` = nothing playing in that zone. Shorter than
    /// `zones` means `None` for the rest.
    pub effects: Vec<Option<EffectInstance>>,
}

impl TrackContent {
    /// One `Normal`, full-opacity track per zone, track `i` → zone `i` with
    /// `effects[i]` — the default show's shape.
    pub fn one_per_zone(effects: Vec<Option<EffectInstance>>) -> Vec<TrackContent> {
        effects
            .into_iter()
            .enumerate()
            .map(|(slot, effect)| TrackContent {
                zones: vec![slot],
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
                effects: vec![effect],
            })
            .collect()
    }
}

/// Build one track's effect instances — one per destination zone — on the caller's
/// thread (a pipeline compile, never on the render thread): register the params
/// under [`track_param_prefix`], write the stored values over the manifest defaults,
/// upload the colour table. `None` if the effect is not in the catalogue.
#[allow(clippy::too_many_arguments)] // each is a distinct piece of the build
pub fn build_track_instances(
    gpu: &Gpu,
    tree: &ParameterTree,
    track: Uuid,
    zones: usize,
    effect_id: &str,
    params: &prisma_core::show::ParamSet,
    palette: Option<&[[f32; 3]]>,
    src: SourceId,
) -> Option<Vec<Option<EffectInstance>>> {
    let manifest = manifest_by_id(effect_id)?;
    let prefix = track_param_prefix(track);
    let effects: Vec<Option<EffectInstance>> = (0..zones)
        .map(|_| {
            let inst = EffectInstance::new(gpu, tree, &prefix, &manifest);
            if let Some(colors) = palette {
                inst.set_palette(gpu, colors);
            }
            Some(inst)
        })
        .collect();
    // After every instance registered, so the values land on the live handles.
    for (name, value) in params.iter() {
        tree.write_path(&format!("{prefix}/param/{name}"), value, src);
    }
    Some(effects)
}

/// Build every track's running content from a Show, in track order (= compositing
/// order, first at the bottom), ready for [`ZoneCompositor::new`]. A track with no
/// active clip, an empty clip or an unknown `effect_id` plays nothing. The GPU work
/// (pipeline compile) happens here, at load — not per frame.
pub fn build_track_effects(
    gpu: &Gpu,
    tree: &ParameterTree,
    patch: &Patch,
    show: &Show,
) -> Vec<TrackContent> {
    let src = show_load_source();
    show.tracks
        .iter()
        .map(|track| {
            let zones = destination_slots(patch, track);
            let effects = track
                .active_clip()
                .and_then(|cell| cell.layers.first())
                .and_then(|layer| match &layer.source {
                    Source::Shader {
                        effect_id,
                        params,
                        palette,
                    } => build_track_instances(
                        gpu,
                        tree,
                        track.id,
                        zones.len(),
                        effect_id,
                        params,
                        palette.as_deref(),
                        src,
                    ),
                })
                .unwrap_or_default();
            TrackContent {
                zones,
                blend_mode: track.blend_mode,
                opacity: track.opacity,
                effects,
            }
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
/// thread on the sender's side; the command carries the built instances and the
/// render loop's only work is the pointer swap. A command sent mid-frame is applied
/// at the **top** of the next `render_into`, so frame N renders wholly with the old
/// effect and frame N+1 wholly with the new one — never a half-and-half frame.
pub enum ZoneCommand {
    /// Replace a track's running effect in each zone it reaches (`effects[k]` for its
    /// k-th destination zone; missing entries clear). An empty list stops the track.
    SetTrackEffects {
        track: usize,
        effects: Vec<Option<EffectInstance>>,
    },
    /// Rewind a track's animation phase (in every zone it reaches) to its start, so its
    /// effect plays from the beginning (a pad in trigger mode, M9).
    Restart { track: usize },
    /// Replace the colour table of a track's running effect (the Tabla de color edited
    /// live). Built by the sender; the render thread only uploads it at the boundary.
    SetPalette { track: usize, colors: Vec<[f32; 3]> },
}

/// The producer end of the frame-boundary command queue. Cloneable and `Send`, so
/// the UI/transport threads can drive content changes without touching the render
/// thread directly. Obtain one from [`ZoneCompositor::command_sender`].
#[derive(Clone)]
pub struct ZoneCommandSender {
    tx: mpsc::Sender<ZoneCommand>,
}

impl ZoneCommandSender {
    /// Queue an effect swap for `track`, applied at the next frame boundary. Returns
    /// `false` if the compositor has been dropped (the render loop is gone).
    pub fn set_track_effects(&self, track: usize, effects: Vec<Option<EffectInstance>>) -> bool {
        self.tx
            .send(ZoneCommand::SetTrackEffects { track, effects })
            .is_ok()
    }

    /// Queue stopping `track` (nothing playing in any zone it reaches).
    pub fn clear_track(&self, track: usize) -> bool {
        self.set_track_effects(track, Vec::new())
    }

    /// Queue a phase rewind for `track` (applied after any swap queued before it).
    pub fn restart_track(&self, track: usize) -> bool {
        self.tx.send(ZoneCommand::Restart { track }).is_ok()
    }

    /// Queue a new colour table for `track`'s running effect (applied after any swap
    /// queued before it, so it lands on the effect that will actually be running).
    pub fn set_track_palette(&self, track: usize, colors: Vec<[f32; 3]>) -> bool {
        self.tx
            .send(ZoneCommand::SetPalette { track, colors })
            .is_ok()
    }
}

/// One (track, zone) effect instance, held by its zone in track order.
struct Instance {
    blend_mode: BlendMode,
    opacity: f32,
    effect: Option<EffectInstance>,
    /// Its render target at the zone's `mapping_size` — only in a zone that can fold
    /// (see [`ZoneSlot::fold`]); the shortcut renders straight into `zone_tex`.
    tex: Option<wgpu::Texture>,
    /// Animation phase in show-ns, integrated from the zone's signed speed.
    phase_ns: f64,
}

impl Instance {
    /// Blending this over black is the identity (`Normal` at full opacity).
    fn is_plain(&self) -> bool {
        self.blend_mode == BlendMode::Normal && self.opacity >= 1.0
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
    /// The zone's folded tracks at `mapping`, once per frame.
    zone_tex: wgpu::Texture,
    /// The placed result at world resolution — the compositor's input for this zone.
    placed_tex: wgpu::Texture,
    /// Per-cell coverage mask (world cells), uploaded once.
    coverage: wgpu::Buffer,
    /// How this zone blends onto the zones below it.
    blend_mode: BlendMode,
    /// Every track reaching this zone, in track (compositing) order.
    instances: Vec<Instance>,
    /// Ping/pong at `mapping` for folding tracks. `None` when the zone is reached by
    /// at most one plain track, which always takes the shortcut.
    fold: Option<(wgpu::Texture, wgpu::Texture)>,
}

impl ZoneSlot {
    fn has_content(&self) -> bool {
        self.instances.iter().any(|i| i.effect.is_some())
    }
}

/// How [`ZoneCompositor::with_options`] builds the content.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompositorOptions {
    /// Fold every zone, even one whose only playing track is plain — disables the
    /// 1:1 shortcut. For tests that compare the two paths; never in the product.
    pub force_fold: bool,
}

/// Where `(track, k)` — track slot, k-th destination zone — lives.
#[derive(Clone, Copy)]
struct At {
    zone: usize,
    index: usize,
}

/// The per-zone compositor. Implements [`FrameContent`]; install it in place of
/// [`IdleContent`](crate::IdleContent) when a show runs.
pub struct ZoneCompositor {
    compositor: Compositor,
    placer: ZonePlacer,
    /// Bottom-to-top compositing order (`PatchIndex::composite_order`), once.
    order: Vec<u32>,
    slots: Vec<ZoneSlot>,
    /// Per track slot: where each of its instances lives, in destination order.
    tracks: Vec<Vec<At>>,
    force_fold: bool,
    /// This frame's per-cell overlap mask (world cells). Persistent; rewritten each
    /// frame from the snapshot with `write_buffer`.
    cell_mask: wgpu::Buffer,
    /// World-sized ping/pong scratch for the zone fold. Persistent.
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
    /// Master show-time at the previous frame, to take the delta the phases integrate.
    /// `None` before the first frame (delta 0).
    last_show_ns: Option<u64>,
}

/// Starting value for every effect instance's phase accumulator, in show-ns (`100` s). It exists to
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
    /// Build the compositor for a compiled layout and the show's tracks, in track
    /// (compositing) order. All GPU resources are allocated here — nothing below
    /// allocates GPU memory per frame.
    pub fn new(
        gpu: &Gpu,
        patch: &Patch,
        index: &PatchIndex,
        tracks: Vec<TrackContent>,
    ) -> ZoneCompositor {
        Self::with_options(gpu, patch, index, tracks, CompositorOptions::default())
    }

    /// [`new`](Self::new) with explicit [`CompositorOptions`].
    pub fn with_options(
        gpu: &Gpu,
        patch: &Patch,
        index: &PatchIndex,
        tracks: Vec<TrackContent>,
        options: CompositorOptions,
    ) -> ZoneCompositor {
        let world = (patch.matrix_size[0].max(1), patch.matrix_size[1].max(1));
        let zone_count = index.zone_count();

        // Distribute each track's instances to the zones it reaches, in track order.
        let mut per_zone: Vec<Vec<Instance>> = (0..zone_count).map(|_| Vec::new()).collect();
        let mut track_at: Vec<Vec<At>> = Vec::with_capacity(tracks.len());
        for track in tracks {
            let mut effects = track.effects.into_iter();
            let mut at = Vec::with_capacity(track.zones.len());
            for &zone in &track.zones {
                let effect = effects.next().flatten();
                if zone >= zone_count {
                    continue;
                }
                at.push(At {
                    zone,
                    index: per_zone[zone].len(),
                });
                per_zone[zone].push(Instance {
                    blend_mode: track.blend_mode,
                    opacity: track.opacity.clamp(0.0, 1.0),
                    effect,
                    tex: None,
                    phase_ns: PHASE_BIAS_NS,
                });
            }
            track_at.push(at);
        }

        let coverage_masks = zone_cell_coverage(patch, index);
        let slots: Vec<ZoneSlot> = per_zone
            .into_iter()
            .enumerate()
            .map(|(slot, mut instances)| {
                let zone = &patch.zones[slot];
                let mapping = (zone.mapping_size[0].max(1), zone.mapping_size[1].max(1));
                let footprint = footprint_from_coverage(&coverage_masks[slot], world.0);
                // A zone folds only if it ever could need to: several tracks, a
                // non-plain one, or a forced fold. Otherwise no extra textures at all.
                let can_fold = options.force_fold
                    || instances.len() > 1
                    || instances.iter().any(|i| !i.is_plain());
                let fold = can_fold.then(|| {
                    for inst in &mut instances {
                        inst.tex = Some(create_render_target(gpu, mapping.0, mapping.1));
                    }
                    (
                        create_render_target(gpu, mapping.0, mapping.1),
                        create_render_target(gpu, mapping.0, mapping.1),
                    )
                });
                ZoneSlot {
                    mapping,
                    footprint,
                    zone_tex: create_render_target(gpu, mapping.0, mapping.1),
                    placed_tex: create_render_target(gpu, world.0, world.1),
                    coverage: create_cell_mask(gpu, &coverage_masks[slot]),
                    blend_mode: zone.blend_mode,
                    instances,
                    fold,
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
            tracks: track_at,
            force_fold: options.force_fold,
            cell_mask,
            ping: create_render_target(gpu, world.0, world.1),
            pong: create_render_target(gpu, world.0, world.1),
            contrib: Vec::new(),
            commands_rx,
            commands_tx,
            last_show_ns: None,
        }
    }

    /// A producer for the frame-boundary command queue (M8-PARTA §3). Hand these to
    /// the UI/transport threads; their calls take effect at the next frame boundary,
    /// never mid-frame.
    pub fn command_sender(&self) -> ZoneCommandSender {
        ZoneCommandSender {
            tx: self.commands_tx.clone(),
        }
    }

    /// Replace a track's running effects directly — the same swap the queue performs,
    /// for callers already holding `&mut`. Textures are unchanged.
    pub fn set_track_effects(&mut self, track: usize, effects: Vec<Option<EffectInstance>>) {
        let Some(at) = self.tracks.get(track) else {
            return;
        };
        let mut effects = effects.into_iter();
        for a in at {
            self.slots[a.zone].instances[a.index].effect = effects.next().flatten();
        }
    }

    /// Apply every queued command. Called at the **top** of `render_into`, so all
    /// structural changes land at the frame boundary and the frame is then rendered
    /// wholly under the post-swap state — never a half-and-half frame. Non-blocking
    /// (`try_recv`); the render thread never waits on a producer.
    fn drain_commands(&mut self, gpu: &Gpu) {
        while let Ok(cmd) = self.commands_rx.try_recv() {
            match cmd {
                ZoneCommand::SetTrackEffects { track, effects } => {
                    self.set_track_effects(track, effects);
                }
                ZoneCommand::Restart { track } => {
                    for a in self.tracks.get(track).into_iter().flatten() {
                        self.slots[a.zone].instances[a.index].phase_ns = PHASE_BIAS_NS;
                    }
                }
                ZoneCommand::SetPalette { track, colors } => {
                    for a in self.tracks.get(track).into_iter().flatten() {
                        if let Some(effect) = &self.slots[a.zone].instances[a.index].effect {
                            effect.set_palette(gpu, &colors);
                        }
                    }
                }
            }
        }
    }

    /// Render zone `s`'s tracks into its `zone_tex`: straight in when one plain track
    /// plays (the shortcut), otherwise folded over black in track order.
    fn render_zone(&mut self, gpu: &Gpu, clock: &ClockSnapshot, s: usize) {
        let compositor = &self.compositor;
        let force_fold = self.force_fold;
        let ZoneSlot {
            mapping,
            zone_tex,
            instances,
            fold,
            ..
        } = &mut self.slots[s];
        let (w, h) = *mapping;
        let playing = instances.iter().filter(|i| i.effect.is_some()).count();
        let Some(last) = instances.iter().rposition(|i| i.effect.is_some()) else {
            return;
        };

        let shortcut = playing == 1 && instances[last].is_plain() && !force_fold;
        let Some((a, b)) = fold.as_ref().filter(|_| !shortcut) else {
            // No fold textures ⇒ at most one plain track reaches this zone.
            let inst = &mut instances[last];
            let zclock = phase_clock(clock, inst.phase_ns);
            if let Some(effect) = inst.effect.as_mut() {
                effect.render(gpu, &zclock, &view(zone_tex), w, h);
            }
            return;
        };

        clear_black(gpu, a);
        let mut acc: &wgpu::Texture = a;
        let mut other: &wgpu::Texture = b;
        for (k, inst) in instances.iter_mut().enumerate() {
            let zclock = phase_clock(clock, inst.phase_ns);
            let (Some(effect), Some(tex)) = (inst.effect.as_mut(), inst.tex.as_ref()) else {
                continue;
            };
            effect.render(gpu, &zclock, &view(tex), w, h);
            if k == last {
                compositor.blend(
                    gpu,
                    acc,
                    tex,
                    &view(zone_tex),
                    inst.blend_mode,
                    inst.opacity,
                );
            } else {
                compositor.blend(gpu, acc, tex, &view(other), inst.blend_mode, inst.opacity);
                std::mem::swap(&mut acc, &mut other);
            }
        }
    }
}

/// A per-instance view of the clock whose **show-time** is its integrated phase —
/// never the master clock (guardrail, M8-PARTA §3). `frame`/`beat`/`bpm` are left as-is;
/// the built-in effects animate off `show_seconds`. The phase is advanced per frame by
/// [`ZoneCompositor`] so a signed speed can run it forward or in reverse.
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
        self.drain_commands(gpu);

        // 0b. Advance every instance's animation phase by its zone's signed speed over
        //     the master's show-time delta. Integrating (rather than scaling absolute
        //     time) is what lets a negative speed play in REVERSE; a zero speed holds
        //     the phase (static). Done for all instances, contributing or not, so a
        //     zone that comes back on resumes where its animation would have been.
        //     Frozen master clock ⇒ zero delta ⇒ everything freezes.
        let delta_ns = match self.last_show_ns {
            Some(prev) => clock.show_ns.saturating_sub(prev) as f64,
            None => 0.0,
        };
        self.last_show_ns = Some(clock.show_ns);
        for (s, slot) in self.slots.iter_mut().enumerate() {
            let speed = zones.get(s).speed_multiplier as f64;
            for inst in &mut slot.instances {
                inst.phase_ns = (inst.phase_ns + delta_ns * speed).max(0.0);
            }
        }

        // 1. Which zones actually contribute this frame, in compositing order. A
        //    zone contributes iff it has live state (contributes_pixels), at least one
        //    playing track, and a non-empty footprint. Reuses `contrib` in place.
        self.contrib.clear();
        for &slot in &self.order {
            let s = slot as usize;
            let slot_ref = &self.slots[s];
            if zones.contributes_pixels(s)
                && slot_ref.has_content()
                && !slot_ref.footprint.is_empty()
            {
                self.contrib.push(slot);
            }
        }

        // 2. Render each contributing zone's tracks at its OWN mapping_size (folded in
        //    track order), then PLACE the result into the world framebuffer.
        for i in 0..self.contrib.len() {
            let s = self.contrib[i] as usize;
            self.render_zone(gpu, clock, s);
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

        // 4. Fold the placed zones into `target` with the UNCHANGED blend machinery
        //    (`blend_zone`), ping-ponging over two persistent scratch textures. Zone
        //    dimmer/colour is applied here only on cells the snapshot marks overlapped
        //    (via cell_mask); elsewhere it is deferred to the output stage (§5.2.1).
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

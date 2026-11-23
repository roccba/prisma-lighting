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
//! # Live track controls, transitions (LIVE-MODEL-SPEC §4.2, §5)
//!
//! Each track's opacity, mute and solo are Parameter Tree values read wait-free at the
//! top of every frame. A clip change can crossfade on clock time (seconds or beats):
//! the instance keeps the outgoing effect, renders both and mixes them by the eased
//! progress, and drops the outgoing one when the fade completes. A launch during a fade
//! fades from exactly what is on screen: the running mix keeps fading underneath (nested,
//! depth cap 2 — deeper, the oldest mix is frozen to a texture). A stop fades to black; a
//! stop during that fade cuts. [`ZoneCommand::Batch`] lands several changes
//! in the same frame (scene launch, GO).
//!
//! # The 1:1 shortcut
//!
//! A zone whose only playing track is `Normal` at full opacity this frame, with no
//! crossfade running, renders that track **straight into the zone texture**, with no
//! fold: blending one `Normal` layer at opacity 1 over black is the identity. The
//! `m8_zone_render` suite asserts the shortcut and the fold produce the same frame.
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

use prisma_core::show::{Show, Source, Track, TrackTransition};
use prisma_core::source::{priority, SourceId};
use prisma_core::tree::ParameterTree;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, Easing, OverlapSnapshot, ParamHandle, TimeBase, Transition};
use prisma_patch::{BlendMode, Patch, PatchIndex};
use prisma_render::blend::Compositor;
use prisma_render::effect::{EffectInstance, EffectParams};
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
/// shares one set of params across its per-zone instances ([`EffectParams`],
/// LIVE-MODEL-SPEC §8.4), so the panel, a knob or MIDI move every zone at once.
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
    /// `[threshold, softness]` for `BlendMode::LumaKey`.
    pub key: [f32; 2],
    /// `0.0..=1.0`. Used when there is no [`live`](Self::live) binding.
    pub opacity: f32,
    /// The track's live controls in the Parameter Tree, read every frame. `None` =
    /// fixed at `opacity`, never muted or soloed (tests).
    pub live: Option<TrackLive>,
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
                key: prisma_render::blend::DEFAULT_KEY,
                opacity: 1.0,
                live: None,
                effects: vec![effect],
            })
            .collect()
    }
}

/// Build one track's effect instances — one per destination zone — on the caller's
/// thread (a pipeline compile, never on the render thread): register the params
/// **once** under [`track_param_prefix`] and build every zone's instance on them (one
/// write moves them all; each zone keeps its own animation phase, so its VEL still
/// applies), write the stored values over the manifest defaults, upload the colour
/// table. `None` if the effect is not in the catalogue.
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
    let shared = EffectParams::register(tree, &prefix, &manifest);
    let effects: Vec<Option<EffectInstance>> = (0..zones)
        .map(|_| {
            let inst = EffectInstance::with_params(gpu, &shared, &manifest);
            if let Some(colors) = palette {
                inst.set_palette(gpu, colors);
            }
            Some(inst)
        })
        .collect();
    // After registering, so the values land on the live handles.
    for (name, value) in params.iter() {
        tree.write_path(&format!("{prefix}/param/{name}"), value, src);
    }
    Some(effects)
}

/// Build every track's running content from a Show, in track order (= compositing
/// order, first at the bottom), ready for [`ZoneCompositor::new`]. `live[t]` binds
/// track `t`'s live controls (missing = fixed). A track with no active clip, an empty
/// clip or an unknown `effect_id` plays nothing. The GPU work (pipeline compile)
/// happens here, at load — not per frame.
pub fn build_track_effects(
    gpu: &Gpu,
    tree: &ParameterTree,
    patch: &Patch,
    show: &Show,
    live: &[TrackLive],
) -> Vec<TrackContent> {
    let src = show_load_source();
    show.tracks
        .iter()
        .enumerate()
        .map(|(t, track)| {
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
                key: [track.key_threshold, track.key_softness],
                opacity: track.opacity,
                live: live.get(t).cloned(),
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
        tree.write_real(&h.white.auto, if l.white_auto { 1.0 } else { 0.0 }, src);
        tree.write_real(&h.white.kelvin, l.white_kelvin, src);
    }
}

/// How a content change lands (LIVE-MODEL-SPEC §4.2): a hard cut, or a crossfade on
/// clock time — show seconds or beats — with an easing curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub time: f64,
    pub base: TimeBase,
    pub easing: Easing,
}

impl Fade {
    /// A hard cut.
    pub const CUT: Fade = Fade {
        time: 0.0,
        base: TimeBase::Seconds,
        easing: Easing::Linear,
    };

    /// A fade of `seconds` show-seconds.
    pub fn seconds(seconds: f64, easing: Easing) -> Fade {
        Fade {
            time: seconds,
            base: TimeBase::Seconds,
            easing,
        }
    }

    /// A track's default transition.
    pub fn of_track(t: &TrackTransition) -> Fade {
        Fade {
            time: t.time,
            base: t.base,
            easing: t.easing,
        }
    }

    /// The fade into a clip: the clip's own `fade_time` when non-zero (in seconds,
    /// with the track's easing), else the track's transition (LIVE-MODEL-SPEC §3).
    pub fn into_clip(track: &TrackTransition, clip_fade: std::time::Duration) -> Fade {
        if clip_fade.is_zero() {
            Fade::of_track(track)
        } else {
            Fade::seconds(clip_fade.as_secs_f64(), track.easing)
        }
    }

    pub fn is_cut(&self) -> bool {
        self.time <= 0.0
    }

    fn start(&self, clock: &ClockSnapshot) -> Transition {
        match self.base {
            TimeBase::Seconds => Transition::seconds(clock, self.time, self.easing),
            TimeBase::Beats => Transition::beats(clock, self.time, self.easing),
        }
    }
}

/// A track's live controls, resolved once and read wait-free every frame.
#[derive(Clone)]
pub struct TrackLive {
    /// Unit level, `0..=1`.
    pub opacity: ParamHandle,
    pub mute: ParamHandle,
    pub solo: ParamHandle,
}

/// A structural change to the running content, applied at the **frame boundary**
/// (M8-PARTA §3, locked decision 3). The expensive part — building an
/// [`EffectInstance`] (which compiles a GPU pipeline) — happens **off** the render
/// thread on the sender's side; the command carries the built instances and the
/// render loop's only work is the pointer swap. A command sent mid-frame is applied
/// at the **top** of the next `render_into`, so frame N renders wholly with the old
/// effect and frame N+1 wholly with the new one — never a half-and-half frame.
pub enum ZoneCommand {
    /// Play new effects on a track — `effects[k]` in its k-th destination zone —
    /// cutting or crossfading from what plays now. Missing entries play nothing there.
    SetTrackEffects {
        track: usize,
        effects: Vec<Option<EffectInstance>>,
        fade: Fade,
    },
    /// Stop a track, fading what plays to black. A stop while the track is already
    /// fading out (or silent) cuts at once — the "second stop" of LIVE-MODEL-SPEC.
    StopTrack { track: usize, fade: Fade },
    /// Rewind a track's animation phase (in every zone it reaches) to its start, so its
    /// effect plays from the beginning (a pad in trigger mode, M9).
    Restart { track: usize },
    /// Replace the colour table of a track's running effect (the Tabla de color edited
    /// live). Built by the sender; the render thread only uploads it at the boundary.
    SetPalette { track: usize, colors: Vec<[f32; 3]> },
    /// Change how a track blends in every zone it reaches (the track popover). Live:
    /// no recompile, nothing reallocated.
    SetBlend {
        track: usize,
        mode: BlendMode,
        /// `[threshold, softness]` for `BlendMode::LumaKey`.
        key: [f32; 2],
    },
    /// Change the order tracks fold in (a header dragged on the grid, LIVE-MODEL-SPEC
    /// §10.2): `rank[track]` is the track's new position. Only each zone's fold order
    /// changes — no instance is rebuilt, so phases, fades and textures carry on.
    Reorder { rank: Vec<usize> },
    /// Several commands applied at the SAME frame boundary, in order — what makes a
    /// scene launch or GO land on every track in one frame.
    Batch(Vec<ZoneCommand>),
}

/// The producer end of the frame-boundary command queue. Cloneable and `Send`, so
/// the UI/transport threads can drive content changes without touching the render
/// thread directly. Obtain one from [`ZoneCompositor::command_sender`].
#[derive(Clone)]
pub struct ZoneCommandSender {
    tx: mpsc::Sender<ZoneCommand>,
}

impl ZoneCommandSender {
    /// Queue a command. Returns `false` if the compositor has been dropped (the render
    /// loop is gone).
    pub fn send(&self, command: ZoneCommand) -> bool {
        self.tx.send(command).is_ok()
    }

    /// Queue a hard-cut effect swap for `track`, applied at the next frame boundary.
    pub fn set_track_effects(&self, track: usize, effects: Vec<Option<EffectInstance>>) -> bool {
        self.play_track(track, effects, Fade::CUT)
    }

    /// Queue an effect change for `track` that lands with `fade`.
    pub fn play_track(
        &self,
        track: usize,
        effects: Vec<Option<EffectInstance>>,
        fade: Fade,
    ) -> bool {
        self.send(ZoneCommand::SetTrackEffects {
            track,
            effects,
            fade,
        })
    }

    /// Queue stopping `track` at once (nothing playing in any zone it reaches).
    pub fn clear_track(&self, track: usize) -> bool {
        self.stop_track(track, Fade::CUT)
    }

    /// Queue stopping `track` with `fade` (a stop during its fade-out cuts).
    pub fn stop_track(&self, track: usize, fade: Fade) -> bool {
        self.send(ZoneCommand::StopTrack { track, fade })
    }

    /// Queue a phase rewind for `track` (applied after any swap queued before it).
    pub fn restart_track(&self, track: usize) -> bool {
        self.send(ZoneCommand::Restart { track })
    }

    /// Fold the tracks in a new order (`rank[track]` = new position), at the next frame
    /// boundary, without rebuilding anything.
    pub fn reorder(&self, rank: Vec<usize>) -> bool {
        self.send(ZoneCommand::Reorder { rank })
    }

    /// Queue a new colour table for `track`'s running effect (applied after any swap
    /// queued before it, so it lands on the effect that will actually be running).
    pub fn set_track_palette(&self, track: usize, colors: Vec<[f32; 3]>) -> bool {
        self.send(ZoneCommand::SetPalette { track, colors })
    }
}

/// A source under a crossfade: a running effect with its own animation phase, or the
/// instance's frozen snapshot (see [`Below`]).
enum Src {
    Effect {
        effect: EffectInstance,
        phase_ns: f64,
    },
    Frozen,
}

impl Src {
    fn advance(&mut self, step: f64) {
        if let Src::Effect { phase_ns, .. } = self {
            *phase_ns = (*phase_ns + step).max(0.0);
        }
    }
}

/// What the playing effect crossfades over (LIVE-MODEL-SPEC §4.2) — exactly what was on
/// screen when the fade started, so a launch never jumps:
/// - `Silence` — nothing (a fade in from black);
/// - `One` — a single source (the common A → B fade);
/// - `Mix` — a launch landed while a fade was running: the whole visible mix keeps fading
///   underneath, still animated (a nested fade). **Depth cap 2:** a launch while a `Mix` is
///   already below freezes that `Mix` into the frozen texture (one GPU copy of its last
///   frame) instead of nesting deeper, so the cost has a ceiling.
enum Below {
    Silence,
    One(Src),
    Mix {
        out: Option<Src>,
        inn: Option<Src>,
        fade: Transition,
    },
}

impl Below {
    fn is_silent(&self) -> bool {
        matches!(self, Below::Silence)
    }

    fn advance(&mut self, step: f64) {
        match self {
            Below::Silence => {}
            Below::One(s) => s.advance(step),
            Below::Mix { out, inn, .. } => {
                for s in [out, inn].into_iter().flatten() {
                    s.advance(step);
                }
            }
        }
    }
}

/// An instance's render targets at its zone's `mapping_size`, all allocated once.
struct Targets {
    /// The playing effect.
    tex: wgpu::Texture,
    /// The (inner) outgoing source.
    tex_out: wgpu::Texture,
    /// The inner incoming source of a nested fade.
    tex_in: wgpu::Texture,
    /// The rendered `Below` of a nested fade.
    tex_below: wgpu::Texture,
    /// The instance's result during a fade.
    tex_mix: wgpu::Texture,
    /// The frozen snapshot a depth-capped fade fades from.
    tex_frozen: wgpu::Texture,
}

impl Targets {
    fn new(gpu: &Gpu, w: u32, h: u32) -> Targets {
        Targets {
            tex: create_render_target(gpu, w, h),
            tex_out: create_render_target(gpu, w, h),
            tex_in: create_render_target(gpu, w, h),
            tex_below: create_render_target(gpu, w, h),
            tex_mix: create_render_target(gpu, w, h),
            tex_frozen: create_render_target(gpu, w, h),
        }
    }
}

/// One (track, zone) effect instance, held by its zone in track order.
struct Instance {
    /// Its track slot (for the track's per-frame opacity / mute / solo).
    track: usize,
    blend_mode: BlendMode,
    /// The luminance key `[threshold, softness]`, read by `BlendMode::LumaKey`.
    key: [f32; 2],
    /// What plays now (the incoming side of a crossfade).
    effect: Option<EffectInstance>,
    /// Animation phase of `effect` in show-ns, integrated from the zone's signed speed.
    phase_ns: f64,
    /// What `effect` fades over, while `fade` runs.
    below: Below,
    /// The running crossfade, if any (eased progress = weight of `effect`).
    fade: Option<Transition>,
    /// `tex_below` holds the current `Below::Mix`'s last rendered frame (false between a
    /// nesting launch and the next render, e.g. two launches landing in one frame).
    below_rendered: bool,
    targets: Targets,
}

impl Instance {
    /// Whether this instance puts anything into its zone (playing or fading out).
    fn has_content(&self) -> bool {
        self.effect.is_some() || (self.fade.is_some() && !self.below.is_silent())
    }

    /// Advance every running source's phase by `step` show-ns.
    fn advance(&mut self, step: f64) {
        self.phase_ns = (self.phase_ns + step).max(0.0);
        self.below.advance(step);
    }

    /// Swap in new content at the frame boundary. With a fade, it starts from exactly
    /// what is on screen: if a fade was already running, that whole mix fades out
    /// underneath (nested); at the depth cap, the older mix is frozen first (`gpu` does the
    /// copy — without one, a depth-capped mix is dropped instead, which only the
    /// GPU-less direct path could hit and it only ever cuts).
    fn play(
        &mut self,
        effect: Option<EffectInstance>,
        fade: Fade,
        clock: &ClockSnapshot,
        gpu: Option<&Gpu>,
    ) {
        let stopping = effect.is_none();
        if fade.is_cut() || (stopping && self.effect.is_none()) {
            // A cut — or a stop while already silent / fading out: end it now.
            self.below = Below::Silence;
            self.fade = None;
            self.effect = effect;
            return;
        }
        let current = self.effect.take().map(|effect| Src::Effect {
            effect,
            phase_ns: self.phase_ns,
        });
        self.below = match self.fade.take() {
            // Nothing fading: the new clip fades over what plays now.
            None => current.map_or(Below::Silence, Below::One),
            // A fade is running: what shows is mix(below, current, outer) — keep it.
            Some(outer) => {
                let inner_out = match std::mem::replace(&mut self.below, Below::Silence) {
                    Below::Silence => None,
                    Below::One(src) => Some(src),
                    Below::Mix { inn, .. } => match gpu {
                        // Depth cap: freeze the older mix's last frame.
                        Some(gpu) if self.below_rendered => {
                            copy_texture(gpu, &self.targets.tex_below, &self.targets.tex_frozen);
                            Some(Src::Frozen)
                        }
                        // Never drawn (two launches in one frame) or no GPU: keep its
                        // incoming side, the newest thing it showed.
                        _ => inn,
                    },
                };
                self.below_rendered = false;
                Below::Mix {
                    out: inner_out,
                    inn: current,
                    fade: outer,
                }
            }
        };
        self.effect = effect;
        self.fade = Some(fade.start(clock));
    }

    /// Render this instance into its own textures and return the one holding the
    /// result. `None` if it shows nothing.
    fn render(
        &mut self,
        gpu: &Gpu,
        compositor: &Compositor,
        clock: &ClockSnapshot,
        (w, h): (u32, u32),
    ) -> Option<&wgpu::Texture> {
        // Retire finished fades (their sources are dropped here; wgpu frees the memory).
        if self.fade.is_some_and(|f| f.is_complete(clock)) {
            self.fade = None;
            self.below = Below::Silence;
        }
        if let Below::Mix { inn, fade, .. } = &mut self.below {
            if fade.is_complete(clock) {
                self.below = inn.take().map_or(Below::Silence, Below::One);
            }
        }
        let t = &self.targets;
        let Some(outer) = self.fade else {
            let effect = self.effect.as_mut()?;
            effect.render(gpu, &phase_clock(clock, self.phase_ns), &view(&t.tex), w, h);
            return Some(&t.tex);
        };
        let below: &wgpu::Texture = match &mut self.below {
            Below::Silence => {
                clear_transparent(gpu, &t.tex_out);
                &t.tex_out
            }
            Below::One(src) => render_src(gpu, clock, Some(src), &t.tex_out, t, (w, h)),
            Below::Mix { out, inn, fade } => {
                let a = render_src(gpu, clock, out.as_mut(), &t.tex_out, t, (w, h));
                let b = render_src(gpu, clock, inn.as_mut(), &t.tex_in, t, (w, h));
                let p = fade.eased(clock);
                compositor.crossfade(gpu, a, b, &view(&t.tex_below), p);
                self.below_rendered = true;
                &t.tex_below
            }
        };
        match self.effect.as_mut() {
            Some(effect) => {
                let c = phase_clock(clock, self.phase_ns);
                effect.render(gpu, &c, &view(&t.tex), w, h);
            }
            None => clear_transparent(gpu, &t.tex),
        }
        // A crossfade, not alpha-over: transparent areas of the incoming clip still
        // fade the outgoing one out.
        compositor.crossfade(gpu, below, &t.tex, &view(&t.tex_mix), outer.eased(clock));
        Some(&t.tex_mix)
    }
}

/// Render one source and return the texture holding it: an effect into `target`, the
/// frozen snapshot as-is, nothing as black in `target`.
fn render_src<'t>(
    gpu: &Gpu,
    clock: &ClockSnapshot,
    src: Option<&mut Src>,
    target: &'t wgpu::Texture,
    targets: &'t Targets,
    (w, h): (u32, u32),
) -> &'t wgpu::Texture {
    match src {
        Some(Src::Effect { effect, phase_ns }) => {
            effect.render(gpu, &phase_clock(clock, *phase_ns), &view(target), w, h);
            target
        }
        Some(Src::Frozen) => &targets.tex_frozen,
        None => {
            clear_transparent(gpu, target);
            target
        }
    }
}

/// GPU copy of one render target into another of the same size (no CPU round trip).
fn copy_texture(gpu: &Gpu, from: &wgpu::Texture, to: &wgpu::Texture) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fade-freeze"),
        });
    encoder.copy_texture_to_texture(from.as_image_copy(), to.as_image_copy(), from.size());
    gpu.queue.submit([encoder.finish()]);
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
    /// Ping/pong at `mapping` for folding tracks.
    fold: (wgpu::Texture, wgpu::Texture),
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

/// A track as the compositor tracks it: where its instances live and its controls.
struct TrackSlot {
    at: Vec<At>,
    live: Option<TrackLive>,
    /// Fixed opacity when there is no live binding.
    opacity: f32,
}

/// A track's controls as read at the top of a frame.
#[derive(Clone, Copy)]
struct TrackFrame {
    opacity: f32,
    /// How present the track is: 1 = audible, 0 = muted or silenced by another
    /// track's solo. Multiplies the opacity; a track at 0 is skipped. Mute and solo
    /// are instant today, so this is 0 or 1 — the seam a mute/solo fade would ramp.
    presence: f32,
}

impl TrackFrame {
    fn audible(&self) -> bool {
        self.presence > 0.0
    }
}

/// The per-zone compositor. Implements [`FrameContent`]; install it in place of
/// [`IdleContent`](crate::IdleContent) when a show runs.
pub struct ZoneCompositor {
    compositor: Compositor,
    placer: ZonePlacer,
    /// Bottom-to-top compositing order (`PatchIndex::composite_order`), once.
    order: Vec<u32>,
    slots: Vec<ZoneSlot>,
    tracks: Vec<TrackSlot>,
    /// This frame's track controls, one per track. Rewritten in place each frame.
    track_frame: Vec<TrackFrame>,
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
        let mapping_of = |slot: usize| {
            let z = &patch.zones[slot];
            (z.mapping_size[0].max(1), z.mapping_size[1].max(1))
        };

        // Distribute each track's instances to the zones it reaches, in track order.
        let mut per_zone: Vec<Vec<Instance>> = (0..zone_count).map(|_| Vec::new()).collect();
        let mut track_slots: Vec<TrackSlot> = Vec::with_capacity(tracks.len());
        for (t, track) in tracks.into_iter().enumerate() {
            let mut effects = track.effects.into_iter();
            let mut at = Vec::with_capacity(track.zones.len());
            for &zone in &track.zones {
                let effect = effects.next().flatten();
                if zone >= zone_count {
                    continue;
                }
                let (w, h) = mapping_of(zone);
                at.push(At {
                    zone,
                    index: per_zone[zone].len(),
                });
                per_zone[zone].push(Instance {
                    track: t,
                    blend_mode: track.blend_mode,
                    key: track.key,
                    effect,
                    phase_ns: PHASE_BIAS_NS,
                    below: Below::Silence,
                    fade: None,
                    below_rendered: false,
                    targets: Targets::new(gpu, w, h),
                });
            }
            track_slots.push(TrackSlot {
                at,
                live: track.live,
                opacity: track.opacity.clamp(0.0, 1.0),
            });
        }

        let coverage_masks = zone_cell_coverage(patch, index);
        let slots: Vec<ZoneSlot> = per_zone
            .into_iter()
            .enumerate()
            .map(|(slot, instances)| {
                let zone = &patch.zones[slot];
                let mapping = mapping_of(slot);
                let footprint = footprint_from_coverage(&coverage_masks[slot], world.0);
                ZoneSlot {
                    mapping,
                    footprint,
                    zone_tex: create_render_target(gpu, mapping.0, mapping.1),
                    placed_tex: create_render_target(gpu, world.0, world.1),
                    coverage: create_cell_mask(gpu, &coverage_masks[slot]),
                    blend_mode: zone.blend_mode,
                    instances,
                    fold: (
                        create_render_target(gpu, mapping.0, mapping.1),
                        create_render_target(gpu, mapping.0, mapping.1),
                    ),
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
        let track_frame = vec![
            TrackFrame {
                opacity: 1.0,
                presence: 1.0,
            };
            track_slots.len()
        ];
        ZoneCompositor {
            compositor: Compositor::new(gpu),
            placer: ZonePlacer::new(gpu),
            order: index.composite_order().to_vec(),
            slots,
            tracks: track_slots,
            track_frame,
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

    /// Replace a track's running effects directly with a hard cut — the same swap the
    /// queue performs, for callers already holding `&mut`. Textures are unchanged.
    pub fn set_track_effects(&mut self, track: usize, effects: Vec<Option<EffectInstance>>) {
        self.apply(
            ZoneCommand::SetTrackEffects {
                track,
                effects,
                fade: Fade::CUT,
            },
            None,
            // A cut never reads the clock.
            &ClockSnapshot {
                frame: 0,
                wall_ns: 0,
                show_ns: 0,
                beat: 0.0,
                bpm: 120.0,
                playing: false,
            },
        );
    }

    /// Apply every queued command. Called at the **top** of `render_into`, so all
    /// structural changes land at the frame boundary and the frame is then rendered
    /// wholly under the post-swap state — never a half-and-half frame. A fade starts
    /// at this frame's clock. Non-blocking (`try_recv`); the render thread never waits
    /// on a producer.
    fn drain_commands(&mut self, gpu: &Gpu, clock: &ClockSnapshot) {
        while let Ok(cmd) = self.commands_rx.try_recv() {
            self.apply(cmd, Some(gpu), clock);
        }
    }

    /// Apply one command. A palette upload needs the GPU; without one it is skipped.
    fn apply(&mut self, cmd: ZoneCommand, gpu: Option<&Gpu>, clock: &ClockSnapshot) {
        // `At` is Copy: index the track's instances without borrowing `self.tracks`
        // across the mutation (and without allocating on the render thread).
        let count = |tracks: &[TrackSlot], t: usize| tracks.get(t).map_or(0, |t| t.at.len());
        match cmd {
            ZoneCommand::SetTrackEffects {
                track,
                effects,
                fade,
            } => {
                let mut effects = effects.into_iter();
                for k in 0..count(&self.tracks, track) {
                    let a = self.tracks[track].at[k];
                    let effect = effects.next().flatten();
                    self.slots[a.zone].instances[a.index].play(effect, fade, clock, gpu);
                }
            }
            ZoneCommand::StopTrack { track, fade } => {
                for k in 0..count(&self.tracks, track) {
                    let a = self.tracks[track].at[k];
                    self.slots[a.zone].instances[a.index].play(None, fade, clock, gpu);
                }
            }
            ZoneCommand::Restart { track } => {
                for k in 0..count(&self.tracks, track) {
                    let a = self.tracks[track].at[k];
                    self.slots[a.zone].instances[a.index].phase_ns = PHASE_BIAS_NS;
                }
            }
            ZoneCommand::SetPalette { track, colors } => {
                let Some(gpu) = gpu else { return };
                for k in 0..count(&self.tracks, track) {
                    let a = self.tracks[track].at[k];
                    if let Some(effect) = &self.slots[a.zone].instances[a.index].effect {
                        effect.set_palette(gpu, &colors);
                    }
                }
            }
            ZoneCommand::SetBlend { track, mode, key } => {
                for k in 0..count(&self.tracks, track) {
                    let a = self.tracks[track].at[k];
                    let inst = &mut self.slots[a.zone].instances[a.index];
                    inst.blend_mode = mode;
                    inst.key = key;
                }
            }
            ZoneCommand::Reorder { rank } => self.reorder(&rank),
            ZoneCommand::Batch(cmds) => {
                for c in cmds {
                    self.apply(c, gpu, clock);
                }
            }
        }
    }

    /// Fold every zone's instances in the order `rank` gives their tracks (`rank[track]`
    /// = new position; a track missing from `rank` keeps its place after the ranked ones)
    /// and point each track at its instances' new indices. In place: an unstable sort of
    /// each zone's instances (one per track, so the keys are distinct) and no allocation.
    fn reorder(&mut self, rank: &[usize]) {
        let key = |track: usize| rank.get(track).copied().unwrap_or(rank.len() + track);
        for (zone, slot) in self.slots.iter_mut().enumerate() {
            slot.instances.sort_unstable_by_key(|inst| key(inst.track));
            for (index, inst) in slot.instances.iter().enumerate() {
                if let Some(at) = self
                    .tracks
                    .get_mut(inst.track)
                    .and_then(|t| t.at.iter_mut().find(|a| a.zone == zone))
                {
                    at.index = index;
                }
            }
        }
    }

    /// Read every track's live controls once, at the top of the frame (wait-free).
    fn read_track_controls(&mut self) {
        let any_solo = self.tracks.iter().any(|t| {
            t.live
                .as_ref()
                .and_then(|l| l.solo.read_bool())
                .unwrap_or(false)
        });
        for (t, frame) in self.tracks.iter().zip(self.track_frame.iter_mut()) {
            *frame = match &t.live {
                None => TrackFrame {
                    opacity: t.opacity,
                    presence: if any_solo { 0.0 } else { 1.0 },
                },
                Some(l) => {
                    let mute = l.mute.read_bool().unwrap_or(false);
                    let solo = l.solo.read_bool().unwrap_or(false);
                    TrackFrame {
                        opacity: l.opacity.read_real().unwrap_or(t.opacity).clamp(0.0, 1.0),
                        presence: if !mute && (!any_solo || solo) {
                            1.0
                        } else {
                            0.0
                        },
                    }
                }
            };
        }
    }

    /// Whether zone `s` shows anything this frame (an audible track with content).
    fn zone_has_content(&self, s: usize) -> bool {
        self.slots[s]
            .instances
            .iter()
            .any(|i| i.has_content() && self.track_frame[i.track].audible())
    }

    /// Render zone `s`'s tracks into its `zone_tex`: straight in when one plain track
    /// plays with no crossfade (the shortcut), otherwise folded over black in track
    /// order with each track's blend mode and this frame's opacity.
    fn render_zone(&mut self, gpu: &Gpu, clock: &ClockSnapshot, s: usize) {
        let compositor = &self.compositor;
        let frames = &self.track_frame;
        let force_fold = self.force_fold;
        let ZoneSlot {
            mapping,
            zone_tex,
            instances,
            fold: (a, b),
            ..
        } = &mut self.slots[s];
        let shows = |i: &Instance| i.has_content() && frames[i.track].audible();
        let playing = instances.iter().filter(|i| shows(i)).count();
        let Some(last) = instances.iter().rposition(shows) else {
            return;
        };

        let only = &instances[last];
        let f = frames[only.track];
        let plain = only.blend_mode == BlendMode::Normal && f.opacity * f.presence >= 1.0;
        if playing == 1 && plain && only.fade.is_none() && !force_fold {
            let inst = &mut instances[last];
            if let Some(effect) = inst.effect.as_mut() {
                let c = phase_clock(clock, inst.phase_ns);
                effect.render(gpu, &c, &view(zone_tex), mapping.0, mapping.1);
            }
            return;
        }

        clear_transparent(gpu, a);
        let mut acc: &wgpu::Texture = a;
        let mut other: &wgpu::Texture = b;
        for (k, inst) in instances.iter_mut().enumerate() {
            if !(inst.has_content() && frames[inst.track].audible()) {
                continue;
            }
            let f = frames[inst.track];
            let (mode, opacity, key) = (inst.blend_mode, f.opacity * f.presence, inst.key);
            let Some(tex) = inst.render(gpu, compositor, clock, *mapping) else {
                continue;
            };
            if k == last {
                compositor.blend_keyed(gpu, acc, tex, &view(zone_tex), mode, opacity, key);
            } else {
                compositor.blend_keyed(gpu, acc, tex, &view(other), mode, opacity, key);
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
/// A zone's opacity on overlapped cells: its dimmer × its presence (LIVE-MODEL-SPEC
/// 2b). Presence is 1 for every zone that is composited at all — a muted or
/// solo-silenced zone is not composited (`contributes_pixels`) — so it is the seam a
/// mute/solo fade would ramp; there are no such fades yet.
fn zone_opacity(zones: &ZoneStates, slot: usize) -> f32 {
    let presence = 1.0;
    zones.get(slot).dimmer() * presence
}

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
        self.drain_commands(gpu, clock);
        self.read_track_controls();

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
            let step = delta_ns * zones.get(s).speed_multiplier as f64;
            for inst in &mut slot.instances {
                inst.advance(step);
            }
        }

        // 1. Which zones actually contribute this frame, in compositing order. A
        //    zone contributes iff it has live state (contributes_pixels), at least one
        //    playing track, and a non-empty footprint. Reuses `contrib` in place.
        self.contrib.clear();
        for &slot in &self.order {
            let s = slot as usize;
            if zones.contributes_pixels(s)
                && self.zone_has_content(s)
                && !self.slots[s].footprint.is_empty()
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
            clear_transparent(gpu, target);
            return;
        };

        clear_transparent(gpu, &self.ping);
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
                zone_opacity(zones, s),
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
            zone_opacity(zones, s),
            &self.cell_mask,
            &zs.coverage,
        );
    }
}

/// Clear an `Rgba16Float` texture to transparent black (premultiplied 0,0,0,0) — the
/// fold's base, a missing side of a crossfade, and the no-content frame. Read as rgb
/// it is black, so the master buffer sees no difference. A bare render pass with a
/// clear load op; no allocation.
fn clear_transparent(gpu: &Gpu, texture: &wgpu::Texture) {
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
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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

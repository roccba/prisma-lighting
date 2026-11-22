//! [`Engine::start`] — assemble the running product — and [`EngineHandle`] — own
//! it and tear it down in order.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use half::f16;
use uuid::Uuid;

use prisma_core::show::Show;
use prisma_core::source::{priority, SourceId};
use prisma_core::time::SystemClock;
use prisma_core::tree::{ParameterTree, WriteOutcome};
use prisma_core::zone::ZoneStates;
use prisma_core::MasterClock;
use prisma_core::{ClockShared, ControllerMappings, Curve, ParamDescriptor, RealRange, SlotIds};
use prisma_midi::MidiRuntime;

use prisma_patch::{LayoutGeneration, Patch, PatchError};

use prisma_output::affinity::pin_current_thread;
use prisma_output::frame::{frame_handoff, Producer};
use prisma_output::highlight::{highlight_handoff, HighlightState};
use prisma_output::netif::{self, InterfaceResolution};
use prisma_output::plan::{OutputPlan, PlanError};
use prisma_output::rate::RatePlan;
use prisma_output::route::DestinationTable;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::timing::ProbeReport;
use prisma_output::transport::UdpOutput;

use prisma_preview::preview_loop::{
    run_preview_loop, PreviewLoop, PreviewLoopConfig, PreviewStats,
};
use prisma_preview::{preview_handoff, PreviewDescriptor, PreviewPlan};

use prisma_core::show::ParamSet;

use prisma_render::live_params::LiveParams;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{
    run_render_loop, ClockControl, RenderLoop, RenderLoopConfig, RenderStats,
};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{Gpu, GpuError};

use crate::clips::{register_clips, ClipController, ClipHost, GridParams};
use crate::config::EngineConfig;
use crate::content::IdleContent;
use crate::zone_content::{
    build_track_effects, seed_live_from_show, track_param_prefix, ZoneCommandSender, ZoneCompositor,
};

/// What content an assembled engine renders. Chosen by the entry point: [`Engine::
/// start`] runs [`ContentSource::Idle`] (no show — black, the M5/M7 behaviour),
/// [`Engine::start_show`] runs a loaded show's per-zone effects.
enum ContentSource {
    /// No show loaded — the render target stays black ([`IdleContent`]).
    Idle,
    /// A loaded show: each track's active clip runs as effects in its zones. Boxed because a
    /// `Show` embeds a whole `Patch` — far larger than the `Idle` variant.
    Show(Box<Show>),
}

/// Why [`Engine::start`] failed. Bring-up only — once running, the engine reports
/// runtime state (link loss, generation mismatches) through the loops, not here.
#[derive(Debug)]
pub enum EngineError {
    /// The patch failed to validate or compile.
    Patch(PatchError),
    /// The patch compiled but its wire output plan could not be built (a
    /// mismatched patch/index pair, or a DMX span past channel 512).
    Plan(PlanError),
    /// The GPU could not be brought up.
    Gpu(GpuError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Patch(e) => write!(f, "patch failed to compile: {e}"),
            EngineError::Plan(e) => write!(f, "output plan failed to compile: {e}"),
            EngineError::Gpu(e) => write!(f, "GPU bring-up failed: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<PatchError> for EngineError {
    fn from(e: PatchError) -> Self {
        EngineError::Patch(e)
    }
}
impl From<PlanError> for EngineError {
    fn from(e: PlanError) -> Self {
        EngineError::Plan(e)
    }
}
impl From<GpuError> for EngineError {
    fn from(e: GpuError) -> Self {
        EngineError::Gpu(e)
    }
}

/// The final tally of a clean shutdown — proof the run happened and the teardown
/// was orderly. Returned by [`EngineHandle::shutdown`].
#[derive(Debug)]
pub struct ShutdownReport {
    /// Render-thread stats at stop (frames it published).
    pub render: RenderStats,
    /// The core the render thread pinned to, if affinity took effect — queryable
    /// achieved state, the M1 pattern extended to the render thread.
    pub render_pinned_core: Option<usize>,
    /// Output-thread report, including the RT scheduling mode it actually achieved
    /// (`ProbeReport::mode`) and its pinned core.
    pub output: ProbeReport,
    /// Preview-thread stats (frames it shipped).
    pub preview: PreviewStats,
    /// `Arc<Gpu>` strong count observed **after every GPU-using thread joined** and
    /// **before** the handle drops the GPU. It must be `1`: only the handle still
    /// holds it, so the drop that follows releases the Vulkan device with nothing
    /// else able to touch it. Anything higher means a GPU handle leaked into a
    /// still-live thread — the exact "app hangs on exit" bug this asserts against.
    pub gpu_strong_refs_at_release: usize,
}

/// The three running loops, taken out of the handle by `shutdown` (or the `Drop`
/// backstop) so teardown runs exactly once.
struct Running {
    render: JoinHandle<(RenderStats, Option<usize>)>,
    output: JoinHandle<ProbeReport>,
    preview: JoinHandle<PreviewStats>,
    render_stop: Arc<AtomicBool>,
    output_stop: Arc<AtomicBool>,
    preview_stop: Arc<AtomicBool>,
}

/// A running engine: the clock, the Parameter Tree, the compiled patch's layout,
/// and the three loops. Hold it for the life of the app; call [`shutdown`] to stop
/// cleanly, or just drop it (the `Drop` backstop tears down in the same order).
///
/// [`shutdown`]: EngineHandle::shutdown
pub struct EngineHandle {
    /// Shared so the MIDI input thread can write it (M9). Rebuilt on every recompile.
    tree: Arc<ParameterTree>,
    /// The MIDI transport (M9): the operator's controller profile and MIDI Learn. It
    /// outlives any one tree — a recompile unbinds it from the old tree and binds it
    /// to the new one, so open hardware ports never notice.
    midi: MidiRuntime,
    /// Every grid parameter (tracks by slot in `Show::tracks` order, scenes, GO).
    /// Empty under `IdleContent`.
    grid: GridParams,
    /// The show's running clip per track at assembly — where the clip controller starts.
    clip_active: Vec<Option<usize>>,
    /// The Show's owner, carried across recompiles like `midi`.
    clip_host: Mutex<Option<Arc<dyn ClipHost>>>,
    /// Plays clips from their tree parameters; running once a host is set.
    clip_controller: Mutex<Option<ClipController>>,
    live: LiveParams,
    /// The master clock's published face (read the live BPM) and its tempo controls.
    clock: Arc<ClockShared>,
    tempo: ClockControl,
    generation: LayoutGeneration,
    /// Kept so shutdown can release the GPU **last**, after the render thread that
    /// used it has joined. This is the only long-lived strong reference besides the
    /// render thread's. Also **reused across a recompile** so the Vulkan device is
    /// not re-initialized on every editor apply (§7 / S9).
    gpu: Arc<Gpu>,
    /// Freeze flag shared with the render loop (§7): while set, content holds and the
    /// output keeps emitting. The editor writes it via [`set_frozen`](Self::set_frozen).
    freeze: Arc<AtomicBool>,
    /// Producer half of the Highlight handoff (§7 / S9): the editor publishes the
    /// selected pixels here and the output thread reads them wait-free.
    highlight: Producer<HighlightState>,
    /// Fixture id → its pixels' component offsets in the master buffer, precompiled
    /// once so [`highlight`](Self::highlight) resolves a selection without walking the
    /// index each call. Rebuilt by a recompile against the new layout.
    fixture_components: HashMap<Uuid, Vec<usize>>,
    /// Length of the master buffer (components), to size/bound the Highlight mask.
    master_len: usize,
    /// Producer for the render content's frame-boundary command queue (M8-PARTA §3),
    /// present only when a show is running ([`Engine::start_show`]). The UI drives
    /// per-zone effect swaps through this; `None` under `IdleContent`.
    zone_commands: Option<ZoneCommandSender>,
    /// How the show's network output resolved at assembly, for the Device Manager.
    routing: OutputRouting,
    inner: Option<Running>,
}

/// How the show's network output resolved when the engine was assembled
/// (docs/DEVICES-SPEC.md §6, §9): the interface it sends from, and the patch universes
/// no enabled node receives — both surfaced as warnings, never as a refusal to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRouting {
    pub interface: InterfaceResolution,
    /// Plan universes (raw port addresses) that no enabled node receives. Empty when
    /// the output is not routed (everything goes to the virtual sink).
    pub uncovered: Vec<u16>,
}

/// A snapshot of a zone's live controls, for the UI to reflect state it did not set
/// itself (another transport, or the show's loaded values). Read wait-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneLiveReadout {
    pub dimmer: f32,
    /// Velocity multiplier, real `-10..=10` (0 = static, negative = reverse).
    pub speed: f32,
    pub solo: bool,
    pub blackout: bool,
    pub strobe_on: bool,
    /// Strobe speed, normalized `0..=1` over its 1–25 Hz range (the UI slider's scale).
    pub strobe_rate: f32,
}

/// A snapshot of a track's live controls (LIVE-MODEL-SPEC §5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackLiveReadout {
    pub opacity: f32,
    pub mute: bool,
    pub solo: bool,
}

/// The tempo range of `/clock/bpm`.
const BPM_MIN: f32 = prisma_core::MIN_BPM as f32;
const BPM_MAX: f32 = prisma_core::MAX_BPM as f32;

/// The source the main window's live controls (faders, solo, mute) write as — UI
/// priority, so a physical fader can take over per the arbitration rules (§4.5).
fn ui_control_source() -> SourceId {
    SourceId::new(priority::UI, 1)
}

/// A fresh MIDI runtime with an empty profile. The host loads the operator's saved
/// profile into it with [`MidiRuntime::set_profile`] after start.
fn new_midi_runtime() -> MidiRuntime {
    MidiRuntime::new(ControllerMappings::new("default"))
}

/// Namespace for engine bring-up.
pub struct Engine;

impl Engine {
    /// Compile `patch`, bring up the GPU, wire the clock + Parameter Tree, and spawn
    /// the render, output and preview loops. Returns once all three are running.
    ///
    /// This is the production assembly, not a test harness: the same path starts the
    /// real product. The render rate is **derived** from the outputs (§1.4) rather
    /// than hardcoded, and the render thread gets affinity (never RT — it stays below
    /// the output thread, §3.3).
    pub fn start(patch: Patch, config: EngineConfig) -> Result<EngineHandle, EngineError> {
        // --- GPU: brought up once here (setup path, blocks), shared by reference. ---
        let gpu = Arc::new(Gpu::new(config.adapter)?);
        Self::assemble(
            patch,
            config,
            gpu,
            ContentSource::Idle,
            new_midi_runtime(),
            None,
        )
    }

    /// Like [`start`](Engine::start), but runs a loaded **Show**: the engine renders
    /// the show's active per-zone effects instead of `IdleContent`, and seeds the
    /// Parameter Tree with the show's live state so it lights up at its saved values.
    /// The show embeds its Patch (M8-SPEC §5), so no separate patch is needed. This
    /// is where the engine stops rendering black and runs real content.
    pub fn start_show(show: Show, config: EngineConfig) -> Result<EngineHandle, EngineError> {
        let gpu = Arc::new(Gpu::new(config.adapter)?);
        let patch = show.patch.clone();
        Self::assemble(
            patch,
            config,
            gpu,
            ContentSource::Show(Box::new(show)),
            new_midi_runtime(),
            None,
        )
    }

    /// Assemble the running engine on an **existing** GPU. Shared by [`start`] (which
    /// creates the GPU) and [`EngineHandle::recompile`] (which reuses it, so an editor
    /// apply does not pay Vulkan re-init). Everything layout-derived is rebuilt from
    /// `patch`, minting a fresh [`LayoutGeneration`].
    ///
    /// [`start`]: Engine::start
    fn assemble(
        patch: Patch,
        config: EngineConfig,
        gpu: Arc<Gpu>,
        content: ContentSource,
        midi: MidiRuntime,
        clip_host: Option<Arc<dyn ClipHost>>,
    ) -> Result<EngineHandle, EngineError> {
        // --- Compile the patch: one layout generation shared by every consumer. ---
        let index = patch.compile()?;
        let generation = index.generation();
        let master_len = index.master_buffer_len();
        let zone_count = index.zone_count();
        let matrix = (patch.matrix_size[0], patch.matrix_size[1]);

        // Fixture → its master-buffer component offsets, for resolving a Highlight
        // selection (§7 / S9) without walking the index per call.
        let mut fixture_components: HashMap<Uuid, Vec<usize>> =
            HashMap::with_capacity(patch.fixtures.len());
        for fixture in &patch.fixtures {
            let mut offsets = Vec::with_capacity(fixture.pixels.len() * 4);
            for local in 0..fixture.pixels.len() as u32 {
                if let Some(g) = index.global_index(fixture.id, local) {
                    let start = index.pixel_component_offset(g) as usize;
                    let count = index.pixel_component_count(g) as usize;
                    offsets.extend(start..start + count);
                }
            }
            fixture_components.insert(fixture.id, offsets);
        }

        // --- Live state: one Parameter Tree binding feeds render, output AND preview. ---
        let tree = Arc::new(ParameterTree::with_defaults());
        let live = LiveParams::register(&tree, &patch);

        // --- Content: black (no show) or the per-zone compositor built from the show.
        //     Built here on the setup thread — the effect pipelines + zone textures
        //     compile ONCE at load, never per frame — then moved into the render
        //     thread. A show also seeds the tree with its saved live state so it
        //     lights up instead of sitting at engine-default dimmers (dark). ---
        //     A show also exposes its track clips as tree targets. ---
        let mut grid = GridParams::default();
        let mut clip_active: Vec<Option<usize>> = Vec::new();
        // Per zone slot, its home track (phase 1 MIDI bridge: grid pads → its clips).
        let mut home_tracks: Vec<Option<Uuid>> = Vec::new();
        // The show's DMX network output (docs/DEVICES-SPEC.md); no show → the default
        // (not routed), i.e. every universe to `config.output_target`.
        let network = match &content {
            ContentSource::Show(show) => show.network.clone(),
            ContentSource::Idle => Default::default(),
        };
        let (zone_content, zone_commands): (Option<ZoneCompositor>, Option<ZoneCommandSender>) =
            match content {
                ContentSource::Idle => (None, None),
                ContentSource::Show(show) => {
                    seed_live_from_show(&tree, &live, &patch, &show);
                    grid = register_clips(&tree, &patch, &show);
                    clip_active = show.tracks.iter().map(|t| t.active).collect();
                    home_tracks = patch
                        .zones
                        .iter()
                        .map(|z| show.home_track(z.id).map(|t| t.id))
                        .collect();
                    let live: Vec<_> = grid.tracks.iter().map(|t| t.live()).collect();
                    let tracks = build_track_effects(&gpu, &tree, &patch, &show, &live);
                    let compositor = ZoneCompositor::new(&gpu, &patch, &index, tracks);
                    let sender = compositor.command_sender();
                    (Some(compositor), Some(sender))
                }
            };

        // --- Output plan + transports + rate. ---
        let plan = OutputPlan::compile(&patch, &index)?;
        let router = OutputRouter::from_plan(plan);
        let universe_count = router.universe_count();
        // Per-universe destinations from the show's nodes, compiled here (setup thread)
        // so the output thread only indexes them. Not routed (Art-Net off / no nodes)
        // → every universe to `config.output_target` (the virtual sink in the app).
        // The show's interface is stored by network; find this machine's NIC on it
        // (docs/DEVICES-SPEC.md §6). Missing → the OS routes, and the UI warns.
        let interface = netif::resolve(network.interface.as_ref(), &netif::host_interfaces());
        let destinations = DestinationTable::compile(
            &router.port_addresses(),
            &network,
            interface.broadcast(),
            config.output_target,
        );
        let routing = OutputRouting {
            interface: interface.clone(),
            uncovered: destinations.uncovered().to_vec(),
        };
        // ONE socket for the whole rig, bound to the resolved interface (or 0.0.0.0).
        let output_socket =
            UdpOutput::bound(SocketAddr::new(IpAddr::V4(interface.bind_address()), 0));
        let artsync = network.artsync && network.artnet_enabled && !network.nodes.is_empty();
        // Render runs at the fastest active output (§1.4), computed from the RatePlan.
        let desired_hz = if universe_count == 0 {
            vec![config.output_rate_hz]
        } else {
            vec![config.output_rate_hz; universe_count]
        };
        let rate = RatePlan::compute(&desired_hz);
        let render_rate = rate.base_hz();

        // --- Handoffs (SPSC triple buffers): render publishes, output/preview read. ---
        let (mut out_producer, out_consumer) = frame_handoff(master_len);
        // Start-up ordering (learned in M5's acceptance test): stage one frame stamped
        // with the current generation so the output guard does not refuse the initial
        // NONE-generation buffers as a mismatch.
        out_producer.set_layout_generation(generation);
        out_producer.publish(|px| px.fill(f16::ZERO));
        let (pv_producer, pv_consumer) = preview_handoff(master_len, zone_count);

        // --- Preview plan + descriptor (CPU only, no GPU). ---
        let preview_plan = PreviewPlan::compile(&patch, &index);
        let descriptor = PreviewDescriptor::build(&preview_plan, &patch, &index);

        // --- Render inputs (built on the main thread; no GPU work here). ---
        let sample_plan = SamplePlan::compile(&patch, &index);
        let counter = OverlapCounter::new(&patch, &index);
        let clock = MasterClock::new(Arc::new(SystemClock::new()));
        // Tempo (LIVE-MODEL-SPEC §7 global bar): `/clock/bpm` and `/clock/tap`, applied
        // by the render loop that owns the clock; the result is read back through the
        // clock's published snapshot.
        let clock_shared = clock.handle();
        let clock_control = ClockControl {
            bpm: tree.register(
                "/clock/bpm",
                ParamDescriptor::level(
                    "clock.bpm",
                    RealRange {
                        min: BPM_MIN,
                        max: BPM_MAX,
                    },
                    Curve::Linear,
                    (120.0 - BPM_MIN) / (BPM_MAX - BPM_MIN),
                ),
            ),
            tap: tree.register("/clock/tap", ParamDescriptor::trigger("clock.tap")),
        };
        let tempo = clock_control.clone();

        // --- Freeze flag (render pauses the clock) + Highlight handoff (output reads
        //     the editor's selection wait-free). §7 / S9. ---
        let freeze = Arc::new(AtomicBool::new(false));
        let (highlight_producer, highlight_consumer) = highlight_handoff(master_len);

        // --- Spawn: output thread (self-elevates to RT — M1). ---
        let output_stop = Arc::new(AtomicBool::new(false));
        let output = {
            let stop = Arc::clone(&output_stop);
            let flat = live.clone();
            let pin = config.output_pin_core;
            std::thread::Builder::new()
                .name("prisma-output".into())
                .spawn(move || {
                    run_output_loop(
                        out_consumer,
                        router,
                        output_socket,
                        destinations,
                        rate,
                        SchedulerConfig {
                            run_for: None,
                            zone_count,
                            flat_source: Some(Box::new(flat)),
                            highlight_consumer: Some(highlight_consumer),
                            stop,
                            pin_to_core: pin,
                            artsync,
                            ..Default::default()
                        },
                    )
                })
                .expect("spawn output thread")
        };

        // --- Spawn: preview thread (normal priority, never RT — §M6). ---
        let preview_stop = Arc::new(AtomicBool::new(false));
        let preview = {
            let stop = Arc::clone(&preview_stop);
            let flat = live.clone();
            let sink = config.preview_sink;
            let rate_hz = config.preview_rate_hz;
            std::thread::Builder::new()
                .name("prisma-preview".into())
                .spawn(move || {
                    let mut preview =
                        PreviewLoop::new(preview_plan, descriptor, pv_consumer, Box::new(flat));
                    if let Some(sink) = sink {
                        preview.attach_sink(sink);
                    }
                    run_preview_loop(
                        preview,
                        PreviewLoopConfig {
                            target_rate_hz: rate_hz,
                            run_for: None,
                            stop,
                        },
                    )
                })
                .expect("spawn preview thread")
        };

        // --- Spawn: render thread (GPU work; affinity only, not RT — §3.3). ---
        let render_stop = Arc::new(AtomicBool::new(false));
        let render = {
            let stop = Arc::clone(&render_stop);
            let gpu = Arc::clone(&gpu);
            let zones = live.clone();
            let pin = config.render_pin_core;
            let freeze = Arc::clone(&freeze);
            std::thread::Builder::new()
                .name("prisma-render".into())
                .spawn(move || {
                    let pinned = pin.and_then(pin_current_thread);
                    // Build the GPU-backed loop pieces on the render thread itself.
                    let sampler = Sampler::new(&gpu, &sample_plan);
                    let mut engine = RenderLoop::with_default_slots(
                        &gpu,
                        sampler,
                        out_producer,
                        matrix,
                        master_len,
                    );
                    engine.attach_preview(pv_producer, zone_count);
                    let mut clock = clock;
                    let mut counter = counter;
                    let cfg = RenderLoopConfig {
                        target_rate_hz: render_rate,
                        run_for: None,
                        stop,
                        zone_source: Some(Box::new(zones)),
                        freeze,
                        // While frozen (editor open) the content is static, so render at a
                        // low rate — re-rendering the identical frame at the full rate is
                        // waste that competes with the editor webview on an integrated GPU.
                        // Output keeps its own fixed rate; Highlight (output-stage) is
                        // unaffected. See RenderLoopConfig::frozen_rate_hz.
                        frozen_rate_hz: 10,
                        clock_control: Some(clock_control),
                    };
                    // The content (built at load on the setup thread) was moved in.
                    // Either arm runs the SAME loop; only the FrameContent type differs.
                    let stats = match zone_content {
                        Some(mut compositor) => run_render_loop(
                            &gpu,
                            &mut engine,
                            &mut clock,
                            &mut counter,
                            ZoneStates::new(zone_count),
                            &mut compositor,
                            cfg,
                        ),
                        None => {
                            let mut idle = IdleContent;
                            run_render_loop(
                                &gpu,
                                &mut engine,
                                &mut clock,
                                &mut counter,
                                ZoneStates::new(zone_count),
                                &mut idle,
                                cfg,
                            )
                        }
                    };
                    (stats, pinned)
                })
                .expect("spawn render thread")
        };

        // --- MIDI (M9): bind the transport to this tree, zones in slot order (the same
        //     `Patch::zones` order `LiveParams` uses) with their home tracks, once every
        //     parameter exists. ---
        midi.set_zone_colours(patch.zones.iter().map(|z| z.display_color).collect());
        midi.bind(
            Arc::clone(&tree),
            SlotIds {
                zones: patch.zones.iter().map(|z| z.id).collect(),
                tracks: home_tracks,
            },
        );

        let handle = EngineHandle {
            tree,
            midi,
            grid,
            clip_active,
            clip_host: Mutex::new(None),
            clip_controller: Mutex::new(None),
            live,
            clock: clock_shared,
            tempo,
            generation,
            gpu,
            freeze,
            highlight: highlight_producer,
            fixture_components,
            master_len,
            zone_commands,
            routing,
            inner: Some(Running {
                render,
                output,
                preview,
                render_stop,
                output_stop,
                preview_stop,
            }),
        };
        if let Some(host) = clip_host {
            handle.set_clip_host(host);
        }
        Ok(handle)
    }
}

impl EngineHandle {
    /// The Parameter Tree — every addressable value (master, per-zone dimmer/colour,
    /// …). Transports and the UI write here; the loops read the same binding.
    pub fn tree(&self) -> &ParameterTree {
        &self.tree
    }

    /// The resolved live-parameter handles for this patch (master + per-zone), so a
    /// caller can write a fader without re-walking the tree.
    pub fn live(&self) -> &LiveParams {
        &self.live
    }

    /// The MIDI transport (M9): set the controller profile, arm MIDI Learn, and feed
    /// it from a hardware input. Cloneable; stays valid across a recompile.
    pub fn midi(&self) -> &MidiRuntime {
        &self.midi
    }

    /// The layout generation the running loops were compiled against. A future
    /// patch recompile (M7 editor apply) mints a new one and the guard rejects
    /// stale pairings.
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// How the show's network output resolved (interface + uncovered universes).
    pub fn output_routing(&self) -> &OutputRouting {
        &self.routing
    }

    /// Number of zones (the `Patch::zones` / `LiveParams` slot count). Slots address
    /// the per-zone live controls below.
    pub fn zone_count(&self) -> usize {
        self.live.zone_count()
    }

    /// Set a zone's dimmer (the zone-row fader), `slot` in `Patch::zones` order.
    /// Written live through the Parameter Tree; the render loop and the output stage
    /// both pick it up (the output stage on the next frame, §1.5). Out-of-range slots
    /// are ignored.
    pub fn set_zone_dimmer(&self, slot: usize, value: f32) {
        if slot < self.live.zone_count() {
            self.tree
                .write_real(&self.live.zone(slot).dimmer, value, ui_control_source());
        }
    }

    /// Set a zone's velocity multiplier (the zone-card speed fader), real `-10..=10`:
    /// 0 = static, positive = forward, negative = REVERSE. Written live through the
    /// Parameter Tree; the render loop integrates it into the zone's animation phase
    /// (`zone_content`). Out-of-range slots are ignored.
    pub fn set_zone_speed(&self, slot: usize, value: f32) {
        if slot < self.live.zone_count() {
            self.tree
                .write_real(&self.live.zone(slot).speed, value, ui_control_source());
        }
    }

    /// Toggle a zone's solo (M8-SPEC §3.1). Solo elsewhere silences other zones'
    /// *output* but not their `is_active` state (the preview/pads keep them, §4.3).
    pub fn set_zone_solo(&self, slot: usize, on: bool) {
        if slot < self.live.zone_count() {
            let v = if on { 1.0 } else { 0.0 };
            self.tree
                .write_real(&self.live.zone(slot).solo, v, ui_control_source());
        }
    }

    /// Toggle a zone's blackout/mute (M8-SPEC §3.1).
    pub fn set_zone_blackout(&self, slot: usize, on: bool) {
        if slot < self.live.zone_count() {
            let v = if on { 1.0 } else { 0.0 };
            self.tree
                .write_real(&self.live.zone(slot).blackout, v, ui_control_source());
        }
    }

    /// Set the master dimmer (the global-operation-bar fader), real `0..=1`. Applied
    /// at the output stage over every pixel (§5.2 step 6), so it takes effect on the
    /// next output frame for the whole rig.
    pub fn set_master_dimmer(&self, value: f32) {
        self.tree
            .write_real(&self.live.master().dimmer, value, ui_control_source());
    }

    /// Toggle the master blackout — kills all output to black while holding the
    /// dimmer's value (blackout). Applied at the output stage over every pixel.
    pub fn set_master_blackout(&self, on: bool) {
        let v = if on { 1.0 } else { 0.0 };
        self.tree
            .write_real(&self.live.master().blackout, v, ui_control_source());
    }

    /// Engage or release the global strobe (`/master/strobe/on`). Free-running (no BPM
    /// sync yet); gates every pixel at the output stage.
    pub fn set_master_strobe_on(&self, on: bool) {
        let v = if on { 1.0 } else { 0.0 };
        self.tree
            .write_real(&self.live.master().strobe.on, v, ui_control_source());
    }

    /// Set the global strobe's speed in Hz (clamped to its 1–25 Hz range), engaged or not.
    pub fn set_master_strobe_rate(&self, rate_hz: f32) {
        self.tree.write_real(
            &self.live.master().strobe.rate,
            rate_hz,
            ui_control_source(),
        );
    }

    /// Engage or release a zone's strobe. Gates the zone's non-overlapped pixels at the
    /// output stage (overlapped pixels are a follow-up, §5.2.1). Slot in `Patch::zones`
    /// order; out-of-range slots are ignored.
    pub fn set_zone_strobe_on(&self, slot: usize, on: bool) {
        if slot < self.live.zone_count() {
            let v = if on { 1.0 } else { 0.0 };
            self.tree
                .write_real(&self.live.zone(slot).strobe.on, v, ui_control_source());
        }
    }

    /// Set a zone's strobe speed in Hz, engaged or not.
    pub fn set_zone_strobe_rate(&self, slot: usize, rate_hz: f32) {
        if slot < self.live.zone_count() {
            self.tree.write_real(
                &self.live.zone(slot).strobe.rate,
                rate_hz,
                ui_control_source(),
            );
        }
    }

    /// The current live controls for a zone, so the UI can show state it did not set
    /// (the show's loaded values, or another transport). Out-of-range → all-default.
    pub fn zone_live(&self, slot: usize) -> ZoneLiveReadout {
        if slot >= self.live.zone_count() {
            return ZoneLiveReadout {
                dimmer: 0.0,
                speed: 1.0,
                solo: false,
                blackout: false,
                strobe_on: false,
                strobe_rate: 0.4,
            };
        }
        let z = self.live.zone(slot);
        ZoneLiveReadout {
            dimmer: z.dimmer.read_real().unwrap_or(0.0),
            speed: z.speed.read_real().unwrap_or(1.0),
            solo: z.solo.read_real().unwrap_or(0.0) > 0.5,
            blackout: z.blackout.read_real().unwrap_or(0.0) > 0.5,
            strobe_on: z.strobe.on.read_bool().unwrap_or(false),
            strobe_rate: z.strobe.rate.read_norm().unwrap_or(0.4),
        }
    }

    /// The producer for effect swaps (M8-PARTA §3), present only when a show is
    /// running ([`Engine::start_show`]). Changes apply at the next frame boundary,
    /// never mid-frame. `None` when the engine runs `IdleContent`.
    pub fn zone_commands(&self) -> Option<&ZoneCommandSender> {
        self.zone_commands.as_ref()
    }

    /// How many tracks the running show has (0 under `IdleContent`).
    pub fn track_count(&self) -> usize {
        self.grid.tracks.len()
    }

    /// The slot (`Show::tracks` position at assembly) of a track, if running.
    pub fn track_slot(&self, track: Uuid) -> Option<usize> {
        self.grid.tracks.iter().position(|c| c.track == track)
    }

    /// Run an effect on a track in every zone it reaches. Builds the
    /// `EffectInstance`s **here**, on the caller's thread (a pipeline compile, done
    /// off the render thread by design), then enqueues the swap through the
    /// frame-boundary command queue (M8-PARTA §3) so it applies whole at the next
    /// frame. Registers the effect's params under `/track/{id}/layer/0` and writes
    /// the given values.
    ///
    /// Returns `false` if no show is running, the slot is out of range, or the
    /// `effect_id` is not in the catalogue.
    pub fn trigger_track_effect(
        &self,
        slot: usize,
        effect_id: &str,
        params: &ParamSet,
        palette: Option<&[[f32; 3]]>,
    ) -> bool {
        let (Some(sender), Some(clips)) = (self.zone_commands.as_ref(), self.grid.tracks.get(slot))
        else {
            return false;
        };
        crate::clips::fire_track_effect(
            &self.gpu,
            &self.tree,
            sender,
            slot,
            clips,
            effect_id,
            params,
            palette,
            ui_control_source(),
        )
    }

    /// Replace the colour table of a track's running effect (the Tabla de color,
    /// live). Lands at the next frame boundary; a no-op if no show is running or the
    /// effect has no palette.
    pub fn set_track_palette(&self, slot: usize, colors: Vec<[f32; 3]>) -> bool {
        match self.zone_commands.as_ref() {
            Some(sender) => sender.set_track_palette(slot, colors),
            None => false,
        }
    }

    /// Hand the engine the Show's owner and start playing clips from the tree:
    /// `/track/{id}/clip/{i}/launch|trigger|hold`. Replaces any previous host; kept
    /// across recompiles. A no-op for content without tracks (idle).
    pub fn set_clip_host(&self, host: Arc<dyn ClipHost>) {
        let mut controller = self.clip_controller.lock().unwrap();
        *controller = None; // stop the old one before a new one reads the same tree
        *self.clip_host.lock().unwrap() = Some(Arc::clone(&host));
        if let Some(sender) = self.zone_commands.clone() {
            *controller = Some(ClipController::start(
                Arc::clone(&self.tree),
                Arc::clone(&self.gpu),
                sender,
                self.grid.clone(),
                self.clip_active.clone(),
                host,
            ));
        }
    }

    /// Latch a track's clip (a UI click): the same tree target a pad in latch mode
    /// fires, so every transport plays clips one way. `false` if no such clip.
    pub fn fire_clip(&self, slot: usize, clip: usize) -> bool {
        match self.grid.tracks.get(slot).and_then(|t| t.launch.get(clip)) {
            Some(h) => self.tree.fire(h, ui_control_source()) == WriteOutcome::Accepted,
            None => false,
        }
    }

    /// Run a track's clip from its start (the clip's `trigger` target): used when a
    /// clip's content was just replaced, so the new effect plays even if that clip was
    /// already the running one. `false` if no such clip.
    pub fn trigger_clip(&self, slot: usize, clip: usize) -> bool {
        match self.grid.tracks.get(slot).and_then(|t| t.trigger.get(clip)) {
            Some(h) => self.tree.fire(h, ui_control_source()) == WriteOutcome::Accepted,
            None => false,
        }
    }

    /// Ask the clip controller to re-read which of a track's clips hold an effect
    /// (after they were edited), so pad LEDs follow.
    pub fn refresh_clips(&self, slot: usize) {
        if let Some(t) = self.grid.tracks.get(slot) {
            self.tree.fire(&t.refresh, ui_control_source());
        }
    }

    /// Fire a trigger as the UI. `false` if it is missing or refused.
    fn fire(&self, h: Option<&prisma_core::ParamHandle>) -> bool {
        h.is_some_and(|h| self.tree.fire(h, ui_control_source()) == WriteOutcome::Accepted)
    }

    /// Arm (or disarm) a track's clip for GO — the same target Shift+click and an
    /// Arm-mode launch reach. `false` if no such clip.
    pub fn arm_clip(&self, slot: usize, clip: usize) -> bool {
        self.fire(self.grid.tracks.get(slot).and_then(|t| t.arm.get(clip)))
    }

    /// Stop a track, fading out with its transition (a stop during the fade cuts).
    pub fn stop_track(&self, slot: usize) -> bool {
        self.fire(self.grid.tracks.get(slot).map(|t| &t.stop))
    }

    /// Disarm every armed clip (`/arm/clear`).
    pub fn clear_arm(&self) -> bool {
        self.fire(self.grid.arm_clear.as_ref())
    }

    /// Stop every track (`/tracks/stop`), each with its transition; a second Stop All
    /// during the fades cuts them.
    pub fn stop_all(&self) -> bool {
        self.fire(self.grid.stop_all.as_ref())
    }

    /// Launch grid row `row` on every track that has a clip there.
    pub fn launch_scene(&self, row: usize) -> bool {
        self.fire(self.grid.scene_launch.get(row))
    }

    /// Arm grid row `row` on every track that has a clip there.
    pub fn arm_scene(&self, row: usize) -> bool {
        self.fire(self.grid.scene_arm.get(row))
    }

    /// GO: play every armed clip in the same frame.
    pub fn go(&self) -> bool {
        self.fire(self.grid.go.as_ref())
    }

    /// Set a track's opacity (`0..=1`), read by the render loop next frame.
    pub fn set_track_opacity(&self, slot: usize, value: f32) {
        if let Some(t) = self.grid.tracks.get(slot) {
            self.tree
                .write_real(&t.opacity, value.clamp(0.0, 1.0), ui_control_source());
        }
    }

    /// Change a track's blend mode (and, for `LumaKey`, its key `[threshold,
    /// softness]`), live (applied at the next frame boundary).
    pub fn set_track_blend(
        &self,
        slot: usize,
        mode: prisma_patch::BlendMode,
        key: [f32; 2],
    ) -> bool {
        match self.zone_commands.as_ref() {
            Some(sender) if slot < self.grid.tracks.len() => {
                sender.send(crate::zone_content::ZoneCommand::SetBlend {
                    track: slot,
                    mode,
                    key,
                })
            }
            _ => false,
        }
    }

    /// Mute or unmute a track.
    pub fn set_track_mute(&self, slot: usize, on: bool) {
        if let Some(t) = self.grid.tracks.get(slot) {
            self.tree
                .write_real(&t.mute, if on { 1.0 } else { 0.0 }, ui_control_source());
        }
    }

    /// Solo or unsolo a track.
    pub fn set_track_solo(&self, slot: usize, on: bool) {
        if let Some(t) = self.grid.tracks.get(slot) {
            self.tree
                .write_real(&t.solo, if on { 1.0 } else { 0.0 }, ui_control_source());
        }
    }

    /// A track's live opacity / mute / solo, wait-free (for the UI and for folding
    /// back into the show before a recompile).
    pub fn track_live(&self, slot: usize) -> Option<TrackLiveReadout> {
        let t = self.grid.tracks.get(slot)?;
        Some(TrackLiveReadout {
            opacity: t.opacity.read_real().unwrap_or(1.0),
            mute: t.mute.read_bool().unwrap_or(false),
            solo: t.solo.read_bool().unwrap_or(false),
        })
    }

    /// The master clock's current tempo (after taps), read wait-free.
    pub fn bpm(&self) -> f64 {
        self.clock.snapshot().bpm
    }

    /// Set the tempo (clamped to the `/clock/bpm` range); applied next frame.
    pub fn set_bpm(&self, bpm: f64) {
        let v = (bpm as f32).clamp(BPM_MIN, BPM_MAX);
        self.tree
            .write_real(&self.tempo.bpm, v, ui_control_source());
    }

    /// One tap of tap tempo.
    pub fn tap(&self) {
        self.tree.fire(&self.tempo.tap, ui_control_source());
    }

    /// Stop the clip controller (before the tree or GPU it uses goes away).
    fn stop_clips(&self) {
        *self.clip_controller.lock().unwrap() = None;
    }

    /// Write one live parameter of a track's running effect (the parameter panel).
    /// Targets `/track/{track}/layer/0/param/{name}` with a **normalized** `0..=1`
    /// value, so the running `EffectInstance` picks it up on its next wait-free read
    /// (§4.5) — the preview and the wire follow immediately, no re-trigger. A no-op if
    /// that path is not registered (nothing running on the track).
    pub fn set_track_param(&self, track: Uuid, name: &str, value_norm: f32) {
        let path = format!("{}/param/{name}", track_param_prefix(track));
        self.tree.write_path(&path, value_norm, ui_control_source());
    }

    /// Stop a track (firing an empty clip). Its zones render nothing from it from the
    /// next frame. Returns `false` if no show is running.
    pub fn clear_track_effect(&self, slot: usize) -> bool {
        match self.zone_commands.as_ref() {
            Some(sender) => sender.clear_track(slot),
            None => false,
        }
    }

    /// Freeze or resume the show content (§7 / S9). Frozen: effects stop advancing and
    /// the rig holds its last frame; the output thread keeps emitting, which is what
    /// makes the editor's Highlight visible without a global flicker. The editor
    /// freezes while it is open and resumes on close.
    pub fn set_frozen(&self, frozen: bool) {
        self.freeze.store(frozen, Ordering::Relaxed);
    }

    /// Whether content is currently frozen.
    pub fn is_frozen(&self) -> bool {
        self.freeze.load(Ordering::Relaxed)
    }

    /// Light the given fixtures on the live output to identify them on the rig (§7 /
    /// S9). `blink` chooses the pattern: `true` for a ~2 Hz blink (spot one among
    /// many), `false` for solid (compare two side by side). An empty selection — or
    /// fixture ids not in the running layout (e.g. just-placed, not yet recompiled) —
    /// clears the highlight. Published wait-free; the output thread picks it up on its
    /// next tick.
    pub fn highlight(&mut self, fixtures: &[Uuid], blink: bool) {
        // Resolve the selection against the running layout into a local mask first,
        // then hand it to the producer — keeps the field borrows cleanly separated.
        let mut mask = vec![false; self.master_len];
        let mut active = false;
        for id in fixtures {
            if let Some(offsets) = self.fixture_components.get(id) {
                for &o in offsets {
                    if let Some(slot) = mask.get_mut(o) {
                        *slot = true;
                        active = true;
                    }
                }
            }
        }
        let state = self.highlight.input();
        state.mask.copy_from_slice(&mask);
        state.active = active;
        state.blink = blink;
        self.highlight.publish();
    }

    /// Clear any Highlight.
    pub fn clear_highlight(&mut self) {
        self.highlight(&[], false);
    }

    /// Apply an edited patch to the live engine (§7 / S9): stop the loops, **reuse the
    /// GPU** (no Vulkan re-init), and re-assemble everything from `patch`, minting a
    /// fresh [`LayoutGeneration`]. The generation guard is the safety net — the new
    /// handoffs stamp the new generation, so the output can never emit a frame from a
    /// different layout than its plan expects. Returns the new handle; the old one is
    /// consumed. Highlight is cleared and any freeze is reset to the new engine's
    /// default (running) — the caller re-applies editor state after.
    pub fn recompile(
        self,
        patch: Patch,
        config: EngineConfig,
    ) -> Result<EngineHandle, EngineError> {
        self.recompile_with(patch, config, ContentSource::Idle)
    }

    /// Like [`recompile`](Self::recompile), but re-runs a **Show** on the new layout
    /// instead of resetting to black — the editor-as-secondary-window path (M8 §6/§7):
    /// closing the editor applies its patch edits to the *running* show, reusing the
    /// GPU. The show embeds its (edited) patch, so the compositor rebuilds its track
    /// effects from the show's active clips, and the tree is re-seeded from the show's
    /// live state. **Note:** because the tree is rebuilt, live values reset to the
    /// show's saved state — the caller re-applies any live tweaks it wants preserved.
    pub fn recompile_show(
        self,
        show: Show,
        config: EngineConfig,
    ) -> Result<EngineHandle, EngineError> {
        let patch = show.patch.clone();
        self.recompile_with(patch, config, ContentSource::Show(Box::new(show)))
    }

    /// Shared teardown + reassembly for [`recompile`](Self::recompile) and
    /// [`recompile_show`](Self::recompile_show): stop the loops in order (keeping the
    /// GPU alive), then assemble a fresh engine on the same device with `content`.
    fn recompile_with(
        mut self,
        patch: Patch,
        config: EngineConfig,
        content: ContentSource,
    ) -> Result<EngineHandle, EngineError> {
        let running = self
            .inner
            .take()
            .expect("recompile: engine already torn down");
        // MIDI and clips first: stop writing the tree that is about to go away.
        self.midi.unbind();
        self.stop_clips();
        let clip_host = self.clip_host.lock().unwrap().take();
        // Same teardown order as `shutdown`, minus the GPU release: joining render
        // drops its GPU clone, leaving this handle's Arc as the sole strong reference.
        running.render_stop.store(true, Ordering::Relaxed);
        let _ = running.render.join();
        running.output_stop.store(true, Ordering::Relaxed);
        running.preview_stop.store(true, Ordering::Relaxed);
        let _ = running.output.join();
        let _ = running.preview.join();
        // Keep the device alive for the fresh assembly; `self` (and its GPU Arc) drops
        // when this returns, but `gpu` carries the only remaining strong reference.
        let gpu = Arc::clone(&self.gpu);
        Engine::assemble(patch, config, gpu, content, self.midi.clone(), clip_host)
    }

    /// Stop the engine cleanly, **in order**, and report the run.
    ///
    /// Order matters and is the point of this method (not something to discover when
    /// the app hangs on exit):
    ///
    /// 1. **Render first.** Signal it and join. It is the producer; stopping it first
    ///    means no new frames enter the handoffs and all GPU work quiesces before we
    ///    touch the device. Output/preview keep resending the last frame meanwhile —
    ///    harmless.
    /// 2. **Output and preview next.** Signal both and join. When their `run_*_loop`
    ///    returns it drops the values it owns — the `UdpOutput` sockets and the
    ///    preview sink — so sockets close here with no extra step.
    /// 3. **GPU last.** With the render thread joined, its `Arc<Gpu>` clone is gone
    ///    and this handle holds the only strong reference (asserted as
    ///    `gpu_strong_refs_at_release == 1`). Returning drops the handle, which drops
    ///    the `Arc`, which releases the Vulkan device — safe because nothing is left
    ///    to use it.
    ///
    /// Each loop checks its stop flag within one tick, so join latency is bounded by
    /// one period per loop, never unbounded.
    pub fn shutdown(mut self) -> ShutdownReport {
        let running = self
            .inner
            .take()
            .expect("shutdown: engine already torn down");

        // 0. MIDI input and the clip controller stop reaching the tree (the controller
        //    also holds a GPU reference, released here before the count below).
        self.midi.unbind();
        self.stop_clips();

        // 1. Render.
        running.render_stop.store(true, Ordering::Relaxed);
        let (render, render_pinned_core) = running.render.join().expect("join render thread");

        // 2. Output + preview.
        running.output_stop.store(true, Ordering::Relaxed);
        running.preview_stop.store(true, Ordering::Relaxed);
        let output = running.output.join().expect("join output thread");
        let preview = running.preview.join().expect("join preview thread");

        // 3. GPU released as `self` drops at the end of this function.
        let gpu_strong_refs_at_release = Arc::strong_count(&self.gpu);

        ShutdownReport {
            render,
            render_pinned_core,
            output,
            preview,
            gpu_strong_refs_at_release,
        }
    }
}

/// The Tauri shell keeps the [`EngineHandle`] in managed state, which requires it
/// to be `Send + Sync`. That crate is excluded from the workspace (desktop-only
/// system deps), so assert the bound *here*, where CI does compile it — a broken
/// `Send`/`Sync` shows up now, not on the manual desktop build.
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    fn check() {
        assert_send_sync::<EngineHandle>();
    }
    let _ = check;
};

impl Drop for EngineHandle {
    /// Backstop teardown: if [`shutdown`](EngineHandle::shutdown) was not called
    /// (e.g. the handle is dropped on a panic path), stop the loops in the same
    /// order and join them so the process never leaks a running thread or a live GPU
    /// device. Results are discarded — a Drop cannot return them. If `shutdown` ran,
    /// `inner` is already `None` and this does nothing (no double join).
    fn drop(&mut self) {
        if let Some(running) = self.inner.take() {
            self.midi.unbind();
            self.stop_clips();
            running.render_stop.store(true, Ordering::Relaxed);
            let _ = running.render.join();
            running.output_stop.store(true, Ordering::Relaxed);
            running.preview_stop.store(true, Ordering::Relaxed);
            let _ = running.output.join();
            let _ = running.preview.join();
            // `self.gpu` drops immediately after this, releasing the GPU last.
        }
    }
}

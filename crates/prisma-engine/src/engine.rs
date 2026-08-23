//! [`Engine::start`] — assemble the running product — and [`EngineHandle`] — own
//! it and tear it down in order.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use half::f16;
use uuid::Uuid;

use prisma_core::show::Show;
use prisma_core::source::{priority, SourceId};
use prisma_core::time::SystemClock;
use prisma_core::tree::ParameterTree;
use prisma_core::zone::ZoneStates;
use prisma_core::MasterClock;

use prisma_patch::{LayoutGeneration, Patch, PatchError};

use prisma_output::affinity::pin_current_thread;
use prisma_output::frame::{frame_handoff, Producer};
use prisma_output::highlight::{highlight_handoff, HighlightState};
use prisma_output::plan::{OutputPlan, PlanError};
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::timing::ProbeReport;
use prisma_output::transport::UdpOutput;

use prisma_preview::preview_loop::{
    run_preview_loop, PreviewLoop, PreviewLoopConfig, PreviewStats,
};
use prisma_preview::{preview_handoff, PreviewDescriptor, PreviewPlan};

use prisma_core::show::ParamSet;

use prisma_render::effect::EffectInstance;
use prisma_render::live_params::LiveParams;
use prisma_render::manifest_by_id;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{run_render_loop, RenderLoop, RenderLoopConfig, RenderStats};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{Gpu, GpuError};

use crate::config::EngineConfig;
use crate::content::IdleContent;
use crate::zone_content::{
    build_zone_effects, seed_live_from_show, ZoneCommandSender, ZoneCompositor,
};

/// What content an assembled engine renders. Chosen by the entry point: [`Engine::
/// start`] runs [`ContentSource::Idle`] (no show — black, the M5/M7 behaviour),
/// [`Engine::start_show`] runs a loaded show's per-zone effects.
enum ContentSource {
    /// No show loaded — the render target stays black ([`IdleContent`]).
    Idle,
    /// A loaded show: its active per-zone cells run as effects. Boxed because a
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
    tree: ParameterTree,
    live: LiveParams,
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
    inner: Option<Running>,
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
}

/// The source the main window's live controls (faders, solo, mute) write as — UI
/// priority, so a physical fader can take over per the arbitration rules (§4.5).
fn ui_control_source() -> SourceId {
    SourceId::new(priority::UI, 1)
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
        Self::assemble(patch, config, gpu, ContentSource::Idle)
    }

    /// Like [`start`](Engine::start), but runs a loaded **Show**: the engine renders
    /// the show's active per-zone effects instead of `IdleContent`, and seeds the
    /// Parameter Tree with the show's live state so it lights up at its saved values.
    /// The show embeds its Patch (M8-SPEC §5), so no separate patch is needed. This
    /// is where the engine stops rendering black and runs real content.
    pub fn start_show(show: Show, config: EngineConfig) -> Result<EngineHandle, EngineError> {
        let gpu = Arc::new(Gpu::new(config.adapter)?);
        let patch = show.patch.clone();
        Self::assemble(patch, config, gpu, ContentSource::Show(Box::new(show)))
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
        let tree = ParameterTree::with_defaults();
        let live = LiveParams::register(&tree, &patch);

        // --- Content: black (no show) or the per-zone compositor built from the show.
        //     Built here on the setup thread — the effect pipelines + zone textures
        //     compile ONCE at load, never per frame — then moved into the render
        //     thread. A show also seeds the tree with its saved live state so it
        //     lights up instead of sitting at engine-default dimmers (dark). ---
        let (zone_content, zone_commands): (Option<ZoneCompositor>, Option<ZoneCommandSender>) =
            match content {
                ContentSource::Idle => (None, None),
                ContentSource::Show(show) => {
                    seed_live_from_show(&tree, &live, &patch, &show);
                    let effects = build_zone_effects(&gpu, &tree, &patch, &index, &show);
                    let compositor = ZoneCompositor::new(&gpu, &patch, &index, effects);
                    let sender = compositor.command_sender();
                    (Some(compositor), Some(sender))
                }
            };

        // --- Output plan + transports + rate. ---
        let plan = OutputPlan::compile(&patch, &index)?;
        let router = OutputRouter::from_plan(plan);
        let universe_count = router.universe_count();
        // One socket per universe, all to the configured target (dev: the virtual
        // sink; prod: a node or broadcast). Per-universe/NIC routing is deferred.
        let transports: Vec<UdpOutput> = (0..universe_count)
            .map(|_| UdpOutput::to_target(config.output_target))
            .collect();
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
            let artsync = config.artsync_target;
            std::thread::Builder::new()
                .name("prisma-output".into())
                .spawn(move || {
                    run_output_loop(
                        out_consumer,
                        router,
                        transports,
                        rate,
                        SchedulerConfig {
                            run_for: None,
                            zone_count,
                            flat_source: Some(Box::new(flat)),
                            highlight_consumer: Some(highlight_consumer),
                            stop,
                            pin_to_core: pin,
                            artsync_target: artsync,
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

        Ok(EngineHandle {
            tree,
            live,
            generation,
            gpu,
            freeze,
            highlight: highlight_producer,
            fixture_components,
            master_len,
            zone_commands,
            inner: Some(Running {
                render,
                output,
                preview,
                render_stop,
                output_stop,
                preview_stop,
            }),
        })
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

    /// The layout generation the running loops were compiled against. A future
    /// patch recompile (M7 editor apply) mints a new one and the guard rejects
    /// stale pairings.
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
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

    /// Set the global strobe: `rate` in Hz (`0` = off) and `duty` `0..=1` (the on
    /// fraction of each cycle). Free-running (no BPM sync in M8); gates every pixel at
    /// the output stage.
    pub fn set_master_strobe(&self, rate_hz: f32, duty: f32) {
        let s = ui_control_source();
        let strobe = &self.live.master().strobe;
        self.tree.write_real(&strobe.rate, rate_hz, s);
        self.tree.write_real(&strobe.duty, duty, s);
    }

    /// Set a zone's strobe (`rate` Hz, `duty` `0..=1`). Gates the zone's non-overlapped
    /// pixels at the output stage (overlapped pixels are a follow-up, §5.2.1). Slot in
    /// `Patch::zones` order; out-of-range slots are ignored.
    pub fn set_zone_strobe(&self, slot: usize, rate_hz: f32, duty: f32) {
        if slot < self.live.zone_count() {
            let s = ui_control_source();
            let strobe = &self.live.zone(slot).strobe;
            self.tree.write_real(&strobe.rate, rate_hz, s);
            self.tree.write_real(&strobe.duty, duty, s);
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
            };
        }
        let z = self.live.zone(slot);
        ZoneLiveReadout {
            dimmer: z.dimmer.read_real().unwrap_or(0.0),
            speed: z.speed.read_real().unwrap_or(1.0),
            solo: z.solo.read_real().unwrap_or(0.0) > 0.5,
            blackout: z.blackout.read_real().unwrap_or(0.0) > 0.5,
        }
    }

    /// The producer for per-zone effect swaps (M8-PARTA §3), present only when a show
    /// is running ([`Engine::start_show`]). The UI/scene-grid (M8 step 4) drives cell
    /// triggering through this; changes apply at the next frame boundary, never
    /// mid-frame. `None` when the engine runs `IdleContent`.
    pub fn zone_commands(&self) -> Option<&ZoneCommandSender> {
        self.zone_commands.as_ref()
    }

    /// Trigger an effect on a zone (M8 step 4 — firing a scene cell). Builds the
    /// [`EffectInstance`] **here**, on the caller's thread (a pipeline compile, done
    /// off the render thread by design), then enqueues the swap through the
    /// frame-boundary command queue (M8-PARTA §3) so it applies whole at the next
    /// frame — the mini-preview and the wire follow. Registers the effect's params
    /// under `/zone/{zone_id}/layer/0` and writes the cell's stored values.
    ///
    /// Returns `false` if no show is running, the slot is out of range, or the
    /// `effect_id` is not in the catalogue.
    pub fn trigger_zone_effect(
        &self,
        slot: usize,
        zone_id: Uuid,
        effect_id: &str,
        params: &ParamSet,
    ) -> bool {
        let Some(sender) = self.zone_commands.as_ref() else {
            return false;
        };
        if slot >= self.live.zone_count() {
            return false;
        }
        let Some(manifest) = manifest_by_id(effect_id) else {
            return false;
        };
        let prefix = format!("/zone/{zone_id}/layer/0");
        let effect = EffectInstance::new(&self.gpu, &self.tree, &prefix, &manifest);
        let src = ui_control_source();
        for (name, value) in params.iter() {
            self.tree
                .write_path(&format!("{prefix}/param/{name}"), value, src);
        }
        sender.set_zone_effect(slot, Some(effect))
    }

    /// Write one live parameter of a zone's active effect (M8 step 5 — the parameter
    /// panel). Targets `/zone/{zone_id}/layer/0/param/{name}` with a **normalized**
    /// `0..=1` value, so the running [`EffectInstance`] picks it up on its next
    /// wait-free read (§4.5) — the preview and the wire follow immediately, no
    /// re-trigger. A no-op if that path is not registered (the zone has no active
    /// effect, so nothing to drive). This is the `active` side of `selected ≠ active`,
    /// exactly like the zone faders.
    pub fn set_zone_param(&self, zone_id: Uuid, name: &str, value_norm: f32) {
        let path = format!("/zone/{zone_id}/layer/0/param/{name}");
        self.tree.write_path(&path, value_norm, ui_control_source());
    }

    /// Clear a zone's active effect (firing an empty cell / stopping the zone). The
    /// zone renders nothing from the next frame. Returns `false` if no show is running.
    pub fn clear_zone_effect(&self, slot: usize) -> bool {
        match self.zone_commands.as_ref() {
            Some(sender) => sender.set_zone_effect(slot, None),
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
    /// GPU. The show embeds its (edited) patch, so the compositor rebuilds its per-zone
    /// effects from the show's active cells, and the tree is re-seeded from the show's
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
        Engine::assemble(patch, config, gpu, content)
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

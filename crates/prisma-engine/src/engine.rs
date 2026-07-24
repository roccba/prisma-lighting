//! [`Engine::start`] — assemble the running product — and [`EngineHandle`] — own
//! it and tear it down in order.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use half::f16;

use prisma_core::time::SystemClock;
use prisma_core::tree::ParameterTree;
use prisma_core::zone::ZoneStates;
use prisma_core::MasterClock;

use prisma_patch::{LayoutGeneration, Patch, PatchError};

use prisma_output::affinity::pin_current_thread;
use prisma_output::frame::frame_handoff;
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

use prisma_render::live_params::LiveParams;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{run_render_loop, RenderLoop, RenderLoopConfig, RenderStats};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{Gpu, GpuError};

use crate::config::EngineConfig;
use crate::content::IdleContent;

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
    /// render thread's.
    gpu: Arc<Gpu>,
    inner: Option<Running>,
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
        // --- Compile the patch: one layout generation shared by every consumer. ---
        let index = patch.compile()?;
        let generation = index.generation();
        let master_len = index.master_buffer_len();
        let zone_count = index.zone_count();
        let matrix = (patch.matrix_size[0], patch.matrix_size[1]);

        // --- GPU: brought up once here (setup path, blocks), shared by reference. ---
        let gpu = Arc::new(Gpu::new(config.adapter)?);

        // --- Live state: one Parameter Tree binding feeds render, output AND preview. ---
        let tree = ParameterTree::with_defaults();
        let live = LiveParams::register(&tree, &patch);

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
            std::thread::Builder::new()
                .name("prisma-render".into())
                .spawn(move || {
                    let pinned = pin.and_then(pin_current_thread);
                    // Build the GPU-backed pieces on the render thread itself.
                    let sampler = Sampler::new(&gpu, &sample_plan);
                    let mut engine = RenderLoop::with_default_slots(
                        &gpu,
                        sampler,
                        out_producer,
                        matrix,
                        master_len,
                    );
                    engine.attach_preview(pv_producer, zone_count);
                    let mut content = IdleContent;
                    let mut clock = clock;
                    let mut counter = counter;
                    let stats = run_render_loop(
                        &gpu,
                        &mut engine,
                        &mut clock,
                        &mut counter,
                        ZoneStates::new(zone_count),
                        &mut content,
                        RenderLoopConfig {
                            target_rate_hz: render_rate,
                            run_for: None,
                            stop,
                            zone_source: Some(Box::new(zones)),
                        },
                    );
                    (stats, pinned)
                })
                .expect("spawn render thread")
        };

        Ok(EngineHandle {
            tree,
            live,
            generation,
            gpu,
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

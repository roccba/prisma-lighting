//! The render loop: clock → content → gather → async readback → frame handoff
//! (CLAUDE.md §3.3, §5.2).
//!
//! The loop owns its own clock and runs independently of the UI (§3.1 principle
//! 1). It never blocks: readback goes through [`ReadbackRing`], which has no
//! blocking wait to call, and publishing goes through the M1 triple buffer, which
//! never waits on the output thread.
//!
//! # What the loop does per frame
//!
//! 1. `MasterClock::tick()` — one coherent [`ClockSnapshot`] threaded by value.
//! 2. Sync the overlap counter from live zone state and publish this frame's
//!    snapshot (§5.2.1's frame-boundary rule).
//! 3. Claim a readback slot. **If none is free, skip the frame** — see
//!    [`ReadbackRing`] for why that is the right response and not a wait.
//! 4. Render the content into the virtual-matrix framebuffer (step 4).
//! 5. Gather framebuffer → master buffer, copying into the claimed slot (step 5).
//! 6. Poll non-blockingly, and publish any readback that has completed —
//!    converting `f32` → `f16` **inside the copy that fills the frame**, a copy
//!    that has to happen anyway, so the conversion is essentially free.
//!
//! A published frame carries the snapshot **it was rendered under**, not the
//! current one: a readback landing now belongs to a frame from a few ticks ago.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot};
use prisma_output::frame::FrameProducer;
use prisma_patch::LayoutGeneration;

use prisma_preview::frame::{PreviewProducer, ZoneActiveMask};

use crate::overlap::OverlapCounter;
use crate::readback_ring::{ReadbackRing, DEFAULT_SLOTS};
use crate::sample::Sampler;

/// Produces one frame's composited virtual-matrix framebuffer.
///
/// Implemented by the engine's zone compositor; a test can implement it with a
/// single clear. Kept as a trait so the loop's timing and readback behaviour can
/// be tested without dragging in effects, zones and blend modes.
pub trait FrameContent {
    /// `zones` is the **same** state the overlap counter just synced from, so the
    /// set of zones this composites and the classification in `overlap` cannot
    /// disagree. Re-reading live state here instead would let a zone flip between
    /// the two and composite against a mask that does not describe it.
    fn render_into(
        &mut self,
        gpu: &crate::Gpu,
        clock: &ClockSnapshot,
        zones: &ZoneStates,
        overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    );
}

/// Running min/mean/max of a latency, without unbounded storage on the hot path.
#[derive(Debug, Clone, Copy)]
pub struct LatencyStats {
    pub count: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub(crate) sum_ns: u128,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self {
            count: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            sum_ns: 0,
        }
    }
}

impl LatencyStats {
    fn record(&mut self, d: Duration) {
        let ns = d.as_nanos() as u64;
        self.count += 1;
        self.min_ns = self.min_ns.min(ns);
        self.max_ns = self.max_ns.max(ns);
        self.sum_ns += ns as u128;
    }

    pub fn mean_ns(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            (self.sum_ns / self.count as u128) as u64
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// What one [`RenderLoop::tick`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Content was rendered and a readback was started.
    Rendered,
    /// Every readback slot was still in flight, so the frame was skipped whole.
    Stalled,
}

/// Measurements the render side contributes to the §1.5 latency budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    pub frames_rendered: u64,
    pub frames_published: u64,
    pub readback_stalls: u64,
    pub readback_failures: u64,
    /// Frame start → submit: the CPU-side render cost.
    pub render: LatencyStats,
    /// Submit → readback observed complete (§1.5's tracked metric).
    pub readback: LatencyStats,
    /// Frame start → frame published: the whole render leg of tick→wire. Add the
    /// output probe's produced→wire to get the full chain.
    pub tick_to_publish: LatencyStats,
}

/// Owns the per-frame GPU mechanics: framebuffer, gather, readback ring, and the
/// producer half of the frame handoff.
pub struct RenderLoop {
    framebuffer: wgpu::Texture,
    sampler: Sampler,
    ring: ReadbackRing,
    producer: FrameProducer,
    generation: LayoutGeneration,
    stats: RenderStats,
    /// Optional second consumer of the master buffer (M6). `None` costs nothing:
    /// no mask update, no second publish.
    preview: Option<PreviewProducer>,
    /// Refreshed at each frame boundary from the same `ZoneStates` the overlap
    /// counter synced from, and stamped onto every published preview frame.
    zone_active: ZoneActiveMask,
}

impl RenderLoop {
    pub fn new(
        gpu: &crate::Gpu,
        sampler: Sampler,
        producer: FrameProducer,
        matrix: (u32, u32),
        component_count: usize,
        slots: usize,
    ) -> RenderLoop {
        let framebuffer =
            crate::readback::create_render_target(gpu, matrix.0.max(1), matrix.1.max(1));
        let generation = sampler.generation();
        RenderLoop {
            framebuffer,
            sampler,
            ring: ReadbackRing::new(gpu, component_count, slots),
            producer,
            generation,
            stats: RenderStats::default(),
            preview: None,
            zone_active: ZoneActiveMask::new(0),
        }
    }

    /// Attach a preview handoff (M6). The loop then publishes every frame to the
    /// preview as well — **after** the output publish, and with its own `f32→f16`
    /// conversion, so the path that feeds the wire is byte-for-byte the work it was
    /// before and cannot be slowed down by switching the preview on.
    ///
    /// `zone_count` sizes the [`ZoneActiveMask`]; it is the patch's zone count, the
    /// same coordinate the overlap counter and the Parameter Tree use.
    pub fn attach_preview(&mut self, preview: PreviewProducer, zone_count: usize) {
        self.preview = Some(preview);
        self.zone_active = ZoneActiveMask::new(zone_count);
    }

    /// Whether a preview handoff is attached.
    #[inline]
    pub fn has_preview(&self) -> bool {
        self.preview.is_some()
    }

    pub fn with_default_slots(
        gpu: &crate::Gpu,
        sampler: Sampler,
        producer: FrameProducer,
        matrix: (u32, u32),
        component_count: usize,
    ) -> RenderLoop {
        Self::new(
            gpu,
            sampler,
            producer,
            matrix,
            component_count,
            DEFAULT_SLOTS,
        )
    }

    pub fn stats(&self) -> RenderStats {
        let mut s = self.stats;
        s.readback_stalls = self.ring.stalls();
        s.readback_failures = self.ring.failures();
        s
    }

    /// Render and publish one frame. Never blocks.
    pub fn tick(
        &mut self,
        gpu: &crate::Gpu,
        clock: &ClockSnapshot,
        zones: &ZoneStates,
        overlap: &Arc<OverlapSnapshot>,
        content: &mut impl FrameContent,
    ) -> TickOutcome {
        let tick_at = Instant::now();

        // The preview's zone mask is settled here, at the frame boundary, from the
        // same state the counter synced from — so the mask, the classification and
        // the content all describe one instant.
        if self.preview.is_some() {
            self.zone_active.update_from(zones);
        }

        // Claim BEFORE doing any work, so a stall costs nothing and adds no GPU
        // load at the moment the GPU is already behind.
        let outcome = match self.ring.try_claim() {
            None => TickOutcome::Stalled,
            Some(slot) => {
                content.render_into(gpu, clock, zones, overlap, &self.framebuffer);
                self.sampler
                    .gather_into(gpu, &self.framebuffer, self.ring.buffer(slot));
                self.ring
                    .begin_map(slot, tick_at, clock.frame, Some(Arc::clone(overlap)));
                self.stats.frames_rendered += 1;
                self.stats.render.record(Instant::now() - tick_at);
                TickOutcome::Rendered
            }
        };

        // Non-blocking: lets completed map callbacks run, waits for nothing.
        self.poll_and_drain(gpu);
        outcome
    }

    /// Poll (non-blocking) and publish anything that has completed.
    ///
    /// Worth calling during the idle wait between frames, not only once per tick:
    /// a readback becomes *usable* only when the loop observes it, so polling only
    /// once per frame quantizes readback latency to a whole frame period. Measured
    /// on real hardware, that alone accounted for ~22 ms of the ~23 ms
    /// tick→publish — the GPU's own readback finishes in tens of microseconds.
    pub fn poll_and_drain(&mut self, gpu: &crate::Gpu) {
        self.ring.poll(gpu);
        self.drain_ready();
    }

    /// Publish any completed readback. Field-destructured so the ring and the
    /// producer are borrowed disjointly.
    fn drain_ready(&mut self) {
        let RenderLoop {
            ring,
            producer,
            generation,
            stats,
            preview,
            zone_active,
            ..
        } = self;
        let now = Instant::now();
        while let Some((readback, tick_to_publish)) = ring.consume_ready(now, |meta, data| {
            producer.set_layout_generation(*generation);
            // The snapshot THIS frame was rendered under — never the current one.
            producer.set_overlap(meta.overlap.clone());
            producer.publish(|px| {
                // f32 → f16 fused into the copy the handoff requires anyway.
                for (dst, &src) in px.iter_mut().zip(data.iter()) {
                    *dst = f16::from_f32(src);
                }
            });

            // The preview goes SECOND and pays for itself: its own conversion, its
            // own copy. The output publish above is untouched by its presence.
            if let Some(preview) = preview.as_mut() {
                preview.set_layout_generation(*generation);
                preview.set_overlap(meta.overlap.clone());
                preview.publish(zone_active, meta.frame, |px| {
                    for (dst, &src) in px.iter_mut().zip(data.iter()) {
                        *dst = f16::from_f32(src);
                    }
                });
            }
            (
                meta.readback_latency(),
                now.saturating_duration_since(meta.tick_at),
            )
        }) {
            stats.frames_published += 1;
            stats.readback.record(readback);
            stats.tick_to_publish.record(tick_to_publish);
        }
    }
}

/// How finely the loop polls for completed readbacks while waiting for the next
/// frame deadline. A readback is only usable once observed, so this bounds the
/// observation delay; at one poll per frame it would instead be a whole frame
/// period. Cheap: `PollType::Poll` is a non-blocking check, not a spin.
const IDLE_POLL_INTERVAL: Duration = Duration::from_micros(500);

/// Supplies live zone state at each frame boundary — the mirror of the output
/// side's `FlatParamSource`. In the engine this reads the Parameter Tree through
/// wait-free handles; in a test it can read shared state.
///
/// The loop refreshes ONE owned `ZoneStates` in place, so nothing allocates per
/// frame, and the counter always syncs against the state as of this boundary.
pub trait ZoneStateSource: Send {
    fn read_into(&self, into: &mut ZoneStates);
}

/// How to drive [`run_render_loop`].
pub struct RenderLoopConfig {
    /// Render cadence — the rate of the fastest active output (§1.4). Rendering
    /// faster than the fastest consumer wastes frames; slower starves it.
    pub target_rate_hz: u32,
    /// Stop after this long (probe/measurement runs). `None` runs until `stop`.
    pub run_for: Option<Duration>,
    /// Cooperative stop flag.
    pub stop: Arc<AtomicBool>,
    /// Where live zone state comes from each frame. `None` keeps whatever the
    /// caller's initial `ZoneStates` held.
    pub zone_source: Option<Box<dyn ZoneStateSource>>,
    /// **Freeze the show content** (M7 §7 / S9). While set, the master clock is
    /// paused: show time (and in-progress transitions) hold, so effects stop
    /// advancing and the rig holds its last frame — while the output thread keeps
    /// emitting, which is what makes the editor's Highlight possible. Cleared = the
    /// clock runs. `None`-equivalent default is "never frozen".
    pub freeze: Arc<AtomicBool>,
    /// Render cadence **while frozen** (the editor is open). Frozen content does not
    /// animate, so re-rendering the identical frame at `target_rate_hz` is pure waste —
    /// and on an integrated GPU that continuous work competes with the editor webview's
    /// own compositing, which reads as general UI sluggishness. Dropping to a low rate
    /// while frozen frees the GPU; the output thread keeps emitting at its own fixed rate
    /// (Highlight is applied there, §7, so it is unaffected), and any zone-state change
    /// is still reflected within one slow frame. Clamped to at most `target_rate_hz`.
    pub frozen_rate_hz: u32,
}

impl Default for RenderLoopConfig {
    fn default() -> Self {
        Self {
            target_rate_hz: 44,
            run_for: None,
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
            freeze: Arc::new(AtomicBool::new(false)),
            frozen_rate_hz: 10,
        }
    }
}

/// Drive the render loop: own the clock, sync the counter at each frame boundary,
/// and render at the target rate until stopped.
pub fn run_render_loop(
    gpu: &crate::Gpu,
    engine: &mut RenderLoop,
    clock: &mut MasterClock,
    counter: &mut OverlapCounter,
    mut zones: ZoneStates,
    content: &mut impl FrameContent,
    config: RenderLoopConfig,
) -> RenderStats {
    let fast_period = Duration::from_nanos(1_000_000_000 / config.target_rate_hz.max(1) as u64);
    // Never faster than the target rate; a mis-set frozen rate cannot speed render up.
    let frozen_hz = config
        .frozen_rate_hz
        .max(1)
        .min(config.target_rate_hz.max(1));
    let frozen_period = Duration::from_nanos(1_000_000_000 / frozen_hz as u64);
    let start = Instant::now();
    // Incremental deadline: the period is chosen per frame from the freeze state (a
    // constant `start + period * frame` cannot express a rate that changes at runtime).
    let mut deadline = start;

    loop {
        // The cadence for THIS frame's wait: slow while frozen (editor open), full rate
        // otherwise. Read once so the wait and the advance agree.
        let frozen = config.freeze.load(Ordering::Relaxed);
        let period = if frozen { frozen_period } else { fast_period };
        deadline += period;
        // Resync if we fell behind (a slow frame, or the rate just changed on
        // freeze/unfreeze) so the loop never bursts back-to-back frames to catch up.
        let now = Instant::now();
        if deadline < now {
            deadline = now + period;
        }

        // Wait for the next deadline, but spend the idle time polling for
        // completed readbacks instead of sleeping through them.
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            engine.poll_and_drain(gpu);
            std::thread::sleep(IDLE_POLL_INTERVAL.min(deadline - now));
        }

        // Frame boundary: match the clock's run/pause to the freeze flag (§7 / S9),
        // then advance time. When frozen, `tick()` returns the held show time, so the
        // content re-renders the same frame and the rig holds — output stays live.
        if frozen {
            clock.pause();
        } else {
            clock.play();
        }
        let snapshot_clock = clock.tick();
        if let Some(source) = &config.zone_source {
            source.read_into(&mut zones);
        }
        counter.sync(&zones);
        let overlap = counter.snapshot();

        engine.tick(gpu, &snapshot_clock, &zones, &overlap, content);

        if config.stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(limit) = config.run_for {
            if Instant::now() - start >= limit {
                break;
            }
        }
    }
    engine.stats()
}

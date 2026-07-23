//! The preview loop: a fixed-interval thread that encodes and ships preview frames
//! (CLAUDE.md M6).
//!
//! # This thread is deliberately NOT real-time — do not "fix" that
//!
//! M1's output thread and M5's render thread elevate their scheduling priority
//! (MMCSS / SCHED_FIFO / etc.) and may pin a core, because a late DMX frame is
//! **visible on the fixtures** and desyncs audio/MIDI — they are on the §1.5
//! latency budget. **The preview is not.** It draws a picture on a screen for a
//! human to glance at; a late or dropped preview frame costs nothing but a
//! slightly stale thumbnail. If the preview competed with output or render for a
//! reserved core or a real-time slot, it could *cause* the very jitter those
//! threads exist to avoid — the preview would be actively harmful.
//!
//! So this loop runs at **normal priority, never elevated, never core-pinned**. It
//! must always lose to output and render. Anyone tempted to copy M1's
//! `elevate_current_thread` here because "it's also a fixed-interval thread"
//! should stop: the fixed interval is for a steady picture, not for timing
//! accuracy, and the whole point is that this thread is *subordinate*. Coarse
//! `sleep`-based timing is not just acceptable, it is correct.
//!
//! # Nothing is watching ⇒ nothing is spent
//!
//! When no consumer is connected the loop does **zero** per-frame work: no sample,
//! no sRGB, no serialization. Not "cheap" — zero. The gate is
//! [`PreviewSink::has_consumers`]; the encoder is only reached past it. With no
//! sink attached at all, the same holds trivially. This is asserted by the P3
//! tests (`no_sink_never_encodes`, `no_consumer_never_encodes`).
//!
//! # Newest-wins, and it emits even without a new render
//!
//! Each tick catches up to the latest rendered frame (newest-wins, like the output
//! handoff) but does **not** require a new one: the flat parameters (master/zone
//! dimmer + colour) are read live every tick, so a fader moves the preview even
//! while the render thread is idle — the §1.5 shortcut, seen on screen. Under
//! backpressure the loop drops frames rather than blocking anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use prisma_output::plan::FlatParams;
use prisma_output::scheduler::FlatParamSource;

use crate::descriptor::PreviewDescriptor;
use crate::encode::encode_frame;
use crate::frame::PreviewConsumer;
use crate::plan::PreviewPlan;

/// Where encoded preview bytes go. Implemented by the P4 transports: an in-process
/// sink for tests and the probe, and a WebSocket server for the browser viewer.
///
/// The trait is intentionally tiny. The sink caches the descriptor and replays it
/// to each newly-connected consumer, and relays frames to all current consumers;
/// the loop knows nothing about connections beyond [`has_consumers`](Self::has_consumers).
pub trait PreviewSink: Send {
    /// Whether at least one consumer is currently connected and wants frames.
    ///
    /// **This is the zero-cost gate.** When it returns `false` the loop encodes
    /// nothing at all — see the module docs. A sink must return `false` whenever
    /// nobody is watching, so that an idle preview truly costs nothing.
    fn has_consumers(&self) -> bool;

    /// Hand the sink the current descriptor (JSON). Called once when the loop
    /// starts serving, and again if the layout generation changes. The sink caches
    /// it and sends it to every consumer that connects later.
    fn send_descriptor(&mut self, descriptor_json: &str);

    /// Relay one encoded binary frame to every current consumer. Must not block the
    /// loop; a slow consumer is dropped, never waited on.
    fn send_frame(&mut self, frame: &[u8]);
}

/// What one [`PreviewLoop::tick`] did. Useful for tests and the probe; the running
/// loop only accumulates [`PreviewStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewOutcome {
    /// A frame was encoded and handed to the sink.
    Sent,
    /// No sink is attached — nothing to do, nothing encoded.
    NoSink,
    /// A sink is attached but no consumer is connected — nothing encoded.
    NoConsumer,
    /// A consumer is connected but no frame has been rendered yet.
    NoFrame,
    /// The held frame's layout generation did not match the plan; skipped.
    GenerationMismatch,
}

/// Counters the loop accumulates. `frames_encoded` is the evidence for the
/// zero-cost property: it stays `0` whenever nothing is watching.
#[derive(Debug, Clone, Copy, Default)]
pub struct PreviewStats {
    pub ticks: u64,
    pub frames_encoded: u64,
    pub frames_sent: u64,
    pub skipped_no_consumer: u64,
    pub skipped_no_frame: u64,
    pub generation_mismatches: u64,
}

/// Owns the per-tick machinery: the plan, the descriptor, the frame handoff's
/// consumer half, the live flat-parameter source, and reusable buffers.
pub struct PreviewLoop {
    plan: PreviewPlan,
    descriptor_json: String,
    descriptor_sent: bool,
    consumer: PreviewConsumer,
    flat_source: Box<dyn FlatParamSource>,
    /// Refilled from live state each tick; never reallocated.
    flat: FlatParams,
    /// The encode scratch buffer; cleared and refilled each tick, never reallocated.
    buf: Vec<u8>,
    sink: Option<Box<dyn PreviewSink>>,
    stats: PreviewStats,
}

impl PreviewLoop {
    /// Build a loop for one compiled patch. `plan` and `descriptor` must come from
    /// the same [`Patch::compile`](prisma_patch::Patch::compile); `flat_source` is
    /// the live master/zone parameters (in the engine, the `LiveParams` binding
    /// shared with the render and output stages).
    pub fn new(
        plan: PreviewPlan,
        descriptor: PreviewDescriptor,
        consumer: PreviewConsumer,
        flat_source: Box<dyn FlatParamSource>,
    ) -> PreviewLoop {
        let zone_count = descriptor.zones.len();
        PreviewLoop {
            plan,
            descriptor_json: descriptor.to_json(),
            descriptor_sent: false,
            consumer,
            flat_source,
            flat: FlatParams::identity(zone_count),
            buf: Vec::new(),
            sink: None,
            stats: PreviewStats::default(),
        }
    }

    /// Attach the transport the loop ships frames to. Without one the loop runs but
    /// encodes nothing (the no-sink zero-cost case).
    pub fn attach_sink(&mut self, sink: Box<dyn PreviewSink>) {
        self.sink = Some(sink);
        // A fresh sink has not seen the descriptor yet.
        self.descriptor_sent = false;
    }

    pub fn stats(&self) -> PreviewStats {
        self.stats
    }

    /// Encode and ship one frame — or skip, at zero cost, when nothing is watching.
    ///
    /// The order of the gates is the whole point: `has_consumers` is checked
    /// **before** the consumer is even advanced or the flat parameters read, so an
    /// unwatched preview never samples a pixel.
    pub fn tick(&mut self) -> PreviewOutcome {
        let PreviewLoop {
            plan,
            descriptor_json,
            descriptor_sent,
            consumer,
            flat_source,
            flat,
            buf,
            sink,
            stats,
        } = self;

        stats.ticks += 1;

        // Gate 1: no transport at all.
        let Some(sink) = sink.as_mut() else {
            return PreviewOutcome::NoSink;
        };
        // Gate 2: transport present but nobody watching. This is the zero-cost
        // path — we return before touching the consumer, the flat source, or the
        // encoder.
        if !sink.has_consumers() {
            stats.skipped_no_consumer += 1;
            return PreviewOutcome::NoConsumer;
        }

        // A newly-attached sink (or a new generation, later) needs the descriptor
        // once; it caches and replays it to each consumer that connects.
        if !*descriptor_sent {
            sink.send_descriptor(descriptor_json);
            *descriptor_sent = true;
        }

        // Catch up to the newest rendered frame, but do not require a new one: the
        // flat parameters move without a re-render, so we re-encode the held frame
        // with live values every tick.
        consumer.advance();
        let frame = consumer.current();
        if frame.frame_id == 0 {
            stats.skipped_no_frame += 1;
            return PreviewOutcome::NoFrame;
        }

        // Live read of master/zone dimmer + colour — the same source the output
        // stage reads, so the preview and the wire move together.
        flat_source.read_into(flat);

        // The wire sequence is a monotonic count of shipped frames, so the consumer
        // can tell a real drop from a normal newest-wins skip (see `encode_frame`).
        let sequence = stats.frames_encoded;
        match encode_frame(plan, frame, flat, sequence, buf) {
            Ok(()) => {
                stats.frames_encoded += 1;
                sink.send_frame(buf);
                stats.frames_sent += 1;
                PreviewOutcome::Sent
            }
            Err(_) => {
                // The held frame was rendered under a different layout than the
                // plan describes (a patch edit is in flight). Skip it; live-patch
                // handling that recompiles the plan is engine-assembly work (M7).
                stats.generation_mismatches += 1;
                PreviewOutcome::GenerationMismatch
            }
        }
    }
}

/// How to drive [`run_preview_loop`].
pub struct PreviewLoopConfig {
    /// Preview cadence. Default 30 Hz. The caller should clamp it to at most the
    /// render rate — emitting faster than frames are produced only re-ships the
    /// same picture with fresh flat parameters, which is occasionally useful (a
    /// fader moving while render is idle) but rarely worth more than ~44 Hz.
    pub target_rate_hz: u32,
    /// Stop after this long (probe/measurement runs). `None` runs until `stop`.
    pub run_for: Option<Duration>,
    /// Cooperative stop flag.
    pub stop: Arc<AtomicBool>,
}

impl Default for PreviewLoopConfig {
    fn default() -> Self {
        Self {
            target_rate_hz: 30,
            run_for: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Drive the preview loop on the current thread until stopped.
///
/// **No priority elevation, no core pinning** — unlike `run_output_loop`, and on
/// purpose (see the module docs). Timing is a plain absolute-deadline sleep: coarse
/// is correct here, because the preview is a picture, not a signal on the §1.5
/// budget.
pub fn run_preview_loop(mut engine: PreviewLoop, config: PreviewLoopConfig) -> PreviewStats {
    let period_ns = (1_000_000_000u64 / config.target_rate_hz.max(1) as u64).max(1);
    let start = Instant::now();
    let mut tick: u64 = 0;

    loop {
        // Absolute deadline so drift cannot accumulate; a plain sleep (no spin) is
        // fine because a few hundred microseconds of jitter is invisible in a
        // preview and must never be bought with CPU that output/render could use.
        let deadline = start + Duration::from_nanos(period_ns.saturating_mul(tick));
        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }

        engine.tick();
        tick += 1;

        if config.stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(run_for) = config.run_for {
            if Instant::now().duration_since(start) >= run_for {
                break;
            }
        }
    }

    engine.stats()
}

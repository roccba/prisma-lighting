//! Fixed-interval output scheduler (CLAUDE.md §1.4, §3.3).
//!
//! One high-priority thread drives all outputs off a single base tick. Timing
//! accuracy is achieved with two mechanisms:
//!
//!   1. **Absolute-deadline scheduling.** Each tick targets `start + n*period`,
//!      an absolute instant — never `sleep(period)`. A late tick does not push
//!      the following ones; drift cannot accumulate.
//!   2. **Park + spin hybrid.** The thread sleeps until a small margin before
//!      the deadline (coarse OS sleep overshoots by hundreds of µs), then
//!      busy-spins the remainder for sub-tick accuracy.
//!
//! The thread also tries to elevate its scheduling priority and records the
//! mode it **actually** achieved ([`SchedulingMode`]) — queryable state, so an
//! operator (or the probe/UI) can tell a real-time thread from a best-effort
//! fallback without reading logs.
//!
//! Slower outputs transmit on integer divisors of the base tick ([`RatePlan`]).
//! A tick with no fresh frame resends the last one (timing over freshness).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::affinity::pin_current_thread;
use crate::frame::{Consumer, FrameConsumer};
use crate::highlight::{blink_on, HighlightFrame, HighlightState};
use crate::plan::FlatParams;
use crate::priority::elevate_current_thread;
use crate::rate::RatePlan;
use crate::router::OutputRouter;
use crate::timing::{ProbeReport, TimingProbe};
use crate::transport::UdpOutput;

/// Configuration for a scheduler run.
pub struct SchedulerConfig {
    /// How long before each deadline to stop the coarse sleep and busy-spin.
    pub spin_margin: Duration,
    /// A tick whose interval exceeds `target + late_threshold` counts as late.
    pub late_threshold: Duration,
    /// Stop after this long (probe/measurement runs). `None` runs until `stop`.
    pub run_for: Option<Duration>,
    /// Cooperative stop flag.
    pub stop: Arc<AtomicBool>,
    /// Pin the output thread to this core. Core reservation attacks the root
    /// cause of jitter (CPU oversubscription) and, unlike RT scheduling, needs
    /// no privileges on Linux or Windows.
    pub pin_to_core: Option<usize>,
    /// If set, broadcast one `ArtSync` to this address after each frame's
    /// `ArtDmx` packets, so sync-capable nodes latch every universe on the same
    /// output frame (tear-free multi-node output, §5.1). `None` = no sync; nodes
    /// latch each `ArtDmx` on receipt. Usually the Art-Net broadcast address.
    pub artsync_target: Option<SocketAddr>,
    /// Number of zones the flat-parameter buffer is sized for; must match the
    /// zone slots the frames' overlap snapshots reference.
    pub zone_count: usize,
    /// Where each output frame's live master/zone dimmer + colour come from.
    /// `None` leaves them at unity, which is exactly the M1 behaviour.
    pub flat_source: Option<Box<dyn FlatParamSource>>,
    /// The editor's live Highlight selection (§7 / S9), read wait-free each tick.
    /// `None` = no Highlight capability (M1/M5 behaviour: never overrides output).
    pub highlight_consumer: Option<Consumer<HighlightState>>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            spin_margin: Duration::from_micros(1500),
            late_threshold: Duration::from_micros(500),
            run_for: None,
            zone_count: 0,
            flat_source: None,
            highlight_consumer: None,
            stop: Arc::new(AtomicBool::new(false)),
            pin_to_core: None,
            artsync_target: None,
        }
    }
}

/// Park (coarse sleep) then spin until `deadline`. If the deadline has already
/// passed (we were late), return immediately.
#[inline]
fn wait_until(deadline: Instant, spin_margin: Duration) {
    let now = Instant::now();
    if let Some(remaining) = deadline.checked_duration_since(now) {
        if remaining > spin_margin {
            std::thread::sleep(remaining - spin_margin);
        }
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
    }
}

/// Run the fixed-interval output loop on the current thread until the configured
/// duration elapses or the stop flag is set. Returns the timing report,
/// including the scheduling mode achieved and per-output link state.
///
/// This consumes its inputs so ownership is unambiguous: the loop is the sole
/// reader of the frame handoff and the sole writer to the sockets.
/// Supplies the live flat parameters for each output frame (§5.2 step 6).
///
/// Implementations read from live state — in the engine, wait-free
/// [`ParamHandle`](prisma_core::ParamHandle) reads, one per zone plus master. The
/// scheduler calls this **once per output frame** into a preallocated
/// [`FlatParams`], so nothing allocates on the hot path and a fader reaches the
/// wire on the next output tick with no re-render.
pub trait FlatParamSource: Send {
    fn read_into(&self, into: &mut FlatParams);
}

pub fn run_output_loop(
    mut consumer: FrameConsumer,
    mut router: OutputRouter,
    mut transports: Vec<UdpOutput>,
    rate: RatePlan,
    config: SchedulerConfig,
) -> ProbeReport {
    assert_eq!(
        transports.len(),
        router.universe_count(),
        "one transport per universe"
    );

    let period = rate.base_period();

    // Optional core pinning (opt-in power-user knob, off by default — see
    // `crate::affinity` for why it is not the default). Real-time elevation is
    // the actual mitigation and stacks on top if a core was pinned.
    let pinned_core = config.pin_to_core.and_then(pin_current_thread);
    let mode = elevate_current_thread(period);
    let period_ns = period.as_nanos() as u64;
    let expected_samples = config
        .run_for
        .map(|d| (d.as_secs_f64() * rate.base_hz() as f64) as usize + 16)
        .unwrap_or(1 << 16);
    let mut probe = TimingProbe::with_capacity(period, config.late_threshold, expected_samples);

    // Optional ArtSync: a dedicated, isolated sender (same link-state/rebind
    // safety as a universe output) and a constant 14-byte packet built once.
    let mut sync_output = config.artsync_target.map(UdpOutput::to_target);
    let mut artsync_packet = Vec::new();
    if sync_output.is_some() {
        crate::packet::artnet::write_artsync(&mut artsync_packet);
    }

    let start = Instant::now();
    let mut tick: u64 = 0;
    // Preallocated once; refilled in place every frame from live state.
    let mut flat = FlatParams::identity(config.zone_count);
    // The Highlight selection, read wait-free each tick (§7 / S9). Owned here so the
    // borrow of `current()` does not tangle with `config`.
    let mut highlight = config.highlight_consumer;

    loop {
        let deadline = start + Duration::from_nanos(period_ns.saturating_mul(tick));
        wait_until(deadline, config.spin_margin);

        let now = Instant::now();

        // Advance the handoff once per tick; false means resend the last frame.
        let fresh = consumer.advance();
        let frame = consumer.current();
        let produced_at = frame.produced_at;
        let master_generation = frame.layout_generation;
        let pixels = &frame.pixels;

        // Live read, once per output frame. The frame's overlap snapshot decides
        // WHICH pixels these apply to; the values themselves are always current.
        if let Some(source) = &config.flat_source {
            source.read_into(&mut flat);
        }

        // Highlight (§7 / S9): read the latest selection wait-free, resolve the blink
        // phase from wall-clock (never the show clock, so it works while content is
        // frozen). `None` unless something is highlighted right now.
        let highlight_frame = highlight.as_mut().and_then(|consumer| {
            consumer.advance();
            let state = consumer.current();
            state.active.then(|| HighlightFrame {
                mask: &state.mask,
                on: blink_on(now.saturating_duration_since(start), state.blink),
            })
        });

        let output_frame = crate::router::OutputFrame {
            master: pixels,
            generation: master_generation,
            overlap: frame.overlap.as_deref(),
            flat: &flat,
            highlight: highlight_frame.as_ref(),
        };

        for (i, transport) in transports.iter_mut().enumerate() {
            if rate.should_send(i, tick) {
                // Encode borrows the router; use it immediately, then release so
                // the next universe can encode. Sending is isolated per output.
                match router.encode_universe_with(i, &output_frame) {
                    Ok(bytes) => {
                        if !transport.send(bytes) {
                            probe.record_drop();
                        }
                    }
                    // Plan/layout generation disagree: emitting would paint wrong
                    // pixels. Skip the whole frame (all universes share this
                    // generation, so the first refusal is decisive) — the outputs
                    // hold their last frame rather than go corrupt.
                    Err(_) => {
                        probe.record_generation_mismatch();
                        break;
                    }
                }
            }
        }

        // Sync latch after this frame's ArtDmx packets, so nodes present every
        // universe on the same output frame. Isolated: a failed sync send never
        // stops the loop.
        if let Some(sync) = sync_output.as_mut() {
            if !sync.send(&artsync_packet) {
                probe.record_drop();
            }
        }

        probe.record_tick(now, produced_at, fresh);
        tick += 1;

        if config.stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(run_for) = config.run_for {
            if now.duration_since(start) >= run_for {
                break;
            }
        }
    }

    let outputs = transports.iter().map(|t| t.status()).collect();
    probe.report(mode, pinned_core, outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_handoff;
    use crate::universe::Universe;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn runs_a_short_loop_and_reports_samples() {
        let (mut producer, consumer) = frame_handoff(6);
        // Stage one frame so the first tick has content.
        producer.publish(|px| px.fill(half::f16::from_f32(0.5)));

        let router = OutputRouter::new(vec![Universe::new(1, 0, 6)]);
        let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        // A real receiver so sends succeed.
        let rx = std::net::UdpSocket::bind(target).unwrap();
        let transports = vec![UdpOutput::to_target(rx.local_addr().unwrap())];

        let rate = RatePlan::compute(&[100]); // 100 Hz keeps the test quick
        let config = SchedulerConfig {
            run_for: Some(Duration::from_millis(200)),
            ..Default::default()
        };

        let report = run_output_loop(consumer, router, transports, rate, config);
        // ~20 ticks at 100 Hz over 200 ms; allow slack for scheduling.
        assert!(report.sample_count >= 10, "got {}", report.sample_count);
        assert_eq!(report.drops, 0);
        assert_eq!(report.outputs.len(), 1);
    }

    #[test]
    fn absolute_deadline_keeps_mean_near_target() {
        let (mut producer, consumer) = frame_handoff(2);
        producer.publish(|px| px.fill(half::f16::from_f32(0.5)));
        let router = OutputRouter::new(vec![Universe::new(1, 0, 2)]);
        let transports = vec![UdpOutput::to_target(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9), // nothing listening: sends may drop, timing still measured
        )];
        let rate = RatePlan::compute(&[200]);
        let config = SchedulerConfig {
            run_for: Some(Duration::from_millis(250)),
            ..Default::default()
        };
        let report = run_output_loop(consumer, router, transports, rate, config);

        // The mean interval should track the 5 ms target within a loose bound
        // (loose because CI machines are noisy; the probe binary reports the
        // real distribution).
        let target_ns = report.target_period.as_nanos() as u64;
        let mean = report.mean_interval_ns;
        assert!(
            mean > target_ns * 8 / 10 && mean < target_ns * 12 / 10,
            "mean {mean} ns strayed from target {target_ns} ns"
        );
    }

    #[test]
    fn emits_artsync_alongside_dmx_when_configured() {
        // The virtual sink counts ArtDmx and ArtSync separately, so it confirms
        // sync packets actually go out with the frame's DMX.
        let handle =
            crate::sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let target = handle.local_addr();

        let (mut producer, consumer) = frame_handoff(6);
        producer.publish(|px| px.fill(half::f16::from_f32(0.5)));
        let router = OutputRouter::new(vec![Universe::new(1, 0, 6)]);
        let transports = vec![UdpOutput::to_target(target)];
        let rate = RatePlan::compute(&[100]);
        let config = SchedulerConfig {
            run_for: Some(Duration::from_millis(200)),
            artsync_target: Some(target),
            ..Default::default()
        };

        run_output_loop(consumer, router, transports, rate, config);
        std::thread::sleep(Duration::from_millis(30));
        let state = handle.stop_and_collect();

        assert!(state.artdmx_packets > 0, "no ArtDmx reached the sink");
        assert!(state.artsync_packets > 0, "no ArtSync reached the sink");
        // One sync per frame: sync count tracks the DMX frame count closely.
        assert!(
            state.artsync_packets >= state.artdmx_packets - 2,
            "expected ~one ArtSync per frame: dmx={} sync={}",
            state.artdmx_packets,
            state.artsync_packets
        );
    }
}

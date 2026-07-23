//! M1 milestone acceptance test — the whole "done when", not just the sink in
//! isolation (CLAUDE.md §6, M1):
//!
//!   "an RGB array written in code appears on physical fixtures **and** on the
//!    virtual sink at a stable rate, verified with a packet capture showing
//!    consistent inter-packet timing, and the timing probe reports inter-packet
//!    jitter."
//!
//! Physical fixtures need hardware, so the software virtual sink is the
//! spec-sanctioned stand-in. This test drives the *real* pipeline end to end:
//! a code-written RGB array → frame handoff → router → fixed-interval output
//! thread → UDP → virtual sink, then checks content, rate, sequence integrity,
//! and that the probe produced timing numbers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use prisma_output::frame::frame_handoff;
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::sink;
use prisma_output::transport::UdpOutput;
use prisma_output::universe::Universe;

const UNIVERSES: usize = 4;
const CHANNELS: usize = 512;

#[test]
fn m1_rgb_array_reaches_sink_at_stable_rate_with_measured_timing() {
    // 1. The software virtual sink stands in for the physical rig.
    let sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = sink.local_addr();

    // 2. An RGB array written in code — a distinctive value per (universe, byte)
    //    so a mis-routed or corrupted universe is detectable.
    let mut master = vec![0u8; UNIVERSES * CHANNELS];
    for u in 0..UNIVERSES {
        for j in 0..CHANNELS {
            master[u * CHANNELS + j] = ((u + 1) * 7 + j) as u8;
        }
    }

    // 3. Push it through the real output pipeline. The canonical buffer is
    //    linear-light f16; the identity-gamma path round-trips each byte b as
    //    f16(b/255) back to exactly b on the wire (verified for all 256 values),
    //    so `master` stays the byte-for-byte expected sink content below.
    let (mut producer, consumer) = frame_handoff(UNIVERSES * CHANNELS);
    let published: Vec<half::f16> = master
        .iter()
        .map(|&b| half::f16::from_f32(b as f32 / 255.0))
        .collect();
    producer.publish(move |px| px.copy_from_slice(&published));

    let maps: Vec<Universe> = (0..UNIVERSES)
        .map(|u| Universe::new((u + 1) as u16, u * CHANNELS, CHANNELS))
        .collect();
    let router = OutputRouter::new(maps);
    let transports: Vec<UdpOutput> = (0..UNIVERSES)
        .map(|_| UdpOutput::to_target(target))
        .collect();
    let rate = RatePlan::compute(&[44; UNIVERSES]);
    let config = SchedulerConfig {
        run_for: Some(Duration::from_millis(1500)),
        ..Default::default()
    };

    let report = run_output_loop(consumer, router, transports, rate, config);
    let state = sink.stop_and_collect();

    // --- The "done when", asserted ---

    // (probe reports inter-packet jitter) — ~66 ticks over 1.5 s at 44 Hz.
    assert!(
        report.sample_count > 30,
        "probe collected too few samples: {}",
        report.sample_count
    );
    assert_eq!(report.drops, 0, "output dropped frames");

    // (an RGB array appears on the virtual sink) — all universes, byte-for-byte.
    assert_eq!(
        state.universes.len(),
        UNIVERSES,
        "not all universes reached the sink"
    );
    for u in 0..UNIVERSES {
        let port = (u + 1) as u16;
        let us = state
            .universes
            .get(&port)
            .unwrap_or_else(|| panic!("universe {port} missing at sink"));

        assert_eq!(
            us.data.as_slice(),
            &master[u * CHANNELS..(u + 1) * CHANNELS],
            "universe {port} content differs from what was written"
        );
        assert_eq!(us.sequence_gaps, 0, "universe {port} had sequence gaps");

        // (at a stable rate) — ~66 packets expected; loose lower bound, and the
        // receive-side mean interval must sit near the 22.7 ms target.
        assert!(
            us.packets >= 40,
            "universe {port} only received {} packets",
            us.packets
        );
        let mean = us.mean_interval().expect("intervals recorded");
        assert!(
            mean > Duration::from_millis(18) && mean < Duration::from_millis(28),
            "universe {port} receive-side mean interval {mean:?} strayed from ~22.7 ms"
        );
    }

    // Nothing foreign or malformed reached the sink.
    assert_eq!(state.non_artnet, 0);
    assert_eq!(state.malformed, 0);
}

//! `probe` — output-timing measurement tool.
//!
//! Drives the fixed-interval output loop against loopback Art-Net and prints a
//! [`ProbeReport`]: achieved scheduling mode, inter-send jitter, the interval
//! distribution, and produced→wire latency. This is the M1 "done when" evidence
//! that timing is acceptable, not just that tests pass (CLAUDE.md §6, §7).
//!
//! Usage: `probe [seconds] [base_hz] [universes] [pin_core]`  (defaults: 20 44 4, no pin)
//!   pin_core: pin the output thread to this core id (core reservation). Omit
//!   to leave it floating.
//!   `PRISMA_NODES=N`: spread the universes over N Art-Net nodes (loopback IPs
//!   127.0.1.1…), to measure the per-node `send_to` routing (docs/DEVICES-SPEC.md §5).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use prisma_core::{ArtNetNode, NetworkConfig, NodeOrigin};
use prisma_output::frame::frame_handoff;
use prisma_output::rate::RatePlan;
use prisma_output::route::DestinationTable;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::transport::UdpOutput;
use prisma_output::universe::Universe;

const CHANNELS_PER_UNIVERSE: usize = 512;
const ARTNET_PORT: u16 = 6454;

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let base_hz: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(44);
    let universes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let pin_core: Option<usize> = args.next().and_then(|s| s.parse().ok());
    // Spin margin in microseconds (how long before each deadline to stop the
    // coarse sleep and busy-spin). Set >= the tick period to busy-poll the whole
    // interval, keeping a reserved core hot (avoids C-state wakeup latency).
    let spin_us: Option<u64> = std::env::var("PRISMA_SPIN_US")
        .ok()
        .and_then(|s| s.parse().ok());

    let pixel_len = universes * CHANNELS_PER_UNIVERSE;

    // Frame handoff + a producer thread that stages frames at ~60 Hz, faster
    // than the 44 Hz output, so the loop exercises both fresh frames and the
    // newest-wins/resend paths under realistic contention.
    let (mut producer, consumer) = frame_handoff(pixel_len);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_producer = Arc::clone(&stop);
    // When reserving a core for output, keep the producer off it (and off its
    // SMT sibling): pin the producer to core 0 so the reserved physical core is
    // truly free of our own threads.
    let pin_producer_away = pin_core.is_some();
    let producer_thread = thread::spawn(move || {
        if pin_producer_away {
            core_affinity::set_for_current(core_affinity::CoreId { id: 0 });
        }
        let mut phase: u8 = 0;
        while !stop_producer.load(Ordering::Relaxed) {
            // A moving linear-light ramp as a test pattern; content is irrelevant
            // to the timing measurement, only that a fresh frame is staged.
            let v = half::f16::from_f32(phase as f32 / 255.0);
            producer.publish(|px| px.fill(v));
            phase = phase.wrapping_add(1);
            thread::sleep(Duration::from_micros(16_667)); // ~60 Hz
        }
    });

    // One universe per output, all to loopback Art-Net (nothing needs to listen
    // for a timing measurement; the kernel drops them).
    let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ARTNET_PORT);
    let nodes: usize = std::env::var("PRISMA_NODES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut maps = Vec::with_capacity(universes);
    for u in 0..universes {
        maps.push(Universe::new(
            (u + 1) as u16,
            u * CHANNELS_PER_UNIVERSE,
            CHANNELS_PER_UNIVERSE,
        ));
    }
    let port_addresses: Vec<u16> = maps.iter().map(|m| m.port_address).collect();
    let destinations = if nodes == 0 {
        DestinationTable::all_to(target, universes)
    } else {
        // Contiguous blocks of universes per node, like real boards.
        let per = universes.div_ceil(nodes).max(1);
        let net = NetworkConfig {
            initialized: true,
            artnet_enabled: true,
            artsync: false,
            interface: None,
            nodes: (0..nodes)
                .map(|k| ArtNetNode {
                    id: uuid::Uuid::nil(),
                    name: format!("node {k}"),
                    ip: Ipv4Addr::new(127, 0, 1 + (k / 250) as u8, 1 + (k % 250) as u8),
                    long_name: String::new(),
                    ports: ((1 + k * per) as u16..=(k * per + per) as u16).collect(),
                    universes: Vec::new(),
                    enabled: true,
                    unicast: true,
                    full_frames: false,
                    origin: NodeOrigin::Manual,
                })
                .collect(),
        };
        DestinationTable::compile(&port_addresses, &net, None, target)
    };
    let router = OutputRouter::new(maps);
    let rate = RatePlan::compute(&vec![base_hz; universes]);

    let mut config = SchedulerConfig {
        run_for: Some(Duration::from_secs(seconds)),
        pin_to_core: pin_core,
        ..Default::default()
    };
    if let Some(us) = spin_us {
        config.spin_margin = Duration::from_micros(us);
    }

    println!(
        "probe: {universes} universes @ {base_hz} Hz for {seconds}s → {}\n\
         (producer staging ~60 Hz; measuring output-thread cadence)\n",
        if nodes == 0 {
            target.to_string()
        } else {
            format!("{nodes} nodes")
        }
    );

    let report = run_output_loop(
        consumer,
        router,
        UdpOutput::to_target(target),
        destinations,
        rate,
        config,
    );

    stop.store(true, Ordering::Relaxed);
    producer_thread.join().ok();

    println!("{report}");
}

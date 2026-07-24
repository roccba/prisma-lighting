//! End-to-end: a canonical (pixel-domain) master buffer, a patch compiled to an
//! [`OutputPlan`], and the real fixed-interval output loop — proving M2's
//! patch→wire binding and, specifically, the layout-generation guard the router
//! enforces on every frame.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use prisma_output::frame::frame_handoff;
use prisma_output::plan::OutputPlan;
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::sink;
use prisma_output::transport::UdpOutput;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, LayoutGeneration, Patch,
    Pixel,
};
use uuid::Uuid;

/// Old u8 "wire-equivalent" values → normalized linear f16 master samples. The
/// canonical master buffer is linear-light f16 (§5.2); an identity-gamma channel
/// round-trips `norm(&[v])` back to exactly `v`, so the wire assertions hold.
fn norm(vals: &[u8]) -> Vec<half::f16> {
    vals.iter()
        .map(|&v| half::f16::from_f32(v as f32 / 255.0))
        .collect()
}

/// Two GRB pixels on universe 1 at channels 1 and 4, gamma identity. Canonical
/// (linear-light) master order is [R,G,B]; on the wire GRB reorders to [G,R,B].
fn grb_patch() -> Patch {
    Patch {
        id: Uuid::from_u128(0x1),
        name: "grb".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: Uuid::from_u128(0xA),
            name: "strip".into(),
            profile: FixtureProfile {
                name: "WS2812 GRB".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(), // gamma 1.0 = identity
            },
            pixels: vec![
                Pixel {
                    index: 0,
                    position: glam::Vec3::new(0.0, 0.0, 0.0),
                    universe: 1,
                    start_channel: 1,
                    color_order: ColorOrder::Grb,
                },
                Pixel {
                    index: 1,
                    position: glam::Vec3::new(1.0, 0.0, 0.0),
                    universe: 1,
                    start_channel: 4,
                    color_order: ColorOrder::Grb,
                },
            ],
            zone: Uuid::nil(),
        }],
        zones: vec![],
    }
}

#[test]
fn canonical_buffer_reaches_sink_with_color_order_applied() {
    let sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = sink.local_addr();

    let patch = grb_patch();
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let router = OutputRouter::from_plan(plan);

    // Canonical pixel-domain master: pixel0 RGB=[10,20,30], pixel1 RGB=[40,50,60].
    let master = norm(&[10, 20, 30, 40, 50, 60]);
    let (mut producer, consumer) = frame_handoff(master.len());
    // The producer renders against this layout — stamp its generation.
    producer.set_layout_generation(index.generation());
    producer.publish(move |px| px.copy_from_slice(&master));

    let transports = vec![UdpOutput::to_target(target)];
    let rate = RatePlan::compute(&[44]);
    let config = SchedulerConfig {
        run_for: Some(Duration::from_millis(300)),
        ..Default::default()
    };
    let report = run_output_loop(consumer, router, transports, rate, config);
    let state = sink.stop_and_collect();

    assert_eq!(report.generation_mismatches, 0, "no mismatch expected");
    let us = state.universes.get(&1).expect("universe 1 at sink");
    // GRB wire order: pixel0 [G,R,B]=[20,10,30], pixel1 [50,40,60].
    assert_eq!(&us.data[..6], &[20, 10, 30, 50, 40, 60]);
}

#[test]
fn stale_generation_is_refused_and_nothing_is_emitted() {
    let sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = sink.local_addr();

    let patch = grb_patch();
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let router = OutputRouter::from_plan(plan);

    let master = norm(&[10, 20, 30, 40, 50, 60]);
    let (mut producer, consumer) = frame_handoff(master.len());
    // Producer stamps a DIFFERENT generation than the plan was compiled against
    // — exactly the "stale plan after a recompile" bug the guard exists for.
    producer.set_layout_generation(LayoutGeneration::NONE);
    producer.publish(move |px| px.copy_from_slice(&master));

    let transports = vec![UdpOutput::to_target(target)];
    let rate = RatePlan::compute(&[44]);
    let config = SchedulerConfig {
        run_for: Some(Duration::from_millis(300)),
        ..Default::default()
    };
    let report = run_output_loop(consumer, router, transports, rate, config);
    let state = sink.stop_and_collect();

    // The guard fired on every tick, and NOT ONE corrupt packet went out.
    assert!(
        report.generation_mismatches > 0,
        "expected the generation guard to fire"
    );
    assert!(
        state.universes.is_empty(),
        "no universe should reach the sink under a generation mismatch, got {:?}",
        state.universes.keys().collect::<Vec<_>>()
    );
    assert_eq!(state.artdmx_packets, 0, "no ArtDmx should be emitted");
}

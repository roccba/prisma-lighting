//! S5 acceptance: the output-stage flat-parameter split (CLAUDE.md §5.2 step 6,
//! §5.2.1, §1.5).
//!
//! The point of this step is the **latency shortcut**, and these tests prove the
//! whole property rather than merely that a dimmer multiplies:
//!
//! 1. A zone dimmer change reaches the wire on the **next output frame with no
//!    re-render** — asserted by holding one frame (same `frame_id`, byte-identical
//!    master buffer) across two encodes and requiring the wire to differ.
//! 2. In the **same** run, an *overlapped* pixel must NOT move with that fader,
//!    because its dimmer was already applied in render (step 4). That asymmetry is
//!    the evidence the split works as designed instead of everything taking one
//!    path.
//! 3. The scalars are applied **before** gamma. With a non-unity gamma the two
//!    orders give different, detectable bytes.
//!
//! The overlap snapshot is built explicitly here rather than by the render
//! counter, so this exercises the output-stage *contract* in isolation.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use half::f16;
use prisma_core::OverlapSnapshot;
use prisma_output::frame::frame_handoff;
use prisma_output::packet::artnet::parse_artdmx;
use prisma_output::plan::{FlatParams, OutputPlan};
use prisma_output::rate::RatePlan;
use prisma_output::router::{OutputFrame, OutputRouter};
use prisma_output::scheduler::{run_output_loop, FlatParamSource, SchedulerConfig};
use prisma_output::sink;
use prisma_output::transport::UdpOutput;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
};
use uuid::Uuid;

/// Two RGB pixels on universe 1 at channels 1 and 4.
fn two_pixel_patch(gamma: f32) -> Patch {
    Patch {
        id: Uuid::from_u128(0x55),
        name: "s5".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: Uuid::from_u128(0xAA),
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration { gamma },
            },
            pixels: (0..2)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
        }],
        zones: vec![],
    }
}

/// A snapshot in which pixel 0 takes the output shortcut under zone `slot`, and
/// pixel 1 is overlapped (its zone dimmer was already applied in render).
fn split_snapshot(slot: u32) -> OverlapSnapshot {
    // 2 pixels, 2 cells, 6 components.
    let mut snap = OverlapSnapshot::zeroed(2, 2, 6);
    {
        let (count, zone_sum) = snap.pixel_arrays_mut();
        count[0] = 1; // singly covered -> shortcut
        zone_sum[0] = slot;
        count[1] = 2; // multiply covered -> dimmed in render
        zone_sum[1] = slot + 1;
    }
    snap.derive_from_pixels(&[0, 1], &[(0, 3), (3, 3)]);
    snap
}

fn full_white_master() -> Vec<f16> {
    vec![f16::from_f32(1.0); 6]
}

/// Encode universe 0 and return its DMX channel data.
fn encode(router: &mut OutputRouter, frame: &OutputFrame<'_>) -> Vec<u8> {
    let pkt = router.encode_universe_with(0, frame).unwrap().to_vec();
    parse_artdmx(&pkt).unwrap().data.to_vec()
}

#[test]
fn a_zone_fader_reaches_the_wire_with_no_re_render() {
    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let snapshot = split_snapshot(0);

    // One frame, published once, then held. Nothing re-renders below this line.
    let master = full_white_master();
    let (mut producer, mut consumer) = frame_handoff(master.len());
    producer.set_layout_generation(index.generation());
    producer.publish(|px| px.copy_from_slice(&master));
    assert!(consumer.advance());

    let frame_id_before = consumer.current().frame_id;
    let pixels_before: Vec<f16> = consumer.current().pixels.to_vec();

    // --- Output frame 1: zone dimmer at full ---
    let mut flat = FlatParams::identity(2);
    flat.set_zone(0, 1.0, [1.0, 1.0, 1.0]);
    let wire_full = encode(
        &mut router,
        &OutputFrame {
            master: &consumer.current().pixels,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );

    // --- The operator moves the fader. NO render happens. ---
    flat.set_zone(0, 0.5, [1.0, 1.0, 1.0]);

    // --- Output frame 2: same frame, same buffer ---
    let wire_dimmed = encode(
        &mut router,
        &OutputFrame {
            master: &consumer.current().pixels,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );

    // The frame genuinely did not change between the two encodes...
    assert_eq!(
        consumer.current().frame_id,
        frame_id_before,
        "no new frame was published"
    );
    assert_eq!(
        &*consumer.current().pixels,
        pixels_before.as_slice(),
        "the master buffer is byte-identical — nothing re-rendered"
    );
    // ...and yet the wire changed. That is the shortcut, not merely a dimmer.
    assert_ne!(
        wire_full, wire_dimmed,
        "the fader must reach the wire without a re-render"
    );
    // Pixel 0 (the shortcut pixel) is at channels 1..3.
    assert_eq!(&wire_full[0..3], &[255, 255, 255]);
    assert_eq!(
        &wire_dimmed[0..3],
        &[128, 128, 128],
        "round(0.5 * 255) = 128"
    );
}

#[test]
fn an_overlapped_pixel_ignores_the_fader_until_a_new_render() {
    // The asymmetry, measured in the same run as the shortcut pixel: one moves,
    // the other cannot — proof the two paths are really separate.
    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let snapshot = split_snapshot(0);
    let master = full_white_master();

    let mut flat = FlatParams::identity(2);
    flat.set_zone(0, 1.0, [1.0, 1.0, 1.0]);
    flat.set_zone(1, 1.0, [1.0, 1.0, 1.0]);
    let before = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );

    // Drive BOTH zone faders down. Only the shortcut pixel may respond.
    flat.set_zone(0, 0.25, [1.0, 1.0, 1.0]);
    flat.set_zone(1, 0.25, [1.0, 1.0, 1.0]);
    let after = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );

    // Pixel 0 = channels 0..3 (shortcut): follows the fader immediately.
    assert_eq!(&before[0..3], &[255, 255, 255]);
    assert_eq!(&after[0..3], &[64, 64, 64], "round(0.25 * 255) = 64");

    // Pixel 1 = channels 3..6 (overlapped): unchanged. Its zone dimmer was
    // applied in render (step 4); applying it here too would be the double-dim
    // the "exactly one stage" invariant forbids.
    assert_eq!(&before[3..6], &[255, 255, 255]);
    assert_eq!(
        &after[3..6],
        &[255, 255, 255],
        "an overlapped pixel must not move until a new render"
    );
}

#[test]
fn master_moves_every_pixel_including_the_overlapped_one() {
    // The complement of the test above: master is unconditional, so it is the
    // control proving the overlapped pixel is not simply frozen.
    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let snapshot = split_snapshot(0);
    let master = full_white_master();

    let mut flat = FlatParams::identity(2);
    flat.set_master(0.5, [1.0, 1.0, 1.0]);
    let wire = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );
    assert_eq!(
        &wire[0..3],
        &[128, 128, 128],
        "shortcut pixel follows master"
    );
    assert_eq!(
        &wire[3..6],
        &[128, 128, 128],
        "overlapped pixel follows master too — it is not frozen, only zone-immune"
    );
}

#[test]
fn the_zone_colour_filter_applies_per_channel() {
    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let snapshot = split_snapshot(0);
    let master = full_white_master();

    let mut flat = FlatParams::identity(2);
    flat.set_zone(0, 1.0, [1.0, 0.5, 0.0]);
    let wire = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );
    assert_eq!(&wire[0..3], &[255, 128, 0], "R, G, B scaled independently");
}

#[test]
fn the_scalars_are_applied_before_gamma() {
    // Gamma 2.0 on a full-white pixel with a 0.5 zone dimmer.
    //
    //   correct  (scale THEN gamma): round(255 * (1.0 * 0.5)^2.0) = round(63.75) = 64
    //   wrong    (gamma THEN scale): round(255 * 1.0^2.0) * 0.5   = 255 * 0.5   = 128
    //
    // The two orders are far apart, so this cannot pass by rounding luck. Order
    // matters because gamma is a non-linear curve: dimming must happen in linear
    // light and the curve be applied to the already-dimmed value.
    let patch = two_pixel_patch(2.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let snapshot = split_snapshot(0);
    let master = full_white_master();

    let mut flat = FlatParams::identity(2);
    flat.set_zone(0, 0.5, [1.0, 1.0, 1.0]);
    let wire = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );
    assert_eq!(
        &wire[0..3],
        &[64, 64, 64],
        "expected scale-then-gamma (64); 128 would mean gamma ran first"
    );

    // Same check for the master scalar, which shares the code path.
    let mut flat = FlatParams::identity(2);
    flat.set_master(0.5, [1.0, 1.0, 1.0]);
    let wire = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: Some(&snapshot),
            flat: &flat,
        },
    );
    assert_eq!(&wire[0..3], &[64, 64, 64]);
}

#[test]
fn a_frame_without_a_snapshot_takes_master_scaling_only() {
    // The M1 degenerate path: no zones, no classification. Zone values must be
    // ignored entirely rather than applied to everything.
    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let mut router = OutputRouter::from_plan(plan);
    let master = full_white_master();

    let mut flat = FlatParams::identity(2);
    flat.set_zone(0, 0.1, [1.0, 1.0, 1.0]);
    flat.set_master(0.5, [1.0, 1.0, 1.0]);
    let wire = encode(
        &mut router,
        &OutputFrame {
            master: &master,
            generation: index.generation(),
            overlap: None,
            flat: &flat,
        },
    );
    assert_eq!(&wire[0..6], &[128, 128, 128, 128, 128, 128]);
}

// ---- end to end, through the real loop and the virtual sink ----------------

struct FixedFlat {
    zone_dimmer: f32,
}

impl FlatParamSource for FixedFlat {
    fn read_into(&self, into: &mut FlatParams) {
        into.set_master(1.0, [1.0, 1.0, 1.0]);
        into.set_zone(0, self.zone_dimmer, [1.0, 1.0, 1.0]);
        into.set_zone(1, 1.0, [1.0, 1.0, 1.0]);
    }
}

#[test]
fn scaled_output_reaches_the_virtual_sink() {
    let sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = sink.local_addr();

    let patch = two_pixel_patch(1.0);
    let index = patch.compile().unwrap();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let router = OutputRouter::from_plan(plan);

    let master = full_white_master();
    let (mut producer, consumer) = frame_handoff(master.len());
    producer.set_layout_generation(index.generation());
    producer.set_overlap(Some(std::sync::Arc::new(split_snapshot(0))));
    producer.publish(move |px| px.copy_from_slice(&master));

    let config = SchedulerConfig {
        run_for: Some(Duration::from_millis(200)),
        zone_count: 2,
        flat_source: Some(Box::new(FixedFlat { zone_dimmer: 0.25 })),
        ..Default::default()
    };
    let report = run_output_loop(
        consumer,
        router,
        vec![UdpOutput::to_target(target)],
        RatePlan::compute(&[44]),
        config,
    );
    let state = sink.stop_and_collect();

    assert_eq!(report.generation_mismatches, 0);
    let us = state.universes.get(&1).expect("universe 1 at sink");
    // Pixel 0 took the shortcut and arrives scaled by the live zone dimmer;
    // pixel 1 is overlapped and arrives untouched by it.
    assert_eq!(
        &us.data[0..3],
        &[64, 64, 64],
        "shortcut pixel, 0.25 applied"
    );
    assert_eq!(
        &us.data[3..6],
        &[255, 255, 255],
        "overlapped pixel untouched"
    );
}

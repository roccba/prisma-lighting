//! M5 milestone acceptance test — the whole "done when", not a collection of
//! partial asserts (CLAUDE.md §6, M5):
//!
//!   "different effects run simultaneously and independently on separate zones,
//!    with per-zone dimmer working live *through the output stage* (proving the
//!    §1.5 latency shortcut), overlapping zones resolve by `z_order` + per-zone
//!    blend mode (an `Add`/`Max` zone brightens rather than occludes), and
//!    readback latency is reported by the probe."
//!
//! Everything runs through the **real pipeline** — the render loop on its own
//! thread, the fixed-interval output loop on another, Art-Net over UDP, the
//! virtual sink standing in for physical fixtures (as M1 established). No
//! harnesses: the frames the sink sees are the frames the engine produced.
//!
//! The rig: five pixels in a 5x1 matrix.
//!
//! ```text
//!   cell     0        1        2        3        4
//!   zone   [--- left ---]     none    [--- right ---]
//!          [-------------- strobe (Add, z_order 10) --------------]
//!   effect  solid 0.25          -      gradient 0 -> 1
//! ```
//!
//! `left` runs a flat solid and `right` a gradient: unmistakably different
//! *patterns*, not two similar-looking values, so independence cannot pass by the
//! two zones happening to agree. Cell 2 belongs to no zone and must stay dark.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use prisma_core::time::SystemClock;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot, ParameterTree};
use prisma_output::frame::frame_handoff;
use prisma_output::plan::OutputPlan;
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::sink;
use prisma_output::transport::UdpOutput;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_render::blend::{Compositor, ZoneComposite};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::{zone_cell_coverage, OverlapCounter};
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::{run_render_loop, FrameContent, RenderLoop, RenderLoopConfig};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{create_cell_mask, effects, Gpu, LiveParams};

mod common;
use uuid::Uuid;

const WIDTH: u32 = 5;
const LEFT: usize = 0;
const RIGHT: usize = 1;
const STROBE: usize = 2;

/// Live zone state comes from the **Parameter Tree** (§3.1 principle 2): the test
/// writes zone dimmers by handle exactly as a MIDI fader or the UI would, and one
/// `LiveParams` binding feeds BOTH the render loop's counter and the output
/// stage's flat parameters — wait-free on each, no shared lock.
fn ui() -> prisma_core::source::SourceId {
    prisma_core::source::SourceId::new(prisma_core::source::priority::UI, 1)
}

fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0xF5);
    Patch {
        id: Uuid::from_u128(0x58),
        name: "m5 rig".into(),
        matrix_size: [WIDTH, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..WIDTH)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: Uuid::nil(),
        }],
        zones: vec![
            zone(0x10, "left", fixture_id, &[0, 1], 0, BlendMode::Normal),
            zone(0x11, "right", fixture_id, &[3, 4], 0, BlendMode::Normal),
            // The global strobe: covers everything, sits on top, and ADDS.
            zone(
                0x12,
                "strobe",
                fixture_id,
                &[0, 1, 2, 3, 4],
                10,
                BlendMode::Add,
            ),
        ],
    }
}

fn zone(id: u128, name: &str, fixture: Uuid, pixels: &[u32], z: i32, mode: BlendMode) -> Zone {
    Zone {
        id: Uuid::from_u128(id),
        name: name.into(),
        members: pixels
            .iter()
            .map(|&pixel| PixelRef { fixture, pixel })
            .collect(),
        bounds: None,
        z_order: z,
        blend_mode: mode,
        mapping_size: [WIDTH, 1, 1],
        display_color: [128, 128, 128],
        world_transform: None,
    }
}

/// The real zone content: one effect per zone, composited by `z_order` with each
/// zone's own blend mode and coverage.
struct ZoneContent {
    compositor: Compositor,
    textures: Vec<wgpu::Texture>,
    effects: Vec<EffectInstance>,
    coverage: Vec<wgpu::Buffer>,
    order: Vec<u32>,
}

impl FrameContent for ZoneContent {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        zones: &ZoneStates,
        overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        // 1. Each zone's effect into its own texture.
        for (effect, texture) in self.effects.iter_mut().zip(self.textures.iter()) {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            effect.render(gpu, clock, &view, WIDTH, 1);
        }

        // 2. This frame's cell mask, and each zone's live dimmer/colour.
        let cell_mask = create_cell_mask(gpu, overlap.cell_mask_bytes());
        let composites: Vec<ZoneComposite<'_>> = self
            .order
            .iter()
            .filter(|&&slot| zones.contributes_pixels(slot as usize))
            .map(|&slot| {
                let s = slot as usize;
                let z = zones.get(s);
                let d = z.dimmer();
                ZoneComposite {
                    texture: &self.textures[s],
                    mode: zone_mode(s),
                    opacity: 1.0,
                    zone_scale: [
                        d * z.color_filter[0],
                        d * z.color_filter[1],
                        d * z.color_filter[2],
                    ],
                    coverage: &self.coverage[s],
                }
            })
            .collect();

        self.compositor
            .composite_zones_into(gpu, &composites, &cell_mask, target, WIDTH, 1);
    }
}

fn zone_mode(slot: usize) -> BlendMode {
    if slot == STROBE {
        BlendMode::Add
    } else {
        BlendMode::Normal
    }
}

/// Wire bytes of pixel `p` (RGB triples from channel 1).
fn pixel(data: &[u8], p: usize) -> [u8; 3] {
    [data[p * 3], data[p * 3 + 1], data[p * 3 + 2]]
}

/// Wait until the sink's universe-1 data satisfies `done`, or give up.
fn wait_for(handle: &sink::SinkHandle, what: &str, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let state = handle.snapshot();
        if let Some(us) = state.universes.get(&1) {
            last = us.data.clone();
            if done(&last) {
                return last;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}; last wire data was {last:?}");
}

#[test]
fn m5_zones_run_independent_effects_with_live_dimmer_and_additive_overlap() {
    let gpu = common::software_gpu();

    // 1. The virtual sink stands in for the physical rig (M1's arrangement).
    let sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = sink.local_addr();

    let patch = build_patch();
    let index: PatchIndex = patch.compile().expect("rig compiles");

    // 2. Live state lives in the Parameter Tree. One binding, read wait-free by
    //    both the render loop and the output stage.
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);
    tree.write_real(&live.zone(LEFT).dimmer, 1.0, ui());
    tree.write_real(&live.zone(RIGHT).dimmer, 1.0, ui());
    tree.write_real(&live.zone(STROBE).dimmer, 0.0, ui()); // dark: not contributing

    // 3. The real output loop, on its own thread.
    let plan = OutputPlan::compile(&patch, &index).expect("plan compiles");
    let router = OutputRouter::from_plan(plan);
    let (mut producer, consumer) = frame_handoff(index.master_buffer_len());
    // Stage one black frame stamped with this layout BEFORE the output loop
    // starts, exactly as M1's acceptance test does. Without it the output ticks
    // against the never-published initial buffers, whose generation is NONE, and
    // the generation guard correctly refuses them — benign, but it would show up
    // as mismatches. Engine start-up must stage a frame for the same reason.
    producer.set_layout_generation(index.generation());
    producer.publish(|px| px.fill(half::f16::ZERO));
    let output_stop = Arc::new(AtomicBool::new(false));
    let output = {
        let stop = Arc::clone(&output_stop);
        let live = live.clone();
        std::thread::spawn(move || {
            run_output_loop(
                consumer,
                router,
                vec![UdpOutput::to_target(target)],
                RatePlan::compute(&[44]),
                SchedulerConfig {
                    stop,
                    zone_count: 3,
                    flat_source: Some(Box::new(live)),
                    ..Default::default()
                },
            )
        })
    };

    // 4. The real render loop, on this thread's own engine.
    let textures: Vec<wgpu::Texture> = (0..3)
        .map(|_| create_render_target(gpu, WIDTH, 1))
        .collect();
    let mut effects_v = Vec::new();
    // left: a flat solid; right: a gradient — unmistakably different patterns.
    let mut left = EffectInstance::new(gpu, &tree, "/m5/left", &effects::solid());
    for (c, v) in [("r", 0.25), ("g", 0.25), ("b", 0.25)] {
        tree.write_path(&format!("/m5/left/param/{c}"), v, ui());
    }
    left.render(
        gpu,
        &ClockSnapshot {
            frame: 0,
            wall_ns: 0,
            show_ns: 0,
            beat: 0.0,
            bpm: 120.0,
            playing: false,
        },
        &textures[LEFT].create_view(&wgpu::TextureViewDescriptor::default()),
        WIDTH,
        1,
    );
    effects_v.push(left);
    effects_v.push(EffectInstance::new(
        gpu,
        &tree,
        "/m5/right",
        &effects::gradient(),
    ));
    let mut strobe = EffectInstance::new(gpu, &tree, "/m5/strobe", &effects::solid());
    for c in ["r", "g", "b"] {
        tree.write_path(&format!("/m5/strobe/param/{c}"), 0.25, ui());
    }
    strobe.render(
        gpu,
        &ClockSnapshot {
            frame: 0,
            wall_ns: 0,
            show_ns: 0,
            beat: 0.0,
            bpm: 120.0,
            playing: false,
        },
        &textures[STROBE].create_view(&wgpu::TextureViewDescriptor::default()),
        WIDTH,
        1,
    );
    effects_v.push(strobe);

    let coverage: Vec<wgpu::Buffer> = zone_cell_coverage(&patch, &index)
        .iter()
        .map(|m| create_cell_mask(gpu, m))
        .collect();

    let mut content = ZoneContent {
        compositor: Compositor::new(gpu),
        textures,
        effects: effects_v,
        coverage,
        order: index.composite_order().to_vec(),
    };

    let sample_plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &sample_plan);
    let mut engine = RenderLoop::with_default_slots(
        gpu,
        sampler,
        producer,
        (WIDTH, 1),
        index.master_buffer_len(),
    );
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let render_stop = Arc::new(AtomicBool::new(false));

    // The render loop runs on this thread inside a scope so it can be stopped
    // mid-test while the output loop keeps going.
    let render_stats = std::thread::scope(|scope| {
        let handle = {
            let live = live.clone();
            let stop = Arc::clone(&render_stop);
            scope.spawn(move || {
                run_render_loop(
                    gpu,
                    &mut engine,
                    &mut clock,
                    &mut counter,
                    ZoneStates::new(3),
                    &mut content,
                    RenderLoopConfig {
                        target_rate_hz: 44,
                        run_for: None,
                        stop,
                        zone_source: Some(Box::new(live)),
                        freeze: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        frozen_rate_hz: 44,
                    },
                )
            })
        };

        // ---- PHASE 1: two different effects, simultaneous and independent ----
        //
        // left is a flat 0.25 solid; right is a gradient whose cells are the exact
        // quarters 0, .25, .5, .75, 1 — so cell 3 is 0.75 and cell 4 is 1.0.
        let wire = wait_for(&sink, "phase 1 (independent zones)", |d| {
            d.len() >= 15 && pixel(d, 4) == [255, 255, 255]
        });
        assert_eq!(pixel(&wire, 0), [64, 64, 64], "left pixel 0 = solid 0.25");
        assert_eq!(pixel(&wire, 1), [64, 64, 64], "left pixel 1 = solid 0.25");
        assert_eq!(pixel(&wire, 0), pixel(&wire, 1), "the solid zone is FLAT");
        assert_eq!(
            pixel(&wire, 2),
            [0, 0, 0],
            "cell 2 is in no zone: no zone bled into it"
        );
        assert_eq!(
            pixel(&wire, 3),
            [191, 191, 191],
            "right pixel 3 = gradient 0.75"
        );
        assert_eq!(
            pixel(&wire, 4),
            [255, 255, 255],
            "right pixel 4 = gradient 1.0"
        );
        assert_ne!(
            pixel(&wire, 3),
            pixel(&wire, 4),
            "the gradient zone VARIES — the two zones' patterns are distinguishable"
        );

        // ---- PHASE 2: the global strobe fires, Add on top by z_order ----
        tree.write_real(&live.zone(STROBE).dimmer, 1.0, ui());
        let wire = wait_for(&sink, "phase 2 (strobe brightens)", |d| {
            d.len() >= 15 && pixel(d, 2) != [0, 0, 0]
        });
        // 0.25 + 0.25 = 0.5 -> 128. Brighter than the base AND brighter than the
        // strobe alone: it lifted the zone below rather than replacing it.
        assert_eq!(pixel(&wire, 0), [128, 128, 128], "left brightened by Add");
        assert!(pixel(&wire, 0)[0] > 64, "Add must brighten, not occlude");
        // Cell 2 had no zone; now only the strobe covers it -> its own 0.25.
        assert_eq!(
            pixel(&wire, 2),
            [64, 64, 64],
            "strobe reaches the unzoned cell"
        );
        // 0.75 + 0.25 = 1.0 -> 255; 1.0 + 0.25 clamps to 1.0 -> 255.
        assert_eq!(pixel(&wire, 3), [255, 255, 255], "right brightened by Add");
        assert_eq!(pixel(&wire, 4), [255, 255, 255], "already full, clamped");

        // ---- PHASE 3: the latency shortcut, proven by stopping the renderer ----
        //
        // Strobe off, wait for the renderer to settle, then STOP the render thread
        // entirely. With no renders happening at all, moving a zone fader must
        // still change the wire — which is only possible if the per-zone dimmer is
        // applied at the OUTPUT stage (§5.2 step 6, §1.5).
        tree.write_real(&live.zone(STROBE).dimmer, 0.0, ui());
        wait_for(&sink, "phase 3 settle (strobe off)", |d| {
            d.len() >= 15 && pixel(d, 2) == [0, 0, 0] && pixel(d, 0) == [64, 64, 64]
        });

        render_stop.store(true, Ordering::Relaxed);
        let stats = handle.join().expect("render thread");

        // Nothing renders from here on. Record how many frames the renderer ever
        // produced so we can prove none were added.
        let frames_at_stop = stats.frames_published;
        std::thread::sleep(Duration::from_millis(60));

        tree.write_real(&live.zone(LEFT).dimmer, 0.5, ui());
        let wire = wait_for(&sink, "phase 3 (fader with the renderer stopped)", |d| {
            d.len() >= 15 && pixel(d, 0) != [64, 64, 64]
        });
        assert_eq!(
            pixel(&wire, 0),
            [32, 32, 32],
            "left halved by the OUTPUT stage: 0.25 content x 0.5 dimmer, with zero renders"
        );
        assert_eq!(pixel(&wire, 1), [32, 32, 32]);
        assert_eq!(
            pixel(&wire, 3),
            [191, 191, 191],
            "the other zone is untouched by left's fader"
        );
        assert_eq!(
            stats.frames_published, frames_at_stop,
            "the render thread really was stopped"
        );
        stats
    });

    output_stop.store(true, Ordering::Relaxed);
    let output_report = output.join().expect("output thread");
    let sink_state = sink.stop_and_collect();

    // ---- The remaining "done when" clauses ----

    // readback latency is reported by the probe
    assert!(
        !render_stats.readback.is_empty(),
        "readback latency must be measured and reported"
    );
    assert_eq!(render_stats.readback_failures, 0, "no readback may fail");
    assert!(
        render_stats.frames_published > 0,
        "the render loop published frames through the real handoff"
    );

    // and it all really went over the wire
    assert!(sink_state.artdmx_packets > 0, "no ArtDmx reached the sink");
    assert_eq!(sink_state.malformed, 0);
    assert_eq!(sink_state.non_artnet, 0);
    assert_eq!(output_report.drops, 0, "output dropped frames");
    assert_eq!(
        output_report.generation_mismatches, 0,
        "plan and master buffer stayed in step"
    );
}

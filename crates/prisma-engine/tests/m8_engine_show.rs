//! M8 step 2 (5) — the engine runs a **Show** end to end.
//!
//! `Engine::start_show` assembles the running product with the per-zone
//! `ZoneCompositor` in place of `IdleContent`: the engine stops rendering black and
//! runs the show's active per-zone effects, seeded with the show's live state. This
//! is validated on the wire (the virtual sink), **not visually** — the preview
//! lights up on screen only once the step-3 UI shows it.
//!
//! One test function only, and its own binary: each `start_show` brings up a Vulkan
//! device and `shutdown` tears one down; sequential bring-up/teardown avoids the
//! loader race (see `boot_shutdown.rs` / `prisma-render/tests/common`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, Layer, ParamSet, Show};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x51);
    let zone_id = Uuid::from_u128(0x1);
    Patch {
        id: Uuid::from_u128(0x5100),
        name: "m8-show".into(),
        matrix_size: [4, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..4)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: zone_id,
        }],
        zones: vec![Zone {
            id: zone_id,
            name: "all".into(),
            members: (0..4)
                .map(|pixel| PixelRef {
                    fixture: fixture_id,
                    pixel,
                })
                .collect(),
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [4, 1, 1],
            display_color: [200, 60, 60],
            world_transform: None,
        }],
    }
}

/// A show whose single zone runs a flat 0.5 solid, fader up.
fn build_show() -> Show {
    let patch = build_patch();
    let zone_id = patch.zones[0].id;
    let mut show = Show::new("m8 test show", patch);

    let mut params = ParamSet::new();
    params.set("r", 0.5);
    params.set("g", 0.5);
    params.set("b", 0.5);

    let zs = show
        .zones
        .iter_mut()
        .find(|z| z.zone_id == zone_id)
        .expect("zone show exists");
    zs.grid.push(Cell::single(Layer::shader("solid", params)));
    zs.active = Some(0);
    zs.live.dimmer = 1.0; // lit — otherwise the (default-down) fader keeps it dark
    show
}

fn wait_for(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if pred() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn engine_runs_a_show_and_lights_the_wire() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let sink_addr = dev_sink.local_addr();

    let mut config = EngineConfig::new(AdapterPreference::Software, sink_addr);
    config.output_rate_hz = 44;

    let show = build_show();
    let engine = Engine::start_show(show, config).expect("engine starts a show");

    // A running show exposes the per-zone command queue (IdleContent would not).
    assert!(
        engine.zone_commands().is_some(),
        "a running show must expose the per-zone command queue"
    );

    // The wire shows the show's real content — a flat 0.5 solid → ~128 on every
    // channel — NOT the black IdleContent would produce. This is the headline: the
    // engine stopped rendering black and ran per-zone content.
    wait_for("the show's content reaches the wire", || {
        let snap = dev_sink.snapshot();
        match snap.universes.get(&1) {
            Some(u) => u.data.len() >= 12 && u.data[0] > 120 && u.data[0] < 136,
            None => false,
        }
    });

    let snap = dev_sink.snapshot();
    let data = &snap.universes.get(&1).expect("universe 1").data;
    // All four pixels are the flat solid — no pixel left black (which would mean the
    // zone did not cover it, or content was still idle).
    for p in 0..4usize {
        let v = data[p * 3];
        assert!(
            (120..136).contains(&v),
            "pixel {p} channel = ~128 (solid 0.5), got {v}"
        );
    }

    // The zone-row fader (M8 step 3) is a live control: halving the zone dimmer
    // halves the wire (0.5 content x 0.5 dimmer -> ~64), applied at the output stage.
    assert_eq!(engine.zone_count(), 1);
    engine.set_zone_dimmer(0, 0.5);
    wait_for("the zone fader reaches the wire", || {
        let snap = dev_sink.snapshot();
        match snap.universes.get(&1) {
            Some(u) => !u.data.is_empty() && (56..72).contains(&u.data[0]),
            None => false,
        }
    });
    let live = engine.zone_live(0);
    assert!(
        (live.dimmer - 0.5).abs() < 0.01,
        "zone_live reflects the fader, got {}",
        live.dimmer
    );

    // Firing a scene cell (M8 step 4): trigger a *different* effect through the
    // frame-boundary queue. The flat solid becomes a gradient — pixel 0 goes dark,
    // pixel 3 rises (each still x0.5 by the zone fader). Proof the swap reached the
    // wire, off the render thread.
    let zone_id = Uuid::from_u128(0x1); // the single zone in build_patch
    assert!(engine.trigger_zone_effect(0, zone_id, "gradient", &ParamSet::new()));
    wait_for("the triggered gradient reaches the wire", || {
        let snap = dev_sink.snapshot();
        match snap.universes.get(&1) {
            // pixel 0 ~ 0 (gradient start x 0.5 dimmer), pixel 3 ~ 127 (gradient end x 0.5).
            Some(u) => u.data.len() >= 12 && u.data[0] < 12 && (118..136).contains(&u.data[9]),
            None => false,
        }
    });

    let report = engine.shutdown();
    assert!(
        report.render.frames_published > 0,
        "the render loop published real frames"
    );
    let sink_state = dev_sink.stop_and_collect();
    assert!(sink_state.artdmx_packets > 0, "ArtDmx reached the sink");
}

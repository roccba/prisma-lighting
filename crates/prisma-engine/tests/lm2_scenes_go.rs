//! Live model phase 2 — scenes, Arm / GO, Arm-mode tracks, stop and live track
//! opacity, end to end through the engine's clip controller, validated on the wire
//! (virtual sink) and on each clip's published `state` (LIVE-MODEL-SPEC §5).
//!
//! One test function, its own binary (one Vulkan bring-up at a time — see m8_engine_show).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, LaunchMode, Layer, ParamSet, Show, Source};
use prisma_engine::{
    ClipEffect, ClipHost, Engine, EngineConfig, EngineHandle, TrackBehaviour, CLIP_ARMED,
    CLIP_ASSIGNED, CLIP_EMPTY, CLIP_RUNNING,
};
use prisma_output::sink;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

const ZA: Uuid = Uuid::from_u128(0xA);
const ZB: Uuid = Uuid::from_u128(0xB);

/// Four RGB pixels on universe 1: zone A = pixels 0–1, zone B = pixels 2–3.
fn patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x52);
    let zone = |id: Uuid, name: &str, first: u32, z: i32| Zone {
        id,
        name: name.into(),
        members: (first..first + 2)
            .map(|pixel| PixelRef {
                fixture: fixture_id,
                pixel,
            })
            .collect(),
        bounds: None,
        z_order: z,
        blend_mode: BlendMode::Normal,
        mapping_size: [2, 1, 1],
        display_color: [200, 60, 60],
        world_transform: None,
    };
    Patch {
        id: Uuid::from_u128(0x5200),
        name: "scenes".into(),
        matrix_size: [4, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
                white: prisma_patch::WhiteProfile::default(),
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
            zone: ZA,
        }],
        zones: vec![zone(ZA, "A", 0, 0), zone(ZB, "B", 2, 1)],
    }
}

fn solid(v: f32) -> Cell {
    let mut p = ParamSet::new();
    p.set("r", v);
    p.set("g", v);
    p.set("b", v);
    Cell::single(Layer::shader("solid", p))
}

/// Track A: row 0 grey 0.5, row 1 white, row 2 empty. Track B: row 0 grey 0.5,
/// row 1 empty, row 2 white. Nothing running.
fn show() -> Show {
    let mut show = Show::new("scenes", patch());
    show.home_track_mut(ZA).unwrap().clips = vec![solid(0.5), solid(1.0), Cell::empty()];
    show.home_track_mut(ZB).unwrap().clips = vec![solid(0.5), Cell::empty(), solid(1.0)];
    for zs in &mut show.zones {
        zs.live.dimmer = 1.0;
    }
    show
}

struct Host(Mutex<Show>);

impl ClipHost for Host {
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect> {
        let show = self.0.lock().unwrap();
        let cell = show.track(track)?.clips.get(clip)?;
        let layer = cell.layers.first()?.clone();
        let Source::Shader {
            effect_id,
            params,
            palette,
        } = layer.source;
        Some(ClipEffect {
            effect_id,
            params,
            palette,
            fade_time: cell.fade_time,
        })
    }
    fn track_behaviour(&self, track: Uuid) -> Option<TrackBehaviour> {
        let show = self.0.lock().unwrap();
        let t = show.track(track)?;
        Some(TrackBehaviour {
            transition: t.transition,
            launch: t.launch,
        })
    }
    fn active_changed(&self, track: Uuid, active: Option<usize>) {
        if let Some(t) = self.0.lock().unwrap().track_mut(track) {
            t.active = active;
        }
    }
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
fn scenes_arm_go_and_track_controls_reach_the_wire() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let show = show();
    let (ta, tb) = (
        show.home_track(ZA).unwrap().id,
        show.home_track(ZB).unwrap().id,
    );
    let host = Arc::new(Host(Mutex::new(show.clone())));
    let engine = Engine::start_show(show, config).expect("engine starts");
    engine.set_clip_host(host.clone());

    // (zone A, zone B) red channel within ranges.
    let wire = |a: (u8, u8), b: (u8, u8)| {
        let snap = dev_sink.snapshot();
        snap.universes.get(&1).is_some_and(|u| {
            u.data.len() >= 12
                && (a.0..=a.1).contains(&u.data[0])
                && (b.0..=b.1).contains(&u.data[6])
        })
    };
    const DARK: (u8, u8) = (0, 4);
    const GREY: (u8, u8) = (120, 136);
    const WHITE: (u8, u8) = (250, 255);
    let state = |e: &EngineHandle, t: Uuid, clip: usize| {
        e.tree()
            .resolve(&format!("/track/{t}/clip/{clip}/state"))
            .unwrap()
            .read_real()
            .unwrap()
    };
    let active = |t: Uuid| host.0.lock().unwrap().track(t).unwrap().active;

    wait_for("initial states", || {
        state(&engine, ta, 1) == CLIP_ASSIGNED && state(&engine, tb, 1) == CLIP_EMPTY
    });
    wait_for("dark at start", || wire(DARK, DARK));

    // Scene 0: both tracks have a clip there → both grey.
    assert!(engine.launch_scene(0));
    wait_for("scene 0 on both zones", || wire(GREY, GREY));
    // Scene 1: only track A has one; B keeps playing its grey.
    assert!(engine.launch_scene(1));
    wait_for("scene 1 changes A only", || wire(WHITE, GREY));
    assert_eq!((active(ta), active(tb)), (Some(1), Some(0)));

    // Arm A row 0 and B row 2: nothing changes until GO; then both at once.
    assert!(engine.arm_clip(0, 0));
    assert!(engine.arm_clip(1, 2));
    wait_for("armed states", || {
        state(&engine, ta, 0) == CLIP_ARMED && state(&engine, tb, 2) == CLIP_ARMED
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(wire(WHITE, GREY), "arming never plays");
    assert!(engine.go());
    wait_for("GO plays both armed clips", || wire(GREY, WHITE));
    wait_for("GO disarms", || {
        state(&engine, ta, 0) == CLIP_RUNNING && state(&engine, tb, 2) == CLIP_RUNNING
    });

    // Disarm everything (`/arm/clear`): an armed clip goes back to loaded.
    assert!(engine.arm_clip(1, 0));
    wait_for("armed", || state(&engine, tb, 0) == CLIP_ARMED);
    assert!(engine.clear_arm());
    wait_for("disarmed", || state(&engine, tb, 0) == CLIP_ASSIGNED);

    // An Arm-mode track: a launch arms instead of playing.
    host.0.lock().unwrap().track_mut(ta).unwrap().launch = LaunchMode::Arm;
    assert!(engine.fire_clip(0, 1));
    wait_for("launch arms on an Arm track", || {
        state(&engine, ta, 1) == CLIP_ARMED
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(wire(GREY, WHITE), "still the old clip until GO");
    assert!(engine.go());
    wait_for("GO plays it", || wire(WHITE, WHITE));

    // Track opacity is live: half opacity halves track A on the wire.
    engine.set_track_opacity(0, 0.5);
    wait_for("opacity reaches the wire", || wire((120, 136), WHITE));
    assert!((engine.track_live(0).unwrap().opacity - 0.5).abs() < 0.01);

    // Mute B; then stop A (hard cut: its transition is 0).
    engine.set_track_mute(1, true);
    wait_for("muted B goes dark", || wire((120, 136), DARK));
    assert!(engine.stop_track(0));
    wait_for("stopped A goes dark", || {
        wire(DARK, DARK) && active(ta).is_none()
    });

    // Stop All (`/tracks/stop`): every track stops in one go.
    engine.set_track_opacity(0, 1.0);
    engine.set_track_mute(1, false);
    host.0.lock().unwrap().track_mut(ta).unwrap().launch = LaunchMode::Direct;
    assert!(engine.launch_scene(0));
    wait_for("scene 0 again", || wire(GREY, GREY));
    assert!(engine.stop_all());
    wait_for("stop all goes dark", || {
        wire(DARK, DARK) && active(ta).is_none() && active(tb).is_none()
    });

    // Tempo: a BPM written to /clock/bpm reaches the master clock.
    engine.set_bpm(150.0);
    wait_for("the clock runs at 150 BPM", || {
        (engine.bpm() - 150.0).abs() < 0.01
    });

    let report = engine.shutdown();
    assert_eq!(
        report.gpu_strong_refs_at_release, 1,
        "clips released the GPU"
    );
    let _ = dev_sink.stop_and_collect();
}

//! Track clips as tree targets (`/track/{id}/clip/{i}/…`, LIVE-MODEL-SPEC §5),
//! played by the engine's clip controller: a momentary hold returns to what ran
//! before, a latch makes a new base, a trigger restarts, an empty clip stops the
//! track, and the whole thing survives a recompile. Validated on the wire (virtual
//! sink) and on each clip's published `state`.
//!
//! One test function, its own binary (one Vulkan bring-up at a time — see m8_engine_show).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, Layer, ParamSet, Show};
use prisma_core::{priority, SourceId};
use prisma_engine::{
    ClipEffect, ClipHost, Engine, EngineConfig, CLIP_ASSIGNED, CLIP_EMPTY, CLIP_RUNNING,
};
use prisma_output::sink;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

const ZONE: Uuid = Uuid::from_u128(0x1);

fn patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x51);
    Patch {
        id: Uuid::from_u128(0x5100),
        name: "cells".into(),
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
            zone: ZONE,
        }],
        zones: vec![Zone {
            id: ZONE,
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

fn solid(v: f32) -> Cell {
    let mut p = ParamSet::new();
    p.set("r", v);
    p.set("g", v);
    p.set("b", v);
    Cell::single(Layer::shader("solid", p))
}

/// The zone's own track: clips 0 = grey 0.5 (running), 1 = white, 2 = empty.
fn show() -> Show {
    let mut show = Show::new("clips", patch());
    let track = show.home_track_mut(ZONE).unwrap();
    track.clips = vec![solid(0.5), solid(1.0), Cell::empty()];
    track.active = Some(0);
    let zs = show.zones.iter_mut().find(|z| z.zone_id == ZONE).unwrap();
    zs.live.dimmer = 1.0;
    show
}

/// The app's role: owns the show, hears about the running clip.
struct Host(Mutex<Show>);

impl ClipHost for Host {
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect> {
        let show = self.0.lock().unwrap();
        let layer = show.track(track)?.clips.get(clip)?.layers.first()?.clone();
        match layer.source {
            prisma_core::show::Source::Shader {
                effect_id,
                params,
                palette,
            } => Some(ClipEffect {
                effect_id,
                params,
                palette,
                fade_time: Duration::ZERO,
            }),
        }
    }
    fn active_changed(&self, track: Uuid, active: Option<usize>) {
        let mut show = self.0.lock().unwrap();
        if let Some(t) = show.track_mut(track) {
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
fn pads_play_clips_through_the_tree() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    // One show for both sides: track ids are minted by `Show::new`.
    let show = show();
    let track = show.home_track(ZONE).unwrap().id;
    let host = Arc::new(Host(Mutex::new(show.clone())));
    let engine = Engine::start_show(show, config()).expect("engine starts");
    engine.set_clip_host(host.clone());

    let wire = |lo: u8, hi: u8| {
        let snap = dev_sink.snapshot();
        snap.universes
            .get(&1)
            .is_some_and(|u| u.data.len() >= 12 && (lo..=hi).contains(&u.data[0]))
    };
    let path = |clip: usize, leaf: &str| format!("/track/{track}/clip/{clip}/{leaf}");
    let state = |engine: &prisma_engine::EngineHandle, cell: usize| {
        engine
            .tree()
            .resolve(&path(cell, "state"))
            .unwrap()
            .read_real()
            .unwrap()
    };
    let midi = SourceId::new(priority::MIDI, 1);
    let active = || host.0.lock().unwrap().track(track).unwrap().active;
    // The old per-zone cell targets are gone.
    assert!(engine
        .tree()
        .resolve(&format!("/zone/{ZONE}/cell/0/launch"))
        .is_none());

    // States published from the show: running, assigned, empty.
    wait_for("initial clip states", || {
        state(&engine, 0) == CLIP_RUNNING
            && state(&engine, 1) == CLIP_ASSIGNED
            && state(&engine, 2) == CLIP_EMPTY
    });
    wait_for("grey on the wire", || wire(120, 136));

    // Momentary: hold white, then release → back to grey.
    let hold1 = engine.tree().resolve(&path(1, "hold")).unwrap();
    engine.tree().write_real(&hold1, 1.0, midi);
    wait_for("held white on the wire", || wire(250, 255));
    wait_for("held clip runs", || {
        state(&engine, 1) == CLIP_RUNNING && state(&engine, 0) == CLIP_ASSIGNED
    });
    assert_eq!(active(), Some(1), "the host hears the running clip");
    engine.tree().write_real(&hold1, 0.0, midi);
    wait_for("release returns to grey", || {
        wire(120, 136) && active() == Some(0)
    });

    // Latch (the UI click path): white becomes the base and stays.
    assert!(engine.fire_clip(0, 1));
    wait_for("latched white", || wire(250, 255) && active() == Some(1));
    // The started clip's stored params are a load, not a lock: the panel (UI tier) can
    // move one right away instead of waiting out the takeover timeout.
    let r = engine
        .tree()
        .resolve(&format!("/track/{track}/layer/0/param/r"))
        .unwrap();
    assert_eq!(
        engine.tree().write(&r, 0.7, SourceId::new(priority::UI, 1)),
        prisma_core::WriteOutcome::Accepted
    );

    // A hold on top of the latch, cleared by a new latch: the hold param is reset so
    // a toggle pad's next tap presses again.
    let hold0 = engine.tree().resolve(&path(0, "hold")).unwrap();
    engine.tree().write_real(&hold0, 1.0, midi);
    wait_for("held grey over white", || wire(120, 136));
    assert!(engine.fire_clip(0, 1));
    wait_for("latch clears the hold", || {
        wire(250, 255) && hold0.read_bool() == Some(false)
    });

    // Trigger restarts (the running clip stays the same); an empty clip stops the track.
    let trig0 = engine.tree().resolve(&path(0, "trigger")).unwrap();
    engine.tree().fire(&trig0, midi);
    wait_for("trigger plays grey", || {
        wire(120, 136) && active() == Some(0)
    });
    assert!(engine.fire_clip(0, 2));
    wait_for("empty clip goes dark", || wire(0, 4) && active() == Some(2));

    // Clip edit: clip 2 gets an effect; a refresh publishes it (running, since active).
    host.0.lock().unwrap().tracks[0].clips[2] = solid(1.0);
    engine.refresh_clips(0);
    wait_for("refreshed state", || state(&engine, 2) == CLIP_RUNNING);

    // Recompile onto the host's current show: the controller follows the new tree.
    let current = host.0.lock().unwrap().clone();
    let engine = engine.recompile_show(current, config()).expect("recompile");
    wait_for("states after recompile", || {
        state(&engine, 2) == CLIP_RUNNING && state(&engine, 0) == CLIP_ASSIGNED
    });
    let hold0 = engine.tree().resolve(&path(0, "hold")).unwrap();
    engine.tree().write_real(&hold0, 1.0, midi);
    wait_for("hold works after recompile", || {
        wire(120, 136) && active() == Some(0)
    });

    // A grid pad learned on track slot 0's clip 1 plays it (here: white, over the held
    // grey).
    engine.tree().write_real(&hold0, 0.0, midi);
    wait_for("hold released", || active() == Some(2));
    let midi_rt = engine.midi();
    midi_rt.arm_learn(prisma_core::MapTarget::TrackSlot {
        slot: 0,
        sub: prisma_core::TrackSub::Clip(1),
    });
    midi_rt.feed(&[0x90, 33, 127]); // learned as a latch → launches clip 1
    wait_for("the pad latched the track's clip 1", || {
        wire(250, 255) && active() == Some(1)
    });

    let report = engine.shutdown();
    assert_eq!(
        report.gpu_strong_refs_at_release, 1,
        "clips released the GPU"
    );
    let _ = dev_sink.stop_and_collect();
}

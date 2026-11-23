//! LIVE-MODEL-SPEC §10.2 through the real engine: reordering tracks is live — no
//! recompile, an armed clip stays armed, the selection stays on the same track, every
//! positional read (the UI's slot, the controller's bank window) follows the new order on
//! screen, and what plays on the wire does not change.
//!
//! One test function, its own binary (one Vulkan bring-up at a time — see m8_engine_show).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, Layer, ParamSet, Show};
use prisma_engine::{ClipEffect, ClipHost, Engine, EngineConfig};
use prisma_midi::DeviceProfile;
use prisma_output::sink;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

const ZA: Uuid = Uuid::from_u128(0xA);
const ZB: Uuid = Uuid::from_u128(0xB);

/// Two zones of one pixel each: zone A on universe 1 channels 1–3, zone B on 4–6.
fn patch() -> Patch {
    let fixture = |id: u128, zone: Uuid, ch: u16| Fixture {
        id: Uuid::from_u128(id),
        name: format!("px{id}"),
        profile: FixtureProfile {
            name: "rgb".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
            white: prisma_patch::WhiteProfile::default(),
        },
        pixels: vec![Pixel {
            index: 0,
            position: glam::Vec3::new(ch as f32 / 3.0, 0.0, 0.0),
            universe: 1,
            start_channel: ch,
            color_order: ColorOrder::Rgb,
        }],
        zone,
    };
    let zone = |id: Uuid, fixture: u128, colour: [u8; 3]| Zone {
        id,
        name: format!("{id}"),
        members: vec![PixelRef {
            fixture: Uuid::from_u128(fixture),
            pixel: 0,
        }],
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        mapping_size: [1, 1, 1],
        display_color: colour,
        world_transform: None,
    };
    Patch {
        id: Uuid::from_u128(0x3300),
        name: "control".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![fixture(0x51, ZA, 1), fixture(0x52, ZB, 4)],
        zones: vec![zone(ZA, 0x51, [255, 0, 0]), zone(ZB, 0x52, [0, 0, 255])],
        groups: Vec::new(),
    }
}

/// A grayscale gradient: its two scalars (`start`, `end`) are the knob parameters 0 and
/// 1. With both ends equal the pixel is that grey wherever it samples.
fn grad(start: f32, end: f32) -> Cell {
    let mut p = ParamSet::new();
    p.set("start", start);
    p.set("end", end);
    Cell::single(Layer::shader("gradient", p))
}

/// The app's role: owns the show, hears about the running clip, the selection and the
/// knob-stored parameters.
#[derive(Default)]
struct Host {
    show: Mutex<Option<Show>>,
    selection: Mutex<Vec<(Option<Uuid>, Option<usize>)>>,
}

impl ClipHost for Host {
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect> {
        let show = self.show.lock().unwrap();
        let layer = show
            .as_ref()?
            .track(track)?
            .clips
            .get(clip)?
            .layers
            .first()?
            .clone();
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
        if let Some(t) = self.show.lock().unwrap().as_mut().unwrap().track_mut(track) {
            t.active = active;
        }
    }
    fn selection_changed(&self, track: Option<Uuid>, clip: Option<usize>) {
        self.selection.lock().unwrap().push((track, clip));
    }
    fn param_stored(&self, track: Uuid, clip: usize, name: &str, value: f32) {
        let mut show = self.show.lock().unwrap();
        let cell = &mut show.as_mut().unwrap().track_mut(track).unwrap().clips[clip];
        let prisma_core::show::Source::Shader { params, .. } = &mut cell.layers[0].source;
        params.set(name, value);
    }
}

impl Host {
    fn last_selection(&self) -> Option<(Option<Uuid>, Option<usize>)> {
        self.selection.lock().unwrap().last().copied()
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

/// Track A (zone A): clip 0 grey, running. Track B (zone B): clip 0 white, clip 1 grey;
/// nothing running.
fn show() -> Show {
    let mut show = Show::new("reorder", patch());
    let a = show.home_track_mut(ZA).unwrap();
    a.clips = vec![grad(0.5, 0.5)];
    a.active = Some(0);
    let b = show.home_track_mut(ZB).unwrap();
    b.clips = vec![grad(1.0, 1.0), grad(0.5, 0.5)];
    b.active = None;
    for z in &mut show.zones {
        z.live.dimmer = 1.0;
    }
    show
}

#[test]
fn reordering_is_live() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let show = show();
    let (ta, tb) = (show.tracks[0].id, show.tracks[1].id);
    let host = Arc::new(Host::default());
    *host.show.lock().unwrap() = Some(show.clone());
    let engine = Engine::start_show(show, config).expect("engine starts");
    engine.set_clip_host(host.clone());

    let px = |at: usize| -> Option<[u8; 3]> {
        let snap = dev_sink.snapshot();
        let u = snap.universes.get(&1)?;
        (u.data.len() >= at + 3).then(|| [u.data[at], u.data[at + 1], u.data[at + 2]])
    };
    let grey = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| (120..=136).contains(v)));
    let white = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v >= 250));
    let dark = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v <= 4));
    wait_for("A grey, B dark", || grey(px(0)) && dark(px(3)));

    // B's clip 1 armed, B selected, B's opacity set — all before the reorder.
    let state = |track: Uuid, clip: usize| {
        engine
            .tree()
            .resolve(&format!("/track/{track}/clip/{clip}/state"))
            .and_then(|h| h.read_real())
    };
    assert!(engine.arm_clip(1, 1));
    wait_for("B clip 1 armed", || state(tb, 1) == Some(3.0));
    engine.select(Some(1), None);
    wait_for("B selected", || {
        host.last_selection() == Some((Some(tb), None))
    });
    engine.set_track_opacity(1, 0.3);

    // Reorder: B first. No recompile — the same engine keeps running.
    assert!(engine.reorder_tracks(&[tb, ta]));
    assert_eq!(state(tb, 1), Some(3.0), "still armed");
    assert_eq!(
        engine.selection(),
        (Some(0), None),
        "the selection follows B to position 0"
    );
    let live = engine.track_live(0).expect("position 0");
    assert!((live.opacity - 0.3).abs() < 1e-3, "position 0 is B now");
    assert!(grey(px(0)) && dark(px(3)), "the wire did not change");
    // A wrong order is refused (and changes nothing).
    assert!(!engine.reorder_tracks(&[tb]));
    assert!(!engine.reorder_tracks(&[tb, tb]));

    // A positional write reaches B too: back to full opacity.
    engine.set_track_opacity(0, 1.0);

    // GO plays the clip that stayed armed.
    assert!(engine.go());
    wait_for("GO plays B's armed clip (grey)", || grey(px(3)));

    // The controller follows the screen: column 1 (track slot 0) is B now. Its top pad
    // (note 32, clip 0) launches B's white clip.
    let midi = engine.midi().clone();
    midi.set_device(DeviceProfile::apc40_mk2());
    midi.seed_device_defaults().expect("seeded");
    midi.feed(&[0x90, 32, 127]);
    midi.feed(&[0x80, 32, 0]);
    wait_for("the pad under column 1 launched B's clip 0", || {
        white(px(3))
    });
    assert!(grey(px(0)), "A untouched");

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

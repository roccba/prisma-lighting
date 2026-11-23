//! Live model phase 3 (docs/LIVE-MODEL-SPEC.md §8.2) through the real engine: the
//! APC40 mkII defaults on a two-track show — a grid pad launches a clip **on the wire**,
//! the selection moves through the tree from the UI (`select`), from MIDI (track select)
//! and from the keyboard paths (`/select/move/*`, `/select/launch`), and a device knob
//! moves the selected clip's parameter: stored in the show, and live on the wire when the
//! clip is running. Bytes enter through `MidiRuntime::feed`, no hardware.
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

/// Track A (zone A): clip 0 grey, clip 1 white, none running. Track B (zone B): clip 0
/// grey running, clip 1 empty.
fn show() -> Show {
    let mut show = Show::new("control", patch());
    let a = show.home_track_mut(ZA).unwrap();
    a.clips = vec![grad(0.5, 0.5), grad(1.0, 1.0)];
    a.active = None;
    let b = show.home_track_mut(ZB).unwrap();
    b.clips = vec![grad(0.5, 0.5), Cell::empty()];
    b.active = Some(0);
    for z in &mut show.zones {
        z.live.dimmer = 1.0;
    }
    show
}

/// The app's role: owns the show, hears about the running clip, the selection and the
/// knob-stored parameters.
#[derive(Default)]
struct Host {
    show: Mutex<Option<Show>>,
    selection: Mutex<Vec<(Option<Uuid>, Option<usize>)>>,
    scenes_fired: Mutex<Vec<usize>>,
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
    fn scene_fired(&self, row: usize) {
        self.scenes_fired.lock().unwrap().push(row);
    }
    fn param_stored(&self, track: Uuid, clip: usize, name: &str, value: f32) {
        let mut show = self.show.lock().unwrap();
        let cell = &mut show.as_mut().unwrap().track_mut(track).unwrap().clips[clip];
        let prisma_core::show::Source::Shader { params, .. } = &mut cell.layers[0].source;
        params.set(name, value);
    }
}

impl Host {
    fn stored(&self, track: Uuid, clip: usize, name: &str) -> Option<f32> {
        let show = self.show.lock().unwrap();
        let cell = &show.as_ref()?.track(track)?.clips[clip];
        let prisma_core::show::Source::Shader { params, .. } = &cell.layers[0].source;
        params.get(name)
    }
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

#[test]
fn pads_knobs_and_selection_through_the_engine() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let show = show();
    let (ta, tb) = (show.tracks[0].id, show.tracks[1].id);
    let host = Arc::new(Host::default());
    *host.show.lock().unwrap() = Some(show.clone());
    let engine = Engine::start_show(show, config).expect("engine starts");
    engine.set_clip_host(host.clone());

    // Universe 1 bytes: zone A's pixel at 0..3, zone B's at 3..6.
    let px = |at: usize| -> Option<[u8; 3]> {
        let snap = dev_sink.snapshot();
        let u = snap.universes.get(&1)?;
        (u.data.len() >= at + 3).then(|| [u.data[at], u.data[at + 1], u.data[at + 2]])
    };
    let grey = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| (120..=136).contains(v)));
    let white = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v >= 250));
    let dark = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v <= 4));
    wait_for("B grey, A dark", || grey(px(3)) && dark(px(0)));

    // --- The APC40 mkII defaults, as on the first connection of an empty profile. ---
    let midi = engine.midi().clone();
    midi.set_device(DeviceProfile::apc40_mk2());
    let (_, added) = midi.seed_device_defaults().expect("seeded");
    assert!(added > 40);

    // A grid pad launches a clip on the wire. Column 1 is track slot 0 = track A; the
    // top row (note 32) is clip 0, the row below (note 24) clip 1 (white).
    midi.feed(&[0x90, 24, 127]);
    wait_for("pad: track A clip 1 (white) on the wire", || white(px(0)));
    midi.feed(&[0x80, 24, 0]);

    // --- Selection from the UI: track B (slot 1), its running clip. ---
    engine.select(Some(1), None);
    wait_for("UI selection reaches the host", || {
        host.last_selection() == Some((Some(tb), None))
    });
    assert_eq!(engine.selection(), (Some(1), None));

    // Knobs drive the continuous parameters only: a colour-only effect has none.
    assert!(prisma_engine::clips::knob_params("solid").is_empty());
    let names: Vec<_> = prisma_engine::clips::knob_params("gradient")
        .iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["start", "end"]);

    // A device knob drives parameter 0 (`start`) of the selected clip — B's running
    // grey — live and stored in the show. Pickup: the knob first meets the value (0.5 →
    // position 64), then turns to the top.
    midi.feed(&[0xB0, 16, 64]);
    midi.feed(&[0xB0, 16, 127]);
    let live_start = engine
        .tree()
        .resolve(&format!("/track/{tb}/layer/0/param/start"))
        .unwrap();
    wait_for("knob → B's live `start`", || {
        live_start.read_real().is_some_and(|v| v > 0.99)
    });
    wait_for("knob value stored in the clip", || {
        host.stored(tb, 0, "start").is_some_and(|v| v > 0.99)
    });

    // --- Selection from MIDI: track select on column 1 (note 51, channel 0) → track A,
    //     on the clip it had last (none → its running clip, white). ---
    midi.feed(&[0x90, 51, 127]);
    wait_for("MIDI track select reaches the host", || {
        host.last_selection() == Some((Some(ta), None))
    });
    // The knobs now show A's clip: `start` = 1.0 → the mirror moved /select/param/0 to 1.
    let knob0 = engine.tree().resolve("/select/param/0").unwrap();
    wait_for("knobs mirror the newly selected clip", || {
        knob0.read_real().is_some_and(|v| v > 0.99)
    });

    // --- Keyboard paths: up/down within the track, Enter launches the selected clip. ---
    assert!(engine.activate_path("/select/move/up")); // white is clip 1 → up = clip 0
    wait_for("move up selects clip 0", || {
        host.last_selection() == Some((Some(ta), Some(0)))
    });
    // Editing a clip that is NOT running stores it but leaves the wire alone.
    midi.feed(&[0xB0, 16, 127]); // knob at 1.0; A clip 0's start is 0.5: not picked up yet
    midi.feed(&[0xB0, 16, 64]); // crosses 0.5 → engaged
    midi.feed(&[0xB0, 16, 0]);
    wait_for("stored on the selected (not running) clip", || {
        host.stored(ta, 0, "start").is_some_and(|v| v < 0.01)
    });
    assert!(
        white(px(0)),
        "a clip that is not running never touches the wire"
    );
    assert!(engine.activate_path("/select/launch"));
    // The launched clip is the edited one (start 0, end 0.5): no longer white.
    wait_for("Enter launched the edited clip", || {
        px(0).is_some_and(|c| c.iter().all(|v| *v <= 136))
    });
    // Right keeps the row: track B, clip 0.
    assert!(engine.activate_path("/select/move/right"));
    wait_for("move right keeps the row", || {
        host.last_selection() == Some((Some(tb), Some(0)))
    });
    // A scene fired by its path (what the keyboard and a controller write) is reported to
    // the host, launched or armed — the UI flashes it.
    assert!(engine.activate_path("/scene/1/launch"));
    assert!(engine.activate_path("/scene/0/arm"));
    wait_for("both scene fires reported", || {
        *host.scenes_fired.lock().unwrap() == [1, 0]
    });

    // Toggles flip (blackout), levels are not actions.
    assert!(engine.activate_path("/master/blackout"));
    wait_for("blackout on the wire", || dark(px(0)) && dark(px(3)));
    assert!(engine.activate_path("/master/blackout"));
    assert!(!engine.activate_path("/master/dimmer"));
    assert!(!engine.activate_path("/no/such/path"));

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

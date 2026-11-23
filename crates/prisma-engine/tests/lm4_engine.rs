//! Live model phase 4 (docs/LIVE-MODEL-SPEC.md §8.4) through the real engine: a track
//! whose destination is two zones lights both on the wire with ONE set of parameters —
//! the panel's write (`set_track_param`) and a device knob (`/select/param/n`) move both
//! zones at once — and a destination change goes through `recompile_show`.
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

/// One track, running a grey gradient, reaching `zones`.
fn show(zones: Vec<Uuid>) -> Show {
    let mut show = Show::new("multizone", patch());
    show.tracks.truncate(1);
    let t = &mut show.tracks[0];
    t.destination.zones = zones;
    t.clips = vec![grad(0.5, 0.5)];
    t.active = Some(0);
    for z in &mut show.zones {
        z.live.dimmer = 1.0;
    }
    show
}

#[test]
fn one_track_two_zones_one_set_of_params() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());

    let px = |at: usize| -> Option<[u8; 3]> {
        let snap = dev_sink.snapshot();
        let u = snap.universes.get(&1)?;
        (u.data.len() >= at + 3).then(|| [u.data[at], u.data[at + 1], u.data[at + 2]])
    };
    let grey = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| (120..=136).contains(v)));
    let white = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v >= 250));
    let dark = |p: Option<[u8; 3]>| p.is_some_and(|c| c.iter().all(|v| *v <= 4));

    // --- Destination zone A only; then the same track reaching A and B, through a
    //     recompile (what the app's "Aplicar destino" does). ---
    let mut single = show(vec![ZA]);
    let engine = Engine::start_show(single.clone(), config()).expect("engine starts");
    wait_for("A grey, B dark", || grey(px(0)) && dark(px(3)));
    single.tracks[0].destination.zones = vec![ZA, ZB];
    let multi = single;
    let track = multi.tracks[0].id;
    let engine = engine
        .recompile_show(multi.clone(), config())
        .expect("recompile with two zones");
    let host = Arc::new(Host::default());
    *host.show.lock().unwrap() = Some(multi);
    engine.set_clip_host(host.clone());
    wait_for("both zones grey after the destination change", || {
        grey(px(0)) && grey(px(3))
    });

    // --- The panel: one write per param, both zones follow. ---
    engine.set_track_param(track, "start", 1.0);
    engine.set_track_param(track, "end", 1.0);
    wait_for("panel → both zones white", || {
        white(px(0)) && white(px(3))
    });

    // --- A device knob on the selected (running) clip: both zones follow. ---
    let midi = engine.midi().clone();
    midi.set_device(DeviceProfile::apc40_mk2());
    midi.seed_device_defaults().expect("seeded");
    engine.select(Some(0), None);
    wait_for("selection reaches the host", || {
        host.last_selection() == Some((Some(track), None))
    });
    // Pickup: meet the value (1.0 → 127), then turn both knobs to 0.
    for cc in [16u8, 17] {
        midi.feed(&[0xB0, cc, 127]);
        midi.feed(&[0xB0, cc, 0]);
    }
    wait_for("knobs → both zones dark", || dark(px(0)) && dark(px(3)));
    assert!(host.stored(track, 0, "start").is_some_and(|v| v < 0.01));

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

//! Live model phase 5, step 2, end to end: the full engine (render loop, gather, output
//! loop, Art-Net) on the demo rig, a track recompiled from "the whole ceiling" to the
//! group "Tubos pares", and the virtual sink receiving light only on the group's pixels
//! (docs/LIVE-MODEL-SPEC.md §8.6).
//!
//! The software adapter (lavapipe) keeps it deterministic.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_core::show::{
    Cell, Destination, GroupMaster, GroupPreset, Layer, ParamSet, Show, Track,
};
use prisma_engine::demo::{demo_show, DEMO_GROUPS, DEMO_ZONES};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_render::AdapterPreference;

/// The demo rig with one track: solid white at full, reaching `dest`.
fn show_with(dest: Destination) -> Show {
    let mut show = demo_show();
    let mut params = ParamSet::new();
    for c in ["r", "g", "b"] {
        params.set(c, 1.0);
    }
    let mut track = Track::new("blanco", dest);
    track.clips = vec![Cell::single(Layer::shader("solid", params))];
    track.active = Some(0);
    show.tracks = vec![track];
    show.validate().expect("valid show");
    show
}

/// Every pixel of the patch: (fixture name, universe, first channel).
fn pixels(show: &Show) -> Vec<(String, u16, u16)> {
    show.patch
        .fixtures
        .iter()
        .flat_map(|f| {
            f.pixels
                .iter()
                .map(move |p| (f.name.clone(), p.universe, p.start_channel))
        })
        .collect()
}

/// Wait until the sink's picture satisfies `pred` (per fixture: lit pixel count), or
/// fail with the last picture.
fn wait_for(
    sink: &sink::SinkHandle,
    px: &[(String, u16, u16)],
    what: &str,
    pred: impl Fn(&HashMap<String, usize>) -> bool,
) -> HashMap<String, usize> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snap = sink.snapshot();
        let mut lit: HashMap<String, usize> = HashMap::new();
        for (name, universe, channel) in px {
            let on = snap.universes.get(universe).is_some_and(|d| {
                let c = (*channel as usize) - 1;
                d.data
                    .get(c..c + 3)
                    .is_some_and(|rgb| rgb.iter().any(|&b| b > 8))
            });
            *lit.entry(name.clone()).or_default() += usize::from(on);
        }
        if pred(&lit) {
            return lit;
        }
        assert!(Instant::now() < deadline, "{what}: {lit:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_recompiled_group_destination_lights_only_the_groups_pixels_on_the_wire() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());

    // Start: the whole ceiling, every tube lit, the curtains dark.
    let whole = show_with(Destination::zone(DEMO_ZONES[0]));
    let px = pixels(&whole);
    let tube = |t: u32| format!("Tubo {t}");
    let engine = Engine::start_show(whole, config()).expect("engine starts");
    wait_for(&dev_sink, &px, "every tube lit", |lit| {
        (1..=8).all(|t| lit.get(&tube(t)) == Some(&60))
    });

    // Recompile, as "Aplicar destino" does: the group of even tubes only.
    let even = show_with(Destination {
        zones: vec![],
        groups: vec![DEMO_GROUPS[0]],
    });
    let engine = engine
        .recompile_show(even, config())
        .expect("recompile applies");
    let lit = wait_for(&dev_sink, &px, "only the even tubes", |lit| {
        (1..=8).all(|t| lit.get(&tube(t)) == Some(if t % 2 == 0 { &60 } else { &0 }))
    });
    // Nothing outside the ceiling either.
    for (name, count) in &lit {
        if !name.starts_with("Tubo") {
            assert_eq!(*count, 0, "{name} stays dark");
        }
    }

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

#[test]
fn group_masters_and_presets_dim_their_pixels_on_the_wire() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let tube = |t: u32| format!("Tubo {t}");

    // The whole ceiling white; the even tubes' master saved at 0; a preset that takes
    // the odd tubes to 0 with no fade.
    let mut show = show_with(Destination::zone(DEMO_ZONES[0]));
    show.group_masters = vec![GroupMaster {
        group: DEMO_GROUPS[0],
        level: 0.0,
    }];
    show.group_presets = vec![GroupPreset {
        id: uuid::Uuid::from_u128(0x9),
        name: "Impares fuera".into(),
        levels: vec![GroupMaster {
            group: DEMO_GROUPS[1],
            level: 0.0,
        }],
        fade: 0.0,
    }];
    show.validate().expect("valid show");
    let px = pixels(&show);
    let engine = Engine::start_show(show, config).expect("engine starts");
    wait_for(
        &dev_sink,
        &px,
        "the saved master darkens the even tubes",
        |lit| (1..=8).all(|t| lit.get(&tube(t)) == Some(if t % 2 == 0 { &0 } else { &60 })),
    );

    // Live: the master up again, through the tree as a fader writes it.
    let even = format!("/group/{}/dimmer", DEMO_GROUPS[0]);
    engine.tree().write_path(
        &even,
        1.0,
        prisma_core::source::SourceId::new(prisma_core::source::priority::UI, 1),
    );
    wait_for(&dev_sink, &px, "every tube lit", |lit| {
        (1..=8).all(|t| lit.get(&tube(t)) == Some(&60))
    });

    // The preset, by its path (a pad or a click).
    assert!(engine.activate_path("/group-preset/0/apply"));
    wait_for(&dev_sink, &px, "the preset darkens the odd tubes", |lit| {
        (1..=8).all(|t| lit.get(&tube(t)) == Some(if t % 2 == 0 { &60 } else { &0 }))
    });

    // Presets replaced in place (the Grupos tab), no recompile: the new preset 0 cuts
    // the even tubes and brings the odd ones back.
    let demo = demo_show();
    engine.set_group_presets(
        &demo.patch.groups,
        &[GroupPreset {
            id: uuid::Uuid::from_u128(0xA),
            name: "Pares fuera".into(),
            levels: vec![
                GroupMaster {
                    group: DEMO_GROUPS[0],
                    level: 0.0,
                },
                GroupMaster {
                    group: DEMO_GROUPS[1],
                    level: 1.0,
                },
            ],
            fade: 0.0,
        }],
    );
    assert!(engine.activate_path("/group-preset/0/apply"));
    wait_for(&dev_sink, &px, "the reloaded preset applies", |lit| {
        (1..=8).all(|t| lit.get(&tube(t)) == Some(if t % 2 == 0 { &0 } else { &60 }))
    });
    assert_eq!(engine.group_levels()[0], 0.0);

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

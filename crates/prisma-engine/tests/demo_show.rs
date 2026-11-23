//! The demo show (`prisma_engine::demo`) loads and makes sense: it survives the file
//! format (save → load), its patch compiles, every clip names a real effect with real
//! parameters, the scenes are the four rows they claim to be, the network goes to the
//! virtual sink, and the real engine lights every zone with it — the RGBW curtain
//! included. It is also the show the UI end-to-end tests run on, so a change that breaks
//! it fails here first.
//!
//! The GPU part is one test function in its own binary (one Vulkan bring-up at a time).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_core::show::LaunchMode;
use prisma_core::show::{load_show, save_show, Show, Source};
use prisma_core::TimeBase;
use prisma_engine::demo::{demo_show, DEMO_SCENES, DEMO_TRACKS, DEMO_ZONES, STROBE_ROW};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{BlendMode, ComponentSet};
use prisma_render::{manifest_by_id, AdapterPreference};

fn effect(show: &Show, track: usize, row: usize) -> Option<&str> {
    let cell = show.tracks[track].clips.get(row)?;
    let Source::Shader { effect_id, .. } = &cell.layers.first()?.source;
    Some(effect_id)
}

#[test]
fn the_demo_survives_the_file_format_and_is_deterministic() {
    let show = demo_show();
    let json = save_show(&show).expect("saves");
    let back = load_show(&json).expect("loads");
    assert_eq!(back, show, "save → load gives the same show");
    assert_eq!(demo_show(), show, "same show, same ids, every call");
}

#[test]
fn the_patch_is_four_zones_with_an_rgbw_curtain() {
    let show = demo_show();
    let patch = &show.patch;
    let index = patch
        .compile()
        .expect("the patch compiles (addresses, positions)");
    let names: Vec<_> = patch.zones.iter().map(|z| z.name.as_str()).collect();
    assert_eq!(
        names,
        ["Techo", "Cortina Norte", "Cortina Este", "Cortina Oeste"]
    );
    let ids: Vec<_> = patch.zones.iter().map(|z| z.id).collect();
    assert_eq!(ids, DEMO_ZONES);

    let px = |zone: usize| patch.zones[zone].members.len();
    assert_eq!(px(0), 8 * 60, "8 tubes of 60");
    for (z, name) in names.iter().enumerate().skip(1) {
        assert_eq!(px(z), 16 * 24, "{name} is 16 × 24");
    }
    assert_eq!(index.pixel_count(), 480 + 3 * 384);
    assert_eq!(patch.zones[0].mapping_size, [60, 8, 1]);
    assert_eq!(patch.zones[2].mapping_size, [16, 24, 1]);

    let all = |zone: usize, c: ComponentSet| {
        patch
            .fixtures
            .iter()
            .filter(|f| f.zone == DEMO_ZONES[zone])
            .all(|f| f.profile.components == c)
    };
    assert!(all(2, ComponentSet::Rgbw), "Cortina Este is RGBW");
    for z in [0, 1, 3] {
        assert!(all(z, ComponentSet::Rgb), "{} is RGB", names[z]);
    }
}

#[test]
fn every_clip_is_a_real_effect_with_real_parameters() {
    let show = demo_show();
    for t in &show.tracks {
        for (row, cell) in t.clips.iter().enumerate() {
            let Some(layer) = cell.layers.first() else {
                continue;
            };
            let Source::Shader {
                effect_id, params, ..
            } = &layer.source;
            let manifest = manifest_by_id(effect_id)
                .unwrap_or_else(|| panic!("{} row {row}: no effect {effect_id}", t.name));
            for (name, value) in params.iter() {
                assert!(
                    manifest.params.iter().any(|p| p.name == name),
                    "{} row {row}: {effect_id} has no {name}",
                    t.name
                );
                assert!(
                    (0.0..=1.0).contains(&value),
                    "{} row {row}: {name} = {value} is not normalized",
                    t.name
                );
            }
            assert!(cell.cell_name.is_some(), "{} row {row} has a label", t.name);
        }
    }
}

#[test]
fn tracks_and_scenes_are_what_the_demo_promises() {
    let show = demo_show();
    let t = &show.tracks;
    let z = |ix: &[usize]| ix.iter().map(|&i| DEMO_ZONES[i]).collect::<Vec<_>>();
    use BlendMode::*;
    let expected = [
        ("Techo · Fondo", z(&[0]), Normal, 3.0, TimeBase::Seconds),
        ("Techo · Golpes", z(&[0]), Add, 0.0, TimeBase::Seconds),
        ("Techo · Máscara", z(&[0]), Mask, 2.0, TimeBase::Seconds),
        (
            "Cortinas · Fondo",
            z(&[1, 2, 3]),
            Normal,
            4.0,
            TimeBase::Seconds,
        ),
        ("Norte · Figuras", z(&[1]), Normal, 1.0, TimeBase::Beats),
        ("Este · Key", z(&[2]), LumaKey, 2.0, TimeBase::Seconds),
        (
            "Oeste · Multiplicar",
            z(&[3]),
            Multiply,
            2.0,
            TimeBase::Beats,
        ),
        (
            "Cortinas · Acentos",
            z(&[1, 2, 3]),
            Max,
            0.5,
            TimeBase::Seconds,
        ),
        ("Strobe", z(&[0, 1, 2, 3]), Add, 0.0, TimeBase::Seconds),
    ];
    assert_eq!(t.len(), expected.len());
    assert!(t.len() > 8, "more tracks than an APC40 bank shows");
    for (track, (name, zones, blend, time, base)) in t.iter().zip(expected) {
        assert_eq!(track.name, name);
        assert_eq!(track.destination.zones, zones, "{name}");
        assert_eq!(track.blend_mode, blend, "{name}");
        assert_eq!(track.transition.time, time, "{name}");
        assert_eq!(track.transition.base, base, "{name}");
    }
    assert_eq!(t.iter().map(|t| t.id).collect::<Vec<_>>(), DEMO_TRACKS);
    // Every zone has at least two tracks of its own besides the shared ones.
    for (zone, own) in [(0, 3), (1, 1), (2, 1), (3, 1)] {
        let alone = t
            .iter()
            .filter(|t| t.destination.zones == [DEMO_ZONES[zone]])
            .count();
        assert_eq!(alone, own, "zone {zone}'s own tracks");
    }
    assert_eq!(
        t.iter()
            .filter(|t| t.launch == LaunchMode::Arm)
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Cortinas · Acentos"]
    );
    assert!(
        t.iter().any(|t| t.opacity < 1.0),
        "a track below full opacity"
    );
    assert!(
        t.iter()
            .flat_map(|t| &t.clips)
            .any(|c| !c.fade_time.is_zero()),
        "a clip with its own fade"
    );

    let labels: Vec<_> = show
        .scenes
        .iter()
        .map(|s| s.name.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(labels, DEMO_SCENES);
    // Every scene but the strobe defines the whole look: a clip on every track.
    for row in [0, 1, 2, 4] {
        for (i, track) in t.iter().enumerate() {
            assert!(effect(&show, i, row).is_some(), "{} row {row}", track.name);
        }
    }
    // The strobe scene: only the strobe, over whatever look runs.
    for (i, track) in t.iter().enumerate() {
        let has = effect(&show, i, STROBE_ROW).is_some();
        assert_eq!(
            has,
            track.name == "Strobe",
            "{} in the strobe row",
            track.name
        );
    }
    assert_eq!(effect(&show, 8, STROBE_ROW), Some("pulse"));

    // The crossfade pairs: same effect, different values, on a track with a transition.
    let params = |track: usize, row: usize| {
        let Source::Shader { params, .. } = &show.tracks[track].clips[row].layers[0].source;
        params.clone()
    };
    for (track, a, b, id) in [(0, 0, 1, "plasma"), (3, 1, 2, "drops")] {
        assert_eq!(effect(&show, track, a), Some(id));
        assert_eq!(effect(&show, track, b), Some(id));
        assert_ne!(params(track, a), params(track, b), "{id} pair differs");
        assert!(t[track].transition.time > 0.0);
    }

    // Every effect of the catalogue is somewhere in the grid.
    let used: HashSet<&str> = (0..t.len())
        .flat_map(|i| (0..t[i].clips.len()).map(move |r| (i, r)))
        .filter_map(|(i, r)| effect(&show, i, r))
        .collect();
    for listing in prisma_render::catalogue() {
        assert!(
            used.contains(listing.id),
            "{} is not in the demo",
            listing.id
        );
    }

    // "Tranquilo" plays when the show opens.
    assert!(t.iter().all(|t| t.active == Some(0)));
    // Zone live state: the RGBW curtain at a fixed warm white, Oeste at its own speed.
    assert!(!show.zones[2].live.white_auto);
    assert_eq!(show.zones[2].live.white_kelvin, 3000.0);
    assert_eq!(show.zones[3].live.speed_multiplier, 0.6);
}

/// The neutral clips really are neutral: for its track's blend, the clip leaves the
/// picture below unchanged (checked against the blend maths in `blend.wgsl`).
#[test]
fn neutral_clips_match_their_tracks_blend() {
    let show = demo_show();
    for track in &show.tracks {
        for (row, cell) in track.clips.iter().enumerate() {
            if cell.cell_color != Some([64, 64, 64]) {
                continue;
            }
            let Source::Shader {
                effect_id, params, ..
            } = &cell.layers[0].source;
            let rgb = |c| params.get(c).unwrap_or(0.0);
            let what = match effect_id.as_str() {
                "solid" if [rgb("r"), rgb("g"), rgb("b")] == [0.0; 3] => "black",
                "solid" if [rgb("r"), rgb("g"), rgb("b")] == [1.0; 3] => "white",
                "gradient"
                    if params.get("start") == Some(0.0)
                        && params.get("end") == Some(0.0)
                        && params.get("background") == Some(0.0) =>
                {
                    "transparent"
                }
                other => panic!("{} row {row}: {other} is not a neutral clip", track.name),
            };
            let ok = match track.blend_mode {
                BlendMode::Add | BlendMode::Max | BlendMode::LumaKey => what == "black",
                BlendMode::Mask | BlendMode::Multiply => what == "white",
                BlendMode::Normal => what == "transparent",
                other => panic!("no neutral clip for {other:?}"),
            };
            assert!(
                ok,
                "{} row {row}: {what} on {:?}",
                track.name, track.blend_mode
            );
        }
    }
}

#[test]
fn the_network_is_the_virtual_sink_only() {
    let net = demo_show().network;
    assert!(net.initialized, "no legacy migration into the demo");
    assert!(!net.artnet_enabled);
    assert!(net.nodes.is_empty());
}

#[test]
fn the_engine_lights_every_zone() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let show = demo_show();
    // The universes of each zone, from the patch.
    let universes = |zone: usize| -> HashSet<u16> {
        show.patch
            .fixtures
            .iter()
            .filter(|f| f.zone == DEMO_ZONES[zone])
            .flat_map(|f| f.pixels.iter().map(|p| p.universe))
            .collect()
    };
    let zones: Vec<HashSet<u16>> = (0..4).map(universes).collect();
    for (a, b) in [(0, 1), (1, 2), (2, 3)] {
        assert!(
            zones[a].is_disjoint(&zones[b]),
            "each zone on its own universes"
        );
    }
    let engine = Engine::start_show(show, config).expect("engine starts");

    let lit = |zone: usize| {
        let snap = dev_sink.snapshot();
        zones[zone].iter().any(|u| {
            snap.universes
                .get(u)
                .is_some_and(|d| d.data.iter().any(|&b| b > 8))
        })
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(0..4).all(lit) {
        assert!(
            Instant::now() < deadline,
            "zones lit: {:?}",
            (0..4).map(lit).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The scenes are tree paths, as from the grid or the keyboard.
    assert!(
        engine.activate_path(&format!("/scene/{STROBE_ROW}/launch")),
        "the Strobe scene"
    );
    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "released the GPU");
    let _ = dev_sink.stop_and_collect();
}

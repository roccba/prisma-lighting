//! M9 Slice 1 through the real engine assembly: a MIDI fader, bound by Learn, drives
//! the same live handles the output stage reads — and the binding survives a
//! recompile onto a Show with different zones (the tree is rebuilt, the runtime is
//! not). Hardware-free: bytes enter through `MidiRuntime::feed`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use prisma_core::show::Show;
use prisma_core::{load_mappings, save_mappings, MapTarget, ZoneSub};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{BlendMode, Patch, Zone};
use prisma_render::AdapterPreference;
use uuid::Uuid;

fn zone(id: Uuid, name: &str) -> Zone {
    Zone {
        id,
        name: name.into(),
        members: Vec::new(),
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        mapping_size: [16, 16, 1],
        display_color: [80, 80, 80],
        world_transform: None,
    }
}

fn show(zones: Vec<Zone>) -> Show {
    Show::new(
        "midi",
        Patch {
            id: Uuid::new_v4(),
            name: "midi".into(),
            matrix_size: [1, 1, 1],
            fixtures: Vec::new(),
            zones,
        },
    )
}

#[test]
fn learned_fader_drives_live_dimmers_and_survives_a_recompile() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());

    let engine = Engine::start_show(
        show(vec![zone(Uuid::new_v4(), "A"), zone(Uuid::new_v4(), "B")]),
        config(),
    )
    .expect("engine starts");
    let midi = engine.midi().clone();
    assert!(midi.is_bound());
    let zone_a_before = engine.live().zone(0).dimmer.read_real();

    // Learn: zone slot 1's dimmer ← APC40 track-2 fader (CC 7, ch 1); master ← CC 14.
    midi.arm_learn(MapTarget::ZoneSlot {
        slot: 1,
        sub: ZoneSub::Dimmer,
    });
    midi.feed(&[0xB1, 7, 127]);
    midi.arm_learn(MapTarget::Path("/master/dimmer".into()));
    midi.feed(&[0xB0, 14, 0]);

    // The live handles the output stage reads follow the hardware.
    let zone_b = || engine.live().zone(1).dimmer.read_real().unwrap();
    assert_eq!(zone_b(), 1.0);
    assert_eq!(engine.live().master().dimmer.read_real(), Some(0.0));
    midi.feed(&[0xB1, 7, 0]);
    assert_eq!(zone_b(), 0.0);
    // Zone A never moved.
    assert_eq!(engine.live().zone(0).dimmer.read_real(), zone_a_before);

    // The profile persists and reloads intact.
    let profile = load_mappings(&save_mappings(&midi.profile()).unwrap()).unwrap();
    assert_eq!(profile.mappings.len(), 2);

    // Recompile onto a different Show: new tree, new zone uuids. The same runtime
    // (same clone the hardware adapter holds) is rebound; slot 1 is the new 2nd zone.
    let engine = engine
        .recompile_show(
            show(vec![
                zone(Uuid::new_v4(), "X"),
                zone(Uuid::new_v4(), "Y"),
                zone(Uuid::new_v4(), "Z"),
            ]),
            config(),
        )
        .expect("recompile");
    assert!(midi.is_bound());
    assert_eq!(midi.resolved_count(), 2);
    midi.feed(&[0xB1, 7, 127]);
    midi.feed(&[0xB0, 14, 127]);
    assert_eq!(engine.live().zone(1).dimmer.read_real(), Some(1.0));
    assert_eq!(engine.live().master().dimmer.read_real(), Some(1.0));

    // Shutdown releases the tree: later input is inert, not a use of a dead engine.
    engine.shutdown();
    assert!(!midi.is_bound());
    midi.feed(&[0xB1, 7, 0]);
    let _ = dev_sink.stop_and_collect();
}

//! M9 Slice 3 — grid cells as tree targets, played by the engine's cell controller:
//! a momentary hold returns to what ran before, a latch makes a new base, a trigger
//! restarts, an empty cell stops the zone, and the whole thing survives a recompile.
//! Validated on the wire (virtual sink) and on each cell's published `state`.
//!
//! One test function, its own binary (one Vulkan bring-up at a time — see m8_engine_show).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, Layer, ParamSet, Show};
use prisma_core::{priority, SourceId};
use prisma_engine::{CellHost, Engine, EngineConfig, CELL_ASSIGNED, CELL_EMPTY, CELL_RUNNING};
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

/// Cells: 0 = grey 0.5 (running), 1 = white, 2 = empty.
fn show() -> Show {
    let mut show = Show::new("cells", patch());
    let zs = show.zones.iter_mut().find(|z| z.zone_id == ZONE).unwrap();
    zs.grid = vec![solid(0.5), solid(1.0), Cell::empty()];
    zs.active = Some(0);
    zs.live.dimmer = 1.0;
    show
}

/// The app's role: owns the show, hears about the running cell.
struct Host(Mutex<Show>);

impl CellHost for Host {
    fn cell_effect(&self, zone: Uuid, cell: usize) -> Option<(String, ParamSet)> {
        let show = self.0.lock().unwrap();
        let layer = show.zone(zone)?.grid.get(cell)?.layers.first()?.clone();
        match layer.source {
            prisma_core::show::Source::Shader { effect_id, params } => Some((effect_id, params)),
        }
    }
    fn active_changed(&self, zone: Uuid, active: Option<usize>) {
        let mut show = self.0.lock().unwrap();
        if let Some(zs) = show.zones.iter_mut().find(|z| z.zone_id == zone) {
            zs.active = active;
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
fn pads_play_cells_through_the_tree() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let host = Arc::new(Host(Mutex::new(show())));
    let engine = Engine::start_show(show(), config()).expect("engine starts");
    engine.set_cell_host(host.clone());

    let wire = |lo: u8, hi: u8| {
        let snap = dev_sink.snapshot();
        snap.universes
            .get(&1)
            .is_some_and(|u| u.data.len() >= 12 && (lo..=hi).contains(&u.data[0]))
    };
    let path = |cell: usize, leaf: &str| format!("/zone/{ZONE}/cell/{cell}/{leaf}");
    let state = |engine: &prisma_engine::EngineHandle, cell: usize| {
        engine
            .tree()
            .resolve(&path(cell, "state"))
            .unwrap()
            .read_real()
            .unwrap()
    };
    let midi = SourceId::new(priority::MIDI, 1);
    let active = || host.0.lock().unwrap().zone(ZONE).unwrap().active;

    // States published from the show: running, assigned, empty.
    wait_for("initial cell states", || {
        state(&engine, 0) == CELL_RUNNING
            && state(&engine, 1) == CELL_ASSIGNED
            && state(&engine, 2) == CELL_EMPTY
    });
    wait_for("grey on the wire", || wire(120, 136));

    // Momentary: hold white, then release → back to grey.
    let hold1 = engine.tree().resolve(&path(1, "hold")).unwrap();
    engine.tree().write_real(&hold1, 1.0, midi);
    wait_for("held white on the wire", || wire(250, 255));
    wait_for("held cell runs", || {
        state(&engine, 1) == CELL_RUNNING && state(&engine, 0) == CELL_ASSIGNED
    });
    assert_eq!(active(), Some(1), "the host hears the running cell");
    engine.tree().write_real(&hold1, 0.0, midi);
    wait_for("release returns to grey", || {
        wire(120, 136) && active() == Some(0)
    });

    // Latch (the UI click path): white becomes the base and stays.
    assert!(engine.fire_cell(0, 1));
    wait_for("latched white", || wire(250, 255) && active() == Some(1));

    // A hold on top of the latch, cleared by a new latch: the hold param is reset so
    // a toggle pad's next tap presses again.
    let hold0 = engine.tree().resolve(&path(0, "hold")).unwrap();
    engine.tree().write_real(&hold0, 1.0, midi);
    wait_for("held grey over white", || wire(120, 136));
    assert!(engine.fire_cell(0, 1));
    wait_for("latch clears the hold", || {
        wire(250, 255) && hold0.read_bool() == Some(false)
    });

    // Trigger restarts (the running cell stays the same); empty cell stops the zone.
    let trig0 = engine.tree().resolve(&path(0, "trigger")).unwrap();
    engine.tree().fire(&trig0, midi);
    wait_for("trigger plays grey", || {
        wire(120, 136) && active() == Some(0)
    });
    assert!(engine.fire_cell(0, 2));
    wait_for("empty cell goes dark", || wire(0, 4) && active() == Some(2));

    // Grid edit: cell 2 gets an effect; a refresh publishes it (running, since active).
    host.0.lock().unwrap().zones[0].grid[2] = solid(1.0);
    engine.refresh_cells(0);
    wait_for("refreshed state", || state(&engine, 2) == CELL_RUNNING);

    // Recompile onto the host's current show: the controller follows the new tree.
    let current = host.0.lock().unwrap().clone();
    let engine = engine.recompile_show(current, config()).expect("recompile");
    wait_for("states after recompile", || {
        state(&engine, 2) == CELL_RUNNING && state(&engine, 0) == CELL_ASSIGNED
    });
    let hold0 = engine.tree().resolve(&path(0, "hold")).unwrap();
    engine.tree().write_real(&hold0, 1.0, midi);
    wait_for("hold works after recompile", || {
        wire(120, 136) && active() == Some(0)
    });

    let report = engine.shutdown();
    assert_eq!(
        report.gpu_strong_refs_at_release, 1,
        "cells released the GPU"
    );
    let _ = dev_sink.stop_and_collect();
}

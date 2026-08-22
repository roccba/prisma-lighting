//! The "start from zero" boot state: a Show with no fixtures and no zones (0 output
//! pixels) must start, run and shut down without panicking. This guards the empty-launch
//! path (app boots empty unless a last show is remembered) against a 0-pixel edge case in
//! the GPU pipeline (e.g. a zero-sized buffer wgpu would reject).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::thread::sleep;
use std::time::Duration;

use prisma_core::show::Show;
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{BlendMode, Patch, Zone};
use prisma_render::AdapterPreference;
use uuid::Uuid;

// Mirrors the app's boot state: no fixtures, a 1×1×1 world, and ONE empty zone (0 members
// ⇒ still 0 output pixels). This is the exact "new configuration" show.
fn empty_show() -> Show {
    let patch = Patch {
        id: Uuid::from_u128(0xE),
        name: "empty".into(),
        matrix_size: [1, 1, 1],
        fixtures: Vec::new(),
        zones: vec![Zone {
            id: Uuid::from_u128(0x1),
            name: "Zona 1".into(),
            members: Vec::new(),
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [16, 16, 1],
            display_color: [80, 80, 80],
            world_transform: None,
        }],
    };
    Show::new("empty", patch)
}

#[test]
fn engine_starts_and_runs_an_empty_show() {
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let sink_addr = dev_sink.local_addr();
    let mut config = EngineConfig::new(AdapterPreference::Software, sink_addr);
    config.output_rate_hz = 44;

    // The claim: an empty show (no fixtures/zones → 0 output pixels) starts, not panics.
    let engine = Engine::start_show(empty_show(), config).expect("engine starts an empty show");
    assert_eq!(engine.zone_count(), 1);
    assert!(
        engine.zone_commands().is_some(),
        "a running show exposes the per-zone queue even when empty"
    );

    // Let the render + output loops spin; they must not crash on 0 pixels.
    sleep(Duration::from_millis(300));

    let report = engine.shutdown();
    assert!(report.render.frames_published > 0, "the render loop ran");
    let _ = dev_sink.stop_and_collect();
}

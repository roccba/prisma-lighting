//! S1 acceptance: the engine assembly boots the whole product outside a test
//! harness and shuts down **cleanly, in order** — the two things S1 must prove.
//!
//! One test function only, on purpose: each `Engine::start` brings up its own
//! Vulkan device and each shutdown tears one down. Running several in parallel
//! (libtest's default) would load and unload the ICD from many threads at once —
//! a loader race (see `prisma-render/tests/common`). Sequential bring-up/teardown
//! on one thread is safe, so the whole S1 story lives in a single test.
//!
//! Runs on the software adapter (lavapipe): no hardware, deterministic, same
//! stand-in the M1/M5/M6 acceptance tests use.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel, PixelRef,
    Zone,
};
use prisma_preview::sink::{ChannelSink, PreviewMessage};
use prisma_render::AdapterPreference;
use uuid::Uuid;

fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x51);
    let zone_id = Uuid::from_u128(0x1);
    Patch {
        id: Uuid::from_u128(0x5100),
        name: "s1".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..2)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: zone_id,
        }],
        zones: vec![Zone {
            id: zone_id,
            name: "all".into(),
            members: vec![
                PixelRef {
                    fixture: fixture_id,
                    pixel: 0,
                },
                PixelRef {
                    fixture: fixture_id,
                    pixel: 1,
                },
            ],
            bounds: None,
            z_order: 0,
            blend_mode: prisma_patch::BlendMode::Normal,
            mapping_size: [2, 1, 1],
            display_color: [200, 60, 60],
            world_transform: None,
        }],
    }
}

fn local_any() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

/// Wait until `pred` holds or fail with `what`.
fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
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
fn engine_boots_all_three_loops_and_shuts_down_cleanly() {
    // ---- Boot ----
    let dev_sink = sink::spawn(local_any()).expect("virtual sink");
    let sink_addr = dev_sink.local_addr();
    let (preview_sink, preview_rx) = ChannelSink::new();

    let mut config = EngineConfig::new(AdapterPreference::Software, sink_addr);
    config.preview_sink = Some(Box::new(preview_sink));
    config.output_rate_hz = 44;
    config.preview_rate_hz = 30;

    let engine = Engine::start(build_patch(), config).expect("engine starts");

    // The Parameter Tree binding is live and addressable straight after start.
    let live = engine.live();
    engine.tree().write_real(
        &live.master().dimmer,
        1.0,
        prisma_core::source::SourceId::new(prisma_core::source::priority::UI, 1),
    );

    // ---- Prove all three loops actually run ----
    // Output: the virtual sink receives ArtDmx (IdleContent → zero data, but the
    // packets flow — always-send).
    wait_until("output loop reaches the virtual sink", || {
        dev_sink.snapshot().universes.contains_key(&1)
    });
    // Preview: a binary frame arrives on the channel sink.
    wait_until("preview loop ships a frame", || {
        while let Ok(msg) = preview_rx.try_recv() {
            if matches!(msg, PreviewMessage::Frame(_)) {
                return true;
            }
        }
        false
    });

    // ---- Ordered shutdown ----
    let report = engine.shutdown();

    assert!(
        report.render.frames_published > 0,
        "render published frames"
    );
    assert!(report.preview.frames_sent > 0, "preview shipped frames");
    assert_eq!(
        report.output.generation_mismatches, 0,
        "no stale frame pairings — the start-up generation stamping worked"
    );
    // The heart of the shutdown criterion: after every GPU-using thread joined,
    // the handle holds the ONLY strong reference to the GPU, so the drop that
    // follows releases the Vulkan device with nothing left to touch it.
    assert_eq!(
        report.gpu_strong_refs_at_release, 1,
        "GPU released cleanly (no handle leaked into a still-running thread)"
    );

    let sink_state = dev_sink.stop_and_collect();
    assert!(
        sink_state.artdmx_packets > 0,
        "the virtual sink received real ArtDmx from the output loop"
    );

    // ---- The Drop backstop: a second engine, dropped WITHOUT shutdown, must tear
    // down promptly rather than hang the process on exit. Sequential with the first
    // (its GPU already released), so no concurrent ICD load/unload. ----
    let dev_sink2 = sink::spawn(local_any()).expect("second virtual sink");
    let engine2 = Engine::start(
        build_patch(),
        EngineConfig::new(AdapterPreference::Software, dev_sink2.local_addr()),
    )
    .expect("second engine starts");
    wait_until("second engine's output runs", || {
        dev_sink2.snapshot().universes.contains_key(&1)
    });

    let dropped_at = Instant::now();
    drop(engine2); // no shutdown() — exercise the Drop backstop
    assert!(
        dropped_at.elapsed() < Duration::from_secs(5),
        "dropping the handle without shutdown must join the loops promptly, not hang"
    );
    let _ = dev_sink2.stop_and_collect();
}

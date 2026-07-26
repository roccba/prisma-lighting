//! S9 acceptance: the editor↔engine seam — **Highlight** lights the selected fixture
//! on the live wire, and **recompile** applies an edited patch to the running engine
//! (reusing the GPU) with a fresh layout generation. Verified against the virtual sink,
//! exactly as the plan says (§7); real hardware is post-M8.
//!
//! One `#[test]` in its own binary, like `boot_shutdown`: each `Engine::start` /
//! `recompile` brings a Vulkan device up (or reuses one), so this stays off libtest's
//! within-binary parallelism. Software adapter (lavapipe), deterministic.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

const FIXTURE: Uuid = Uuid::from_u128(0x51);
const ZONE: Uuid = Uuid::from_u128(0x1);

/// A one-fixture rig with `n` RGB pixels packed from universe 1, channel 1.
fn build_patch(n: u32) -> Patch {
    Patch {
        id: Uuid::from_u128(0x5100),
        name: "s9".into(),
        matrix_size: [n.max(1), 1, 1],
        fixtures: vec![Fixture {
            id: FIXTURE,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..n)
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
            members: vec![],
            bounds: None,
            z_order: 0,
            blend_mode: prisma_patch::BlendMode::Normal,
            mapping_size: [n.max(1), 1, 1],
            display_color: [200, 60, 60],
            world_transform: None,
        }],
    }
}

fn local_any() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

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
fn highlight_lights_the_wire_and_recompile_reshapes_the_layout() {
    let dev_sink = sink::spawn(local_any()).expect("virtual sink");
    let mut config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    config.output_rate_hz = 44;

    // Start on a 2-pixel fixture (channels 1..=6). Not frozen: IdleContent is black
    // regardless, so the wire is all-zero until we highlight.
    let mut engine = Engine::start(build_patch(2), config).expect("engine starts");
    let gen0 = engine.generation();

    // Read universe 1's channel bytes from the sink.
    let channels = |ch: usize| -> Option<u8> {
        dev_sink
            .snapshot()
            .universes
            .get(&1)
            .and_then(|u| u.data.get(ch).copied())
    };

    wait_until("output reaches the sink, black", || channels(0) == Some(0));

    // --- Highlight solid: every channel of the fixture goes white ---
    engine.highlight(&[FIXTURE], false);
    wait_until("the highlighted fixture lights white", || {
        (0..6).all(|c| channels(c) == Some(255))
    });

    // --- Clear: back to black ---
    engine.clear_highlight();
    wait_until("the highlight clears", || {
        (0..6).all(|c| channels(c) == Some(0))
    });

    // --- Recompile onto a 3-pixel fixture (channels 1..=9): new layout generation,
    //     GPU reused, output resumes. ---
    let mut engine = engine
        .recompile(
            build_patch(3),
            EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr()),
        )
        .expect("recompile applies the new patch");
    assert_ne!(
        engine.generation(),
        gen0,
        "recompile must mint a fresh layout generation"
    );

    // The new third pixel (channel 7 → index 6) exists in the running layout only after
    // the recompile — highlighting it proves the layout was truly reshaped live.
    engine.highlight(&[FIXTURE], false);
    wait_until("the recompiled third pixel lights", || {
        (0..9).all(|c| channels(c) == Some(255))
    });

    let report = engine.shutdown();
    assert_eq!(
        report.gpu_strong_refs_at_release, 1,
        "GPU released cleanly even after a recompile reused it"
    );
    let _ = dev_sink.stop_and_collect();
}

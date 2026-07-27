//! M7 acceptance: the whole milestone done-criterion walked through the **real path**,
//! the same shape as the M1/M5/M6 acceptance tests — not scattered unit asserts, but a
//! single run of an actual load-in workflow:
//!
//! 1. build a rig with several **independent zones** (the sample wedding rig);
//! 2. **place fixtures** of one type in one action (count > 1) with a signal direction;
//! 3. a **DMX collision** is detected (discreetly) and then undone;
//! 4. edit a zone's **matrix** (its render resolution);
//! 5. **save to `.prismap`, close, reopen** → everything intact;
//! 6. start the engine on the reopened patch and **Highlight** a fixture — it lights on
//!    the wire (virtual sink) — and **freeze** holds.
//!
//! Steps 1–5 run through the editor crate's public API — exactly what the Tauri
//! commands wrap (`prisma-editor` is a dev-dep here); step 6 through the engine and the
//! virtual sink. Software adapter (lavapipe), own binary (one engine bring-up, like
//! `boot_shutdown` / `s9`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_editor::{
    next_free_address, plan, samples, Direction, EditOp, FixtureArrangement, FixtureType,
    PatchDraft, PlacementSpec, Rotation,
};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{load_patch, save_patch, ComponentSet, Diagnostic};
use prisma_render::AdapterPreference;
use uuid::Uuid;

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

/// A 30-px RGB tube type from the seeded library.
fn tube_type() -> FixtureType {
    FixtureType {
        name: "30px tube".into(),
        pixel_count: 30,
        components: ComponentSet::Rgb,
        color_order: prisma_patch::ColorOrder::Grb,
        arrangement: FixtureArrangement::Strip,
        roles: Vec::new(),
    }
}

/// A placement spec: `count` tubes stepping in `x`, running `direction`, addressed from
/// `(universe, channel)`, into `zone`.
fn placement(
    zone: Uuid,
    count: u32,
    start: [i32; 3],
    universe: u16,
    channel: u16,
) -> PlacementSpec {
    PlacementSpec {
        fixture_type: tube_type(),
        name_prefix: "Patio Extra".into(),
        start_number: 1,
        rotation: Rotation::R0,
        count: [count, 1, 1],
        offset: [3, 0, 0],
        start_position: start,
        direction: Direction::Down,
        snake: false,
        start_universe: universe,
        start_channel: channel,
        channel_offset: 0,
        universe_offset: 0,
        fixtures_per_universe: None,
        zone,
    }
}

/// Append planned fixtures to the draft as one undoable op (mirrors the Tauri command).
fn place(draft: &mut PatchDraft, spec: &PlacementSpec) {
    let fixtures = plan(spec).expect("placement plans");
    let base = draft.patch().fixtures.len();
    let at = fixtures
        .into_iter()
        .enumerate()
        .map(|(i, f)| (base + i, f))
        .collect();
    draft
        .apply(EditOp::InsertFixtures { at }, None)
        .expect("insert applies");
}

#[test]
fn a_full_montage_edits_persists_and_lights_the_rig() {
    // 1. A rig with several INDEPENDENT ZONES (booth / dance floor / patio / bar).
    let mut draft = PatchDraft::new(samples::wedding_rig());
    assert_eq!(draft.patch().zones.len(), 4, "four independent zones");
    let fixtures0 = draft.patch().fixtures.len();
    assert!(draft.wire_faults().is_empty(), "the sample rig is clean");

    let patio = draft
        .patch()
        .zones
        .iter()
        .find(|z| z.name == "Patio")
        .expect("patio zone")
        .id;

    // 2. PLACE fixtures in one action (3 tubes) with a signal direction, into the patio.
    // Address them just past the rig's current usage (the "next free" the editor
    // suggests), so there is no collision.
    let (u, c) = next_free_address(draft.patch());
    place(&mut draft, &placement(patio, 3, [40, 4, 0], u, c));
    assert_eq!(
        draft.patch().fixtures.len(),
        fixtures0 + 3,
        "three tubes placed"
    );
    assert!(
        draft.wire_faults().is_empty(),
        "clean placement raises no faults"
    );

    // The signal direction is honoured: the placed tube's pixels run DOWN (y increases
    // from first to last), which is what the chase direction depends on.
    let placed = &draft.patch().fixtures[fixtures0];
    assert!(
        placed.pixels.last().unwrap().position.y > placed.pixels[0].position.y,
        "pixels run in the Down signal direction"
    );

    // 3. A DMX COLLISION is detected — discreetly, as a wire fault — then undone so the
    // saved patch is clean. Placing another tube onto universe 1 channel 1 (which the
    // booth already uses) collides.
    place(&mut draft, &placement(patio, 1, [46, 4, 0], 1, 1));
    let faults = draft.wire_faults();
    assert!(
        faults
            .iter()
            .any(|d| matches!(d, Diagnostic::AddressCollision { .. })),
        "the overlapping address is reported as a collision, got {faults:?}"
    );
    assert!(draft.undo(), "undo the colliding placement");
    assert!(
        draft.wire_faults().is_empty(),
        "the patch is clean again after undo"
    );

    // 4. Edit a zone's MATRIX (its render resolution, §2.4).
    place_matrix(&mut draft, patio, [40, 30, 1]);
    let patio_size = draft
        .patch()
        .zones
        .iter()
        .find(|z| z.id == patio)
        .unwrap()
        .mapping_size;
    assert_eq!(patio_size, [40, 30, 1], "the zone matrix was resized");

    // 5. SAVE to a .prismap, "close", REOPEN → everything intact.
    let path = std::env::temp_dir().join(format!("prisma-m7-{}.prismap", std::process::id()));
    std::fs::write(&path, save_patch(draft.patch()).expect("serialize")).expect("write");
    let reopened = load_patch(&std::fs::read_to_string(&path).expect("read")).expect("load");
    assert_eq!(
        &reopened,
        draft.patch(),
        "the reopened patch matches byte-for-byte — placement, direction, matrix edit, zones"
    );
    let _ = std::fs::remove_file(&path);

    // 6. Start the engine on the reopened patch and HIGHLIGHT a fixture — it lights on
    // the wire — then FREEZE holds.
    let dev_sink = sink::spawn(local_any()).expect("virtual sink");
    let mut config = EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    config.output_rate_hz = 44;
    let mut engine = Engine::start(reopened.clone(), config).expect("engine starts on the rig");

    // The booth's first tube is fixture 0 at universe 1, channel 1 (30 px → ch 1..=90).
    let first = reopened.fixtures[0].id;
    let ch0 = || {
        dev_sink
            .snapshot()
            .universes
            .get(&1)
            .and_then(|u| u.data.first().copied())
    };
    wait_until("output reaches the sink, black", || ch0() == Some(0));

    engine.highlight(&[first], false); // solid
    wait_until("the highlighted fixture lights on the wire", || {
        ch0() == Some(255)
    });

    engine.set_frozen(true);
    assert!(
        engine.is_frozen(),
        "content freezes while the editor holds the output"
    );

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1, "clean shutdown");
    let _ = dev_sink.stop_and_collect();
}

/// Set a zone's matrix (mirrors the `editor_set_zone_mapping_size` op).
fn place_matrix(draft: &mut PatchDraft, zone: Uuid, size: [u32; 3]) {
    draft
        .apply(EditOp::SetZoneMappingSize { zone, size }, None)
        .expect("resize applies");
}

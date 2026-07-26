//! Tests for [`Patch::wire_faults`] — the editor's collect-all detection of physical
//! DMX wire faults (M7 / S6 replanned). Two things matter: that it collects **every**
//! fault (not just the first), and that it reports **only** the two genuine wire faults
//! — address collisions and out-of-range channels — never the valid choices that
//! out-of-matrix positions (§2.4) and shared cells (§4.4) are.

use glam::Vec3;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Diagnostic, Fixture, FixtureProfile, GammaCalibration,
    Patch, Pixel, Zone,
};
use uuid::Uuid;

fn profile() -> FixtureProfile {
    FixtureProfile {
        name: "RGB".into(),
        components: ComponentSet::Rgb,
        gamma: GammaCalibration::default(),
    }
}

/// A tube of `n` RGB pixels laid horizontally from `(x0, y)`, tightly packed on the
/// wire from `(universe, start_channel)`.
fn tube(
    id: u128,
    zone: Uuid,
    n: u32,
    x0: i32,
    y: i32,
    universe: u16,
    start_channel: u16,
) -> Fixture {
    Fixture {
        id: Uuid::from_u128(id),
        name: format!("tube-{id}"),
        profile: profile(),
        pixels: (0..n)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new((x0 + i as i32) as f32, y as f32, 0.0),
                universe,
                start_channel: start_channel + (i as u16) * 3,
                color_order: ColorOrder::Rgb,
            })
            .collect(),
        zone,
    }
}

fn zone(id: u128, size: [u32; 3]) -> Zone {
    Zone {
        id: Uuid::from_u128(id),
        name: format!("zone-{id}"),
        members: Vec::new(),
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        mapping_size: size,
        display_color: [128, 128, 128],
        world_transform: None,
    }
}

fn patch(fixtures: Vec<Fixture>, zones: Vec<Zone>, matrix: [u32; 3]) -> Patch {
    Patch {
        id: Uuid::from_u128(0x1),
        name: "rig".into(),
        matrix_size: matrix,
        fixtures,
        zones,
    }
}

#[test]
fn a_clean_patch_has_no_faults() {
    let z = Uuid::from_u128(0x10);
    let p = patch(
        vec![tube(0xA, z, 4, 0, 0, 1, 1), tube(0xB, z, 4, 0, 1, 1, 100)],
        vec![zone(0x10, [4, 2, 1])],
        [4, 2, 1],
    );
    assert!(p.validate().is_ok());
    assert!(
        p.wire_faults().is_empty(),
        "clean patch: {:?}",
        p.wire_faults()
    );
}

#[test]
fn out_of_matrix_and_shared_cells_are_not_faults() {
    // Two tubes stacked on the same row (every cell holds two pixels) and both mapped
    // partly outside a 2-wide matrix. Neither is a wire fault: shared cells are
    // arithmetic (§4.4) and out-of-matrix is a valid placement (§2.4). Addresses are
    // disjoint, so `wire_faults` must be empty AND the patch must still compile.
    let z = Uuid::from_u128(0x10);
    let p = patch(
        vec![tube(0xA, z, 8, 0, 0, 1, 1), tube(0xB, z, 8, 0, 0, 1, 100)],
        vec![zone(0x10, [2, 1, 1])],
        [2, 1, 1],
    );
    assert!(
        p.validate().is_ok(),
        "out-of-matrix / shared cells must compile"
    );
    assert!(
        p.wire_faults().is_empty(),
        "no wire faults expected, got {:?}",
        p.wire_faults()
    );
}

#[test]
fn collects_all_collisions_at_once_not_just_the_first() {
    let z = Uuid::from_u128(0x10);
    // Two independent collisions in one patch: (A,B) on universe 1 and (C,D) on
    // universe 2. Fail-fast validate would surface only one.
    let p = patch(
        vec![
            tube(0xA, z, 4, 0, 0, 1, 1),
            tube(0xB, z, 4, 0, 1, 1, 1),
            tube(0xC, z, 4, 0, 2, 2, 50),
            tube(0xD, z, 4, 0, 3, 2, 50),
        ],
        vec![zone(0x10, [4, 4, 1])],
        [4, 4, 1],
    );
    let collisions = p
        .wire_faults()
        .into_iter()
        .filter(|d| matches!(d, Diagnostic::AddressCollision { .. }))
        .count();
    assert_eq!(collisions, 2, "both collisions collected");
}

#[test]
fn overlapping_tubes_merge_into_one_collision_not_one_per_channel() {
    // Two 10-px RGB tubes fully overlapping in addressing = 30 colliding channels, but
    // it is ONE operator mistake: a single diagnostic spanning the range, both tubes'
    // pixels marked for the canvas.
    let z = Uuid::from_u128(0x10);
    let p = patch(
        vec![tube(0xA, z, 10, 0, 0, 1, 1), tube(0xB, z, 10, 0, 1, 1, 1)],
        vec![zone(0x10, [10, 2, 1])],
        [10, 2, 1],
    );
    let collisions: Vec<_> = p
        .wire_faults()
        .into_iter()
        .filter_map(|d| match d {
            Diagnostic::AddressCollision {
                channel_start,
                channel_end,
                fixtures,
                marks,
                ..
            } => Some((channel_start, channel_end, fixtures, marks)),
            _ => None,
        })
        .collect();
    assert_eq!(collisions.len(), 1, "one merged collision");
    let (start, end, fixtures, marks) = &collisions[0];
    assert_eq!((*start, *end), (1, 30));
    assert_eq!(fixtures.len(), 2);
    assert_eq!(marks.len(), 20, "every pixel of both tubes is marked");
}

#[test]
fn channel_out_of_range_is_reported_and_still_blocks_compile() {
    // The IMPORT case (§9): an address past 512. Surfaced by the editor AND rejected by
    // the compile gate — it is a physical impossibility, not a mapping choice.
    let z = Uuid::from_u128(0x10);
    let mut f = tube(0xA, z, 1, 0, 0, 1, 511);
    f.pixels[0].start_channel = 511; // RGB needs 511,512,513 → 513 is out of range
    let p = patch(vec![f], vec![zone(0x10, [1, 1, 1])], [1, 1, 1]);
    assert!(p
        .wire_faults()
        .iter()
        .any(|d| matches!(d, Diagnostic::ChannelOutOfRange { start: 511, .. })));
    assert!(
        p.validate().is_err(),
        "out-of-range channel must block compile"
    );
}

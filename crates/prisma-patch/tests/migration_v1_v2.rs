//! v1 → v2 schema migration (M7): each zone gains its own `mapping_size`,
//! `display_color` and optional `world_transform`; each fixture gains `zone`
//! ownership derived from v1's overlap membership.
//!
//! The migration is exercised through the public `load_patch` path (a v1 envelope
//! in, a fully-populated v2 `Patch` out) so these tests pin the observable
//! behaviour, not the private JSON poking.

use prisma_patch::{load_patch, Patch};
use uuid::Uuid;

/// Build a v1 patch envelope (schema_version 1, no per-zone fields, no fixture
/// ownership) from raw fixture/zone JSON fragments.
fn v1_envelope(matrix: &str, fixtures: &str, zones: &str) -> String {
    format!(
        r#"{{
            "format": "prisma.patch",
            "schema_version": 1,
            "patch": {{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "v1 rig",
                "matrix_size": {matrix},
                "fixtures": [{fixtures}],
                "zones": [{zones}]
            }}
        }}"#
    )
}

/// A single RGB pixel fragment at `pos` addressed at `chan`.
fn pixel(index: u32, x: f32, chan: u16) -> String {
    format!(
        r#"{{ "index": {index}, "position": [{x}.0, 0.0, 0.0], "universe": 1, "start_channel": {chan}, "color_order": "Rgb" }}"#
    )
}

fn fixture(id: &str, name: &str, pixels: &str) -> String {
    format!(
        r#"{{ "id": "{id}", "name": "{name}", "profile": {{ "name": "p", "components": "Rgb" }}, "pixels": [{pixels}] }}"#
    )
}

fn v1_zone(id: &str, name: &str, members: &str, z: i32) -> String {
    format!(
        r#"{{ "id": "{id}", "name": "{name}", "members": [{members}], "z_order": {z}, "blend_mode": "Normal" }}"#
    )
}

fn member(fixture_id: &str, pixel: u32) -> String {
    format!(r#"{{ "fixture": "{fixture_id}", "pixel": {pixel} }}"#)
}

const FA: &str = "00000000-0000-0000-0000-00000000000a";
const FB: &str = "00000000-0000-0000-0000-00000000000b";
const Z_SECTOR: &str = "00000000-0000-0000-0000-000000000010";
const Z_GLOBAL: &str = "00000000-0000-0000-0000-000000000011";

fn owner_of(patch: &Patch, fixture_id: &str) -> Uuid {
    patch
        .fixtures
        .iter()
        .find(|f| f.id == Uuid::parse_str(fixture_id).unwrap())
        .expect("fixture present")
        .zone
}

#[test]
fn seeds_each_zone_mapping_from_the_old_global_matrix() {
    let json = v1_envelope(
        "[8, 4, 1]",
        &fixture(FA, "A", &pixel(0, 0.0, 1)),
        &v1_zone(Z_SECTOR, "sector", &member(FA, 0), 0),
    );
    let patch = load_patch(&json).unwrap();
    // Every zone starts as large as the whole v1 matrix; the operator resizes.
    assert_eq!(patch.zones[0].mapping_size, [8, 4, 1]);
    // A palette colour was assigned (not the neutral serde default grey), and
    // world placement defaults to unset.
    assert_ne!(patch.zones[0].display_color, [128, 128, 128]);
    assert_eq!(patch.zones[0].world_transform, None);
    // The migrated patch is valid (mapping_size >= 1 on every axis).
    patch.compile().expect("migrated patch compiles");
}

#[test]
fn ownership_prefers_the_smallest_covering_zone_over_a_global_one() {
    // Fixture A is covered by a 1-member sector zone AND a 2-member global zone.
    // Ownership must go to the sector, never the global.
    let fixtures = format!(
        "{}, {}",
        fixture(FA, "A", &pixel(0, 0.0, 1)),
        fixture(FB, "B", &pixel(0, 1.0, 10)),
    );
    let zones = format!(
        "{}, {}",
        v1_zone(Z_SECTOR, "sector", &member(FA, 0), 0),
        v1_zone(
            Z_GLOBAL,
            "global",
            &format!("{}, {}", member(FA, 0), member(FB, 0)),
            10
        ),
    );
    let patch = load_patch(&v1_envelope("[4, 1, 1]", &fixtures, &zones)).unwrap();
    assert_eq!(owner_of(&patch, FA), Uuid::parse_str(Z_SECTOR).unwrap());
    // Fixture B is only in the global zone, so it owns B.
    assert_eq!(owner_of(&patch, FB), Uuid::parse_str(Z_GLOBAL).unwrap());
}

#[test]
fn ownership_ties_break_by_declaration_order() {
    // Fixture A sits in two equal-size zones; the earlier-declared one wins.
    let za = "00000000-0000-0000-0000-0000000000a0";
    let zb = "00000000-0000-0000-0000-0000000000b0";
    let zones = format!(
        "{}, {}",
        v1_zone(za, "first", &member(FA, 0), 0),
        v1_zone(zb, "second", &member(FA, 0), 0),
    );
    let patch = load_patch(&v1_envelope(
        "[4, 1, 1]",
        &fixture(FA, "A", &pixel(0, 0.0, 1)),
        &zones,
    ))
    .unwrap();
    assert_eq!(owner_of(&patch, FA), Uuid::parse_str(za).unwrap());
}

#[test]
fn an_orphan_fixture_gets_a_synthesized_default_zone() {
    // A v1 patch with a fixture but no zone covering it — representable, since v1
    // allowed an empty/partial zone set.
    let patch = load_patch(&v1_envelope(
        "[4, 1, 1]",
        &fixture(FA, "A", &pixel(0, 0.0, 1)),
        "", // no zones
    ))
    .unwrap();
    // Exactly one zone was synthesized, and the orphan is assigned to it.
    assert_eq!(patch.zones.len(), 1);
    assert_eq!(patch.zones[0].name, "Default");
    assert_eq!(owner_of(&patch, FA), patch.zones[0].id);
    // It is a valid, loadable zone.
    patch.compile().expect("synthesized zone compiles");
}

#[test]
fn does_not_synthesize_a_default_zone_when_every_fixture_is_covered() {
    let patch = load_patch(&v1_envelope(
        "[4, 1, 1]",
        &fixture(FA, "A", &pixel(0, 0.0, 1)),
        &v1_zone(Z_SECTOR, "sector", &member(FA, 0), 0),
    ))
    .unwrap();
    assert_eq!(patch.zones.len(), 1, "no extra zone should be added");
    assert_eq!(patch.zones[0].id, Uuid::parse_str(Z_SECTOR).unwrap());
}

//! S8 persistence: a `.prismap` is the M2 patch envelope (JSON, versioned) written
//! to a file with a distinct extension. These tests pin the two things Bauti asked
//! to confirm: a save→open round-trip preserves **everything** the editor added in
//! S0, and opening an older-schema file migrates it.

use prisma_editor::samples::wedding_rig;
use prisma_patch::{load_patch, save_patch, WorldTransform};

#[test]
fn round_trip_preserves_all_editor_fields() {
    let patch = wedding_rig();

    // A `.prismap`'s contents: the patch, serialized through the same envelope the
    // model uses (format + schema_version + migration chain).
    let json = save_patch(&patch).expect("serialize");
    let back = load_patch(&json).expect("deserialize");

    // Byte-for-byte equality is the strongest guarantee that nothing was dropped —
    // and it covers every S0 field because `Patch: PartialEq` is structural.
    assert_eq!(
        patch, back,
        "the whole patch must survive a save/open round-trip"
    );

    // Spot-check the S0 additions explicitly, so a regression names the field:
    for (a, b) in patch.zones.iter().zip(&back.zones) {
        assert_eq!(a.mapping_size, b.mapping_size, "zone mapping_size (§2.4)");
        assert_eq!(
            a.display_color, b.display_color,
            "zone display_color (§2.3)"
        );
        assert_eq!(
            a.world_transform, b.world_transform,
            "zone world_transform (§2.4)"
        );
    }
    for (a, b) in patch.fixtures.iter().zip(&back.fixtures) {
        assert_eq!(a.zone, b.zone, "fixture zone ownership (§2.2)");
    }
}

#[test]
fn world_transform_round_trips_when_set() {
    // The optional field must survive when it *is* set, not only when None.
    let mut patch = wedding_rig();
    patch.zones[0].world_transform = Some(WorldTransform {
        position: glam::Vec3::new(10.0, -4.0, 0.0),
        scale: glam::Vec3::new(2.0, 2.0, 1.0),
    });
    let back = load_patch(&save_patch(&patch).unwrap()).unwrap();
    assert_eq!(
        patch.zones[0].world_transform,
        back.zones[0].world_transform
    );
    assert_eq!(patch, back);
}

#[test]
fn opening_an_older_schema_prismap_migrates() {
    // A `.prismap` written by an OLD version of Prisma: schema v1, with no per-zone
    // mapping_size / display_color / world_transform and no fixture zone ownership.
    // The M2 migration chain must bring it up to the current schema on open.
    let v1 = r#"{
        "format": "prisma.patch",
        "schema_version": 1,
        "patch": {
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "old venue",
            "matrix_size": [12, 4, 1],
            "fixtures": [{
                "id": "00000000-0000-0000-0000-0000000000aa",
                "name": "Old Tube",
                "profile": { "name": "rgb", "components": "Rgb" },
                "pixels": [
                    { "index": 0, "position": [0.0, 0.0, 0.0], "universe": 1, "start_channel": 1, "color_order": "Grb" },
                    { "index": 1, "position": [1.0, 0.0, 0.0], "universe": 1, "start_channel": 4, "color_order": "Grb" }
                ]
            }],
            "zones": [{
                "id": "00000000-0000-0000-0000-000000000010",
                "name": "sector",
                "members": [
                    { "fixture": "00000000-0000-0000-0000-0000000000aa", "pixel": 0 },
                    { "fixture": "00000000-0000-0000-0000-0000000000aa", "pixel": 1 }
                ],
                "z_order": 0,
                "blend_mode": "Normal"
            }]
        }
    }"#;

    let patch = load_patch(v1).expect("a v1 .prismap must open (migrated) on a current build");

    // The migration seeded the S0 fields the old file lacked:
    assert_eq!(
        patch.zones[0].mapping_size,
        [12, 4, 1],
        "zone mapping_size seeded from the old global matrix"
    );
    assert_ne!(
        patch.zones[0].display_color,
        [0, 0, 0],
        "a display colour was assigned"
    );
    // And derived fixture ownership.
    assert_eq!(
        patch.fixtures[0].zone, patch.zones[0].id,
        "the fixture was assigned to the only covering zone"
    );
    // The migrated patch is valid and usable.
    patch.compile().expect("migrated v1 .prismap compiles");
}

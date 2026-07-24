//! Integration tests for the patch model: serialization round-trips, versioning
//! behaviour, validation, and the runtime index (global indexing + generation +
//! zone membership).

use glam::Vec3;
use prisma_patch::{
    load_patch, save_patch, BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile,
    GammaCalibration, LayoutGeneration, Patch, PatchError, Pixel, PixelRef, Zone,
    CURRENT_SCHEMA_VERSION, FORMAT_ID,
};
use uuid::Uuid;

/// A small two-fixture rig with an overlapping zone, mirroring the real test
/// setup (pixel tubes, multiple sectors) at a size that is easy to reason about.
///
/// - Fixture A: 3 RGB pixels, universe 1, channels 1,4,7.
/// - Fixture B: 2 RGBW pixels, universe 1, channels 20,24.
/// - Zone "sector-a": all of A.
/// - Zone "sector-b": all of B.
/// - Zone "global-strobe": everything (overlaps both), higher z_order, Add blend.
fn sample_patch() -> (Patch, Uuid, Uuid) {
    let fa = Uuid::from_u128(0xA);
    let fb = Uuid::from_u128(0xB);

    let fixture_a = Fixture {
        id: fa,
        name: "Tube A".into(),
        profile: FixtureProfile {
            name: "WS2812 GRB".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
        },
        pixels: (0..3)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new(i as f32, 0.0, 0.0),
                universe: 1,
                start_channel: 1 + (i as u16) * 3,
                color_order: ColorOrder::Grb,
            })
            .collect(),
        zone: Uuid::from_u128(0x10), // owned by sector-a
    };

    let fixture_b = Fixture {
        id: fb,
        name: "Tube B".into(),
        profile: FixtureProfile {
            name: "SK6812 RGBW".into(),
            components: ComponentSet::Rgbw,
            gamma: GammaCalibration { gamma: 2.2 },
        },
        pixels: (0..2)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new(i as f32, 1.0, 0.0),
                universe: 1,
                start_channel: 20 + (i as u16) * 4,
                color_order: ColorOrder::Rgb,
            })
            .collect(),
        zone: Uuid::from_u128(0x11), // owned by sector-b
    };

    let all_a: Vec<PixelRef> = (0..3)
        .map(|p| PixelRef {
            fixture: fa,
            pixel: p,
        })
        .collect();
    let all_b: Vec<PixelRef> = (0..2)
        .map(|p| PixelRef {
            fixture: fb,
            pixel: p,
        })
        .collect();
    let everything: Vec<PixelRef> = all_a.iter().chain(all_b.iter()).copied().collect();

    let patch = Patch {
        id: Uuid::from_u128(0x1),
        name: "Test Rig".into(),
        matrix_size: [3, 2, 1],
        fixtures: vec![fixture_a, fixture_b],
        zones: vec![
            Zone {
                id: Uuid::from_u128(0x10),
                name: "sector-a".into(),
                members: all_a,
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [3, 1, 1],
                display_color: [0xE6, 0x39, 0x46],
                world_transform: None,
            },
            Zone {
                id: Uuid::from_u128(0x11),
                name: "sector-b".into(),
                members: all_b,
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [2, 1, 1],
                display_color: [0x45, 0x7B, 0x9D],
                world_transform: None,
            },
            Zone {
                id: Uuid::from_u128(0x12),
                name: "global-strobe".into(),
                members: everything,
                bounds: None,
                z_order: 10,
                blend_mode: BlendMode::Add,
                mapping_size: [3, 2, 1],
                display_color: [0x2A, 0x9D, 0x8F],
                world_transform: None,
            },
        ],
    };
    (patch, fa, fb)
}

#[test]
fn round_trips_through_json() {
    let (patch, _, _) = sample_patch();
    let json = save_patch(&patch).unwrap();
    let back = load_patch(&json).unwrap();
    assert_eq!(patch, back, "patch must survive a save/load round-trip");
}

#[test]
fn envelope_carries_format_and_current_version() {
    let (patch, _, _) = sample_patch();
    let json = save_patch(&patch).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["format"], FORMAT_ID);
    assert_eq!(value["schema_version"], CURRENT_SCHEMA_VERSION);
    assert!(value["written_by"].is_string());
}

#[test]
fn refuses_a_file_from_a_newer_schema() {
    let (patch, _, _) = sample_patch();
    let json = save_patch(&patch).unwrap();
    // Forge a future schema version.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["schema_version"] = serde_json::json!(CURRENT_SCHEMA_VERSION + 5);
    let forged = serde_json::to_string(&value).unwrap();

    match load_patch(&forged) {
        Err(PatchError::NewerThanReader { file, reader }) => {
            assert_eq!(file, CURRENT_SCHEMA_VERSION + 5);
            assert_eq!(reader, CURRENT_SCHEMA_VERSION);
        }
        other => panic!("expected NewerThanReader, got {other:?}"),
    }
}

#[test]
fn rejects_non_prisma_json() {
    let junk = r#"{"format":"something.else","schema_version":1,"patch":{}}"#;
    assert!(matches!(
        load_patch(junk),
        Err(PatchError::BadFormat { .. })
    ));
}

#[test]
fn unknown_fields_default_gracefully() {
    // An older file lacking `written_by` and a fixture lacking `gamma`/`bounds`
    // must still load: those fields are #[serde(default)].
    let json = r#"{
        "format": "prisma.patch",
        "schema_version": 1,
        "patch": {
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "Old",
            "matrix_size": [1, 1, 1],
            "fixtures": [{
                "id": "00000000-0000-0000-0000-00000000000a",
                "name": "F",
                "profile": { "name": "p", "components": "Rgb" },
                "pixels": [{
                    "index": 0, "position": [0.0, 0.0, 0.0],
                    "universe": 1, "start_channel": 1, "color_order": "Rgb"
                }]
            }],
            "zones": []
        }
    }"#;
    let patch = load_patch(json).unwrap();
    assert_eq!(patch.fixtures[0].profile.gamma, GammaCalibration::default());
}

#[test]
fn compile_assigns_dense_global_indices_and_layout() {
    let (patch, fa, fb) = sample_patch();
    let index = patch.compile().unwrap();

    // 3 RGB + 2 RGBW = 5 pixels; 3*3 + 2*4 = 17 canonical bytes.
    assert_eq!(index.pixel_count(), 5);
    assert_eq!(index.master_buffer_len(), 17);

    // Fixture A pixels are globals 0,1,2 at byte offsets 0,3,6.
    assert_eq!(index.global_index(fa, 0), Some(0));
    assert_eq!(index.global_index(fa, 2), Some(2));
    assert_eq!(index.pixel_component_offset(2), 6);
    assert_eq!(index.pixel_component_count(2), 3);

    // Fixture B pixels are globals 3,4 at byte offsets 9,13 (RGBW).
    assert_eq!(index.global_index(fb, 0), Some(3));
    assert_eq!(index.pixel_component_offset(3), 9);
    assert_eq!(index.pixel_component_count(3), 4);

    // Out-of-range / unknown refs resolve to None.
    assert_eq!(index.global_index(fa, 3), None);
    assert_eq!(index.global_index(Uuid::from_u128(0xDEAD), 0), None);
}

#[test]
fn zone_membership_resolves_dedups_and_sorts() {
    let (patch, _, _) = sample_patch();
    let index = patch.compile().unwrap();

    // Zone order matches Patch::zones: 0=sector-a, 1=sector-b, 2=global-strobe.
    assert_eq!(index.zone_members(0), &[0, 1, 2]);
    assert_eq!(index.zone_members(1), &[3, 4]);
    // The global strobe overlaps everything: all five globals, sorted, unique.
    assert_eq!(index.zone_members(2), &[0, 1, 2, 3, 4]);
}

#[test]
fn duplicate_member_refs_collapse() {
    let (mut patch, fa, _) = sample_patch();
    // Add fixture A pixel 0 twice more to sector-a; it must count once.
    patch.zones[0].members.push(PixelRef {
        fixture: fa,
        pixel: 0,
    });
    patch.zones[0].members.push(PixelRef {
        fixture: fa,
        pixel: 0,
    });
    let index = patch.compile().unwrap();
    assert_eq!(index.zone_members(0), &[0, 1, 2]);
}

#[test]
fn every_compile_mints_a_fresh_generation() {
    let (patch, _, _) = sample_patch();
    let a = patch.compile().unwrap();
    let b = patch.compile().unwrap();
    assert_ne!(
        a.generation(),
        b.generation(),
        "each compile must mint a distinct generation so caches invalidate"
    );
    assert_ne!(a.generation(), LayoutGeneration::NONE);
}

#[test]
fn detects_address_collision() {
    let (mut patch, _, _) = sample_patch();
    // Point fixture B's first pixel at fixture A's channel 1.
    patch.fixtures[1].pixels[0].universe = 1;
    patch.fixtures[1].pixels[0].start_channel = 1;
    assert!(matches!(
        patch.validate(),
        Err(PatchError::AddressCollision { .. })
    ));
}

#[test]
fn detects_channel_out_of_range() {
    let (mut patch, _, _) = sample_patch();
    // RGBW pixel starting at 511 would need channels 511..514.
    patch.fixtures[1].pixels[0].start_channel = 511;
    assert!(matches!(
        patch.validate(),
        Err(PatchError::ChannelOutOfRange { .. })
    ));
}

#[test]
fn detects_dangling_pixel_ref() {
    let (mut patch, fa, _) = sample_patch();
    patch.zones[0].members.push(PixelRef {
        fixture: fa,
        pixel: 99,
    });
    assert!(matches!(
        patch.validate(),
        Err(PatchError::DanglingPixelRef { .. })
    ));
}

#[test]
fn detects_non_dense_pixel_index() {
    let (mut patch, _, _) = sample_patch();
    patch.fixtures[0].pixels[1].index = 7; // should be 1
    assert!(matches!(
        patch.validate(),
        Err(PatchError::NonDensePixelIndex { .. })
    ));
}

#[test]
fn color_order_permutations_are_correct() {
    // Wire position -> canonical component. GRB means wire[0]=G(1), wire[1]=R(0),
    // wire[2]=B(2).
    let profile = FixtureProfile {
        name: "p".into(),
        components: ComponentSet::Rgb,
        gamma: GammaCalibration::default(),
    };
    assert_eq!(profile.wire_order(ColorOrder::Grb).as_slice(), &[1, 0, 2]);

    // RGBW appends White (canonical 3) last, keeping the RGB permutation.
    let rgbw = FixtureProfile {
        name: "p".into(),
        components: ComponentSet::Rgbw,
        gamma: GammaCalibration::default(),
    };
    assert_eq!(rgbw.wire_order(ColorOrder::Grb).as_slice(), &[1, 0, 2, 3]);
}

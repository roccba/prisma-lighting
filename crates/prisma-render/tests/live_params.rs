//! Zone + master live state bound to the Parameter Tree (CLAUDE.md §3.1
//! principle 2, §4.5) — closing M5's "zone state is not addressable" gap.
//!
//! What matters here is not that a multiply works, but that the state is
//! *addressable*: a transport can write `/zone/{id}/dimmer` and both the render
//! loop's counter and the output stage's flat parameters observe it, each through
//! its own wait-free handle read rather than a shared lock.

use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::ParameterTree;
use prisma_output::plan::FlatParams;
use prisma_output::scheduler::FlatParamSource;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::render_loop::ZoneStateSource;
use prisma_render::LiveParams;
use uuid::Uuid;

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

/// Two zones over two pixels.
fn patch() -> Patch {
    let fixture = Uuid::from_u128(0xAB);
    Patch {
        id: Uuid::from_u128(0x77),
        name: "lp".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: fixture,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
                white: prisma_patch::WhiteProfile::default(),
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
            zone: Uuid::nil(),
        }],
        zones: (0..2)
            .map(|i| Zone {
                id: Uuid::from_u128(0x100 + i as u128),
                name: format!("z{i}"),
                members: vec![PixelRef { fixture, pixel: i }],
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [2, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            })
            .collect(),
        groups: Vec::new(),
    }
}

#[test]
fn every_zone_and_master_parameter_is_addressable_by_path() {
    let patch = patch();
    let tree = ParameterTree::with_defaults();
    let _live = LiveParams::register(&tree, &patch);

    // The UI and MIDI-learn build themselves by WALKING the tree, so the paths
    // must actually be there — not merely reachable through a Rust handle.
    let paths: Vec<String> = tree.walk("").iter().map(|n| n.path.to_string()).collect();
    for expected in [
        "/master/dimmer",
        "/master/color/r",
        "/master/color/g",
        "/master/color/b",
    ] {
        assert!(paths.contains(&expected.to_string()), "missing {expected}");
    }
    for zone in &patch.zones {
        for leaf in [
            "dimmer", "color/r", "color/g", "color/b", "speed", "solo", "blackout",
        ] {
            let p = format!("/zone/{}/{leaf}", zone.id);
            assert!(paths.contains(&p), "missing {p}");
        }
    }
}

#[test]
fn a_write_by_path_reaches_both_the_render_and_output_readers() {
    // The whole point: ONE parameter, observed independently by the two stages.
    let patch = patch();
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);

    let path = format!("/zone/{}/dimmer", patch.zones[0].id);
    let handle = tree.resolve(&path).expect("registered");
    tree.write_real(&handle, 0.5, ui());

    // Render side.
    let mut zones = ZoneStates::new(2);
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(
        (zones.get(0).dimmer() - 0.5).abs() < 1e-3,
        "render side sees the write"
    );
    assert!(zones.contributes_pixels(0), "and the predicate follows");
    assert!(!zones.contributes_pixels(1), "zone 1 is still down");

    // Output side, from the same binding.
    let mut flat = FlatParams::identity(2);
    FlatParamSource::read_into(&live, &mut flat);
    assert_eq!(flat.zone_count(), 2);
}

#[test]
fn the_dimmer_is_curved_not_linear() {
    // §4.5: "dimmers should not be linear". A transport writes the normalized
    // fader position; the engine consumes the curved real value.
    let patch = patch();
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);

    // Fader at the midpoint must land well below half brightness.
    tree.write(&live.zone(0).dimmer, 0.5, ui());
    let mut zones = ZoneStates::new(2);
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(
        zones.get(0).dimmer() < 0.3,
        "midpoint fader should be well under 50% light, got {}",
        zones.get(0).dimmer()
    );

    // Writing a REAL value round-trips through the curve.
    tree.write_real(&live.zone(0).dimmer, 0.5, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!((zones.get(0).dimmer() - 0.5).abs() < 1e-3);
}

#[test]
fn solo_and_blackout_flow_through_the_tree_into_the_predicates() {
    let patch = patch();
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);
    let mut zones = ZoneStates::new(2);

    tree.write_real(&live.zone(0).dimmer, 1.0, ui());
    tree.write_real(&live.zone(1).dimmer, 1.0, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(zones.contributes_pixels(0) && zones.contributes_pixels(1));

    // Solo zone 1 by path.
    tree.write(&live.zone(1).solo, 1.0, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(
        !zones.contributes_pixels(0),
        "silenced by another zone's solo"
    );
    assert!(zones.contributes_pixels(1));
    assert!(zones.is_active(0), "but still active for the console");

    // Release solo, then blackout zone 0.
    tree.write(&live.zone(1).solo, 0.0, ui());
    tree.write(&live.zone(0).blackout, 1.0, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(!zones.contributes_pixels(0) && !zones.is_active(0));
    assert!(zones.contributes_pixels(1));
}

#[test]
fn the_hysteresis_latch_survives_across_reads() {
    // The latch lives in the ZoneStates the loop keeps between frames, so reading
    // repeatedly must not reset it — otherwise a fader parked in the dead band
    // would flap the overlap counter every frame (§4.3).
    let patch = patch();
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);
    let mut zones = ZoneStates::new(2);

    tree.write_real(&live.zone(0).dimmer, 1.0, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(zones.get(0).dimmer_up());

    // Into the dead band: the latch must HOLD its previous true across many reads.
    tree.write_real(&live.zone(0).dimmer, 0.0008, ui());
    for _ in 0..10 {
        ZoneStateSource::read_into(&live, &mut zones);
        assert!(zones.get(0).dimmer_up(), "dead band must hold, not flap");
    }

    // Below the fall threshold it finally clears.
    tree.write_real(&live.zone(0).dimmer, 0.0, ui());
    ZoneStateSource::read_into(&live, &mut zones);
    assert!(!zones.get(0).dimmer_up());
}

/// Three one-pixel fixtures in one zone; group A = fixtures 0 and 1, group B = 1 and 2.
fn grouped() -> Patch {
    let fixture = |i: u32| Fixture {
        id: Uuid::from_u128(0xF0 + i as u128),
        name: format!("f{i}"),
        profile: FixtureProfile {
            name: "rgb".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
            white: prisma_patch::WhiteProfile::default(),
        },
        pixels: vec![Pixel {
            index: 0,
            position: glam::Vec3::new(i as f32, 0.0, 0.0),
            universe: 1,
            start_channel: (i as u16) * 3 + 1,
            color_order: ColorOrder::Rgb,
        }],
        zone: Uuid::from_u128(0x100),
    };
    let fid = |i: u128| Uuid::from_u128(0xF0 + i);
    let mut p = patch();
    p.matrix_size = [3, 1, 1];
    p.fixtures = (0..3).map(fixture).collect();
    p.zones.truncate(1);
    p.zones[0].members.clear();
    p.groups = vec![
        prisma_patch::Group {
            id: Uuid::from_u128(0xA),
            name: "A".into(),
            members: vec![fid(0), fid(1)],
            display_color: [0, 0, 0],
        },
        prisma_patch::Group {
            id: Uuid::from_u128(0xB),
            name: "B".into(),
            members: vec![fid(1), fid(2)],
            display_color: [0, 0, 0],
        },
    ];
    p.fill_zone_members_from_ownership();
    p
}

#[test]
fn group_masters_scale_each_pixel_by_the_product_of_its_groups() {
    use prisma_output::plan::scale_component;
    let patch = grouped();
    let index = patch.compile().expect("compiles");
    let tree = ParameterTree::with_defaults();
    let mut live = LiveParams::register(&tree, &patch);
    live.register_groups(&tree, &patch, &index);
    let a = tree
        .resolve("/group/00000000-0000-0000-0000-00000000000a/dimmer")
        .expect("A");
    let b = tree
        .resolve("/group/00000000-0000-0000-0000-00000000000b/dimmer")
        .expect("B");

    // Open by default: the wire is unchanged (×1 exactly).
    let mut flat = FlatParams::identity(1);
    FlatParamSource::read_into(&live, &mut flat);
    let comp = |flat: &FlatParams, pixel: u32, ch: usize| {
        let src = index.pixel_component_offset(pixel) as usize + ch;
        scale_component(0.7, src, ch, None, flat)
    };
    for p in 0..3 {
        assert_eq!(
            comp(&flat, p, 0),
            0.7,
            "pixel {p} untouched by open masters"
        );
    }

    // A at half, B at 0.4: f0 × 0.5, f1 × 0.5 × 0.4, f2 × 0.4 — on every channel.
    tree.write_real(&a, 0.5, ui());
    tree.write_real(&b, 0.4, ui());
    FlatParamSource::read_into(&live, &mut flat);
    for ch in 0..3 {
        assert!((comp(&flat, 0, ch) - 0.7 * 0.5).abs() < 1e-5);
        assert!((comp(&flat, 1, ch) - 0.7 * 0.5 * 0.4).abs() < 1e-5);
        assert!((comp(&flat, 2, ch) - 0.7 * 0.4).abs() < 1e-5);
    }
    // A master at zero darkens its groups' pixels only.
    tree.write_real(&a, 0.0, ui());
    tree.write_real(&b, 1.0, ui());
    FlatParamSource::read_into(&live, &mut flat);
    assert_eq!(comp(&flat, 0, 0), 0.0);
    assert_eq!(comp(&flat, 1, 0), 0.0);
    assert!((comp(&flat, 2, 0) - 0.7).abs() < 1e-6);
}

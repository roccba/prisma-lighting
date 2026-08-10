//! M8 step 1 acceptance: the Show model (`.prisma`) round-trips and its
//! persistence enforces the envelope contract (CLAUDE.md §4.3, M8-SPEC §5).
//!
//! Headless — no GPU. This is the "done when: a `.prisma` loads/saves round-trip
//! identical; no UI" gate of Part A step 1.

use prisma_core::show::{load_show, save_show, Cell, Layer, ParamSet, Show, ShowError};
use prisma_core::show::{CURRENT_SCHEMA_VERSION, FORMAT_ID};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use uuid::Uuid;

/// A minimal but valid two-zone patch: two RGB tubes on row 0, one zone each,
/// distinct DMX addresses, all positions inside the matrix. Enough for the show
/// model to validate against.
fn two_zone_patch() -> Patch {
    let fix_a = Uuid::from_u128(0xF1);
    let fix_b = Uuid::from_u128(0xF2);
    let zone_a = Uuid::from_u128(0x0A);
    let zone_b = Uuid::from_u128(0x0B);

    let tube = |id: Uuid, name: &str, x0: f32, chan0: u16, zone: Uuid| Fixture {
        id,
        name: name.into(),
        profile: FixtureProfile {
            name: "rgb".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
        },
        pixels: (0..4)
            .map(|i| Pixel {
                index: i,
                position: glam::Vec3::new(x0 + i as f32, 0.0, 0.0),
                universe: 1,
                start_channel: chan0 + (i as u16) * 3,
                color_order: ColorOrder::Rgb,
            })
            .collect(),
        zone,
    };

    let zone = |id: Uuid, name: &str, fixture: Uuid, z: i32| Zone {
        id,
        name: name.into(),
        members: (0..4).map(|p| PixelRef { fixture, pixel: p }).collect(),
        bounds: None,
        z_order: z,
        blend_mode: BlendMode::Normal,
        mapping_size: [4, 1, 1],
        display_color: [128, 128, 128],
        world_transform: None,
    };

    Patch {
        id: Uuid::from_u128(0x5107),
        name: "two-zone".into(),
        matrix_size: [8, 1, 1],
        fixtures: vec![
            tube(fix_a, "booth", 0.0, 1, zone_a),
            tube(fix_b, "ceiling", 4.0, 13, zone_b),
        ],
        zones: vec![
            zone(zone_a, "Booth", fix_a, 0),
            zone(zone_b, "Ceiling", fix_b, 1),
        ],
    }
}

/// A show over `two_zone_patch`, with content so the round-trip has something to
/// compare: an effect in each zone's grid, one active, live values nudged.
fn populated_show() -> Show {
    let patch = two_zone_patch();
    let zone_a = patch.zones[0].id;
    let zone_b = patch.zones[1].id;
    let mut show = Show::new("Wedding", patch);

    // Booth: a plasma with tweaked params, running.
    {
        let mut params = ParamSet::new();
        params.set("scale", 0.3);
        params.set("speed", 0.7);
        let zs = show
            .zones
            .iter_mut()
            .find(|z| z.zone_id == zone_a)
            .expect("booth zone show exists");
        zs.grid.push(Cell::single(Layer::shader("plasma", params)));
        zs.grid.push(Cell::empty());
        zs.active = Some(0);
        zs.live.dimmer = 0.8;
        zs.live.color_filter = [1.0, 0.5, 0.2];
    }

    // Ceiling: a wave, assigned but not running; a non-default blend.
    {
        let mut params = ParamSet::new();
        params.set("frequency", 0.4);
        let mut layer = Layer::shader("wave", params);
        layer.blend_mode = BlendMode::Add;
        layer.opacity = 0.6;
        let zs = show
            .zones
            .iter_mut()
            .find(|z| z.zone_id == zone_b)
            .expect("ceiling zone show exists");
        zs.grid.push(Cell::single(layer));
        zs.active = None;
        zs.live.speed_multiplier = 1.5;
    }

    show.master.dimmer = 0.9;
    show
}

#[test]
fn show_round_trips_identically() {
    let show = populated_show();
    show.validate().expect("hand-built show is valid");

    let json = save_show(&show).expect("save");
    let reloaded = load_show(&json).expect("load");

    assert_eq!(
        show, reloaded,
        "a saved show must reload byte-for-byte equal"
    );
    reloaded.validate().expect("reloaded show is still valid");
}

#[test]
fn envelope_carries_format_and_version() {
    let show = populated_show();
    let json = save_show(&show).expect("save");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");

    assert_eq!(value["format"], FORMAT_ID);
    assert_eq!(value["schema_version"], CURRENT_SCHEMA_VERSION);
    // The embedded patch is standalone (no separate patch_id).
    assert!(value["show"]["patch"].is_object(), "patch is embedded");
    assert!(value["show"].get("patch_id").is_none(), "no patch_id field");
}

#[test]
fn a_newer_file_is_refused_not_mis_parsed() {
    let show = populated_show();
    let json = save_show(&show).expect("save");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("json");
    value["schema_version"] = serde_json::json!(CURRENT_SCHEMA_VERSION + 1);
    let bumped = serde_json::to_string(&value).expect("reserialize");

    match load_show(&bumped) {
        Err(ShowError::NewerThanReader { file, reader }) => {
            assert_eq!(file, CURRENT_SCHEMA_VERSION + 1);
            assert_eq!(reader, CURRENT_SCHEMA_VERSION);
        }
        other => panic!("expected NewerThanReader, got {other:?}"),
    }
}

#[test]
fn a_patch_file_is_rejected_by_the_show_loader() {
    // A `.prismap` (or any non-show JSON) must not be mis-parsed as a show.
    let patch = two_zone_patch();
    let patch_json = prisma_patch::save_patch(&patch).expect("save patch");
    match load_show(&patch_json) {
        Err(ShowError::BadFormat { found }) => assert_eq!(found, prisma_patch::FORMAT_ID),
        other => panic!("expected BadFormat, got {other:?}"),
    }
}

#[test]
fn validate_rejects_an_out_of_range_active_cell() {
    let mut show = populated_show();
    let zone_a = show.patch.zones[0].id;
    let zs = show.zones.iter_mut().find(|z| z.zone_id == zone_a).unwrap();
    zs.active = Some(99); // grid has 2 cells

    match show.validate() {
        Err(ShowError::ActiveOutOfRange {
            active, grid_len, ..
        }) => {
            assert_eq!(active, 99);
            assert_eq!(grid_len, 2);
        }
        other => panic!("expected ActiveOutOfRange, got {other:?}"),
    }
}

#[test]
fn validate_requires_one_zone_show_per_patch_zone() {
    // Drop a zone show: the bijection onto patch zones is now incomplete.
    let mut show = populated_show();
    show.zones.pop();
    match show.validate() {
        Err(ShowError::MissingZones { expected, found }) => {
            assert_eq!(expected, 2);
            assert_eq!(found, 1);
        }
        other => panic!("expected MissingZones, got {other:?}"),
    }
}

#[test]
fn validate_rejects_an_unknown_zone_id() {
    let mut show = populated_show();
    show.zones[0].zone_id = Uuid::from_u128(0xDEAD);
    match show.validate() {
        Err(ShowError::UnknownZone { zone }) => assert_eq!(zone, Uuid::from_u128(0xDEAD)),
        other => panic!("expected UnknownZone, got {other:?}"),
    }
}

#[test]
fn new_show_builds_one_zone_show_per_patch_zone() {
    let patch = two_zone_patch();
    let show = Show::new("fresh", patch.clone());
    assert_eq!(show.zones.len(), patch.zones.len());
    for z in &patch.zones {
        assert!(
            show.zone(z.id).is_some(),
            "every patch zone has a zone show"
        );
    }
    show.validate().expect("a fresh show is valid");
}

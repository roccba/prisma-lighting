//! Live model phase 5, step 2 — groups as a track destination (docs/LIVE-MODEL-SPEC.md
//! §4.1 step 3, §8.5): a track that reaches a zone through a group renders there as for
//! the whole zone and is confined to the group's cells; a group spanning two zones
//! reaches both; a zone named directly wins over a group of it; with a zone matrix
//! coarser than the rig, a texel shared by a group pixel and another pixel is in.
//!
//! Runs on lavapipe (deterministic).

use half::f16;
use prisma_core::show::{Cell, Destination, Layer, ParamSet, Show, ZoneReach};
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_engine::{build_track_effects, destination_slots, group_zone_mask, ZoneCompositor};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Group, Patch,
    PatchIndex, Pixel, Zone,
};
use prisma_render::overlap::OverlapCounter;
use prisma_render::place::Footprint;
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::FrameContent;
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::Gpu;
use uuid::Uuid;

mod common;
use common::software_gpu;

const EPS: f32 = 1.0 / 128.0;
const ZA: Uuid = Uuid::from_u128(0xA0);
const ZB: Uuid = Uuid::from_u128(0xB0);
const G: Uuid = Uuid::from_u128(0x6);

fn fixture_id(i: u32) -> Uuid {
    Uuid::from_u128(0x100 + i as u128)
}

/// Eight one-pixel fixtures in a row: 0–3 owned by zone A, 4–7 by zone B. Group G =
/// fixtures 1, 2 (zone A) and 5 (zone B). `mapping_a` is zone A's matrix width.
fn rig(mapping_a: u32) -> Patch {
    let profile = FixtureProfile {
        name: "rgb".into(),
        components: ComponentSet::Rgb,
        gamma: GammaCalibration::default(),
        white: prisma_patch::WhiteProfile::default(),
    };
    let zone = |id: Uuid, w: u32, z: i32| Zone {
        id,
        name: format!("{id}"),
        members: vec![],
        bounds: None,
        z_order: z,
        blend_mode: BlendMode::Normal,
        mapping_size: [w, 1, 1],
        display_color: [128, 128, 128],
        world_transform: None,
    };
    let mut p = Patch {
        id: Uuid::from_u128(0x95),
        name: "lm5".into(),
        matrix_size: [8, 1, 1],
        fixtures: (0..8)
            .map(|i| Fixture {
                id: fixture_id(i),
                name: format!("f{i}"),
                profile: profile.clone(),
                pixels: vec![Pixel {
                    index: 0,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                }],
                zone: if i < 4 { ZA } else { ZB },
            })
            .collect(),
        zones: vec![zone(ZA, mapping_a, 0), zone(ZB, 4, 1)],
        groups: vec![Group {
            id: G,
            name: "G".into(),
            members: vec![fixture_id(5), fixture_id(1), fixture_id(2)],
            display_color: [1, 2, 3],
        }],
    };
    p.fill_zone_members_from_ownership();
    p.validate().expect("valid rig");
    p
}

fn solid(red: f32) -> Cell {
    let mut params = ParamSet::new();
    params.set("r", red);
    params.set("g", 0.0);
    params.set("b", 0.0);
    Cell::single(Layer::shader("solid", params))
}

/// A show on `patch` with, bottom to top, a background track over both zones at red
/// 0.25 and a top track at red 1.0 reaching `top`.
fn show(patch: &Patch, top: Destination) -> Show {
    let mut s = Show::new("lm5", patch.clone());
    s.tracks.clear();
    let mut bg = prisma_core::show::Track::new("fondo", Destination::zone(ZA));
    bg.destination.zones.push(ZB);
    bg.clips = vec![solid(0.25)];
    bg.active = Some(0);
    let mut t = prisma_core::show::Track::new("arriba", top);
    t.clips = vec![solid(1.0)];
    t.active = Some(0);
    s.tracks = vec![bg, t];
    s.validate().expect("valid show");
    s
}

fn lit(n: usize) -> ZoneStates {
    let mut z = ZoneStates::new(n);
    for slot in 0..n {
        z.set_dimmer(slot, 1.0);
    }
    z
}

/// One frame of `show`, gathered: each pixel's red (master buffer, linear).
fn red(gpu: &Gpu, show: &Show) -> Vec<f32> {
    let patch = &show.patch;
    let index: PatchIndex = patch.compile().expect("compiles");
    let tree = ParameterTree::with_defaults();
    let tracks = build_track_effects(gpu, &tree, patch, show, &[]);
    let mut content = ZoneCompositor::new(gpu, patch, &index, tracks);
    let zones = lit(patch.zones.len());
    let mut counter = OverlapCounter::new(patch, &index);
    counter.sync(&zones);
    let overlap = counter.snapshot();
    let fb = create_render_target(gpu, patch.matrix_size[0], 1);
    let clock = ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns: 0,
        beat: 0.0,
        bpm: 120.0,
        playing: true,
    };
    content.render_into(gpu, &clock, &zones, &overlap, &fb);
    let sampler = Sampler::new(gpu, &SamplePlan::compile(patch, &index));
    sampler.gather(gpu, &fb);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| master[index.pixel_component_offset(g) as usize].to_f32())
        .collect()
}

fn assert_red(got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            (g - w).abs() <= EPS,
            "pixel {i}: got {g}, want {w} (all: {got:?})"
        );
    }
}

fn groups(g: &[Uuid]) -> Destination {
    Destination {
        zones: vec![],
        groups: g.to_vec(),
    }
}

#[test]
fn a_group_track_lights_only_the_groups_fixtures_in_every_zone_it_reaches() {
    let gpu = software_gpu();
    let s = show(&rig(4), groups(&[G]));
    // Reaches both zones (the group spans them), through the group in each.
    assert_eq!(destination_slots(&s.patch, &s.tracks[1]), vec![0, 1]);
    let got = red(gpu, &s);
    assert_red(&got, &[0.25, 1.0, 1.0, 0.25, 0.25, 1.0, 0.25, 0.25]);
}

#[test]
fn a_zone_named_directly_wins_over_a_group_of_it() {
    let gpu = software_gpu();
    let top = Destination {
        zones: vec![ZA],
        groups: vec![G],
    };
    let s = show(&rig(4), top);
    assert_eq!(
        s.tracks[1].destination.reach(&s.patch),
        vec![(0, ZoneReach::Whole), (1, ZoneReach::Groups(vec![G]))]
    );
    let got = red(gpu, &s);
    assert_red(&got, &[1.0, 1.0, 1.0, 1.0, 0.25, 1.0, 0.25, 0.25]);
}

#[test]
fn a_group_with_no_fixture_in_a_zone_does_not_reach_it() {
    let mut patch = rig(4);
    patch.groups[0].members = vec![fixture_id(6)];
    let s = show(&patch, groups(&[G]));
    assert_eq!(destination_slots(&s.patch, &s.tracks[1]), vec![1]);
    let got = red(software_gpu(), &s);
    assert_red(&got, &[0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 1.0, 0.25]);
}

#[test]
fn a_coarse_zone_matrix_lights_the_whole_shared_texel() {
    // Zone A at 2 texels over 4 cells: fixtures 0–1 share texel 0, 2–3 texel 1. The
    // group has fixtures 1 and 2, so both texels are in.
    let patch = rig(2);
    let foot = Footprint {
        origin: [0.0, 0.0],
        size: [4.0, 1.0],
    };
    assert_eq!(group_zone_mask(&patch, 0, &[G], foot, (2, 1)), vec![1, 1]);
    // Only fixture 1: texel 0 alone.
    let mut one = patch.clone();
    one.groups[0].members = vec![fixture_id(1)];
    assert_eq!(group_zone_mask(&one, 0, &[G], foot, (2, 1)), vec![1, 0]);
    let s = show(&one, groups(&[G]));
    let got = red(software_gpu(), &s);
    assert_red(&got, &[1.0, 1.0, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25]);
}

#[test]
fn a_destination_naming_an_unknown_group_is_refused() {
    let mut s = show(&rig(4), groups(&[G]));
    s.tracks[1].destination.groups.push(Uuid::from_u128(0xDEAD));
    assert!(s.validate().is_err());
}

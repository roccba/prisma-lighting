//! Live model phase 4 — a track that reaches several zones (docs/LIVE-MODEL-SPEC.md
//! §8.4): one set of effect parameters shared by its per-zone instances, so one write
//! (the panel, a knob, MIDI) moves every zone in the same frame; each zone keeps its
//! own animation phase (its VEL); and a crossfade between two clips of the same track
//! keeps the outgoing clip's parameters until it is gone.
//!
//! Runs on lavapipe (deterministic).

use half::f16;
use prisma_core::show::ParamSet;
use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, Easing, ParameterTree};
use prisma_engine::{
    build_track_instances, track_param_prefix, Fade, TrackContent, ZoneCompositor,
};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::OverlapCounter;
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::FrameContent;
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::Gpu;
use uuid::Uuid;

mod common;
use common::software_gpu;

const EPS: f32 = 1.0 / 128.0;
const SEC: u64 = 1_000_000_000;

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

/// A single-row RGB rig: zone `i` owns `per_zone` consecutive pixels, mapping 1:1.
fn rig(zones: usize, per_zone: u32) -> Patch {
    let fixture_id = Uuid::from_u128(0xF9);
    let width = zones as u32 * per_zone;
    Patch {
        id: Uuid::from_u128(0x92),
        name: "lm2".into(),
        matrix_size: [width, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
                white: prisma_patch::WhiteProfile::default(),
            },
            pixels: (0..width)
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
        zones: (0..zones)
            .map(|z| Zone {
                id: Uuid::from_u128(0x200 + z as u128),
                name: format!("z{z}"),
                members: (0..per_zone)
                    .map(|p| PixelRef {
                        fixture: fixture_id,
                        pixel: z as u32 * per_zone + p,
                    })
                    .collect(),
                bounds: None,
                z_order: z as i32,
                blend_mode: BlendMode::Normal,
                mapping_size: [per_zone, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            })
            .collect(),
    }
}

fn clock_at(show_ns: u64, bpm: f64) -> ClockSnapshot {
    let s = show_ns as f64 / 1e9;
    ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns,
        beat: s * bpm / 60.0,
        bpm,
        playing: true,
    }
}

fn lit(n: usize) -> ZoneStates {
    let mut z = ZoneStates::new(n);
    for slot in 0..n {
        z.set_dimmer(slot, 1.0);
    }
    z
}

/// One frame at `clock`, gathered: each pixel's red component (master buffer, linear).
fn frame(
    gpu: &Gpu,
    patch: &Patch,
    index: &PatchIndex,
    content: &mut impl FrameContent,
    zones: &ZoneStates,
    clock: &ClockSnapshot,
) -> Vec<f32> {
    let mut counter = OverlapCounter::new(patch, index);
    counter.sync(zones);
    let overlap = counter.snapshot();
    let fb = create_render_target(gpu, patch.matrix_size[0], 1);
    content.render_into(gpu, clock, zones, &overlap, &fb);
    let sampler = Sampler::new(gpu, &SamplePlan::compile(patch, index));
    sampler.gather(gpu, &fb);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| master[index.pixel_component_offset(g) as usize].to_f32())
        .collect()
}

fn all(px: &[f32], v: f32) -> bool {
    px.iter().all(|c| (c - v).abs() <= EPS)
}

fn grey_params(v: f32) -> ParamSet {
    let mut p = ParamSet::new();
    for c in ["r", "g", "b"] {
        p.set(c, v);
    }
    p
}

/// `track`'s instances of `effect_id` for `zones` zones, built the way the clip
/// controller builds them (one registration, shared).
fn instances(
    gpu: &Gpu,
    tree: &ParameterTree,
    track: Uuid,
    zones: usize,
    effect_id: &str,
    params: &ParamSet,
) -> Vec<Option<EffectInstance>> {
    build_track_instances(gpu, tree, track, zones, effect_id, params, None, ui())
        .expect("effect in the catalogue")
}

fn two_zone_track(effects: Vec<Option<EffectInstance>>) -> TrackContent {
    TrackContent {
        zones: vec![0, 1],
        blend_mode: BlendMode::Normal,
        key: prisma_render::blend::DEFAULT_KEY,
        opacity: 1.0,
        live: None,
        effects,
    }
}

#[test]
fn one_write_moves_every_zone_of_the_track() {
    let gpu = software_gpu();
    let mut patch = rig(2, 4);
    // Different render resolutions per zone: the params are the same values anyway.
    patch.zones[1].mapping_size = [8, 3, 1];
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let id = Uuid::from_u128(0x7A);
    let effects = instances(gpu, &tree, id, 2, "solid", &grey_params(0.3));
    let mut c = ZoneCompositor::new(gpu, &patch, &index, vec![two_zone_track(effects)]);
    let zones = lit(2);
    let now = clock_at(0, 120.0);

    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(all(&f, 0.3), "both zones start on the stored value: {f:?}");

    // One write at the track's path — what the panel and a knob do.
    for ch in ["r", "g", "b"] {
        let path = format!("{}/param/{ch}", track_param_prefix(id));
        tree.write_path(&path, 0.7, ui());
    }
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(all(&f, 0.7), "one write moved both zones: {f:?}");
}

#[test]
fn each_zone_keeps_its_own_speed() {
    let gpu = software_gpu();
    let patch = rig(2, 16);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let id = Uuid::from_u128(0x7B);
    let effects = instances(gpu, &tree, id, 2, "wave", &ParamSet::new());
    let mut c = ZoneCompositor::new(gpu, &patch, &index, vec![two_zone_track(effects)]);
    let mut zones = lit(2);
    zones.set_speed_multiplier(0, 1.0);
    zones.set_speed_multiplier(1, 0.0); // zone 1 holds its phase

    let t0 = frame(gpu, &patch, &index, &mut c, &zones, &clock_at(SEC, 120.0));
    assert_eq!(
        t0[0..16],
        t0[16..32],
        "same params, same phase: same picture"
    );
    let t1 = frame(
        gpu,
        &patch,
        &index,
        &mut c,
        &zones,
        &clock_at(SEC + SEC / 3, 120.0),
    );
    assert_ne!(t1[0..16], t0[0..16], "zone 0 animates");
    assert_eq!(t1[16..32], t0[16..32], "zone 1 at speed 0 holds");
}

#[test]
fn a_crossfade_keeps_the_outgoing_clips_params() {
    let gpu = software_gpu();
    let patch = rig(2, 4);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let id = Uuid::from_u128(0x7C);
    let a = instances(gpu, &tree, id, 2, "solid", &grey_params(0.2));
    let mut c = ZoneCompositor::new(gpu, &patch, &index, vec![two_zone_track(a)]);
    let zones = lit(2);
    let t0 = 10 * SEC;
    frame(gpu, &patch, &index, &mut c, &zones, &clock_at(t0, 120.0));

    // Clip B of the SAME effect on the same track: its params register the same paths,
    // so the outgoing clip's handles die. It must keep fading with 0.2, not 0.
    let b = instances(gpu, &tree, id, 2, "solid", &grey_params(0.8));
    assert!(c
        .command_sender()
        .play_track(0, b, Fade::seconds(2.0, Easing::Linear)));
    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));
    let start = at(&mut c, t0);
    assert!(all(&start, 0.2), "the fade starts on clip A: {start:?}");
    let mid = at(&mut c, t0 + SEC);
    assert!(all(&mid, 0.5), "halfway between 0.2 and 0.8: {mid:?}");
    let end = at(&mut c, t0 + 2 * SEC);
    assert!(all(&end, 0.8), "lands on clip B: {end:?}");
}

/// LIVE-MODEL-SPEC §10.2: a reorder changes only the fold order, at the frame boundary.
/// Nothing is rebuilt, so an animated clip keeps its phase: after the reorder the frame is
/// exactly what a compositor that never reordered shows.
#[test]
fn a_reorder_changes_the_fold_without_restarting_anything() {
    let gpu = software_gpu();
    let patch = rig(1, 16);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let one_zone = |effects: Vec<Option<EffectInstance>>, blend: BlendMode| TrackContent {
        zones: vec![0],
        blend_mode: blend,
        key: prisma_render::blend::DEFAULT_KEY,
        opacity: 1.0,
        live: None,
        effects,
    };
    // Track 0: an animated wave. Track 1: opaque grey, Normal, on top of it.
    let build = |n: u128| {
        let wave = instances(gpu, &tree, Uuid::from_u128(n), 1, "wave", &ParamSet::new());
        let grey = instances(
            gpu,
            &tree,
            Uuid::from_u128(n + 1),
            1,
            "solid",
            &grey_params(0.5),
        );
        vec![
            one_zone(wave, BlendMode::Normal),
            one_zone(grey, BlendMode::Normal),
        ]
    };
    let mut moved = ZoneCompositor::new(gpu, &patch, &index, build(0x90));
    let zones = lit(1);
    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));

    let t0 = at(&mut moved, SEC);
    assert!(all(&t0, 0.5), "the grey track is on top: {t0:?}");

    // The wave goes on top. Its animation has been running for 1 s and must not restart.
    assert!(moved.command_sender().reorder(vec![1, 0]));
    let after = at(&mut moved, 2 * SEC);
    assert!(!all(&after, 0.5), "the wave is on top now: {after:?}");

    // A compositor built with the wave on top from the start, same clock history: the
    // same picture, so the moved wave did not restart.
    let reference = build(0xA0);
    let mut built_on_top =
        ZoneCompositor::new(gpu, &patch, &index, reference.into_iter().rev().collect());
    at(&mut built_on_top, SEC);
    assert_eq!(
        at(&mut built_on_top, 2 * SEC),
        after,
        "same phase: nothing restarted"
    );

    // And back: the grey track on top again.
    assert!(moved.command_sender().reorder(vec![0, 1]));
    assert!(all(&at(&mut moved, 3 * SEC), 0.5));
}

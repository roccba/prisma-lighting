//! Live model phase 2b — alpha (docs/LIVE-MODEL-SPEC.md §8, 2b): effects with a
//! transparent background let the tracks below show through, a luminance key cuts
//! sources without alpha, clip crossfades are linear mixes (not alpha-over), and on
//! overlapped pixels a zone's dimmer is an opacity — a lowered upper zone reveals the
//! zone below instead of black.
//!
//! Runs on lavapipe (deterministic). Values are read from the master buffer (linear,
//! before the output stage), which is the premultiplied picture over black.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, Easing, ParameterTree};
use prisma_engine::{Fade, TrackContent, ZoneCompositor};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_render::effect::EffectInstance;
use prisma_render::effects;
use prisma_render::overlap::OverlapCounter;
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::FrameContent;
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::Gpu;
use uuid::Uuid;

mod common;
use common::software_gpu;

const EPS: f32 = 1.0 / 64.0;
const SEC: u64 = 1_000_000_000;

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

/// A single-row RGB rig of `width` pixels; each zone is `(member pixels, z_order)`
/// and renders at its own width.
fn rig(width: u32, zones: &[(&[u32], i32)]) -> Patch {
    let fixture_id = Uuid::from_u128(0xA1);
    Patch {
        id: Uuid::from_u128(0xA2),
        name: "lm2b".into(),
        matrix_size: [width, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
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
        zones: zones
            .iter()
            .enumerate()
            .map(|(i, (members, z))| Zone {
                id: Uuid::from_u128(0x300 + i as u128),
                name: format!("z{i}"),
                members: members
                    .iter()
                    .map(|&pixel| PixelRef {
                        fixture: fixture_id,
                        pixel,
                    })
                    .collect(),
                bounds: None,
                z_order: *z,
                blend_mode: BlendMode::Normal,
                mapping_size: [members.len() as u32, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            })
            .collect(),
    }
}

fn still(show_ns: u64) -> ClockSnapshot {
    ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns,
        beat: 0.0,
        bpm: 120.0,
        playing: true,
    }
}

/// One frame, gathered: each pixel's rgb (master buffer, linear).
fn frame(
    gpu: &Gpu,
    patch: &Patch,
    index: &PatchIndex,
    content: &mut impl FrameContent,
    zones: &ZoneStates,
    clock: &ClockSnapshot,
) -> Vec<[f32; 3]> {
    let mut counter = OverlapCounter::new(patch, index);
    counter.sync(zones);
    let overlap = counter.snapshot();
    let fb = create_render_target(gpu, patch.matrix_size[0], 1);
    content.render_into(gpu, clock, zones, &overlap, &fb);
    let sampler = Sampler::new(gpu, &SamplePlan::compile(patch, index));
    sampler.gather(gpu, &fb);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| {
            let o = index.pixel_component_offset(g) as usize;
            [
                master[o].to_f32(),
                master[o + 1].to_f32(),
                master[o + 2].to_f32(),
            ]
        })
        .collect()
}

fn lit(n: usize) -> ZoneStates {
    let mut z = ZoneStates::new(n);
    for slot in 0..n {
        z.set_dimmer(slot, 1.0);
    }
    z
}

fn near(px: [f32; 3], want: [f32; 3]) -> bool {
    (0..3).all(|i| (px[i] - want[i]).abs() <= EPS)
}

fn solid(gpu: &Gpu, tree: &ParameterTree, prefix: &str, rgb: [f32; 3]) -> EffectInstance {
    let inst = EffectInstance::new(gpu, tree, prefix, &effects::solid());
    for (c, v) in ["r", "g", "b"].iter().zip(rgb) {
        tree.write_path(&format!("{prefix}/param/{c}"), v, ui());
    }
    inst
}

/// A red/black colour scroll (hard stripes), with the given background (0 =
/// transparent, 1 = black).
fn red_black_scroll(
    gpu: &Gpu,
    tree: &ParameterTree,
    prefix: &str,
    background: f32,
) -> EffectInstance {
    let inst = EffectInstance::new(gpu, tree, prefix, &effects::color_scroll());
    inst.set_palette(gpu, &[[1.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
    tree.write_path(&format!("{prefix}/param/background"), background, ui());
    inst
}

fn track(zone: usize, mode: BlendMode, effect: Option<EffectInstance>) -> TrackContent {
    TrackContent {
        zones: vec![zone],
        blend_mode: mode,
        key: prisma_render::blend::DEFAULT_KEY,
        opacity: 1.0,
        live: None,
        effects: vec![effect],
    }
}

const RED: [f32; 3] = [1.0, 0.0, 0.0];
const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];

/// The spec's test: a red/black scroll with a transparent background, Normal over
/// green, shows red and green at the stripe centres — never black. With a black
/// background the black stripes cover the green.
#[test]
fn a_transparent_red_black_scroll_over_green_shows_red_and_green() {
    let gpu = software_gpu();
    let all: Vec<u32> = (0..32).collect();
    let patch = rig(32, &[(&all, 0)]);
    let index = patch.compile().unwrap();
    let zones = lit(1);
    let clock = still(0);

    // The scroll alone tells where its stripe centres are.
    let tree = ParameterTree::with_defaults();
    let mut alone = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(
            0,
            BlendMode::Normal,
            Some(red_black_scroll(gpu, &tree, "/s", 0.0)),
        )],
    );
    let stripes = frame(gpu, &patch, &index, &mut alone, &zones, &clock);
    let red: Vec<usize> = (0..32).filter(|&i| near(stripes[i], RED)).collect();
    let black: Vec<usize> = (0..32).filter(|&i| near(stripes[i], BLACK)).collect();
    assert!(
        !red.is_empty() && !black.is_empty(),
        "the scroll shows both stripes: {stripes:?}"
    );

    for (background, gap) in [(0.0, GREEN), (1.0, BLACK)] {
        let tree = ParameterTree::with_defaults();
        let mut over = ZoneCompositor::new(
            gpu,
            &patch,
            &index,
            vec![
                track(0, BlendMode::Normal, Some(solid(gpu, &tree, "/g", GREEN))),
                track(
                    0,
                    BlendMode::Normal,
                    Some(red_black_scroll(gpu, &tree, "/s", background)),
                ),
            ],
        );
        let f = frame(gpu, &patch, &index, &mut over, &zones, &clock);
        for &i in &red {
            assert!(
                near(f[i], RED),
                "bg {background}: red stripe pixel {i} = {:?}",
                f[i]
            );
        }
        for &i in &black {
            assert!(
                near(f[i], gap),
                "bg {background}: black stripe pixel {i} = {:?}, want {gap:?}",
                f[i]
            );
        }
        if background == 0.0 {
            for (i, px) in f.iter().enumerate() {
                assert!(px[0] + px[1] > 0.9, "pixel {i} went dark: {px:?}");
            }
        }
    }
}

/// Outside a transparent shape the track below shows.
#[test]
fn outside_a_shape_the_track_below_shows() {
    let gpu = software_gpu();
    let all: Vec<u32> = (0..16).collect();
    let patch = rig(16, &[(&all, 0)]);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let shape = EffectInstance::new(gpu, &tree, "/sh", &effects::simple_shape());
    // On a one-row matrix the shape's radius is in row heights: make it big enough to
    // cover the middle pixels.
    for (c, v) in [("r", 1.0), ("g", 0.0), ("b", 0.0), ("size", 1.0)] {
        tree.write_path(&format!("/sh/param/{c}"), v, ui());
    }
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(0, BlendMode::Normal, Some(solid(gpu, &tree, "/g", GREEN))),
            track(0, BlendMode::Normal, Some(shape)),
        ],
    );
    let f = frame(gpu, &patch, &index, &mut c, &lit(1), &still(0));
    assert!(
        f.iter().any(|px| near(*px, GREEN)),
        "outside the shape: green {f:?}"
    );
    assert!(
        f.iter().any(|px| px[0] > 0.5),
        "inside the shape: red {f:?}"
    );
    assert!(f.iter().all(|px| px[0] + px[1] > 0.9), "never black: {f:?}");
}

/// A luminance key cuts the dark part of a source without alpha.
#[test]
fn a_luma_key_cuts_the_dark_part_of_an_opaque_source() {
    let gpu = software_gpu();
    let all: Vec<u32> = (0..16).collect();
    let patch = rig(16, &[(&all, 0)]);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    // An opaque left-to-right ramp 0 → 1.
    let ramp = EffectInstance::new(gpu, &tree, "/r", &effects::gradient());
    tree.write_path("/r/param/start", 0.0, ui());
    tree.write_path("/r/param/end", 1.0, ui());
    tree.write_path("/r/param/background", 1.0, ui());
    let mut keyed = track(0, BlendMode::LumaKey, Some(ramp));
    keyed.key = [0.5, 0.01];
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(0, BlendMode::Normal, Some(solid(gpu, &tree, "/g", GREEN))),
            keyed,
        ],
    );
    let f = frame(gpu, &patch, &index, &mut c, &lit(1), &still(0));
    assert!(near(f[0], GREEN), "dark end keyed out: {:?}", f[0]);
    let bright = f[15];
    assert!(
        bright[0] > 0.9 && bright[2] > 0.9,
        "bright end kept: {bright:?}"
    );
}

/// A crossfade between pictures with transparency is a linear mix: inside, the two
/// colours half and half; outside (transparent in both), the track below throughout.
#[test]
fn a_crossfade_between_transparent_clips_mixes_without_a_pop() {
    let gpu = software_gpu();
    let all: Vec<u32> = (0..16).collect();
    let patch = rig(16, &[(&all, 0)]);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let shape = |prefix: &str, rgb: [f32; 3]| {
        let s = EffectInstance::new(gpu, &tree, prefix, &effects::simple_shape());
        for (c, v) in ["r", "g", "b"].iter().zip(rgb) {
            tree.write_path(&format!("{prefix}/param/{c}"), v, ui());
        }
        tree.write_path(&format!("{prefix}/param/size"), 1.0, ui());
        s
    };
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(
                0,
                BlendMode::Normal,
                Some(solid(gpu, &tree, "/g", [0.0, 0.2, 0.0])),
            ),
            track(0, BlendMode::Normal, Some(shape("/a", [1.0, 0.0, 0.0]))),
        ],
    );
    let zones = lit(1);
    let before = frame(gpu, &patch, &index, &mut c, &zones, &still(10 * SEC));
    let centre = before
        .iter()
        .position(|px| px[0] > 0.99)
        .expect("a fully covered pixel inside the shape");
    let outside = before
        .iter()
        .position(|px| near(*px, [0.0, 0.2, 0.0]))
        .expect("a pixel outside the shape");

    c.command_sender().play_track(
        1,
        vec![Some(shape("/b", [0.0, 0.0, 1.0]))],
        Fade::seconds(2.0, Easing::Linear),
    );
    frame(gpu, &patch, &index, &mut c, &zones, &still(10 * SEC));
    let mid = frame(gpu, &patch, &index, &mut c, &zones, &still(11 * SEC));
    assert!(
        near(mid[centre], [0.5, 0.0, 0.5]),
        "inside, half and half: {:?}",
        mid[centre]
    );
    assert!(
        near(mid[outside], [0.0, 0.2, 0.0]),
        "outside, the track below: {:?}",
        mid[outside]
    );
}

/// Zone over zone: on overlapped pixels the upper zone's dimmer is an opacity. At 0
/// the lower zone shows; at 0.5 they mix half and half (not a darkened upper zone).
/// A pixel only the upper zone covers keeps getting its dimmer at the output stage.
#[test]
fn an_upper_zones_dimmer_is_its_opacity_over_the_zone_below() {
    let gpu = software_gpu();
    // Zone 0 (below): pixels 0..=2. Zone 1 (above): pixels 1..=3. Overlap: 1, 2.
    let patch = rig(4, &[(&[0, 1, 2], 0), (&[1, 2, 3], 1)]);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(0, BlendMode::Normal, Some(solid(gpu, &tree, "/lo", GREEN))),
            track(1, BlendMode::Normal, Some(solid(gpu, &tree, "/hi", RED))),
        ],
    );
    let mut zones = lit(2);

    zones.set_dimmer(1, 0.0);
    let off = frame(gpu, &patch, &index, &mut c, &zones, &still(0));
    for p in [1, 2] {
        assert!(
            near(off[p], GREEN),
            "upper at 0: the lower zone at pixel {p}: {:?}",
            off[p]
        );
    }

    zones.set_dimmer(1, 0.5);
    let half = frame(gpu, &patch, &index, &mut c, &zones, &still(0));
    for p in [1, 2] {
        assert!(
            near(half[p], [0.5, 0.5, 0.0]),
            "upper at 0.5: a 50/50 mix at {p}: {:?}",
            half[p]
        );
    }
    // Pixel 3 is the upper zone's alone: no overlap, its dimmer is deferred to the
    // output stage (the latency shortcut), so the master buffer holds full red.
    assert!(
        near(half[3], RED),
        "upper-only pixel, dimmed at output: {:?}",
        half[3]
    );
    assert!(near(half[0], GREEN), "lower-only pixel: {:?}", half[0]);
}

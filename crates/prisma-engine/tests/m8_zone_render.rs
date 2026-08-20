//! M8 step 2 — the per-zone render architecture, validated end to end
//! (`ZoneCompositor`, CLAUDE.md M8 / docs/M8-PARTA.md §2).
//!
//! The gate test (`prisma-render/tests/m8_place.rs`) proved the *placement
//! primitive* byte-identical to M5 in the identity case. These tests prove the
//! *assembled architecture*: zones rendering at **genuinely different
//! `mapping_size`s**, each placed into the world framebuffer at its own footprint,
//! composited by the unchanged M5 machinery, and gathered to the master buffer —
//! independent per zone, which is the product's whole thesis.
//!
//! Runs on lavapipe (deterministic). Grayscale content, so the red component of
//! each master pixel identifies the value.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_engine::{ZoneCommandSender, ZoneCompositor};
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

const EPS: f32 = 1.0 / 128.0;

fn still_clock() -> ClockSnapshot {
    ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns: 0,
        beat: 0.0,
        bpm: 120.0,
        playing: false,
    }
}

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

/// A single-row RGB rig of `width` pixels, with the given zones. Each zone is
/// `(name, member cells, mapping_width, blend)`; members are cell/pixel indices on
/// row 0.
fn build_patch(width: u32, zones: &[(&str, &[u32], u32, BlendMode)]) -> Patch {
    let fixture_id = Uuid::from_u128(0xF8);
    Patch {
        id: Uuid::from_u128(0x82),
        name: "m8".into(),
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
            .map(|(i, (name, members, mapping_w, mode))| Zone {
                id: Uuid::from_u128(0x100 + i as u128),
                name: (*name).into(),
                members: members
                    .iter()
                    .map(|&pixel| PixelRef {
                        fixture: fixture_id,
                        pixel,
                    })
                    .collect(),
                bounds: None,
                z_order: i as i32,
                blend_mode: *mode,
                mapping_size: [*mapping_w, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            })
            .collect(),
    }
}

fn gradient_effect(gpu: &Gpu, tree: &ParameterTree, prefix: &str) -> EffectInstance {
    let inst = EffectInstance::new(gpu, tree, prefix, &effects::gradient());
    tree.write_path(&format!("{prefix}/param/start"), 0.0, ui());
    tree.write_path(&format!("{prefix}/param/end"), 1.0, ui());
    inst
}

fn solid_effect(gpu: &Gpu, tree: &ParameterTree, prefix: &str, rgb: [f32; 3]) -> EffectInstance {
    let inst = EffectInstance::new(gpu, tree, prefix, &effects::solid());
    for (c, v) in ["r", "g", "b"].iter().zip(rgb) {
        tree.write_path(&format!("{prefix}/param/{c}"), v, ui());
    }
    inst
}

/// Drive one frame of `content` into a world framebuffer, gather to the master
/// buffer, and return each pixel's RGB (from the master buffer, before the output
/// stage's flat parameters).
fn render_and_gather(
    gpu: &Gpu,
    patch: &Patch,
    index: &PatchIndex,
    content: &mut impl FrameContent,
    zones: &ZoneStates,
) -> Vec<[f32; 3]> {
    let mut counter = OverlapCounter::new(patch, index);
    counter.sync(zones);
    let overlap = counter.snapshot();

    let framebuffer = create_render_target(gpu, patch.matrix_size[0], patch.matrix_size[1].max(1));
    content.render_into(gpu, &still_clock(), zones, &overlap, &framebuffer);

    let sampler = Sampler::new(gpu, &SamplePlan::compile(patch, index));
    sampler.gather(gpu, &framebuffer);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| {
            let off = index.pixel_component_offset(g) as usize;
            [
                master[off].to_f32(),
                master[off + 1].to_f32(),
                master[off + 2].to_f32(),
            ]
        })
        .collect()
}

/// Like [`render_and_gather`] but at a caller-chosen clock, so a test can advance show
/// time frame by frame (the persistent `content` keeps its per-zone phase between calls).
fn render_and_gather_at(
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

    let framebuffer = create_render_target(gpu, patch.matrix_size[0], patch.matrix_size[1].max(1));
    content.render_into(gpu, clock, zones, &overlap, &framebuffer);

    let sampler = Sampler::new(gpu, &SamplePlan::compile(patch, index));
    sampler.gather(gpu, &framebuffer);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| {
            let off = index.pixel_component_offset(g) as usize;
            [
                master[off].to_f32(),
                master[off + 1].to_f32(),
                master[off + 2].to_f32(),
            ]
        })
        .collect()
}

fn playing_clock(show_ns: u64) -> ClockSnapshot {
    ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns,
        beat: 0.0,
        bpm: 120.0,
        playing: true,
    }
}

fn lit_zones(n: usize) -> ZoneStates {
    let mut z = ZoneStates::new(n);
    for slot in 0..n {
        z.set_dimmer(slot, 1.0);
    }
    z
}

fn approx(px: [f32; 3], v: f32) -> bool {
    px.iter().all(|&c| (c - v).abs() <= EPS)
}

/// THE step-2 architecture test: two zones at **different `mapping_size`s** run
/// independent effects, each placed into its own footprint, gathered correctly.
#[test]
fn two_zones_at_different_mapping_sizes_render_independently() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();

    // Booth: cells 0..4, rendered at mapping_size 2 (coarser than its 4-cell
    // footprint). Ceiling: cells 4..8, rendered at mapping_size 4 (== footprint).
    let patch = build_patch(
        8,
        &[
            ("booth", &[0, 1, 2, 3], 2, BlendMode::Normal),
            ("ceiling", &[4, 5, 6, 7], 4, BlendMode::Normal),
        ],
    );
    let index = patch.compile().expect("rig compiles");

    // Booth: a flat solid (0.5) — a *different effect* from the ceiling, and flat so
    // its confinement to cells 0..4 is unambiguous. Ceiling: a gradient at its
    // native 4-cell resolution.
    let effects = vec![
        Some(solid_effect(gpu, &tree, "/z/booth", [0.5, 0.5, 0.5])),
        Some(gradient_effect(gpu, &tree, "/z/ceiling")),
    ];
    let mut content = ZoneCompositor::new(gpu, &patch, &index, effects);
    let px = render_and_gather(gpu, &patch, &index, &mut content, &lit_zones(2));

    // Booth (0..4): flat 0.5 everywhere, nothing bled in from the ceiling.
    for (p, pix) in px[0..4].iter().enumerate() {
        assert!(
            approx(*pix, 0.5),
            "booth pixel {p} = solid 0.5, got {pix:?}"
        );
    }
    // Ceiling (4..8): gradient at native res 4 → 0, 1/3, 2/3, 1.
    let expect = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];
    for (i, pix) in px[4..8].iter().enumerate() {
        assert!(
            approx(*pix, expect[i]),
            "ceiling pixel {} = gradient {}, got {pix:?}",
            i + 4,
            expect[i]
        );
    }
    // Independence: the two zones' patterns are distinguishable (booth flat, ceiling
    // varying), so neither overwrote the other.
    assert!(
        (px[7][0] - px[0][0]).abs() > 0.3,
        "the two zones show different content"
    );
}

/// The proof that `mapping_size` actually drives the render resolution (not the
/// world matrix): a gradient rendered at mapping_size 2 and placed into a 4-cell
/// footprint must upscale to [0, 0, 1, 1] by nearest — NOT the [0, 1/3, 2/3, 1] a
/// world-resolution render would produce. This is what distinguishes M8's per-zone
/// render from M5's single-resolution render.
#[test]
fn a_zone_renders_at_its_own_mapping_size_and_upscales_nearest() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();

    let patch = build_patch(4, &[("zone", &[0, 1, 2, 3], 2, BlendMode::Normal)]);
    let index = patch.compile().expect("rig compiles");

    let effects = vec![Some(gradient_effect(gpu, &tree, "/z/only"))];
    let mut content = ZoneCompositor::new(gpu, &patch, &index, effects);
    let px = render_and_gather(gpu, &patch, &index, &mut content, &lit_zones(1));

    // gradient@2 = [0, 1]; placed nearest into 4 cells = [0, 0, 1, 1]. A
    // world-resolution (4) render would instead be [0, 1/3, 2/3, 1].
    assert!(approx(px[0], 0.0), "cell 0, got {:?}", px[0]);
    assert!(
        approx(px[1], 0.0),
        "cell 1 (still source texel 0), got {:?}",
        px[1]
    );
    assert!(
        approx(px[2], 1.0),
        "cell 2 (source texel 1), got {:?}",
        px[2]
    );
    assert!(approx(px[3], 1.0), "cell 3, got {:?}", px[3]);
}

/// A zone that is not contributing (dimmer down) must put nothing on the wire, and
/// must not stop the other zone — the compositor filters on `contributes_pixels`.
#[test]
fn a_dark_zone_contributes_nothing_and_does_not_block_others() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();

    let patch = build_patch(
        8,
        &[
            ("a", &[0, 1, 2, 3], 4, BlendMode::Normal),
            ("b", &[4, 5, 6, 7], 4, BlendMode::Normal),
        ],
    );
    let index = patch.compile().expect("rig compiles");
    let effects = vec![
        Some(solid_effect(gpu, &tree, "/z/a", [1.0, 1.0, 1.0])),
        Some(solid_effect(gpu, &tree, "/z/b", [1.0, 1.0, 1.0])),
    ];
    let mut content = ZoneCompositor::new(gpu, &patch, &index, effects);

    // Zone a lit, zone b dark.
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 0.0);
    let px = render_and_gather(gpu, &patch, &index, &mut content, &zones);

    for (p, pix) in px[0..4].iter().enumerate() {
        assert!(approx(*pix, 1.0), "lit zone a pixel {p}, got {pix:?}");
    }
    for (i, pix) in px[4..8].iter().enumerate() {
        assert!(
            approx(*pix, 0.0),
            "dark zone b pixel {} is black, got {pix:?}",
            i + 4
        );
    }
}

/// The command queue (M8-PARTA §3): a queued effect swap applies **whole** at the
/// next frame boundary — never a half-and-half frame. Two zones are swapped in one
/// batch to also prove the drain applies *all* pending commands before rendering
/// (not one-per-frame): both flip in the SAME frame, and no pixel is left at the
/// old value.
#[test]
fn a_queued_effect_swap_applies_whole_at_the_next_frame() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let patch = build_patch(
        8,
        &[
            ("a", &[0, 1, 2, 3], 4, BlendMode::Normal),
            ("b", &[4, 5, 6, 7], 4, BlendMode::Normal),
        ],
    );
    let index = patch.compile().expect("rig compiles");

    // Both zones start on a flat 0.25 solid.
    let effects = vec![
        Some(solid_effect(gpu, &tree, "/q/a0", [0.25, 0.25, 0.25])),
        Some(solid_effect(gpu, &tree, "/q/b0", [0.25, 0.25, 0.25])),
    ];
    let mut content = ZoneCompositor::new(gpu, &patch, &index, effects);
    let sender = content.command_sender();
    let zones = lit_zones(2);

    // Frame N: the OLD effect, whole — every pixel 0.25.
    let frame_n = render_and_gather(gpu, &patch, &index, &mut content, &zones);
    for (p, pix) in frame_n.iter().enumerate() {
        assert!(
            approx(*pix, 0.25),
            "frame N pixel {p} = old effect, got {pix:?}"
        );
    }

    // Queue a swap for BOTH zones. In the app the UI thread builds the
    // EffectInstance (a pipeline compile) off the render thread and sends it; here
    // the test thread stands in for that producer.
    assert!(sender.set_zone_effect(
        0,
        Some(solid_effect(gpu, &tree, "/q/a1", [0.75, 0.75, 0.75]))
    ));
    assert!(sender.set_zone_effect(1, Some(solid_effect(gpu, &tree, "/q/b1", [0.9, 0.9, 0.9]))));

    // Frame N+1: the swap applied at the boundary. BOTH zones show their NEW effect,
    // wholly — no pixel remains at 0.25 (a half-and-half frame), and both zones
    // flipped in this one frame (the drain applied all queued commands first).
    let frame_np1 = render_and_gather(gpu, &patch, &index, &mut content, &zones);
    for (p, pix) in frame_np1[0..4].iter().enumerate() {
        assert!(
            approx(*pix, 0.75),
            "frame N+1 zone a pixel {p} = new effect, got {pix:?}"
        );
    }
    for (i, pix) in frame_np1[4..8].iter().enumerate() {
        assert!(
            approx(*pix, 0.9),
            "frame N+1 zone b pixel {} = new effect, got {pix:?}",
            i + 4
        );
    }
}

/// The producer must be `Send` so the UI/transport threads can drive content
/// changes — the whole point of a cross-thread frame-boundary queue.
#[test]
fn the_command_sender_is_send() {
    fn is_send<T: Send>() {}
    is_send::<ZoneCommandSender>();
}

/// An EMPTY patch (zero zones, zero fixtures) must compile and render without panicking:
/// the editor lets the operator delete down to zero zones and build up from scratch, so
/// the whole pipeline has to survive an empty rig (the world auto-fits to a 1×1 canvas,
/// buffers are size-guarded). If this ever panics, the editor's "remove last zone" is
/// unsafe and the guard belongs back.
#[test]
fn an_empty_patch_compiles_and_renders() {
    let gpu = software_gpu();
    let mut patch = Patch {
        id: Uuid::from_u128(0xE0),
        name: "empty".into(),
        matrix_size: [1, 1, 1],
        fixtures: vec![],
        zones: vec![],
    };
    patch.fit_matrix_size();
    assert_eq!(patch.matrix_size, [1, 1, 1]);
    let index = patch.compile().expect("empty patch compiles");
    assert_eq!(index.zone_count(), 0);
    assert_eq!(index.pixel_count(), 0);

    let mut comp = ZoneCompositor::new(&gpu, &patch, &index, vec![]);
    let zones = ZoneStates::new(0);
    let out = render_and_gather(&gpu, &patch, &index, &mut comp, &zones);
    assert!(out.is_empty(), "an empty rig gathers to no pixels");
}

/// Bipolar velocity: a negative zone speed runs the zone's effect in REVERSE — the exact
/// inverse of forward. Advancing the phase forward one step and then, at negative speed,
/// back the same step returns the zone to the frame it started on; a positive-only clock
/// (the old absolute-time scaling) could never do this. Uses NOISE, whose field is a
/// hash of the (space, time) lattice — animated by time like the trig effects but free of
/// GPU-`sin` large-argument inaccuracy, so the check is exact.
#[test]
fn negative_zone_speed_runs_the_effect_in_reverse() {
    let gpu = software_gpu();
    let patch = build_patch(8, &[("z", &[0, 1, 2, 3, 4, 5, 6, 7], 8, BlendMode::Normal)]);
    let index = patch.compile().expect("rig compiles");

    let tree = ParameterTree::with_defaults();
    let noise = EffectInstance::new(&gpu, &tree, "/z/n", &effects::noise());
    // Effect speed real 1.0 (norm 0.25 over 0..4) so time drives the field.
    tree.write_path("/z/n/param/speed", 0.25, ui());
    let mut zones = lit_zones(1);
    let mut content = ZoneCompositor::new(&gpu, &patch, &index, vec![Some(noise)]);

    let dt_ns = 80_000_000u64; // 0.08 s per step

    // Frame A (seed): zero delta, phase = bias.
    zones.set_speed_multiplier(0, 4.0);
    let a = render_and_gather_at(
        &gpu,
        &patch,
        &index,
        &mut content,
        &zones,
        &playing_clock(0),
    );

    // Frame B: forward one step at +4 → phase advances, the field moves.
    let b = render_and_gather_at(
        &gpu,
        &patch,
        &index,
        &mut content,
        &zones,
        &playing_clock(dt_ns),
    );
    assert!(
        a.iter().zip(&b).any(|(x, y)| (x[0] - y[0]).abs() > 0.008),
        "forward playback must move the field"
    );

    // Frame C: reverse the same step at -4 → phase returns to the bias, so the render
    // returns to frame A. Only a signed (bipolar) phase can undo the advance like this.
    zones.set_speed_multiplier(0, -4.0);
    let c = render_and_gather_at(
        &gpu,
        &patch,
        &index,
        &mut content,
        &zones,
        &playing_clock(2 * dt_ns),
    );
    for i in 0..8 {
        assert!(
            (a[i][0] - c[i][0]).abs() <= EPS,
            "pixel {i}: reverse must return to the start ({} vs {})",
            a[i][0],
            c[i][0]
        );
    }
}

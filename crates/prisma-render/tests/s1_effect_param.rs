//! S1 acceptance: the effect ↔ Parameter Tree seam.
//!
//! Per Bauti's requirement, these tests prove the **whole path**, not just that a
//! shader draws: a value is written into the Parameter Tree *by path* → resolved
//! to the handle the effect holds → picked up by the uniform packing → and the
//! rendered pixel changes accordingly. This is the join M3 (params) and M5
//! (render) meet at, and the net that every future effect relies on.
//!
//! Runs on **lavapipe** (deterministic bit-for-bit) so it gates in CI. The shader
//! does f32 arithmetic (unlike S0's constant passthrough), so pixels are checked
//! with a small tolerance — still far tighter than needed to tell 0.0 from 0.25
//! from 0.5, which is what "the tree value reached the pixel" requires.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_render::effect::EffectInstance;
use prisma_render::effects;
use prisma_render::readback::{create_render_target, read_rgba16f};

mod common;
use common::software_gpu;

/// Clock input for a static effect: only `resolution`/params matter here; time is
/// exercised by animated effects in later steps.
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

/// Tolerance covering shader-side f32 rounding + f16 storage. ~1/256, i.e. finer
/// than 8-bit output resolution, yet coarse enough to be robust across adapters.
const EPS: f32 = 1.0 / 256.0;

fn red(px: [f16; 4]) -> f32 {
    px[0].to_f32()
}

#[track_caller]
fn assert_gray(px: [f16; 4], expected: f32) {
    let (r, g, b) = (px[0].to_f32(), px[1].to_f32(), px[2].to_f32());
    assert!(
        (r - expected).abs() < EPS && (g - expected).abs() < EPS && (b - expected).abs() < EPS,
        "expected gray {expected}, got ({r}, {g}, {b})"
    );
}

/// Build a software (lavapipe) GPU; the deterministic correctness target.
#[test]
fn tree_write_flows_through_uniform_to_pixel() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut effect = EffectInstance::new(gpu, &tree, "/test/gradient", &effects::gradient());

    // 5 columns → normalized positions t = 0, .25, .5, .75, 1 (all exact in f16).
    let (w, h) = (5u32, 1u32);
    let target = create_render_target(gpu, w, h);
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let clock = still_clock();
    let src = SourceId::new(priority::UI, 1);

    // --- 1. Defaults (start=0.0, end=1.0): a full 0→1 ramp across the row. ---
    effect.render(gpu, &clock, &view, w, h);
    let px = read_rgba16f(gpu, &target, w, h);
    assert_gray(px[0], 0.0);
    assert_gray(px[1], 0.25);
    assert_gray(px[2], 0.5);
    assert_gray(px[3], 0.75);
    assert_gray(px[4], 1.0);

    // --- 2. Write `start` via the TREE (by path). The effect never re-resolves;
    //         it reads the handle it already holds. The left edge must move. ---
    let out = tree
        .write_path("/test/gradient/param/start", 0.5, src)
        .expect("param path must exist (registered by the effect)");
    assert_eq!(out, prisma_core::WriteOutcome::Accepted);

    effect.render(gpu, &clock, &view, w, h);
    let px = read_rgba16f(gpu, &target, w, h);
    assert_gray(px[0], 0.5); // left edge followed `start`
    assert_gray(px[2], 0.75); // mix(0.5, 1.0, 0.5)
    assert_gray(px[4], 1.0); // right edge unchanged (`end` still default)

    // --- 3. Distinctive flat value on BOTH params: proves it is really the tree
    //         values reaching the shader, not a fixed gradient. start==end==0.375
    //         (exact in f16) → every column identical. ---
    tree.write_path("/test/gradient/param/start", 0.375, src);
    tree.write_path("/test/gradient/param/end", 0.375, src);
    effect.render(gpu, &clock, &view, w, h);
    let px = read_rgba16f(gpu, &target, w, h);
    for (i, p) in px.iter().enumerate() {
        assert!(
            (red(*p) - 0.375).abs() < EPS,
            "column {i} should be the flat tree value 0.375, got {}",
            red(*p)
        );
    }
}

#[test]
fn defaults_come_from_the_manifest_descriptor() {
    // The effect is rendered with NO writes at all: the pixels must reflect the
    // manifest defaults (start=0.0, end=1.0) that registration seeded into the
    // tree — i.e. the descriptor's `default_norm` is the initial uniform value.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut effect = EffectInstance::new(gpu, &tree, "/test/g2", &effects::gradient());

    let (w, h) = (3u32, 1u32);
    let target = create_render_target(gpu, w, h);
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    effect.render(gpu, &still_clock(), &view, w, h);
    let px = read_rgba16f(gpu, &target, w, h);
    assert_gray(px[0], 0.0); // start default
    assert_gray(px[1], 0.5); // midpoint
    assert_gray(px[2], 1.0); // end default
}

#[test]
fn effect_registers_its_params_into_the_tree() {
    // The manifest — not the UI — populates the tree (§3.1 principle 2, §5.3): the
    // paths exist after construction and are walkable for MIDI-learn/UI.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let _effect = EffectInstance::new(gpu, &tree, "/test/g3", &effects::gradient());

    let nodes = tree.walk("/test/g3");
    let paths: Vec<&str> = nodes.iter().map(|n| &*n.path).collect();
    assert!(paths.contains(&"/test/g3/param/start"), "paths: {paths:?}");
    assert!(paths.contains(&"/test/g3/param/end"), "paths: {paths:?}");
}

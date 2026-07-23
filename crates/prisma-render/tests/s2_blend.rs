//! S2 acceptance: layer blend modes + opacity, in linear light.
//!
//! # How the golden references are derived (read this before trusting the test)
//!
//! Each expected value is computed **from the mathematical definition of the blend
//! mode**, in linear space, in plain `f32` — NOT by running the shader and copying
//! what it produced. If the shader implemented a mode wrongly, this reference would
//! still be right and the test would fail. The formulas (a = layer below, b = layer
//! on top), matching the doc block in `shaders/blend.wgsl`:
//!
//! ```text
//! Normal   = b
//! Add      = clamp(a + b, 0, 1)
//! Multiply = a * b
//! Screen   = 1 - (1 - a)(1 - b)
//! Mask     = a * luma(b),  luma = 0.2126 r + 0.7152 g + 0.0722 b   (Rec.709 linear)
//! Max      = max(a, b)
//! opacity: out = a + (blended - a) * opacity      // = mix(a, blended, opacity)
//! ```
//!
//! The probe colors are deliberately **intermediate, per-channel-distinct, and
//! chosen so a + b < 1** — no blend against black or against 1.0, where several
//! modes collapse to the same value. `distinct_modes_are_actually_distinct` asserts
//! that at these colors Add, Multiply and Screen are pairwise far apart, so a mode
//! test cannot pass by the modes happening to coincide.
//!
//! Runs on lavapipe (deterministic). The shader does f32 math and stores f16, so
//! pixels are checked within EPS — far tighter than the ≥0.1 gaps between modes.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_patch::BlendMode;
use prisma_render::blend::{Compositor, Layer};
use prisma_render::effect::EffectInstance;
use prisma_render::effects;
use prisma_render::readback::{create_render_target, read_rgba16f};
mod common;

use common::software_gpu;
use prisma_render::Gpu;

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

// ---- reference blend math (from the definitions, independent of the shader) ----

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn blended_ref(a: [f32; 3], b: [f32; 3], mode: BlendMode) -> [f32; 3] {
    let f = |i: usize| -> f32 {
        match mode {
            BlendMode::Normal => b[i],
            BlendMode::Add => (a[i] + b[i]).clamp(0.0, 1.0),
            BlendMode::Multiply => a[i] * b[i],
            BlendMode::Screen => 1.0 - (1.0 - a[i]) * (1.0 - b[i]),
            BlendMode::Mask => a[i] * luma(b),
            BlendMode::Max => a[i].max(b[i]),
        }
    };
    [f(0), f(1), f(2)]
}

/// out = mix(a, blended, opacity), per the opacity convention.
fn with_opacity(a: [f32; 3], blended: [f32; 3], opacity: f32) -> [f32; 3] {
    [
        a[0] + (blended[0] - a[0]) * opacity,
        a[1] + (blended[1] - a[1]) * opacity,
        a[2] + (blended[2] - a[2]) * opacity,
    ]
}

fn linf(x: [f32; 3], y: [f32; 3]) -> f32 {
    (0..3).map(|i| (x[i] - y[i]).abs()).fold(0.0, f32::max)
}

// ---- GPU helpers ----

/// Render a flat linear color into a fresh target via the `solid` effect. Uses the
/// real effect→tree→uniform path (S1), so the compositor consumes exactly what the
/// engine would feed it.
fn render_solid(gpu: &Gpu, tree: &ParameterTree, prefix: &str, color: [f32; 3]) -> wgpu::Texture {
    let mut inst = EffectInstance::new(gpu, tree, prefix, &effects::solid());
    let src = SourceId::new(priority::UI, 1);
    tree.write_path(&format!("{prefix}/param/r"), color[0], src);
    tree.write_path(&format!("{prefix}/param/g"), color[1], src);
    tree.write_path(&format!("{prefix}/param/b"), color[2], src);
    let tex = create_render_target(gpu, 2, 2);
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    inst.render(gpu, &still_clock(), &view, 2, 2);
    tex
}

#[track_caller]
fn assert_uniform(px: &[[f16; 4]], expected: [f32; 3], ctx: &str) {
    for (i, p) in px.iter().enumerate() {
        let got = [p[0].to_f32(), p[1].to_f32(), p[2].to_f32()];
        assert!(
            linf(got, expected) < EPS,
            "{ctx}: texel {i} = {got:?}, expected {expected:?} (Δ {})",
            linf(got, expected)
        );
    }
}

/// Blend `b`-on-`a` with `mode`/`opacity` through the real stack: a black base,
/// then `a` as a Normal bottom layer (recovering `a` exactly), then `b` on top.
fn blend_on_gpu(
    gpu: &Gpu,
    tree: &ParameterTree,
    comp: &Compositor,
    a: [f32; 3],
    b: [f32; 3],
    mode: BlendMode,
    opacity: f32,
) -> Vec<[f16; 4]> {
    let tex_a = render_solid(gpu, tree, "/s2/a", a);
    let tex_b = render_solid(gpu, tree, "/s2/b", b);
    let out = comp.composite_over_black(
        gpu,
        &[
            Layer {
                texture: &tex_a,
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
            Layer {
                texture: &tex_b,
                mode,
                opacity,
            },
        ],
        2,
        2,
    );
    read_rgba16f(gpu, &out, 2, 2)
}

const A: [f32; 3] = [0.5, 0.4, 0.2];
const B: [f32; 3] = [0.3, 0.5, 0.1];

#[test]
fn distinct_modes_are_actually_distinct() {
    // Guards the whole suite: at (A, B) the three "arithmetic" modes must be far
    // apart, so no mode test can pass by two modes coinciding (Bauti's concern).
    let add = blended_ref(A, B, BlendMode::Add); // (0.8, 0.9, 0.3)
    let mul = blended_ref(A, B, BlendMode::Multiply); // (0.15, 0.20, 0.02)
    let scr = blended_ref(A, B, BlendMode::Screen); // (0.65, 0.70, 0.28)
    assert!(linf(add, mul) > 0.1, "Add vs Multiply too close");
    assert!(linf(add, scr) > 0.1, "Add vs Screen too close");
    assert!(linf(mul, scr) > 0.1, "Multiply vs Screen too close");
}

#[test]
fn every_blend_mode_matches_its_linear_definition() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let comp = Compositor::new(gpu);

    for mode in [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Mask,
        BlendMode::Max,
    ] {
        let px = blend_on_gpu(gpu, &tree, &comp, A, B, mode, 1.0);
        let want = blended_ref(A, B, mode);
        assert_uniform(&px, want, &format!("{mode:?}"));
    }
}

#[test]
fn opacity_lerps_between_accumulator_and_blend() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let comp = Compositor::new(gpu);

    // Screen at 0.5 opacity: out = mix(A, Screen(A,B), 0.5).
    let full = blended_ref(A, B, BlendMode::Screen);
    let want = with_opacity(A, full, 0.5);
    let px = blend_on_gpu(gpu, &tree, &comp, A, B, BlendMode::Screen, 0.5);
    assert_uniform(&px, want, "Screen @ opacity 0.5");

    // And it must land strictly between A and the full-opacity blend — proof the
    // opacity actually mixes rather than being ignored either way.
    assert!(linf(want, A) > EPS, "opacity 0.5 must differ from A");
    assert!(
        linf(want, full) > EPS,
        "opacity 0.5 must differ from full blend"
    );
}

#[test]
fn stack_folds_bottom_to_top_and_order_matters() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let comp = Compositor::new(gpu);

    let c0 = [0.2, 0.3, 0.1];
    let c1 = [0.3, 0.2, 0.4];
    let c2 = [0.5, 0.6, 0.7];
    let t0 = render_solid(gpu, &tree, "/s2/l0", c0);
    let t1 = render_solid(gpu, &tree, "/s2/l1", c1);
    let t2 = render_solid(gpu, &tree, "/s2/l2", c2);

    // Forward stack over black: Normal(c0) → Add(c1) → Multiply(c2).
    // black → c0 → clamp(c0+c1) → that * c2, computed here from the definitions.
    let after_add = blended_ref(c0, c1, BlendMode::Add); // (0.5, 0.5, 0.5)
    let reference = blended_ref(after_add, c2, BlendMode::Multiply); // (0.25, 0.30, 0.35)

    let forward = comp.composite_over_black(
        gpu,
        &[
            Layer {
                texture: &t0,
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
            Layer {
                texture: &t1,
                mode: BlendMode::Add,
                opacity: 1.0,
            },
            Layer {
                texture: &t2,
                mode: BlendMode::Multiply,
                opacity: 1.0,
            },
        ],
        2,
        2,
    );
    assert_uniform(
        &read_rgba16f(gpu, &forward, 2, 2),
        reference,
        "forward fold",
    );

    // Reversed order composites differently (blending is not commutative). We only
    // need it to differ from `reference`, proving order is respected.
    let reversed = comp.composite_over_black(
        gpu,
        &[
            Layer {
                texture: &t2,
                mode: BlendMode::Multiply,
                opacity: 1.0,
            },
            Layer {
                texture: &t1,
                mode: BlendMode::Add,
                opacity: 1.0,
            },
            Layer {
                texture: &t0,
                mode: BlendMode::Normal,
                opacity: 1.0,
            },
        ],
        2,
        2,
    );
    let rev = read_rgba16f(gpu, &reversed, 2, 2);
    let rev0 = [rev[0][0].to_f32(), rev[0][1].to_f32(), rev[0][2].to_f32()];
    assert!(
        linf(rev0, reference) > 0.1,
        "reversed stack should differ from forward; got {rev0:?} vs {reference:?}"
    );
}

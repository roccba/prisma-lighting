//! The built-in effect library (CLAUDE.md §5.3) — closing M5's "only two of the
//! agreed three-or-four effects" gap.
//!
//! `solid` and `gradient` are covered by S1/S2; this checks the two animated
//! additions, **plasma** and **wave**, and that the four are actually distinct
//! patterns rather than four ways of drawing the same thing (which is what would
//! make a preview useless for judging them).
//!
//! Runs on lavapipe, deterministic.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_render::effect::EffectInstance;
use prisma_render::effects;
use prisma_render::readback::{create_render_target, read_rgba16f};
mod common;

use common::software_gpu;
use prisma_render::Gpu;

const WIDTH: u32 = 5;
const EPS: f32 = 1.0 / 128.0;

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

fn clock_at(seconds: f64) -> ClockSnapshot {
    ClockSnapshot {
        frame: 0,
        wall_ns: 0,
        show_ns: (seconds * 1e9) as u64,
        beat: 0.0,
        bpm: 120.0,
        playing: true,
    }
}

/// Render one effect across `WIDTH` cells and return the red channel per cell.
fn render(gpu: &Gpu, effect: &mut EffectInstance, clock: &ClockSnapshot) -> Vec<f32> {
    let tex = create_render_target(gpu, WIDTH, 1);
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    effect.render(gpu, clock, &view, WIDTH, 1);
    read_rgba16f(gpu, &tex, WIDTH, 1)
        .iter()
        .map(|p: &[f16; 4]| p[0].to_f32())
        .collect()
}

#[test]
fn wave_is_a_sine_across_the_matrix() {
    // Defaults: frequency 1.0 cycle across the width, speed 0. At show time 0 the
    // five cells sit at x = 0, .25, .5, .75, 1 of one full cycle, so the values
    // are exactly the sine's quarter points — derived from the definition, not
    // read off the shader.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut wave = EffectInstance::new(gpu, &tree, "/fx/wave", &effects::wave());

    let v = render(gpu, &mut wave, &clock_at(0.0));
    let expected = [0.5, 1.0, 0.5, 0.0, 0.5];
    for (i, want) in expected.iter().enumerate() {
        assert!(
            (v[i] - want).abs() < EPS,
            "cell {i} = {}, expected {want} (0.5 + 0.5*sin(2pi*x))",
            v[i]
        );
    }
}

#[test]
fn wave_animates_with_show_time() {
    // Speed 1 cycle/s: half a second later the sine is inverted.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut wave = EffectInstance::new(gpu, &tree, "/fx/wave2", &effects::wave());
    tree.write_real(&tree.resolve("/fx/wave2/param/speed").unwrap(), 1.0, ui());

    let at0 = render(gpu, &mut wave, &clock_at(0.0));
    let at_half = render(gpu, &mut wave, &clock_at(0.5));
    // sin(theta + pi) = -sin(theta), so 0.5 + 0.5*sin flips about 0.5.
    for i in 0..WIDTH as usize {
        assert!(
            ((at0[i] - 0.5) + (at_half[i] - 0.5)).abs() < EPS,
            "cell {i}: {} and {} should be mirrored about 0.5",
            at0[i],
            at_half[i]
        );
    }
    assert!(
        at0.iter().zip(&at_half).any(|(a, b)| (a - b).abs() > 0.1),
        "a paused-looking wave means show time never reached the shader"
    );
}

#[test]
fn plasma_is_a_varying_field_driven_by_its_parameters() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut plasma = EffectInstance::new(gpu, &tree, "/fx/plasma", &effects::plasma());

    let v = render(gpu, &mut plasma, &clock_at(0.0));
    assert!(
        v.iter().any(|x| (x - v[0]).abs() > 0.02),
        "plasma must vary across the matrix, got {v:?}"
    );
    assert!(
        v.iter().all(|x| (0.0..=1.0).contains(x)),
        "stays in range: {v:?}"
    );

    // `scale` is a real parameter, not decoration: changing it changes the field.
    let scale = tree.resolve("/fx/plasma/param/scale").unwrap();
    tree.write_real(&scale, 18.0, ui());
    let wide = render(gpu, &mut plasma, &clock_at(0.0));
    assert!(
        v.iter().zip(&wide).any(|(a, b)| (a - b).abs() > 0.05),
        "scale must change the field: {v:?} vs {wide:?}"
    );
}

#[test]
fn plasma_animates_with_show_time() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut plasma = EffectInstance::new(gpu, &tree, "/fx/plasma2", &effects::plasma());
    let at0 = render(gpu, &mut plasma, &clock_at(0.0));
    let later = render(gpu, &mut plasma, &clock_at(1.0));
    assert!(
        at0.iter().zip(&later).any(|(a, b)| (a - b).abs() > 0.05),
        "plasma must evolve over show time"
    );
}

#[test]
fn the_four_core_effects_are_distinguishable_patterns() {
    // The milestone asks for three or four core effects. Four that look alike
    // would not let anyone judge a preview, so assert they actually differ.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();

    let mut solid = EffectInstance::new(gpu, &tree, "/fx/s", &effects::solid());
    for c in ["r", "g", "b"] {
        tree.write_path(&format!("/fx/s/param/{c}"), 0.5, ui());
    }
    let mut gradient = EffectInstance::new(gpu, &tree, "/fx/g", &effects::gradient());
    let mut plasma = EffectInstance::new(gpu, &tree, "/fx/p", &effects::plasma());
    let mut wave = EffectInstance::new(gpu, &tree, "/fx/w", &effects::wave());

    let clock = clock_at(0.0);
    let patterns = [
        ("solid", render(gpu, &mut solid, &clock)),
        ("gradient", render(gpu, &mut gradient, &clock)),
        ("plasma", render(gpu, &mut plasma, &clock)),
        ("wave", render(gpu, &mut wave, &clock)),
    ];

    // Solid is the only flat one; the other three vary.
    assert!(
        patterns[0]
            .1
            .iter()
            .all(|x| (x - patterns[0].1[0]).abs() < EPS),
        "solid must be flat"
    );
    for (name, p) in &patterns[1..] {
        assert!(
            p.iter().any(|x| (x - p[0]).abs() > 0.05),
            "{name} must vary across the matrix"
        );
    }

    // And no two are the same pattern.
    for i in 0..patterns.len() {
        for j in (i + 1)..patterns.len() {
            let (a, b) = (&patterns[i], &patterns[j]);
            assert!(
                a.1.iter().zip(&b.1).any(|(x, y)| (x - y).abs() > 0.05),
                "{} and {} render the same thing: {:?} vs {:?}",
                a.0,
                b.0,
                a.1,
                b.1
            );
        }
    }
}

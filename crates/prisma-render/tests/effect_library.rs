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

/// Render an effect across a small 2D target and return the red channel per cell,
/// row-major. A 2D target is needed for the effects whose pattern is 2D (radial,
/// fire, sparkle) — a single row would miss their vertical/radial structure.
fn render_2d(
    gpu: &Gpu,
    effect: &mut EffectInstance,
    clock: &ClockSnapshot,
    w: u32,
    h: u32,
) -> Vec<[f32; 4]> {
    let tex = create_render_target(gpu, w, h);
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    effect.render(gpu, clock, &view, w, h);
    read_rgba16f(gpu, &tex, w, h)
        .iter()
        .map(|p: &[f16; 4]| [p[0].to_f32(), p[1].to_f32(), p[2].to_f32(), p[3].to_f32()])
        .collect()
}

#[test]
fn noise_is_a_varying_animated_field() {
    // Value-noise fBm: varies across the matrix, evolves over show time, and stays
    // in range. These are the properties a preview needs; the exact values are the
    // hash's business.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut noise = EffectInstance::new(gpu, &tree, "/fx/noise", &effects::noise());
    tree.write_path("/fx/noise/param/speed", 1.0, ui());

    let at0 = render_2d(gpu, &mut noise, &clock_at(0.0), 8, 8);
    let reds: Vec<f32> = at0.iter().map(|p| p[0]).collect();
    assert!(
        reds.iter().any(|x| (x - reds[0]).abs() > 0.02),
        "noise must vary across the matrix, got {reds:?}"
    );
    assert!(
        reds.iter().all(|x| (0.0..=1.0).contains(x)),
        "stays in range: {reds:?}"
    );
    let later = render_2d(gpu, &mut noise, &clock_at(1.0), 8, 8);
    assert!(
        at0.iter()
            .zip(&later)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.02),
        "noise must evolve over show time"
    );
}

#[test]
fn fire_emits_colour_hotter_at_the_base() {
    // Fire is a colour effect: the bottom rows carry more energy than the top, and
    // its ramp is warm (red channel dominates green dominates blue overall).
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut fire = EffectInstance::new(gpu, &tree, "/fx/fire", &effects::fire());
    tree.write_path("/fx/fire/param/speed", 1.0, ui());

    let h = 16u32;
    let w = 8u32;
    // Average red over several show times to smooth out the animated flicker before
    // comparing top vs bottom (a single instant can leave a cold patch at the base).
    let mut top = 0.0f32;
    let mut bottom = 0.0f32;
    for step in 0..8 {
        let frame = render_2d(gpu, &mut fire, &clock_at(step as f64 * 0.2), w, h);
        for x in 0..w as usize {
            top += frame[x][0];
            bottom += frame[(h as usize - 1) * w as usize + x][0];
        }
    }
    assert!(
        bottom > top,
        "fire must be hotter at the base: bottom red {bottom} vs top {top}"
    );
    // Warm ramp: across the whole field red carries more than blue.
    let frame = render_2d(gpu, &mut fire, &clock_at(1.3), w, h);
    let sum_r: f32 = frame.iter().map(|p| p[0]).sum();
    let sum_b: f32 = frame.iter().map(|p| p[2]).sum();
    assert!(
        sum_r > sum_b,
        "fire ramp is warm: red {sum_r} vs blue {sum_b}"
    );
}

#[test]
fn radial_is_symmetric_about_the_centre() {
    // Concentric rings: cells equidistant from the centre share a value. Check the
    // four corners of an odd-sized square agree (all at the same radius).
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut radial = EffectInstance::new(gpu, &tree, "/fx/radial", &effects::radial());
    tree.write_path("/fx/radial/param/frequency", 3.0, ui());
    tree.write_path("/fx/radial/param/speed", 0.0, ui());

    let n = 9usize;
    let f = render_2d(gpu, &mut radial, &clock_at(0.0), n as u32, n as u32);
    let at = |x: usize, y: usize| f[y * n + x][0];
    let tl = at(0, 0);
    let tr = at(n - 1, 0);
    let bl = at(0, n - 1);
    let br = at(n - 1, n - 1);
    for (name, v) in [("tr", tr), ("bl", bl), ("br", br)] {
        assert!(
            (v - tl).abs() < EPS,
            "corner {name} ({v}) should equal tl ({tl}) at equal radius"
        );
    }
    // And it actually rings: the centre differs from the corners.
    assert!(
        (at(n / 2, n / 2) - tl).abs() > 0.05,
        "radial must vary from centre to edge"
    );
}

#[test]
fn sparkle_is_sparse_and_animates() {
    // Only a fraction of cells light at any instant, and the field changes over time
    // (twinkle). At default density most of a small field is dark.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut sparkle = EffectInstance::new(gpu, &tree, "/fx/sparkle", &effects::sparkle());
    tree.write_path("/fx/sparkle/param/speed", 2.0, ui());

    let (w, h) = (16u32, 16u32);
    let at0 = render_2d(gpu, &mut sparkle, &clock_at(0.0), w, h);
    let lit = at0.iter().filter(|p| p[0] > 0.5).count();
    let total = at0.len();
    assert!(
        lit < total / 2,
        "sparkle must be sparse: {lit}/{total} cells lit"
    );
    let later = render_2d(gpu, &mut sparkle, &clock_at(0.5), w, h);
    assert!(
        at0.iter()
            .zip(&later)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1),
        "sparkle must twinkle over show time"
    );
}

#[test]
fn color_scroll_bands_come_from_the_palette() {
    // With a two-colour palette (red, blue), a static wide scroll paints red at the
    // band start and blue at its middle — the colours the operator chose, not a fixed
    // ramp. Proves the palette actually feeds the shader.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs", &effects::color_scroll());
    // write_real: these are REAL values (count 2, width 1), not normalized.
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("speed", 0.0);
    set("width", 1.0);
    set("offset", 0.0);
    set("direction", 0.0); // Right
    set("count", 2.0);
    set("c0r", 1.0);
    set("c0g", 0.0);
    set("c0b", 0.0); // red
    set("c1r", 0.0);
    set("c1g", 0.0);
    set("c1b", 1.0); // blue

    let f = render_2d(gpu, &mut cs, &clock_at(0.0), 8, 1);
    // Left edge (phase ~0) is the first palette colour: red-dominant.
    assert!(
        f[0][0] > f[0][2],
        "band start should be red-dominant, got {:?}",
        f[0]
    );
    // A cell near the middle (phase ~0.5) reaches the second colour: blue-dominant.
    assert!(
        f.iter().any(|p| p[2] > p[0]),
        "the palette's blue must appear somewhere along the band: {f:?}"
    );
}

#[test]
fn color_scroll_scrolls_over_show_time_and_turns_with_direction() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs2", &effects::color_scroll());
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs2/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("width", 1.0);
    set("offset", 0.0);
    // Real 1.0 cycle/s: half a second is half a cycle (a visible move, not an integer
    // shift that would look unchanged).
    set("speed", 1.0);

    // Right: the pattern varies along x and is constant down each column.
    set("direction", 0.0);
    let right = render_2d(gpu, &mut cs, &clock_at(0.0), 6, 6);
    let at = |v: &[[f32; 4]], x: usize, y: usize| v[y * 6 + x][0];
    for x in 0..6 {
        assert!(
            (at(&right, x, 0) - at(&right, x, 5)).abs() < EPS,
            "Right scroll: column {x} should be constant down y"
        );
    }
    assert!(
        (0..6).any(|x| (at(&right, x, 0) - at(&right, 0, 0)).abs() > 0.02),
        "Right scroll must vary across x"
    );

    // Scrolls over show time.
    let later = render_2d(gpu, &mut cs, &clock_at(0.5), 6, 6);
    assert!(
        right
            .iter()
            .zip(&later)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.02),
        "colour scroll must move over show time"
    );

    // Down (rose index 2 → norm 2/7): now constant across each row, varying down y.
    set("direction", 2.0 / 7.0);
    let down = render_2d(gpu, &mut cs, &clock_at(0.0), 6, 6);
    for y in 0..6 {
        assert!(
            (at(&down, 0, y) - at(&down, 5, y)).abs() < EPS,
            "Down scroll: row {y} should be constant across x"
        );
    }
}

#[test]
fn simple_shape_draws_a_centred_shape_and_the_enum_switches_it() {
    // A white circle at the default centre lights the middle and leaves the corners
    // dark; selecting a different shape through the enum index changes the field.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut shape = EffectInstance::new(gpu, &tree, "/fx/shape", &effects::simple_shape());
    // Sharpen the edge so the small test grid classifies cleanly.
    tree.write_path("/fx/shape/param/border", 0.02, ui());

    let n = 9usize;
    let circle = render_2d(gpu, &mut shape, &clock_at(0.0), n as u32, n as u32);
    let at = |v: &[[f32; 4]], x: usize, y: usize| v[y * n + x][0];
    assert!(
        at(&circle, n / 2, n / 2) > 0.5,
        "circle must light the centre, got {}",
        at(&circle, n / 2, n / 2)
    );
    for (name, x, y) in [("tl", 0, 0), ("tr", n - 1, 0), ("br", n - 1, n - 1)] {
        assert!(
            at(&circle, x, y) < 0.5,
            "circle must leave corner {name} dark, got {}",
            at(&circle, x, y)
        );
    }

    // The enum selects the SDF by index: the star (index 5) differs from the circle.
    let shape_h = tree.resolve("/fx/shape/param/shape").unwrap();
    tree.write_real(&shape_h, 5.0, ui());
    let star = render_2d(gpu, &mut shape, &clock_at(0.0), n as u32, n as u32);
    assert!(
        circle
            .iter()
            .zip(&star)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1),
        "changing the shape enum must change the rendered field"
    );
}

#[test]
fn metaballs_form_a_field_that_moves_and_responds_to_count() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut mb = EffectInstance::new(gpu, &tree, "/fx/mb", &effects::metaballs());
    tree.write_path("/fx/mb/param/speed", 1.5, ui());

    let (w, h) = (12u32, 12u32);
    let at0 = render_2d(gpu, &mut mb, &clock_at(0.0), w, h);
    let reds: Vec<f32> = at0.iter().map(|p| p[0]).collect();
    assert!(
        reds.iter().any(|x| *x > 0.5),
        "metaballs must light some cells, got max {}",
        reds.iter().cloned().fold(0.0f32, f32::max)
    );
    assert!(
        reds.iter().any(|x| (x - reds[0]).abs() > 0.05),
        "metaballs must vary across the matrix"
    );
    assert!(
        reds.iter().all(|x| (0.0..=1.0).contains(x)),
        "stays in range: {reds:?}"
    );
    // The field evolves over show time.
    let later = render_2d(gpu, &mut mb, &clock_at(0.7), w, h);
    assert!(
        at0.iter()
            .zip(&later)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.05),
        "metaballs must move over show time"
    );
    // `count` is a real parameter: fewer blobs changes the field.
    let count = tree.resolve("/fx/mb/param/count").unwrap();
    tree.write_real(&count, 1.0, ui());
    let one = render_2d(gpu, &mut mb, &clock_at(0.0), w, h);
    assert!(
        at0.iter()
            .zip(&one)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.05),
        "changing the blob count must change the field"
    );
}

#[test]
fn drops_fall_and_animate() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut drops = EffectInstance::new(gpu, &tree, "/fx/drops", &effects::drops());
    tree.write_path("/fx/drops/param/speed", 1.5, ui());

    let (w, h) = (12u32, 12u32);
    // Sample several instants: with moving heads, at least one instant must light cells
    // and the field must change between instants.
    let a = render_2d(gpu, &mut drops, &clock_at(0.10), w, h);
    let b = render_2d(gpu, &mut drops, &clock_at(0.37), w, h);
    let lit_any = a.iter().chain(&b).any(|p| p[0] > 0.3);
    assert!(lit_any, "drops must light some cells across the fall");
    assert!(
        a.iter().zip(&b).any(|(x, y)| (x[0] - y[0]).abs() > 0.1),
        "drops must animate over show time"
    );
    assert!(
        a.iter().all(|p| (0.0..=1.0).contains(&p[0])),
        "stays in range"
    );
}

#[test]
fn explosions_burst_and_animate() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut ex = EffectInstance::new(gpu, &tree, "/fx/ex", &effects::explosions());
    // One burst per second, plenty of particles, so a burst is clearly visible.
    tree.write_path("/fx/ex/param/rate", 1.0, ui());
    tree.write_path("/fx/ex/param/particles", 40.0, ui());
    tree.write_path("/fx/ex/param/size", 0.7, ui());

    let (w, h) = (16u32, 16u32);
    // Sample across one burst's life: early (just fired) it lights near its centre,
    // and the field changes as particles expand and fall.
    let early = render_2d(gpu, &mut ex, &clock_at(0.05), w, h);
    let mid = render_2d(gpu, &mut ex, &clock_at(0.45), w, h);
    let lit = early.iter().chain(&mid).any(|p| p[0] > 0.3);
    assert!(lit, "an explosion must light some cells");
    assert!(
        early
            .iter()
            .zip(&mid)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1),
        "explosion particles must move over show time"
    );
    assert!(
        early.iter().all(|p| (0.0..=1.0).contains(&p[0])),
        "stays in range"
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

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

/// Index of a "Scroll de color" movement mode by its label (`"Group|Label"`).
fn scroll_mode(label: &str) -> f32 {
    effects::SCROLL_MODES
        .iter()
        .position(|m| m.ends_with(label))
        .unwrap_or_else(|| panic!("no scroll mode {label}")) as f32
}

#[test]
fn color_scroll_bands_come_from_the_palette_with_hard_cuts() {
    // Two colours (red, blue), each band 50 % of an 8-px row, standing still: the left
    // half is pure red and the right half pure blue — the operator's colours, and with
    // "Atenuar" OFF (the default) a hard cut, no blend in between.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs", &effects::color_scroll());
    // write_real: these are REAL values, not normalized.
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("bpm", 0.0);
    set("color_width", 50.0);
    set("mode", scroll_mode("|Derecha"));
    cs.set_palette(gpu, &[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]); // red, blue

    let f = render_2d(gpu, &mut cs, &clock_at(0.0), 8, 1);
    let reds = f.iter().filter(|p| p[0] > 0.99 && p[2] < 0.01).count();
    let blues = f.iter().filter(|p| p[2] > 0.99 && p[0] < 0.01).count();
    assert_eq!((reds, blues), (4, 4), "hard cut red|blue, got {f:?}");

    // "Atenuar" ON blends: some cell is a mix of both.
    set("fade", 1.0);
    let g = render_2d(gpu, &mut cs, &clock_at(0.0), 8, 1);
    assert!(
        g.iter().any(|p| p[0] > 0.05 && p[2] > 0.05),
        "with Atenuar the bands blend: {g:?}"
    );
}

#[test]
fn color_scroll_palette_is_a_list_of_up_to_255_colours() {
    // 255 distinct greys, each band 1 % of a 255-px row: the row shows many different
    // colours — the table is not capped at the old 8 slots.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs4", &effects::color_scroll());
    assert!(cs.has_palette());
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs4/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("bpm", 0.0);
    set("color_width", 1.0 / 2.55); // ~1 px per colour at 255 px
    set("mode", scroll_mode("|Derecha"));
    let colours: Vec<[f32; 3]> = (0..prisma_render::PALETTE_MAX)
        .map(|i| {
            let v = i as f32 / 254.0;
            [v, v, v]
        })
        .collect();
    cs.set_palette(gpu, &colours);
    let f = render_2d(gpu, &mut cs, &clock_at(0.0), 255, 1);
    let mut distinct: Vec<u32> = f.iter().map(|p| (p[0] * 1000.0) as u32).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 100,
        "only {} distinct colours",
        distinct.len()
    );
}

#[test]
fn color_scroll_moves_with_bpm_and_turns_with_the_mode() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs2", &effects::color_scroll());
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs2/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("color_width", 20.0);
    set("bpm", 60.0); // one step (≥ 1 px) per second

    // Derecha: the pattern varies along x and is constant down each column.
    set("mode", scroll_mode("|Derecha"));
    let right = render_2d(gpu, &mut cs, &clock_at(0.0), 6, 6);
    let at = |v: &[[f32; 4]], x: usize, y: usize| v[y * 6 + x];
    let diff = |a: [f32; 4], b: [f32; 4]| (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max);
    for x in 0..6 {
        assert!(
            diff(at(&right, x, 0), at(&right, x, 5)) < EPS,
            "Derecha: column {x} should be constant down y"
        );
    }
    assert!(
        (0..6).any(|x| diff(at(&right, x, 0), at(&right, 0, 0)) > 0.02),
        "Derecha must vary across x"
    );

    // It moves over show time (two steps later the bands have shifted).
    let later = render_2d(gpu, &mut cs, &clock_at(2.0), 6, 6);
    assert!(
        right.iter().zip(&later).any(|(a, b)| diff(*a, *b) > 0.02),
        "the scroll must move over show time"
    );

    // Inferior: now constant across each row, varying down y.
    set("mode", scroll_mode("|Inferior"));
    let down = render_2d(gpu, &mut cs, &clock_at(0.0), 6, 6);
    for y in 0..6 {
        assert!(
            diff(at(&down, 0, y), at(&down, 5, y)) < EPS,
            "Inferior: row {y} should be constant across x"
        );
    }

    // Frontal (pure 3-D depth): no X/Y extent — the whole matrix is one colour.
    set("mode", scroll_mode("|Frontal"));
    let front = render_2d(gpu, &mut cs, &clock_at(0.3), 6, 6);
    assert!(
        front.iter().all(|p| diff(*p, front[0]) < EPS),
        "Frontal on a flat rig is uniform"
    );
}

#[test]
fn color_scroll_shapes_are_symmetric_about_the_centre() {
    // "Explosionar círculo" on a square matrix: the colour depends only on the distance
    // from the centre, so mirrored cells match.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cs = EffectInstance::new(gpu, &tree, "/fx/cs3", &effects::color_scroll());
    let set = |p: &str, v: f32| {
        let h = tree.resolve(&format!("/fx/cs3/param/{p}")).unwrap();
        tree.write_real(&h, v, ui());
    };
    set("bpm", 0.0);
    set("color_width", 15.0);
    set("mode", scroll_mode("|Explosionar círculo"));
    let n = 8usize;
    let f = render_2d(gpu, &mut cs, &clock_at(0.0), n as u32, n as u32);
    let at = |x: usize, y: usize| f[y * n + x];
    for y in 0..n {
        for x in 0..n {
            let m = at(n - 1 - x, n - 1 - y);
            let c = at(x, y);
            assert!(
                (0..3).all(|i| (c[i] - m[i]).abs() < EPS),
                "circle not symmetric at ({x},{y})"
            );
        }
    }
    // And it is not uniform: centre and corner differ.
    let (c, k) = (at(n / 2, n / 2), at(0, 0));
    assert!(
        (0..3).any(|i| (c[i] - k[i]).abs() > 0.02),
        "circle must have rings"
    );
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
fn pulse_follows_the_bpm_envelope_and_strobes_without_fade() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut pulse = EffectInstance::new(gpu, &tree, "/fx/pulse", &effects::pulse());
    let set = |path: &str, v: f32| {
        let h = tree.resolve(path).unwrap();
        tree.write_real(&h, v, ui());
    };
    // 60 BPM = one pulse per second; default ratio 50 (symmetric), fade on, white.
    set("/fx/pulse/param/speed", 60.0);
    let lum = |p: &mut EffectInstance, t: f64| render_2d(gpu, p, &clock_at(t), 4, 4)[5][0];

    assert!(lum(&mut pulse, 0.0) < EPS, "a pulse starts dark");
    assert!(
        (lum(&mut pulse, 0.5) - 1.0).abs() < EPS,
        "peaks mid-cycle at ratio 50"
    );
    assert!(
        (lum(&mut pulse, 0.25) - 0.5).abs() < 0.02,
        "smooth rise halfway at a quarter cycle"
    );
    assert!(
        (lum(&mut pulse, 0.75) - 0.5).abs() < 0.02,
        "symmetric fall at ratio 50"
    );

    // Ratio 0 = rise only: still climbing late in the cycle.
    set("/fx/pulse/param/ratio", 0.0);
    assert!(
        lum(&mut pulse, 0.9) > 0.9,
        "ratio 0 rises across the whole cycle"
    );

    // Fade off = hard strobe: only fully on or fully off.
    set("/fx/pulse/param/ratio", 50.0);
    set("/fx/pulse/param/fade", 0.0);
    for (t, on) in [(0.1, false), (0.3, true), (0.5, true), (0.9, false)] {
        let v = lum(&mut pulse, t);
        let want = if on { 1.0 } else { 0.0 };
        assert!(
            (v - want).abs() < EPS,
            "strobe at {t}s must be {want}, got {v}"
        );
    }

    // Each pulse takes the next colour of the table: red, then blue.
    pulse.set_palette(gpu, &[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
    let px = |p: &mut EffectInstance, t: f64| render_2d(gpu, p, &clock_at(t), 4, 4)[5];
    let first = px(&mut pulse, 0.5);
    let second = px(&mut pulse, 1.5);
    assert!(
        first[0] > 0.9 && first[2] < EPS,
        "pulse 1 is red: {first:?}"
    );
    assert!(
        second[2] > 0.9 && second[0] < EPS,
        "pulse 2 is blue: {second:?}"
    );
}

#[test]
fn color_change_cycles_through_four_colours_globally() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut cc = EffectInstance::new(gpu, &tree, "/fx/cc", &effects::color_change());
    let speed = tree.resolve("/fx/cc/param/speed").unwrap();
    tree.write_real(&speed, 60.0, ui());
    // 60 BPM = one colour per second. Factory colours: red, yellow, green, blue.
    let rgb = |cc: &mut EffectInstance, t: f64| {
        let px = render_2d(gpu, cc, &clock_at(t), 4, 4);
        assert!(
            px.iter().all(|p| (p[0] - px[0][0]).abs() < EPS
                && (p[1] - px[0][1]).abs() < EPS
                && (p[2] - px[0][2]).abs() < EPS),
            "colour change is global: every pixel the same"
        );
        [px[0][0], px[0][1], px[0][2]]
    };
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02);

    assert!(
        close(rgb(&mut cc, 0.0), [1.0, 0.0, 0.0]),
        "starts on colour 1"
    );
    assert!(close(rgb(&mut cc, 0.5), [1.0, 0.5, 0.0]), "blends 1→2");
    assert!(
        close(rgb(&mut cc, 1.0), [1.0, 1.0, 0.0]),
        "colour 2 after a beat"
    );
    assert!(close(rgb(&mut cc, 2.0), [0.0, 1.0, 0.0]), "colour 3");
    assert!(close(rgb(&mut cc, 3.0), [0.0, 0.0, 1.0]), "colour 4");
    assert!(close(rgb(&mut cc, 3.5), [0.5, 0.0, 0.5]), "wraps 4→1");
    assert!(
        close(rgb(&mut cc, 4.0), [1.0, 0.0, 0.0]),
        "back to colour 1"
    );

    // Ping-pong: 1 2 3 4 3 2 1 … — after colour 4 it heads back to colour 3.
    let mode = tree.resolve("/fx/cc/param/color_mode").unwrap();
    tree.write_real(&mode, 1.0, ui());
    assert!(
        close(rgb(&mut cc, 3.0), [0.0, 0.0, 1.0]),
        "ping-pong reaches colour 4"
    );
    assert!(
        close(rgb(&mut cc, 3.5), [0.0, 0.5, 0.5]),
        "ping-pong turns 4→3"
    );
    assert!(
        close(rgb(&mut cc, 6.0), [1.0, 0.0, 0.0]),
        "ping-pong back to colour 1"
    );

    // Aleatorio: every beat lands on a table colour, the order is not the sequence,
    // and the choice is stable (stateless hash of the beat).
    tree.write_real(&mode, 2.0, ui());
    let table = effects::COLOR_CHANGE_PALETTE;
    let picks: Vec<usize> = (0..16)
        .map(|b| {
            let c = rgb(&mut cc, b as f64);
            table
                .iter()
                .position(|t| close(c, *t))
                .unwrap_or_else(|| panic!("beat {b} must land on a table colour, got {c:?}"))
        })
        .collect();
    assert!(
        picks.iter().enumerate().any(|(b, i)| *i != b % 4),
        "random order differs from the sequence: {picks:?}"
    );
    assert!(
        close(rgb(&mut cc, 5.0), table[picks[5]]),
        "random pick is stable"
    );

    // The table is a list: two colours alternate.
    cc.set_palette(gpu, &[[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    tree.write_real(&mode, 0.0, ui());
    assert!(
        close(rgb(&mut cc, 0.0), [0.0, 1.0, 0.0]),
        "custom table colour 1"
    );
    assert!(
        close(rgb(&mut cc, 1.0), [0.0, 0.0, 1.0]),
        "custom table colour 2"
    );
    assert!(
        close(rgb(&mut cc, 2.0), [0.0, 1.0, 0.0]),
        "custom table wraps"
    );
}

#[test]
fn bounce_draws_objects_that_move_and_respond_to_count() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let mut bounce = EffectInstance::new(gpu, &tree, "/fx/bounce", &effects::bounce());

    let (w, h) = (16u32, 16u32);
    let a = render_2d(gpu, &mut bounce, &clock_at(0.1), w, h);
    let lit = a.iter().filter(|p| p[0] > 0.5).count();
    assert!(lit > 0, "bounce must light some cells");
    assert!(
        lit < (w * h) as usize / 2,
        "small objects leave most of the canvas dark"
    );
    assert!(
        a.iter().all(|p| (0.0..=1.0).contains(&p[0])),
        "stays in range"
    );
    let b = render_2d(gpu, &mut bounce, &clock_at(0.3), w, h);
    assert!(
        a.iter().zip(&b).any(|(x, y)| (x[0] - y[0]).abs() > 0.3),
        "objects must move over show time"
    );
    let count = tree.resolve("/fx/bounce/param/count").unwrap();
    tree.write_real(&count, 10.0, ui());
    let many = render_2d(gpu, &mut bounce, &clock_at(0.1), w, h);
    assert!(
        many.iter().filter(|p| p[0] > 0.5).count() > lit,
        "more objects light more cells"
    );
    // Each axis mode is a different field.
    let dir = tree.resolve("/fx/bounce/param/direction").unwrap();
    tree.write_real(&dir, 0.0, ui());
    let horiz = render_2d(gpu, &mut bounce, &clock_at(0.1), w, h);
    tree.write_real(&dir, 1.0, ui());
    let vert = render_2d(gpu, &mut bounce, &clock_at(0.1), w, h);
    assert!(
        horiz
            .iter()
            .zip(&vert)
            .any(|(x, y)| (x[0] - y[0]).abs() > 0.3),
        "horizontal and vertical bounce differ"
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

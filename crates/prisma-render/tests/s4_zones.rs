//! S4 acceptance: zone→master composition, the incremental overlap counter, and
//! the frame-boundary snapshot (CLAUDE.md §5.2 step 4, §5.2.1).
//!
//! This is the step that proves the product thesis: different effects running
//! **simultaneously and independently** on separate zones, overlap resolved by
//! `z_order` + per-zone `blend_mode` so an `Add`/`Max` zone *brightens* rather than
//! occludes, and each pixel's zone dimmer applied in exactly one stage.
//!
//! The counter tests are headless (no GPU): its correctness is an ordering and
//! bookkeeping property, not a visual one.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::ClockSnapshot;
use prisma_core::ParameterTree;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_render::blend::{Compositor, ZoneComposite};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::{zone_cell_coverage, OverlapCounter};
use prisma_render::readback::create_render_target;
use prisma_render::sample::{SamplePlan, Sampler};
mod common;

use common::software_gpu;
use prisma_render::{create_cell_mask, effects, Gpu};
use uuid::Uuid;

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

/// Build a single-fixture RGB patch: one pixel per entry of `xs` (all on row 0),
/// plus zones given as `(name, member pixel indices, z_order, blend_mode)`.
fn build_patch(width: u32, xs: &[f32], zones: &[(&str, Vec<u32>, i32, BlendMode)]) -> Patch {
    let fixture_id = Uuid::from_u128(0xF1);
    Patch {
        id: Uuid::from_u128(0x54),
        name: "s4".into(),
        matrix_size: [width, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: xs
                .iter()
                .enumerate()
                .map(|(i, &x)| Pixel {
                    index: i as u32,
                    position: glam::Vec3::new(x, 0.0, 0.0),
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
            .map(|(i, (name, members, z, mode))| Zone {
                id: Uuid::from_u128(0x100 + i as u128),
                name: (*name).into(),
                members: members
                    .iter()
                    .map(|&p| PixelRef {
                        fixture: fixture_id,
                        pixel: p,
                    })
                    .collect(),
                bounds: None,
                z_order: *z,
                blend_mode: *mode,
                mapping_size: [width, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            })
            .collect(),
    }
}

/// Render a flat colour across a matrix-sized texture.
fn solid_texture(
    gpu: &Gpu,
    tree: &ParameterTree,
    prefix: &str,
    rgb: [f32; 3],
    w: u32,
) -> wgpu::Texture {
    let mut inst = EffectInstance::new(gpu, tree, prefix, &effects::solid());
    let src = SourceId::new(priority::UI, 1);
    tree.write_path(&format!("{prefix}/param/r"), rgb[0], src);
    tree.write_path(&format!("{prefix}/param/g"), rgb[1], src);
    tree.write_path(&format!("{prefix}/param/b"), rgb[2], src);
    let tex = create_render_target(gpu, w, 1);
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    inst.render(gpu, &still_clock(), &view, w, 1);
    tex
}

/// Render the horizontal gradient across a matrix-sized texture.
fn gradient_texture(gpu: &Gpu, tree: &ParameterTree, prefix: &str, w: u32) -> wgpu::Texture {
    let mut inst = EffectInstance::new(gpu, tree, prefix, &effects::gradient());
    let tex = create_render_target(gpu, w, 1);
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    inst.render(gpu, &still_clock(), &view, w, 1);
    tex
}

/// Composite the given zones and gather to the master buffer, returning the red
/// component of each global pixel (the test content is greyscale).
#[allow(clippy::too_many_arguments)]
fn composite_and_gather(
    gpu: &Gpu,
    patch: &Patch,
    index: &PatchIndex,
    zones: &[ZoneComposite<'_>],
    cell_mask: &wgpu::Buffer,
) -> Vec<f32> {
    let comp = Compositor::new(gpu);
    let w = patch.matrix_size[0];
    let framebuffer = comp.composite_zones(gpu, zones, cell_mask, w, 1);
    let plan = SamplePlan::compile(patch, index);
    let sampler = Sampler::new(gpu, &plan);
    sampler.gather(gpu, &framebuffer);
    let master: Vec<f16> = sampler.read_master_f16(gpu);
    (0..index.pixel_count())
        .map(|g| master[index.pixel_component_offset(g) as usize].to_f32())
        .collect()
}

// =========================================================================
// 1. The counter — headless
// =========================================================================

#[test]
fn counter_increments_and_decrements_on_contribution_flips() {
    // Two zones over 3 pixels: A={0,1}, B={1,2}. Pixel 1 is the shared one.
    let patch = build_patch(
        3,
        &[0.0, 1.0, 2.0],
        &[
            ("a", vec![0, 1], 0, BlendMode::Normal),
            ("b", vec![1, 2], 0, BlendMode::Normal),
        ],
    );
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);

    // Nothing contributes yet.
    assert_eq!(counter.sync(&zones), 0);
    for p in 0..3 {
        assert_eq!(counter.count(p), 0);
    }

    // A up: its two pixels gain one cover each.
    zones.set_dimmer(0, 1.0);
    assert_eq!(counter.sync(&zones), 1, "one zone flipped");
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [1, 1, 0]
    );
    assert!(!counter.is_overlapped(1), "one zone is not an overlap");

    // B up: pixel 1 is now multiply covered.
    zones.set_dimmer(1, 1.0);
    assert_eq!(counter.sync(&zones), 1);
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [1, 2, 1]
    );
    assert!(counter.is_overlapped(1));

    // B down again: the counter returns exactly where it was — no drift.
    zones.set_dimmer(1, 0.0);
    assert_eq!(counter.sync(&zones), 1);
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [1, 1, 0]
    );
    assert!(!counter.is_overlapped(1));
}

#[test]
fn zone_sum_recovers_the_sole_covering_zone_after_a_two_to_one_decrement() {
    // The case a plain `sole_zone` array cannot maintain: when the count drops
    // from 2 to 1, which zone remains? The sum knows without any search.
    let patch = build_patch(
        1,
        &[0.0],
        &[
            ("a", vec![0], 0, BlendMode::Normal),
            ("b", vec![0], 0, BlendMode::Normal),
        ],
    );
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);

    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    assert_eq!(snap.count(0), 2);
    assert_eq!(snap.sole_zone(0), None, "two zones: no sole zone");

    // Drop zone 0 → only zone 1 remains, and the sum reports exactly slot 1.
    zones.set_dimmer(0, 0.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    assert_eq!(snap.count(0), 1);
    assert_eq!(snap.sole_zone(0), Some(1));

    // And the other way round: only zone 0 remains → slot 0.
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 0.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    assert_eq!(snap.sole_zone(0), Some(0));
}

#[test]
fn solo_flips_every_non_soloed_zone_in_one_pass() {
    // The documented worst case. Pressing solo on one zone must decrement every
    // other contributing zone at once — a single linear pass, done immediately,
    // never amortized across frames.
    let patch = build_patch(
        3,
        &[0.0, 1.0, 2.0],
        &[
            ("a", vec![0], 0, BlendMode::Normal),
            ("b", vec![1], 0, BlendMode::Normal),
            ("c", vec![2], 0, BlendMode::Normal),
        ],
    );
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(3);
    for slot in 0..3 {
        zones.set_dimmer(slot, 1.0);
    }
    counter.sync(&zones);
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [1, 1, 1]
    );

    // Solo zone 1: zones 0 and 2 stop contributing in the same sync.
    assert!(zones.set_solo(1, true), "0 -> 1 crosses zero");
    assert_eq!(
        counter.sync(&zones),
        2,
        "both non-soloed zones flipped at once"
    );
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [0, 1, 0]
    );

    // A second solo does NOT cross zero: only the pressed zone changes.
    assert!(!zones.set_solo(2, true), "1 -> 2 does not cross zero");
    assert_eq!(counter.sync(&zones), 1);
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [0, 1, 1]
    );

    // Releasing all solos restores every zone.
    zones.set_solo(1, false);
    assert!(zones.set_solo(2, false), "1 -> 0 crosses back");
    counter.sync(&zones);
    assert_eq!(
        [counter.count(0), counter.count(1), counter.count(2)],
        [1, 1, 1]
    );
}

#[test]
fn a_zone_silenced_by_anothers_solo_stops_contributing_but_stays_active() {
    // The reason the counter keys on contributes_pixels() and NOT is_active():
    // the console must keep showing the muted zone's state.
    let patch = build_patch(
        2,
        &[0.0, 1.0],
        &[
            ("a", vec![0], 0, BlendMode::Normal),
            ("b", vec![1], 0, BlendMode::Normal),
        ],
    );
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);

    zones.set_solo(1, true);
    counter.sync(&zones);
    assert_eq!(counter.count(0), 0, "zone a was decremented");
    assert!(!zones.contributes_pixels(0));
    assert!(zones.is_active(0), "but it keeps its state for the UI");
}

#[test]
fn the_snapshot_is_reused_untouched_when_nothing_flips() {
    let patch = build_patch(1, &[0.0], &[("a", vec![0], 0, BlendMode::Normal)]);
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(1);
    zones.set_dimmer(0, 1.0);
    counter.sync(&zones);

    let first = counter.snapshot();
    counter.sync(&zones); // no flip
    let second = counter.snapshot();
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "an unchanged frame must hand back the same snapshot, not a copy"
    );

    // A flip must produce different content. `first` is still held here, so the
    // counter cannot recycle it in place — proving in-flight snapshots are safe.
    zones.set_dimmer(0, 0.0);
    counter.sync(&zones);
    let third = counter.snapshot();
    assert_eq!(
        first.count(0),
        1,
        "the held snapshot was not mutated underneath"
    );
    assert_eq!(third.count(0), 0);
}

// =========================================================================
// 2. The mixed cell — the case the cell promotion exists for
// =========================================================================

/// Two pixels sharing one cell: pixel 0 covered by zones A and B, pixel 1 by A
/// only. Cell 1 is therefore overlapped, so render dims it — and NEITHER pixel
/// may be dimmed again at the output stage.
fn mixed_cell_patch() -> Patch {
    // Both pixels at x = 1.0 → the same cell in a 2-wide matrix.
    build_patch(
        2,
        &[1.0, 1.0],
        &[
            ("a", vec![0, 1], 0, BlendMode::Normal),
            ("b", vec![0], 5, BlendMode::Add),
        ],
    )
}

#[test]
fn a_mixed_cell_denies_the_output_shortcut_to_both_its_pixels() {
    let patch = mixed_cell_patch();
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 0.5);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();

    assert_eq!(snap.count(0), 2, "pixel 0 is covered by both zones");
    assert_eq!(snap.count(1), 1, "pixel 1 is covered by zone A only");
    assert!(snap.cell_overlapped(1), "their shared cell is overlapped");

    // Pixel 0: overlapped, obviously no output-stage scaling.
    for k in 0..3 {
        let c = index.pixel_component_offset(0) as usize + k;
        assert_eq!(snap.zone_at_output(c), None);
    }
    // Pixel 1: count == 1, yet ALSO denied — render already dimmed its cell.
    // If someone "simplifies" the promotion away, this is what breaks: pixel 1
    // would be dimmed a second time at output.
    for k in 0..3 {
        let c = index.pixel_component_offset(1) as usize + k;
        assert_eq!(
            snap.zone_at_output(c),
            None,
            "component {c} shares an overlapped cell — dimming it at output would double-dim it"
        );
    }
}

#[test]
fn neither_pixel_of_a_mixed_cell_is_dimmed_twice() {
    // The value-level half of the same case. Zone A renders white at dimmer 0.5;
    // zone B contributes black (its dimmer is up, so it counts toward overlap, but
    // its content adds nothing). The composited cell must be 0.5 — NOT 0.25.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let patch = mixed_cell_patch();
    let index = patch.compile().unwrap();

    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 0.5);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    let cell_mask = create_cell_mask(gpu, snap.cell_mask_bytes());
    let coverage = zone_cell_coverage(&patch, &index);
    let cov_bufs: Vec<wgpu::Buffer> = coverage.iter().map(|m| create_cell_mask(gpu, m)).collect();

    let white = solid_texture(gpu, &tree, "/s4/a", [1.0, 1.0, 1.0], 2);
    let black = solid_texture(gpu, &tree, "/s4/b", [0.0, 0.0, 0.0], 2);

    let composites = vec![
        ZoneComposite {
            texture: &white,
            mode: BlendMode::Normal,
            opacity: 1.0,
            zone_scale: [0.5, 0.5, 0.5], // zone A dimmer
            coverage: &cov_bufs[0],
        },
        ZoneComposite {
            texture: &black,
            mode: BlendMode::Add,
            opacity: 1.0,
            zone_scale: [1.0, 1.0, 1.0],
            coverage: &cov_bufs[1],
        },
    ];
    let values = composite_and_gather(gpu, &patch, &index, &composites, &cell_mask);

    for (p, v) in values.iter().enumerate() {
        assert!(
            (v - 0.5).abs() < EPS,
            "pixel {p} = {v}, expected 0.5 — 0.25 would mean the zone dimmer was applied twice"
        );
    }
}

// =========================================================================
// 3. The thesis: independent zones, and Add/Max brightening
// =========================================================================

#[test]
fn two_zones_run_unmistakably_different_effects_independently() {
    // Five cells. Zone "left" (pixels 0,1) runs a flat solid; zone "right"
    // (pixels 3,4) runs a gradient. Pixel 2 belongs to no zone. The two effects
    // are different in PATTERN, not just value — flat versus varying — so this
    // cannot pass by the two zones happening to look alike.
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let patch = build_patch(
        5,
        &[0.0, 1.0, 2.0, 3.0, 4.0],
        &[
            ("left", vec![0, 1], 0, BlendMode::Normal),
            ("right", vec![3, 4], 0, BlendMode::Normal),
        ],
    );
    let index = patch.compile().unwrap();

    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    let cell_mask = create_cell_mask(gpu, snap.cell_mask_bytes());
    let coverage = zone_cell_coverage(&patch, &index);
    let cov_bufs: Vec<wgpu::Buffer> = coverage.iter().map(|m| create_cell_mask(gpu, m)).collect();

    let flat = solid_texture(gpu, &tree, "/s4/left", [0.25, 0.25, 0.25], 5);
    // Gradient defaults: 0 at the left edge, 1 at the right, over 5 cells → the
    // exact quarters 0, .25, .5, .75, 1.
    let ramp = gradient_texture(gpu, &tree, "/s4/right", 5);

    let composites = vec![
        ZoneComposite {
            texture: &flat,
            mode: BlendMode::Normal,
            opacity: 1.0,
            zone_scale: [1.0, 1.0, 1.0],
            coverage: &cov_bufs[0],
        },
        ZoneComposite {
            texture: &ramp,
            mode: BlendMode::Normal,
            opacity: 1.0,
            zone_scale: [1.0, 1.0, 1.0],
            coverage: &cov_bufs[1],
        },
    ];
    let v = composite_and_gather(gpu, &patch, &index, &composites, &cell_mask);

    // Left zone: flat, and untouched by the gradient.
    assert!((v[0] - 0.25).abs() < EPS, "pixel 0 = {}", v[0]);
    assert!((v[1] - 0.25).abs() < EPS, "pixel 1 = {}", v[1]);
    assert!((v[0] - v[1]).abs() < EPS, "the solid zone must be flat");
    // Unzoned pixel stays dark: no zone bled into it.
    assert!(v[2].abs() < EPS, "pixel 2 belongs to no zone, got {}", v[2]);
    // Right zone: the gradient's own values, not the solid's.
    assert!((v[3] - 0.75).abs() < EPS, "pixel 3 = {}", v[3]);
    assert!((v[4] - 1.0).abs() < EPS, "pixel 4 = {}", v[4]);
    assert!(
        (v[3] - v[4]).abs() > 0.1,
        "the gradient zone must vary — otherwise the patterns are not distinguishable"
    );
}

#[test]
fn an_add_zone_brightens_the_zone_below_instead_of_occluding_it() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    // Both zones cover both pixels; the strobe sits above by z_order.
    let patch = build_patch(
        2,
        &[0.0, 1.0],
        &[
            ("base", vec![0, 1], 0, BlendMode::Normal),
            ("strobe", vec![0, 1], 10, BlendMode::Add),
        ],
    );
    let index = patch.compile().unwrap();
    // The compositing order is the patch index's, not the declaration order.
    assert_eq!(index.composite_order(), &[0, 1], "strobe composites on top");

    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    assert!(snap.cell_overlapped(0) && snap.cell_overlapped(1));

    let cell_mask = create_cell_mask(gpu, snap.cell_mask_bytes());
    let coverage = zone_cell_coverage(&patch, &index);
    let cov_bufs: Vec<wgpu::Buffer> = coverage.iter().map(|m| create_cell_mask(gpu, m)).collect();

    let base = solid_texture(gpu, &tree, "/s4/base", [0.25, 0.25, 0.25], 2);
    let flash = solid_texture(gpu, &tree, "/s4/flash", [0.5, 0.5, 0.5], 2);

    let mk = |mode: BlendMode| {
        vec![
            ZoneComposite {
                texture: &base,
                mode: BlendMode::Normal,
                opacity: 1.0,
                zone_scale: [1.0, 1.0, 1.0],
                coverage: &cov_bufs[0],
            },
            ZoneComposite {
                texture: &flash,
                mode,
                opacity: 1.0,
                zone_scale: [1.0, 1.0, 1.0],
                coverage: &cov_bufs[1],
            },
        ]
    };

    // Add: 0.25 + 0.5 = 0.75 — brighter than either input, so the strobe lifted
    // the zone below rather than replacing it.
    let added = composite_and_gather(gpu, &patch, &index, &mk(BlendMode::Add), &cell_mask);
    for (p, v) in added.iter().enumerate() {
        assert!((v - 0.75).abs() < EPS, "pixel {p} = {v}, expected 0.75");
        assert!(*v > 0.25, "must brighten the base, not occlude it");
        assert!(*v > 0.5, "must exceed the strobe's own value too");
    }

    // Max: max(0.25, 0.5) = 0.5 — also non-occluding (it keeps the brighter of
    // the two), and distinguishable from Add.
    let maxed = composite_and_gather(gpu, &patch, &index, &mk(BlendMode::Max), &cell_mask);
    for (p, v) in maxed.iter().enumerate() {
        assert!((v - 0.5).abs() < EPS, "pixel {p} = {v}, expected 0.5");
    }

    // Contrast: with Normal the strobe WOULD occlude, landing on its own value.
    // Asserting this pins down that Add/Max are doing something different.
    let normal = composite_and_gather(gpu, &patch, &index, &mk(BlendMode::Normal), &cell_mask);
    for v in &normal {
        assert!((v - 0.5).abs() < EPS);
    }
    assert!(
        (added[0] - normal[0]).abs() > 0.1,
        "Add must differ from Normal, or the blend mode is being ignored"
    );
}

#[test]
fn an_inactive_zone_costs_the_rig_nothing() {
    // §5.2.1's "gain": while the global strobe is not contributing, no pixel is
    // overlapped, so the whole rig keeps the output-stage shortcut.
    let patch = build_patch(
        2,
        &[0.0, 1.0],
        &[
            ("base", vec![0, 1], 0, BlendMode::Normal),
            ("strobe", vec![0, 1], 10, BlendMode::Add),
        ],
    );
    let index = patch.compile().unwrap();
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0); // base up, strobe still down
    counter.sync(&zones);
    let snap = counter.snapshot();

    for p in 0..2 {
        assert_eq!(snap.count(p), 1);
        assert!(!snap.cell_overlapped(p));
        assert_eq!(snap.sole_zone(p), Some(0), "covered by the base zone alone");
        // Every component keeps the shortcut.
        for k in 0..3 {
            let c = index.pixel_component_offset(p as u32) as usize + k;
            assert!(snap.zone_at_output(c).is_some());
        }
    }

    // The strobe fires: now, and only now, the shortcut is surrendered.
    zones.set_dimmer(1, 1.0);
    counter.sync(&zones);
    let snap = counter.snapshot();
    for p in 0..2 {
        assert_eq!(snap.count(p), 2);
        assert!(snap.cell_overlapped(p));
        for k in 0..3 {
            let c = index.pixel_component_offset(p as u32) as usize + k;
            assert_eq!(snap.zone_at_output(c), None);
        }
    }
}

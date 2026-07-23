//! P0 acceptance: the preview plan and the shared output-stage scale
//! (CLAUDE.md M6, §5.2 step 6, §5.2.1).
//!
//! # Read this before changing an assertion: preview and wire are compared in
//! # LINEAR LIGHT, on purpose
//!
//! The preview and the wire are **not** byte-identical, and that is a design
//! decision, not a defect (M6 design point 2):
//!
//! - the **wire** applies the *per-fixture gamma* — LED calibration, a property of
//!   the physical fixture;
//! - the **preview** applies *sRGB* — the transfer function of the screen it is
//!   drawn on.
//!
//! Two different curves, for two different destinations. **Linear light — before
//! either curve — is the one place where they must agree**, and it is therefore the
//! only place these tests compare them. Each test states the expected linear value
//! independently and checks both consumers against it.
//!
//! A test that compared preview bytes with wire bytes *after* the curves would fail
//! for a patch with any calibration, and the tempting "fix" would be to apply the
//! fixture's gamma to the preview — which would make the preview show the LED's
//! correction curve instead of the light in the room, and would silently couple the
//! preview to a fixture property that has nothing to do with a monitor. Do not do
//! that. `the_preview_does_not_apply_the_fixture_gamma` below exists to catch it.
//!
//! What P0 proves: the preview reads the *canonical* master buffer (never the wire
//! order), applies the *same* flat-parameter stage as the output encoder via the
//! shared `scale_component`, and therefore reproduces the §5.2.1 shortcut/overlap
//! split exactly instead of showing it as an inconsistency.

use half::f16;
use prisma_core::OverlapSnapshot;
use prisma_output::packet::artnet::parse_artdmx;
use prisma_output::plan::{FlatParams, OutputPlan};
use prisma_output::router::{OutputFrame, OutputRouter};
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, PatchIndex, Pixel,
};
use prisma_preview::PreviewPlan;
use uuid::Uuid;

/// Two pixels on universe 1 at channels 1 and 4.
fn two_pixel_patch(gamma: f32, components: ComponentSet, order: ColorOrder) -> Patch {
    let per_pixel = components.count() as u16;
    Patch {
        id: Uuid::from_u128(0x60),
        name: "m6-p0".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: Uuid::from_u128(0xAA),
            name: "strip".into(),
            profile: FixtureProfile {
                name: "p".into(),
                components,
                gamma: GammaCalibration { gamma },
            },
            pixels: (0..2)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: i as u16 * per_pixel + 1,
                    color_order: order,
                })
                .collect(),
        }],
        zones: vec![],
    }
}

/// A snapshot where pixel 0 takes the output shortcut under zone `slot`, and pixel
/// 1 is overlapped (its zone dimmer was already applied in render, step 4).
fn split_snapshot(slot: u32, components_per_pixel: u8) -> OverlapSnapshot {
    let n = components_per_pixel as u32;
    let mut snap = OverlapSnapshot::zeroed(2, 2, (n * 2) as usize);
    {
        let (count, zone_sum) = snap.pixel_arrays_mut();
        count[0] = 1;
        zone_sum[0] = slot;
        count[1] = 2; // multiply covered
        zone_sum[1] = slot + 1;
    }
    snap.derive_from_pixels(
        &[0, 1],
        &[(0, components_per_pixel), (n, components_per_pixel)],
    );
    snap
}

/// Encode universe 0 and return its DMX channel data.
fn wire(
    index: &PatchIndex,
    plan: OutputPlan,
    master: &[f16],
    overlap: Option<&OverlapSnapshot>,
    flat: &FlatParams,
) -> Vec<u8> {
    let mut router = OutputRouter::from_plan(plan);
    let frame = OutputFrame {
        master,
        generation: index.generation(),
        overlap,
        flat,
    };
    let pkt = router.encode_universe_with(0, &frame).unwrap().to_vec();
    parse_artdmx(&pkt).unwrap().data.to_vec()
}

fn master_of(values: &[f32]) -> Vec<f16> {
    values.iter().map(|&v| f16::from_f32(v)).collect()
}

/// Identity-gamma wire code for a linear value — the quantizer stated once, so a
/// test can anchor the wire side to the same linear truth as the preview side.
fn wire_code(linear: f32) -> u8 {
    (linear.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[test]
fn preview_and_wire_agree_in_linear_light() {
    // Deliberately dyadic values throughout: exact in f16 and f32, so an
    // equality assertion here cannot pass or fail on rounding luck.
    let patch = two_pixel_patch(1.0, ComponentSet::Rgb, ColorOrder::Rgb);
    let index = patch.compile().unwrap();
    let preview = PreviewPlan::compile(&patch, &index);
    let snapshot = split_snapshot(0, 3);

    let master = master_of(&[1.0, 0.5, 0.25, 0.5, 0.25, 0.125]);
    let mut flat = FlatParams::identity(2);
    flat.set_master(0.5, [1.0, 1.0, 0.5]);
    flat.set_zone(0, 0.5, [1.0, 0.5, 1.0]);

    // The linear truth, computed here by hand, independently of both consumers.
    // Pixel 0 takes the shortcut: zone scale THEN master scale.
    let expected_p0 = [
        1.0 * (0.5 * 1.0) * (0.5 * 1.0),  // 0.25
        0.5 * (0.5 * 0.5) * (0.5 * 1.0),  // 0.0625
        0.25 * (0.5 * 1.0) * (0.5 * 0.5), // 0.03125
    ];
    // Pixel 1 is overlapped: its zone dimmer was applied in render, so only the
    // master scale applies here. Getting this wrong is the double-dim of §5.2.1.
    let expected_p1 = [
        0.5 * (0.5 * 1.0),   // 0.25
        0.25 * (0.5 * 1.0),  // 0.125
        0.125 * (0.5 * 0.5), // 0.03125
    ];

    let mut out = Vec::new();
    preview.sample_into(&master, Some(&snapshot), &flat, &mut out);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], expected_p0, "shortcut pixel, linear light");
    assert_eq!(out[1], expected_p1, "overlapped pixel, linear light");

    // The wire, anchored to the SAME linear values. This byte-level equality holds
    // only because this fixture's gamma is 1.0 — it is a consequence of the linear
    // agreement above, never the contract. See the module docs.
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let data = wire(&index, plan, &master, Some(&snapshot), &flat);
    for c in 0..3 {
        assert_eq!(data[c], wire_code(expected_p0[c]), "wire, pixel 0 ch {c}");
        assert_eq!(
            data[3 + c],
            wire_code(expected_p1[c]),
            "wire, pixel 1 ch {c}"
        );
    }
}

#[test]
fn the_preview_does_not_apply_the_fixture_gamma() {
    // gamma 2.0: the wire bends, the preview must not. If someone ever "fixes" the
    // preview by reusing the fixture LUT, this fails immediately.
    let patch = two_pixel_patch(2.0, ComponentSet::Rgb, ColorOrder::Rgb);
    let index = patch.compile().unwrap();
    let preview = PreviewPlan::compile(&patch, &index);

    let master = master_of(&[0.25, 0.25, 0.25, 0.25, 0.25, 0.25]);
    let flat = FlatParams::identity(2);

    let mut out = Vec::new();
    preview.sample_into(&master, None, &flat, &mut out);
    assert_eq!(
        out[0], [0.25; 3],
        "the preview stops at linear light; the screen's own curve comes later"
    );

    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let data = wire(&index, plan, &master, None, &flat);
    let bent = (255.0 * 0.25f32.powf(2.0)).round() as u8; // 16
    assert_eq!(data[0], bent, "the wire DID apply the fixture gamma");
    assert_ne!(
        bent,
        wire_code(0.25),
        "the two sides must actually diverge here, or this test proves nothing"
    );
}

#[test]
fn an_overlapped_pixel_ignores_the_zone_fader_in_the_preview_too() {
    // The same asymmetry the output stage has (S5's `flat_params` tests), now
    // required of the preview: one master buffer, the zone fader moved between two
    // samples. The shortcut pixel follows it; the overlapped one cannot, because
    // its zone dimmer is already baked into the buffer.
    let patch = two_pixel_patch(1.0, ComponentSet::Rgb, ColorOrder::Rgb);
    let index = patch.compile().unwrap();
    let preview = PreviewPlan::compile(&patch, &index);
    let snapshot = split_snapshot(0, 3);

    let master = master_of(&[1.0; 6]);
    let mut flat = FlatParams::identity(2);

    flat.set_zone(0, 1.0, [1.0; 3]);
    let mut full = Vec::new();
    preview.sample_into(&master, Some(&snapshot), &flat, &mut full);

    // Fader down. Nothing re-renders: the master buffer is the same slice.
    flat.set_zone(0, 0.25, [1.0; 3]);
    let mut dimmed = Vec::new();
    preview.sample_into(&master, Some(&snapshot), &flat, &mut dimmed);

    assert_eq!(full[0], [1.0; 3]);
    assert_eq!(
        dimmed[0], [0.25; 3],
        "shortcut pixel follows the live fader"
    );
    assert_eq!(
        full[1], dimmed[1],
        "overlapped pixel was dimmed in render; dimming it here would be twice"
    );
}

#[test]
fn colour_order_is_a_wire_fact_and_never_reaches_the_preview() {
    // The master buffer is canonical [R,G,B(,W)]; ColorOrder is a permutation
    // compiled into the wire plan. A screen has no colour order, so the same master
    // content must preview identically under any order — while the wire differs.
    let master = master_of(&[1.0, 0.5, 0.25, 0.0, 0.0, 0.0]);
    let flat = FlatParams::identity(0);

    let rgb = two_pixel_patch(1.0, ComponentSet::Rgb, ColorOrder::Rgb);
    let grb = two_pixel_patch(1.0, ComponentSet::Rgb, ColorOrder::Grb);
    let (i_rgb, i_grb) = (rgb.compile().unwrap(), grb.compile().unwrap());

    let mut a = Vec::new();
    let mut b = Vec::new();
    PreviewPlan::compile(&rgb, &i_rgb).sample_into(&master, None, &flat, &mut a);
    PreviewPlan::compile(&grb, &i_grb).sample_into(&master, None, &flat, &mut b);
    assert_eq!(a, b, "colour order must not change what the preview shows");
    assert_eq!(a[0], [1.0, 0.5, 0.25]);

    let wire_rgb = wire(
        &i_rgb,
        OutputPlan::compile(&rgb, &i_rgb).unwrap(),
        &master,
        None,
        &flat,
    );
    let wire_grb = wire(
        &i_grb,
        OutputPlan::compile(&grb, &i_grb).unwrap(),
        &master,
        None,
        &flat,
    );
    assert_ne!(
        wire_rgb[..3],
        wire_grb[..3],
        "the wire DOES permute, or this test proves nothing"
    );
}

#[test]
fn white_folds_into_rgb_carrying_the_dimmer_but_not_the_colour_filter() {
    // A white emitter puts white light in the room, so the preview adds W to all
    // three channels. It is scaled by the dimmer alone — a colour filter has no
    // meaning for a white emitter (§5.2 step 6) — which is exactly what W gets on
    // the wire. So preview and wire agree about white too.
    let patch = two_pixel_patch(1.0, ComponentSet::Rgbw, ColorOrder::Rgb);
    let index = patch.compile().unwrap();
    let preview = PreviewPlan::compile(&patch, &index);
    let snapshot = split_snapshot(0, 4);

    let master = master_of(&[0.25, 0.125, 0.0625, 0.5, 0.0, 0.0, 0.0, 0.0]);
    let mut flat = FlatParams::identity(2);
    // Blue filtered to zero, so a tinted white would be visible as a zero blue.
    flat.set_zone(0, 0.5, [1.0, 0.5, 0.0]);

    let mut out = Vec::new();
    preview.sample_into(&master, Some(&snapshot), &flat, &mut out);

    let white = 0.5 * 0.5; // dimmer only
    assert_eq!(
        out[0],
        [
            0.25 * (0.5 * 1.0) + white,
            0.125 * (0.5 * 0.5) + white,
            0.0625 * (0.5 * 0.0) + white,
        ]
    );
    assert!(
        out[0][2] > 0.0,
        "blue is filtered out of the emitters, but the white emitter still lights"
    );

    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let data = wire(&index, plan, &master, Some(&snapshot), &flat);
    assert_eq!(
        data[2], 0,
        "the blue emitter is filtered to zero on the wire"
    );
    assert_eq!(
        data[3],
        wire_code(white),
        "the white emitter carries the dimmer only — same as the preview folds in"
    );
}

#[test]
fn the_plan_carries_its_layout_generation_and_geometry() {
    let patch = two_pixel_patch(1.0, ComponentSet::Rgb, ColorOrder::Rgb);
    let index = patch.compile().unwrap();
    let preview = PreviewPlan::compile(&patch, &index);

    // The generation guard's raw material: a consumer compares this against the
    // generation stamped on the frame, exactly as the router does.
    assert_eq!(preview.generation(), index.generation());
    assert_eq!(preview.pixel_count(), 2);
    assert_eq!(preview.matrix_size(), [2, 1, 1]);
    assert_eq!(
        preview.positions(),
        &[glam::Vec2::new(0.0, 0.0), glam::Vec2::new(1.0, 0.0)],
        "positions are the layout the consumer draws at"
    );
    // Canonical offsets, in global pixel order.
    assert_eq!(preview.pixel(0).src[..3], [0, 1, 2]);
    assert_eq!(preview.pixel(1).src[..3], [3, 4, 5]);
    assert!(!preview.pixel(0).has_white());
}

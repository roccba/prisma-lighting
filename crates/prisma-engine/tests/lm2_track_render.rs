//! Live model phase 2 — the track controls and transitions in the compositor
//! (docs/LIVE-MODEL-SPEC.md §4.2, §5): live opacity / mute / solo read every frame,
//! crossfades on clock time (seconds and beats), a stop that fades to black and a
//! second stop that cuts, batches that land whole, and the 1:1 shortcut still equal
//! to the fold.
//!
//! Runs on lavapipe (deterministic). Grey solids, so the red component of each master
//! pixel identifies the value.

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, Easing, ParamDescriptor, ParameterTree, TimeBase};
use prisma_engine::{
    CompositorOptions, Fade, TrackContent, TrackLive, TrackTransition, ZoneCommand, ZoneCompositor,
};
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

fn grey(gpu: &Gpu, tree: &ParameterTree, prefix: &str, v: f32) -> EffectInstance {
    let inst = EffectInstance::new(gpu, tree, prefix, &effects::solid());
    for c in ["r", "g", "b"] {
        tree.write_path(&format!("{prefix}/param/{c}"), v, ui());
    }
    inst
}

/// Track `n`'s live controls, registered the way the engine does.
fn live(tree: &ParameterTree, n: usize) -> TrackLive {
    TrackLive {
        opacity: tree.register(
            &format!("/t/{n}/opacity"),
            ParamDescriptor::unit_level("o", 1.0),
        ),
        mute: tree.register(&format!("/t/{n}/mute"), ParamDescriptor::toggle("m", false)),
        solo: tree.register(&format!("/t/{n}/solo"), ParamDescriptor::toggle("s", false)),
    }
}

fn track(zone: usize, live: Option<TrackLive>, effect: Option<EffectInstance>) -> TrackContent {
    TrackContent {
        zones: vec![zone],
        blend_mode: BlendMode::Normal,
        key: prisma_render::blend::DEFAULT_KEY,
        opacity: 1.0,
        live,
        effects: vec![effect],
    }
}

fn all(px: &[f32], v: f32) -> bool {
    px.iter().all(|c| (c - v).abs() <= EPS)
}

#[test]
fn live_opacity_and_mute_change_the_frame_without_a_recompile() {
    let gpu = software_gpu();
    let patch = rig(1, 4);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let top = live(&tree, 1);
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(0, Some(live(&tree, 0)), Some(grey(gpu, &tree, "/a", 0.2))),
            track(0, Some(top.clone()), Some(grey(gpu, &tree, "/b", 0.8))),
        ],
    );
    let zones = lit(1);
    let now = clock_at(0, 120.0);
    assert!(all(&frame(gpu, &patch, &index, &mut c, &zones, &now), 0.8));

    tree.write_real(&top.opacity, 0.5, ui());
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(all(&f, 0.5), "opacity 0.5 mixes halfway: {f:?}");

    tree.write_real(&top.opacity, 1.0, ui());
    tree.write_real(&top.mute, 1.0, ui());
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(
        all(&f, 0.2),
        "a muted top track leaves the one below: {f:?}"
    );
}

#[test]
fn solo_isolates_tracks_across_zones() {
    let gpu = software_gpu();
    let patch = rig(2, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let (a, b) = (live(&tree, 0), live(&tree, 1));
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![
            track(0, Some(a.clone()), Some(grey(gpu, &tree, "/a", 0.6))),
            track(1, Some(b.clone()), Some(grey(gpu, &tree, "/b", 0.9))),
        ],
    );
    let zones = lit(2);
    let now = clock_at(0, 120.0);

    tree.write_real(&a.solo, 1.0, ui());
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(all(&f[0..2], 0.6), "the soloed track plays: {f:?}");
    assert!(
        all(&f[2..4], 0.0),
        "the other zone's track is silenced: {f:?}"
    );

    tree.write_real(&b.solo, 1.0, ui());
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(
        all(&f[0..2], 0.6) && all(&f[2..4], 0.9),
        "both soloed: {f:?}"
    );
}

#[test]
fn a_seconds_crossfade_mixes_on_clock_time_and_ends_on_the_shortcut() {
    let gpu = software_gpu();
    let patch = rig(1, 4);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.2)))],
    );
    let zones = lit(1);
    let t0 = 10 * SEC;
    frame(gpu, &patch, &index, &mut c, &zones, &clock_at(t0, 120.0));

    let sender = c.command_sender();
    let fade = Fade::seconds(2.0, Easing::Linear);
    assert!(sender.play_track(0, vec![Some(grey(gpu, &tree, "/b", 0.8))], fade));

    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));
    let start = at(&mut c, t0);
    assert!(
        all(&start, 0.2),
        "the fade starts on the outgoing clip: {start:?}"
    );
    let mid = at(&mut c, t0 + SEC);
    assert!(all(&mid, 0.5), "halfway through 2 s: {mid:?}");
    let end = at(&mut c, t0 + 2 * SEC);
    assert!(
        all(&end, 0.8),
        "the fade lands on the incoming clip: {end:?}"
    );

    // After the fade the instance is back on the 1:1 shortcut: bit-identical to a
    // compositor that cut straight to the incoming clip.
    let mut cut = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/c", 0.8)))],
    );
    assert_eq!(at(&mut c, t0 + 3 * SEC), at(&mut cut, t0 + 3 * SEC));
}

#[test]
fn a_beats_crossfade_follows_the_tempo() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.0)))],
    );
    let zones = lit(1);
    let fade = Fade {
        time: 4.0,
        base: TimeBase::Beats,
        easing: Easing::Linear,
    };
    c.command_sender()
        .play_track(0, vec![Some(grey(gpu, &tree, "/b", 1.0))], fade);
    // 4 beats at 240 BPM = 1 s: a quarter of a second is a quarter of the fade.
    frame(gpu, &patch, &index, &mut c, &zones, &clock_at(0, 240.0));
    let q = frame(
        gpu,
        &patch,
        &index,
        &mut c,
        &zones,
        &clock_at(SEC / 4, 240.0),
    );
    assert!(all(&q, 0.25), "1 of 4 beats: {q:?}");
}

#[test]
fn a_clip_fade_overrides_the_track_transition() {
    let track = TrackTransition {
        time: 4.0,
        base: TimeBase::Beats,
        easing: Easing::EaseIn,
    };
    let own = Fade::into_clip(&track, std::time::Duration::from_millis(1500));
    assert_eq!(own, Fade::seconds(1.5, Easing::EaseIn));
    let inherited = Fade::into_clip(&track, std::time::Duration::ZERO);
    assert_eq!(inherited, Fade::of_track(&track));
}

#[test]
fn stop_fades_to_black_and_a_second_stop_cuts() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.8)))],
    );
    let zones = lit(1);
    let sender = c.command_sender();
    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));

    sender.stop_track(0, Fade::seconds(2.0, Easing::Linear));
    assert!(all(&at(&mut c, 0), 0.8));
    let half = at(&mut c, SEC);
    assert!(all(&half, 0.4), "halfway to black: {half:?}");

    sender.stop_track(0, Fade::seconds(2.0, Easing::Linear));
    let cut = at(&mut c, SEC);
    assert!(all(&cut, 0.0), "the second stop cuts at once: {cut:?}");
}

#[test]
fn a_fade_from_silence_fades_in_from_black() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(gpu, &patch, &index, vec![track(0, None, None)]);
    let zones = lit(1);
    c.command_sender().play_track(
        0,
        vec![Some(grey(gpu, &tree, "/a", 0.8))],
        Fade::seconds(2.0, Easing::Linear),
    );
    frame(gpu, &patch, &index, &mut c, &zones, &clock_at(0, 120.0));
    let half = frame(gpu, &patch, &index, &mut c, &zones, &clock_at(SEC, 120.0));
    assert!(all(&half, 0.4), "half way up from black: {half:?}");
}

#[test]
fn a_batch_lands_every_track_in_the_same_frame() {
    let gpu = software_gpu();
    let patch = rig(2, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, None), track(1, None, None)],
    );
    let zones = lit(2);
    let now = clock_at(0, 120.0);
    assert!(all(&frame(gpu, &patch, &index, &mut c, &zones, &now), 0.0));

    assert!(c.command_sender().send(ZoneCommand::Batch(vec![
        ZoneCommand::SetTrackEffects {
            track: 0,
            effects: vec![Some(grey(gpu, &tree, "/a", 0.3))],
            fade: Fade::CUT,
        },
        ZoneCommand::SetTrackEffects {
            track: 1,
            effects: vec![Some(grey(gpu, &tree, "/b", 0.7))],
            fade: Fade::CUT,
        },
    ])));
    let f = frame(gpu, &patch, &index, &mut c, &zones, &now);
    assert!(all(&f[0..2], 0.3) && all(&f[2..4], 0.7), "{f:?}");
}

/// With live controls bound, the shortcut (live opacity back at 1) still renders the
/// same frame as the forced fold — also right after leaving a partial opacity.
#[test]
fn the_shortcut_matches_the_fold_with_live_opacity() {
    let gpu = software_gpu();
    let patch = rig(1, 4);
    let index = patch.compile().unwrap();
    let zones = lit(1);
    let build = |tree: &ParameterTree, options| {
        let l = live(tree, 0);
        let plasma = EffectInstance::new(gpu, tree, "/p", &effects::plasma());
        let c = ZoneCompositor::with_options(
            gpu,
            &patch,
            &index,
            vec![track(0, Some(l.clone()), Some(plasma))],
            options,
        );
        (c, l)
    };
    let (ta, tb) = (
        ParameterTree::with_defaults(),
        ParameterTree::with_defaults(),
    );
    let (mut short, la) = build(&ta, CompositorOptions::default());
    let (mut fold, lb) = build(&tb, CompositorOptions { force_fold: true });
    for (step, opacity) in [(0u64, 0.4f32), (1, 1.0), (2, 1.0)] {
        ta.write_real(&la.opacity, opacity, ui());
        tb.write_real(&lb.opacity, opacity, ui());
        let now = clock_at(step * 90_000_000, 120.0);
        let a = frame(gpu, &patch, &index, &mut short, &zones, &now);
        let b = frame(gpu, &patch, &index, &mut fold, &zones, &now);
        assert!(a.iter().any(|v| *v > EPS), "not black");
        assert_eq!(a, b, "step {step} (opacity {opacity})");
    }
}

/// A launch during a crossfade starts from exactly what is on screen: the frame right
/// after the launch (same clock) equals the frame before it, bit for bit, and the old
/// fade keeps running underneath until it lands.
#[test]
fn a_launch_during_a_fade_does_not_jump() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.2)))],
    );
    let zones = lit(1);
    let sender = c.command_sender();
    let fade = Fade::seconds(2.0, Easing::Linear);
    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));

    let t0 = 10 * SEC;
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/b", 0.6))], fade);
    at(&mut c, t0); // A → B starts
    let before = at(&mut c, t0 + SEC);
    assert!(all(&before, 0.4), "A→B halfway: {before:?}");

    sender.play_track(0, vec![Some(grey(gpu, &tree, "/c", 1.0))], fade);
    let after = at(&mut c, t0 + SEC);
    assert_eq!(
        after, before,
        "the launch must not change the frame it lands on"
    );

    // One second later: A→B has landed (B = 0.6), and B→C is halfway: 0.8.
    let later = at(&mut c, t0 + 2 * SEC);
    assert!(all(&later, 0.8), "nested fade underneath: {later:?}");
    let end = at(&mut c, t0 + 3 * SEC);
    assert!(all(&end, 1.0), "lands on C: {end:?}");
}

/// At the depth cap (a launch while a nested fade runs), the oldest mix is frozen — and
/// still nothing jumps.
#[test]
fn a_third_launch_freezes_the_oldest_mix_without_a_jump() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.0)))],
    );
    let zones = lit(1);
    let sender = c.command_sender();
    let fade = Fade::seconds(4.0, Easing::Linear);
    let at =
        |c: &mut ZoneCompositor, ns| frame(gpu, &patch, &index, c, &zones, &clock_at(ns, 120.0));

    let t0 = 10 * SEC;
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/b", 0.8))], fade);
    at(&mut c, t0);
    at(&mut c, t0 + SEC); // A→B at 1/4
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/c", 0.4))], fade);
    let nested = at(&mut c, t0 + 2 * SEC); // depth 2
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/d", 1.0))], fade);
    let frozen = at(&mut c, t0 + 2 * SEC);
    assert_eq!(
        frozen, nested,
        "freezing at the depth cap must not change the frame"
    );
    let end = at(&mut c, t0 + 6 * SEC);
    assert!(all(&end, 1.0), "lands on D: {end:?}");
}

/// Two launches in the same frame on one track (the mix below was never drawn): nothing
/// to freeze yet, the last launch still lands.
#[test]
fn two_launches_in_one_frame_still_land() {
    let gpu = software_gpu();
    let patch = rig(1, 2);
    let index = patch.compile().unwrap();
    let tree = ParameterTree::with_defaults();
    let mut c = ZoneCompositor::new(
        gpu,
        &patch,
        &index,
        vec![track(0, None, Some(grey(gpu, &tree, "/a", 0.2)))],
    );
    let zones = lit(1);
    let sender = c.command_sender();
    let fade = Fade::seconds(1.0, Easing::Linear);
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/b", 0.5))], fade);
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/c", 0.7))], fade);
    sender.play_track(0, vec![Some(grey(gpu, &tree, "/d", 0.9))], fade);
    frame(gpu, &patch, &index, &mut c, &zones, &clock_at(0, 120.0));
    let end = frame(
        gpu,
        &patch,
        &index,
        &mut c,
        &zones,
        &clock_at(3 * SEC, 120.0),
    );
    assert!(all(&end, 0.9), "{end:?}");
}

//! M9 Slice 1 — MIDI-in + Learn + Absolute, driven through the byte seam with no
//! `midir` and no hardware (M9-SPEC §13).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use prisma_core::{
    load_mappings, priority, save_mappings, ControlKind, ControllerMappings, Curve, ManualClock,
    MapTarget, MidiControl, MidiMapping, ParamDescriptor, ParameterTree, RealRange, ResponseCurve,
    SourceId, Takeover, TreeConfig, WriteOutcome, ZoneSub,
};
use prisma_midi::{MidiNotice, MidiRuntime};
use uuid::Uuid;

/// A tree shaped like the engine's: master dimmer + per-zone dimmer/solo, on a
/// manual clock so arbitration timing is deterministic.
fn engine_like_tree(zones: &[Uuid]) -> (Arc<ParameterTree>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.clone(),
        takeover_timeout: 1_000,
    });
    let dimmer = || {
        ParamDescriptor::level(
            "dimmer",
            RealRange { min: 0.0, max: 1.0 },
            Curve::Exponential { gamma: 2.2 },
            0.0,
        )
    };
    tree.register("/master/dimmer", dimmer());
    tree.register("/clock/tap", ParamDescriptor::trigger("clock.tap"));
    for id in zones {
        tree.register(&format!("/zone/{id}/dimmer"), dimmer());
        tree.register(
            &format!("/zone/{id}/speed"),
            ParamDescriptor::level(
                "speed",
                RealRange {
                    min: -10.0,
                    max: 10.0,
                },
                Curve::Linear,
                0.55,
            )
            .with_centre_detent(0.4),
        );
        tree.register(
            &format!("/zone/{id}/solo"),
            ParamDescriptor::toggle("solo", false),
        );
    }
    (Arc::new(tree), clock)
}

/// The parameter's REAL value — what the UI fader shows and writes.
fn real(tree: &ParameterTree, path: &str) -> f32 {
    tree.resolve(path).unwrap().read_real().unwrap()
}

fn cc(channel: u8, control: u8) -> MidiControl {
    MidiControl {
        channel,
        kind: ControlKind::Cc(control),
    }
}

fn zone_dimmer(slot: u16) -> MapTarget {
    MapTarget::ZoneSlot {
        slot,
        sub: ZoneSub::Dimmer,
    }
}

#[test]
fn learn_binds_the_next_fader_and_it_drives_the_dimmer() {
    let zones = [Uuid::new_v4(), Uuid::new_v4()];
    let (tree, _clock) = engine_like_tree(&zones);
    let rt = MidiRuntime::new(ControllerMappings::new("test"));
    rt.bind(Arc::clone(&tree), zones.to_vec());

    let learned = Arc::new(Mutex::new(None));
    {
        let learned = learned.clone();
        rt.set_listener(Some(Arc::new(move |n| {
            if let MidiNotice::Learned { profile, .. } = n {
                *learned.lock().unwrap() = Some(profile);
            }
        })));
    }

    // Unmapped input does nothing.
    rt.feed(&[0xB1, 7, 127]);
    assert_eq!(real(&tree, &format!("/zone/{}/dimmer", zones[1])), 0.0);

    // Arm Learn on zone slot 1's dimmer; a note-off must not be captured.
    rt.arm_learn(zone_dimmer(1));
    rt.feed(&[0x80, 3, 0]);
    assert!(rt.learning().is_some());

    // APC40 track-2 fader: CC 7 on channel 1. Learned under soft takeover (the
    // default): touched at half travel while the dimmer is at 0, it binds but does NOT
    // jump the dimmer there…
    rt.feed(&[0xB1, 7, 64]);
    assert!(rt.learning().is_none());
    let path = format!("/zone/{}/dimmer", zones[1]);
    assert_eq!(real(&tree, &path), 0.0);
    // …it picks the value up when it passes through it (64 → 0 crosses 0)…
    rt.feed(&[0xB1, 7, 0]);
    assert_eq!(real(&tree, &path), 0.0);

    // …and from then on keeps driving it, end to end.
    rt.feed(&[0xB1, 7, 127]);
    assert_eq!(real(&tree, &path), 1.0);
    rt.feed(&[0xB1, 7, 0]);
    assert_eq!(real(&tree, &path), 0.0);
    // Other zones are untouched (per-zone independence).
    assert_eq!(real(&tree, &format!("/zone/{}/dimmer", zones[0])), 0.0);

    // The notice carries a persistable profile that round-trips through JSON.
    let profile = learned.lock().unwrap().clone().expect("learned notice");
    let reloaded = load_mappings(&save_mappings(&profile).unwrap()).unwrap();
    assert_eq!(reloaded, profile);
    assert_eq!(
        reloaded.for_target(&zone_dimmer(1)).unwrap().control,
        cc(1, 7)
    );
}

#[test]
fn a_persisted_profile_follows_zone_slots_into_another_show() {
    let mut profile = ControllerMappings::new("portable");
    profile.bind(MidiMapping {
        led: None,
        control: cc(0, 7),
        target: zone_dimmer(0),
        mode: Default::default(),
        curve: ResponseCurve::Linear,
        takeover: Takeover::Jump,
    });
    profile.bind(MidiMapping {
        led: None,
        control: cc(0, 14),
        target: MapTarget::Path("/master/dimmer".into()),
        mode: Default::default(),
        curve: ResponseCurve::Linear,
        takeover: Takeover::Jump,
    });
    let rt = MidiRuntime::new(load_mappings(&save_mappings(&profile).unwrap()).unwrap());

    // Show A.
    let show_a = [Uuid::new_v4()];
    let (tree_a, _) = engine_like_tree(&show_a);
    rt.bind(Arc::clone(&tree_a), show_a.to_vec());
    rt.feed(&[0xB0, 7, 127]);
    assert_eq!(real(&tree_a, &format!("/zone/{}/dimmer", show_a[0])), 1.0);

    // Recompile into Show B (different uuids, a fresh tree): slot 0 is B's first zone.
    rt.unbind();
    rt.feed(&[0xB0, 7, 0]); // dropped while unbound
    assert_eq!(real(&tree_a, &format!("/zone/{}/dimmer", show_a[0])), 1.0);
    let show_b = [Uuid::new_v4(), Uuid::new_v4()];
    let (tree_b, _) = engine_like_tree(&show_b);
    rt.bind(Arc::clone(&tree_b), show_b.to_vec());
    assert_eq!(rt.resolved_count(), 2);
    rt.feed(&[0xB0, 7, 127]);
    rt.feed(&[0xB0, 14, 127]);
    assert_eq!(real(&tree_b, &format!("/zone/{}/dimmer", show_b[0])), 1.0);
    assert_eq!(real(&tree_b, "/master/dimmer"), 1.0);
}

#[test]
fn slots_past_the_end_stay_in_the_profile_but_do_not_bind() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    rt.arm_learn(zone_dimmer(5));
    rt.feed(&[0xB5, 7, 100]);
    assert_eq!(rt.profile().mappings.len(), 1);
    assert_eq!(rt.resolved_count(), 0);
}

#[test]
fn midi_takes_a_ui_held_parameter_and_holds_it_against_the_ui() {
    let zones = [Uuid::new_v4()];
    let (tree, clock) = engine_like_tree(&zones);
    let path = format!("/zone/{}/dimmer", zones[0]);
    let handle = tree.resolve(&path).unwrap();
    let ui = SourceId::new(priority::UI, 1);

    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    rt.arm_learn(zone_dimmer(0));

    // The UI is dragging the fader…
    assert_eq!(tree.write(&handle, 0.2, ui), WriteOutcome::Accepted);
    let ui_real = real(&tree, &path);
    // …a physical fader somewhere else is ignored until it meets the value (pickup)…
    rt.feed(&[0xB0, 7, 127]);
    assert_eq!(real(&tree, &path), ui_real);
    // …and once it does, it takes the parameter (MIDI > UI).
    rt.feed(&[0xB0, 7, (ui_real * 127.0).round() as u8]);
    rt.feed(&[0xB0, 7, 127]);
    assert_eq!(real(&tree, &path), 1.0);
    // The UI cannot fight it back while the hardware is active.
    assert!(matches!(
        tree.write(&handle, 0.3, ui),
        WriteOutcome::Rejected { owner } if owner.priority == priority::MIDI
    ));
    // After the idle timeout the UI takes it over cleanly.
    clock.advance(1_000);
    assert_eq!(tree.write(&handle, 0.3, ui), WriteOutcome::Accepted);
}

#[test]
fn absolute_semantics_curves_toggles_and_triggers() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let solo = format!("/zone/{}/solo", zones[0]);
    let mut profile = ControllerMappings::new("t");
    profile.bind(MidiMapping {
        led: None,
        control: cc(0, 14),
        target: MapTarget::Path("/master/dimmer".into()),
        mode: Default::default(),
        curve: ResponseCurve::Exponential { gamma: 2.0 },
        takeover: Takeover::Jump,
    });
    let pad = |note| MidiControl {
        channel: 0,
        kind: ControlKind::Note(note),
    };
    profile.bind(MidiMapping {
        led: None,
        control: pad(50),
        target: MapTarget::ZoneSlot {
            slot: 0,
            sub: ZoneSub::Solo,
        },
        mode: Default::default(),
        curve: ResponseCurve::Linear,
        takeover: Takeover::Jump,
    });
    profile.bind(MidiMapping {
        led: None,
        control: pad(99),
        target: MapTarget::Path("/clock/tap".into()),
        mode: Default::default(),
        curve: ResponseCurve::Linear,
        takeover: Takeover::Jump,
    });

    let applied = Arc::new(AtomicUsize::new(0));
    let rt = MidiRuntime::new(profile);
    {
        let applied = applied.clone();
        rt.set_listener(Some(Arc::new(move |n| {
            if matches!(n, MidiNotice::Applied { .. }) {
                applied.fetch_add(1, Ordering::Relaxed);
            }
        })));
    }
    rt.bind(Arc::clone(&tree), zones.to_vec());

    // The mapping curve shapes the normalized value (0.5 → 0.25 at gamma 2).
    rt.feed(&[0xB0, 14, 127]);
    rt.feed(&[0xB0, 14, 0]);
    let half = [0xB0, 14, 64];
    rt.feed(&half);
    let expected = (64.0f32 / 127.0).powf(2.0);
    assert!((real(&tree, "/master/dimmer") - expected).abs() < 1e-5);

    // A note under Absolute writes its velocity; its note-off carries no value.
    rt.feed(&[0x90, 50, 127]);
    assert_eq!(tree.resolve(&solo).unwrap().read_bool(), Some(true));
    rt.feed(&[0x80, 50, 0]);
    assert_eq!(tree.resolve(&solo).unwrap().read_bool(), Some(true));

    // A trigger fires once per press, never on release.
    let tap = tree.resolve("/clock/tap").unwrap();
    rt.feed(&[0x90, 99, 127]);
    rt.feed(&[0x80, 99, 0]);
    rt.feed(&[0x90, 99, 127]);
    assert_eq!(tap.fire_count(), Some(2));

    // Applied is reported for accepted writes only: 3 CC + 1 note + 2 fires.
    assert_eq!(applied.load(Ordering::Relaxed), 6);
}

#[test]
fn unmap_and_cancel_learn() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());

    rt.arm_learn(zone_dimmer(0));
    rt.cancel_learn();
    rt.feed(&[0xB0, 7, 127]);
    assert!(rt.profile().mappings.is_empty());

    rt.arm_learn(zone_dimmer(0));
    rt.feed(&[0xB0, 7, 0]); // learned at the dimmer's value (0): picked up at once
    rt.feed(&[0xB0, 7, 127]);
    let updated = rt.unmap(&zone_dimmer(0)).expect("was mapped");
    assert!(updated.mappings.is_empty());
    assert!(rt.unmap(&zone_dimmer(0)).is_none());
    rt.feed(&[0xB0, 7, 0]);
    assert_eq!(real(&tree, &format!("/zone/{}/dimmer", zones[0])), 1.0);
}

/// Regression (reported on the APC40): the dimmer has an exponential descriptor curve,
/// and the MIDI path used to push the fader position through it — hardware at 50% gave a
/// 22% dimmer while the UI at 50% gave 50%, so the low half of the physical fader was
/// dead. A hardware control and the UI control must land on the same real value.
#[test]
fn hardware_fader_matches_the_ui_fader_on_a_curved_dimmer() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let path = format!("/zone/{}/dimmer", zones[0]);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    rt.arm_learn(zone_dimmer(0));

    for value in [0u8, 13, 38, 64, 100, 127] {
        rt.feed(&[0xB0, 7, value]);
        let hardware = real(&tree, &path);

        // What the UI fader at the same position writes (engine `set_zone_dimmer`).
        let ui_tree = engine_like_tree(&zones).0;
        let h = ui_tree.resolve(&path).unwrap();
        ui_tree.write_real(&h, value as f32 / 127.0, SourceId::new(priority::UI, 1));
        let ui = h.read_real().unwrap();

        assert!(
            (hardware - ui).abs() < 1e-4,
            "CC {value}: hardware {hardware} vs UI {ui}"
        );
    }
    // Explicitly: half travel is half the dimmer, not 0.5^2.2 ≈ 0.22.
    rt.feed(&[0xB0, 7, 64]);
    assert!(real(&tree, &path) > 0.49);
}

/// A 7-bit fader has no exact centre: on the ±10 zone speed, CC 63 is -0.08 and CC 64 is
/// +0.08 — a residual crawl. The parameter's centre detent (the UI VEL fader's ±0.4) snaps
/// both to exactly static, and does not swallow real travel outside the band.
#[test]
fn speed_fader_centre_snaps_to_static() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let path = format!("/zone/{}/speed", zones[0]);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    let speed = MapTarget::ZoneSlot {
        slot: 0,
        sub: ZoneSub::Speed,
    };
    rt.arm_learn(speed.clone());
    rt.feed(&[0xB0, 8, 64]);
    // This test is about the detent arithmetic, not takeover: every message applies.
    rt.set_takeover(&speed, Takeover::Jump).unwrap();

    for value in [62u8, 63, 64, 65] {
        rt.feed(&[0xB0, 8, value]);
        assert_eq!(real(&tree, &path), 0.0, "CC {value} must be static");
    }
    rt.feed(&[0xB0, 8, 67]);
    assert!(real(&tree, &path) > 0.4);
    rt.feed(&[0xB0, 8, 60]);
    assert!(real(&tree, &path) < -0.4);
    rt.feed(&[0xB0, 8, 0]);
    assert_eq!(real(&tree, &path), -10.0);
}

/// Focus follow: an applied notice names the zone slot its control drives — by slot or
/// by a literal `/zone/{uuid}/…` path — and nothing for a global like the master.
#[test]
fn applied_notice_names_the_zone_the_control_drives() {
    let zones = [Uuid::new_v4(), Uuid::new_v4()];
    let (tree, _) = engine_like_tree(&zones);
    let mut profile = ControllerMappings::new("t");
    for (control, target) in [
        (cc(0, 7), zone_dimmer(1)),
        (
            cc(1, 7),
            MapTarget::Path(format!("/zone/{}/dimmer", zones[0])),
        ),
        (cc(0, 14), MapTarget::Path("/master/dimmer".into())),
    ] {
        profile.bind(MidiMapping {
            led: None,
            control,
            target,
            mode: Default::default(),
            curve: ResponseCurve::Linear,
            takeover: Takeover::Jump,
        });
    }
    let rt = MidiRuntime::new(profile);
    rt.bind(Arc::clone(&tree), zones.to_vec());
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = seen.clone();
        rt.set_listener(Some(Arc::new(move |n| {
            if let MidiNotice::Applied { zone, .. } = n {
                seen.lock().unwrap().push(zone);
            }
        })));
    }
    rt.feed(&[0xB0, 7, 100]);
    rt.feed(&[0xB1, 7, 100]);
    rt.feed(&[0xB0, 14, 100]);
    assert_eq!(*seen.lock().unwrap(), vec![Some(1), Some(0), None]);
}

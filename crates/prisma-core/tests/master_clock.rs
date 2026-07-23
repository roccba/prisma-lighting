//! Integration tests for the master clock (CLAUDE.md M4 done-criterion) and its
//! seam with the Parameter Tree (M3): the clock is the engine's single
//! `TimeSource`, tap tempo sets BPM addressably, and a fade runs on clock time
//! independently of render rate.

use std::sync::Arc;

use prisma_core::{
    priority, Curve, Easing, ManualClock, MasterClock, ParamDescriptor, ParameterTree, RealRange,
    SourceId, TimeSource, Transition, TreeConfig, WriteOutcome,
};

const SEC: u64 = 1_000_000_000;

fn src(prio: u8, tag: u32) -> SourceId {
    SourceId::new(prio, tag)
}

/// Point 1: the MasterClock is the production `TimeSource` for arbitration. The
/// tree's takeover-by-timeout is measured against the clock's wall time, so
/// advancing the (manual) wall clock releases an idle owner.
#[test]
fn clock_drives_parameter_tree_arbitration() {
    let wall = Arc::new(ManualClock::new());
    let clock = MasterClock::new(wall.clone());

    // The tree reads time from the clock handle — one monotonic base for the
    // whole engine.
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.handle(),
        takeover_timeout: SEC, // 1 second of owner idle
    });

    let h = tree.register(
        "/master/dimmer",
        ParamDescriptor::unit_level("master.dimmer", 1.0),
    );

    // MIDI (high) takes it.
    assert_eq!(
        tree.write(&h, 0.8, src(priority::MIDI, 1)),
        WriteOutcome::Accepted
    );
    // UI (low) rejected while MIDI still owns it.
    assert!(matches!(
        tree.write(&h, 0.2, src(priority::UI, 2)),
        WriteOutcome::Rejected { .. }
    ));

    // Advance wall time past the timeout via the same clock the tree reads.
    wall.advance(SEC);
    assert_eq!(
        tree.write(&h, 0.2, src(priority::UI, 2)),
        WriteOutcome::Accepted
    );
    assert_eq!(h.read_norm(), Some(0.2));
}

/// Point 4 / done-when: the clock's human-rate controls are addressable by path
/// (bpm here), while frame/beat are read directly off the snapshot — never
/// pushed to the tree per frame.
#[test]
fn bpm_is_addressable_by_path_while_beat_is_read_direct() {
    let wall = Arc::new(ManualClock::new());
    let mut clock = MasterClock::new(wall.clone());
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.handle(),
        takeover_timeout: SEC,
    });

    // /clock/bpm exists in the tree, introspectable, mapped to a real BPM range.
    let bpm = tree.register(
        "/clock/bpm",
        ParamDescriptor::level(
            "clock.bpm",
            RealRange {
                min: 20.0,
                max: 300.0,
            },
            Curve::Linear,
            0.5,
        ),
    );
    assert!(tree
        .walk("/clock/")
        .iter()
        .any(|n| n.path.as_ref() == "/clock/bpm"));

    // A transport writes BPM by path; the engine reads it back and applies it.
    tree.write_real(&bpm, 120.0, src(priority::UI, 1));
    clock.set_bpm(bpm.read_real().unwrap() as f64);
    assert!((clock.bpm() - 120.0).abs() < 0.5);

    // Beat is NOT a tree parameter — it is read from the snapshot, wait-free.
    clock.play();
    wall.advance(SEC);
    let snap = clock.tick();
    assert!((snap.beat - 2.0).abs() < 1e-6); // 120 BPM → 2 beats in 1 s
    assert!(
        tree.resolve("/clock/beat").is_none(),
        "beat must not flood the tree"
    );
}

/// Tap tempo, driven through the tree by path: a `/clock/tap` trigger fires, the
/// engine taps the clock, and BPM lands at the tapped value — addressably.
#[test]
fn tap_trigger_by_path_sets_bpm() {
    use prisma_core::TriggerCursor;

    let wall = Arc::new(ManualClock::new());
    let mut clock = MasterClock::new(wall.clone());
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.handle(),
        takeover_timeout: SEC,
    });
    let tap = tree.register("/clock/tap", ParamDescriptor::trigger("clock.tap"));
    let mut cursor = TriggerCursor::new();
    cursor.poll(tap.fire_count().unwrap()); // adopt baseline

    // Four taps 500 ms apart, delivered as trigger fires by path. The engine
    // drains the trigger each "frame" and taps the clock.
    for i in 0..4 {
        if i > 0 {
            wall.advance(SEC / 2);
        }
        tree.fire(&tap, src(priority::MIDI, 1));
        // Engine side: how many fires since last frame → tap that many times.
        let fires = cursor.poll(tap.fire_count().unwrap());
        for _ in 0..fires {
            clock.tap();
        }
    }

    assert!(
        (clock.bpm() - 120.0).abs() < 1.0,
        "tapped BPM: {}",
        clock.bpm()
    );
}

/// Done-when: a fade runs on clock time, independent of render rate. The same
/// elapsed show-time yields the same progress whether we tick once or a hundred
/// times to get there.
#[test]
fn fade_is_independent_of_render_rate() {
    // Coarse: two big ticks.
    let coarse_progress = {
        let wall = Arc::new(ManualClock::new());
        let mut c = MasterClock::new(wall.clone());
        c.play();
        let start = c.tick();
        let t = Transition::seconds(&start, 2.0, Easing::Linear);
        wall.advance(SEC);
        t.raw_progress(&c.tick())
    };

    // Fine: a hundred small ticks to reach the same 1 s.
    let fine_progress = {
        let wall = Arc::new(ManualClock::new());
        let mut c = MasterClock::new(wall.clone());
        c.play();
        let start = c.tick();
        let t = Transition::seconds(&start, 2.0, Easing::Linear);
        let mut last = start;
        for _ in 0..100 {
            wall.advance(SEC / 100);
            last = c.tick();
        }
        t.raw_progress(&last)
    };

    assert!((coarse_progress - 0.5).abs() < 1e-6);
    assert!((fine_progress - 0.5).abs() < 1e-6);
    assert!(
        (coarse_progress - fine_progress).abs() < 1e-6,
        "progress must not depend on tick count"
    );
}

/// The clock handle satisfies `TimeSource` directly — the same object the tree
/// holds is a monotonic wall clock for anyone else (probe, feedback).
#[test]
fn clock_handle_is_a_time_source() {
    let wall = Arc::new(ManualClock::new());
    let clock = MasterClock::new(wall.clone());
    let handle = clock.handle();
    let t0 = handle.now();
    wall.advance(1234);
    assert_eq!(handle.now() - t0, 1234);
}

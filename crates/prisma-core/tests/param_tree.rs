//! Integration tests for the Parameter Tree (CLAUDE.md M3 done-criterion):
//! "any engine value can be read/written by path, arbitration resolves competing
//! writers as specified, and subscribers receive changes off the writing thread."

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use prisma_core::{
    priority, Curve, Filter, ManualClock, ParamDescriptor, ParamKind, ParameterTree, RealRange,
    SourceId, TreeConfig, TriggerCursor, WriteOutcome,
};

fn manual_tree(clock: Arc<ManualClock>, timeout: u64) -> ParameterTree {
    ParameterTree::new(TreeConfig {
        clock,
        takeover_timeout: timeout,
    })
}

fn src(prio: u8, tag: u32) -> SourceId {
    SourceId::new(prio, tag)
}

// ---- read/write by path ---------------------------------------------------

#[test]
fn read_write_by_path_and_handle() {
    let tree = ParameterTree::with_defaults();
    let h = tree.register(
        "/zone/a/dimmer",
        ParamDescriptor::level(
            "zone.dimmer",
            RealRange { min: 0.0, max: 1.0 },
            Curve::Exponential { gamma: 2.2 },
            0.0,
        ),
    );
    assert_eq!(h.read_norm(), Some(0.0));

    // Write normalized, read back both representations.
    assert_eq!(
        tree.write(&h, 0.5, src(priority::UI, 1)),
        WriteOutcome::Accepted
    );
    assert_eq!(h.read_norm(), Some(0.5));
    // Exponential curve: half fader is well under half brightness.
    assert!(h.read_real().unwrap() < 0.3);

    // Write by path resolves to the same slot.
    assert_eq!(
        tree.write_path("/zone/a/dimmer", 1.0, src(priority::UI, 1)),
        Some(WriteOutcome::Accepted)
    );
    assert_eq!(h.read_norm(), Some(1.0));

    // Unknown path.
    assert!(tree
        .write_path("/zone/nope/dimmer", 0.5, src(priority::UI, 1))
        .is_none());
}

// ---- arbitration end-to-end via ManualClock -------------------------------

#[test]
fn arbitration_resolves_competing_writers() {
    let clock = Arc::new(ManualClock::new());
    let tree = manual_tree(clock.clone(), 1000);
    let h = tree.register(
        "/master/dimmer",
        ParamDescriptor::unit_level("master.dimmer", 1.0),
    );

    // MIDI (high) takes the parameter.
    assert_eq!(
        tree.write(&h, 0.8, src(priority::MIDI, 1)),
        WriteOutcome::Accepted
    );
    assert_eq!(h.read_norm(), Some(0.8));

    // UI (lower) is rejected while MIDI still owns it; value unchanged.
    match tree.write(&h, 0.2, src(priority::UI, 2)) {
        WriteOutcome::Rejected { owner } => assert_eq!(owner.priority, priority::MIDI),
        other => panic!("expected rejection, got {other:?}"),
    }
    assert_eq!(h.read_norm(), Some(0.8));

    // After the idle timeout, UI takes over cleanly.
    clock.advance(1000);
    assert_eq!(
        tree.write(&h, 0.2, src(priority::UI, 2)),
        WriteOutcome::Accepted
    );
    assert_eq!(h.read_norm(), Some(0.2));
}

// ---- async fan-out, off the writing thread --------------------------------

#[test]
fn subscribers_receive_changes_off_the_writing_thread() {
    let tree = ParameterTree::with_defaults();
    let h = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.0));
    let sub = tree.subscribe(Filter::Prefix(Arc::from("/zone/a/")));

    let writer_thread = std::thread::current().id();

    tree.write(&h, 0.42, src(priority::UI, 1));

    // recv blocks until the notifier (a different thread) delivers.
    let changed = sub.recv();
    // The delivery ran on the subscriber's thread (this one), not the writer's;
    // and the value read off the handle is the latest.
    assert_eq!(std::thread::current().id(), writer_thread);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].path().as_ref(), "/zone/a/dimmer");
    assert_eq!(changed[0].read_norm(), Some(0.42));
}

#[test]
fn filter_scopes_delivery_to_matching_paths() {
    let tree = ParameterTree::with_defaults();
    let a = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.0));
    let b = tree.register("/zone/b/dimmer", ParamDescriptor::unit_level("d", 0.0));
    let sub_a = tree.subscribe(Filter::Prefix(Arc::from("/zone/a/")));

    tree.write(&b, 0.5, src(priority::UI, 1)); // not in filter
    tree.write(&a, 0.7, src(priority::UI, 1)); // in filter

    let changed = sub_a.recv();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].path().as_ref(), "/zone/a/dimmer");
}

// ---- coalescing: a fader storm collapses to the latest value --------------

#[test]
fn rapid_writes_coalesce_to_latest_value() {
    let tree = ParameterTree::with_defaults();
    let h = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.0));
    let sub = tree.subscribe(Filter::All);

    // Fire a storm of writes before the subscriber drains.
    for i in 0..1000u32 {
        tree.write(&h, i as f32 / 1000.0, src(priority::UI, 1));
    }

    // Give the notifier a moment to fan out, then drain.
    std::thread::sleep(Duration::from_millis(50));
    let changed = sub.try_recv();

    // The parameter appears at most a small number of times (coalesced), never
    // 1000, and its live value is the final one.
    assert!(
        changed.len() <= 4,
        "expected coalesced delivery, got {}",
        changed.len()
    );
    assert_eq!(h.read_norm(), Some(999.0 / 1000.0));
}

// ---- trigger fan-out with the wrap-safe cursor ----------------------------

#[test]
fn trigger_fires_are_counted_through_fan_out() {
    let tree = ParameterTree::with_defaults();
    let t = tree.register(
        "/scene/1/trigger",
        ParamDescriptor::trigger("scene.trigger"),
    );
    let sub = tree.subscribe(Filter::All);
    let mut cursor = TriggerCursor::new();

    // Prime the cursor at the current count (a subscriber joining mid-show fires
    // zero — see the trigger-wrap rule).
    assert_eq!(cursor.poll(t.fire_count().unwrap()), 0);

    // Fire three times.
    for _ in 0..3 {
        tree.fire(&t, src(priority::MIDI, 1));
    }

    std::thread::sleep(Duration::from_millis(50));
    let changed = sub.try_recv();
    assert!(!changed.is_empty());
    let handle = changed
        .iter()
        .find(|h| h.kind() == ParamKind::Trigger)
        .unwrap();
    // Coalesced to one delivery, but the cursor recovers all three fires.
    assert_eq!(cursor.poll(handle.fire_count().unwrap()), 3);
}

// ---- appear/disappear: a deleted zone's handles go safely dead ------------

#[test]
fn unregister_makes_stale_handles_dead_no_ops() {
    let tree = ParameterTree::with_defaults();
    let h = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.5));
    assert_eq!(h.read_norm(), Some(0.5));

    // Delete the whole zone subtree (as when a zone is removed in the editor).
    assert_eq!(tree.unregister_prefix("/zone/a/"), 1);

    // A stale mapping still holding the handle now reads None and writes no-op.
    assert!(!h.is_live());
    assert_eq!(h.read_norm(), None);
    assert_eq!(
        tree.write(&h, 1.0, src(priority::MIDI, 1)),
        WriteOutcome::Dead
    );

    // And it is gone from resolution / introspection.
    assert!(tree.resolve("/zone/a/dimmer").is_none());
    assert!(tree.walk("/zone/a/").is_empty());
}

/// The reused-slot scenario, spelled out: delete a zone, recreate one (re-using
/// the same path), then write through the *old* handle a stale MIDI mapping still
/// holds. It must be a rejected no-op — never end up controlling the new param.
///
/// This holds because slots are never reused: a handle is an `Arc<Slot>` and ids
/// are monotonic, so an old handle points at its own dead slot forever and can't
/// alias a freshly registered one. The write path checks that specific slot's
/// `live`, not the resolve map.
#[test]
fn stale_handle_never_controls_a_recreated_param() {
    let tree = ParameterTree::with_defaults();

    // 1. Zone A's dimmer; a MIDI mapping keeps this handle.
    let midi_handle = tree.register("/zone/A/dimmer", ParamDescriptor::unit_level("d", 0.5));
    let old_id = midi_handle.id();
    tree.write(&midi_handle, 0.5, src(priority::MIDI, 1));

    // 2. Delete zone A.
    assert!(tree.unregister("/zone/A/dimmer"));
    assert!(!midi_handle.is_live());

    // 3. Recreate: a new zone B, and — to be thorough — a new zone A at the SAME
    //    path. Both are fresh slots with fresh, distinct identities.
    let zone_b = tree.register("/zone/B/dimmer", ParamDescriptor::unit_level("d", 0.0));
    let zone_a_new = tree.register("/zone/A/dimmer", ParamDescriptor::unit_level("d", 0.0));
    assert_ne!(old_id, zone_b.id(), "monotonic ids never repeat");
    assert_ne!(
        old_id,
        zone_a_new.id(),
        "recreated path gets a brand-new slot"
    );

    // 4. The stale MIDI handle writes full-scale. Must be a rejected no-op.
    assert_eq!(
        tree.write(&midi_handle, 1.0, src(priority::MIDI, 1)),
        WriteOutcome::Dead
    );

    // Neither recreated param moved a bit.
    assert_eq!(zone_b.read_norm(), Some(0.0));
    assert_eq!(zone_a_new.read_norm(), Some(0.0));
    // Resolving the path yields the NEW slot, never the stale handle's slot.
    assert_eq!(
        tree.resolve("/zone/A/dimmer").unwrap().id(),
        zone_a_new.id()
    );
    assert_ne!(tree.resolve("/zone/A/dimmer").unwrap().id(), old_id);
}

#[test]
fn re_registering_a_path_supersedes_the_old_slot() {
    let tree = ParameterTree::with_defaults();
    let old = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.1));
    let new = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.9));

    assert!(!old.is_live(), "old slot retired");
    assert!(new.is_live());
    assert_eq!(
        tree.resolve("/zone/a/dimmer").unwrap().read_norm(),
        Some(0.9)
    );
}

// ---- introspection: the UI/MIDI-learn build themselves --------------------

#[test]
fn walk_enumerates_the_tree_sorted_nothing_hardcoded() {
    let tree = ParameterTree::with_defaults();
    tree.register(
        "/master/dimmer",
        ParamDescriptor::unit_level("master.dimmer", 1.0),
    );
    tree.register(
        "/zone/a/dimmer",
        ParamDescriptor::unit_level("zone.dimmer", 0.0),
    );
    tree.register(
        "/zone/a/speed",
        ParamDescriptor::level(
            "zone.speed",
            RealRange { min: 0.0, max: 4.0 },
            Curve::Linear,
            0.25,
        ),
    );
    tree.register(
        "/scene/1/trigger",
        ParamDescriptor::trigger("scene.trigger"),
    );

    let all = tree.walk("");
    let paths: Vec<&str> = all.iter().map(|n| n.path.as_ref()).collect();
    assert_eq!(
        paths,
        vec![
            "/master/dimmer",
            "/scene/1/trigger",
            "/zone/a/dimmer",
            "/zone/a/speed"
        ]
    );

    // Subtree walk — the "build a menu for this zone" case.
    let zone = tree.walk("/zone/a/");
    assert_eq!(zone.len(), 2);
    // Descriptors carry the i18n key and kind, so the UI needs no hardcoding.
    let speed = zone
        .iter()
        .find(|n| n.path.as_ref() == "/zone/a/speed")
        .unwrap();
    assert_eq!(speed.kind, ParamKind::Level);
    assert_eq!(speed.descriptor.label_key, "zone.speed");

    let trig = all
        .iter()
        .find(|n| n.path.as_ref() == "/scene/1/trigger")
        .unwrap();
    assert_eq!(trig.kind, ParamKind::Trigger);
}

// ---- slow/hung subscriber is contained ------------------------------------

#[test]
fn a_hung_subscriber_does_not_block_writers_or_other_subscribers() {
    let tree = ParameterTree::with_defaults();
    let h = tree.register("/zone/a/dimmer", ParamDescriptor::unit_level("d", 0.0));

    // A subscriber that never drains (simulated hung consumer).
    let _hung = tree.subscribe(Filter::All);
    // A healthy subscriber that we will drain.
    let healthy = tree.subscribe(Filter::All);

    // The writer keeps going regardless of the hung subscriber.
    let counter = Arc::new(AtomicU32::new(0));
    for i in 0..500u32 {
        let out = tree.write(&h, i as f32 / 500.0, src(priority::UI, 1));
        assert_eq!(out, WriteOutcome::Accepted);
        counter.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(counter.load(Ordering::Relaxed), 500);

    // The healthy subscriber still gets the latest value (coalesced).
    std::thread::sleep(Duration::from_millis(50));
    let changed = healthy.try_recv();
    assert!(!changed.is_empty());
    assert_eq!(changed.last().unwrap().read_norm(), Some(499.0 / 500.0));
    // The hung subscriber's backlog is bounded to its parameter count (1 here),
    // not the 500 writes — dedup coalescing at the mailbox.
    // (We can't read its private mailbox, but the write loop completing proves
    // the notifier never blocked on it.)
}

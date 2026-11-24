//! Group presets (docs/LIVE-MODEL-SPEC.md §8.5 step 3): `/group-preset/{i}/apply` fades
//! the group masters it stores; the fade lands even while a controller owns a master,
//! and a hand on a master during the fade stops the fade for that group only.
//!
//! Headless: a real Parameter Tree and the real fader thread, no GPU.

use std::sync::Arc;
use std::time::{Duration, Instant};

use prisma_core::source::{priority, SourceId};
use prisma_core::{ParamDescriptor, ParamHandle, ParameterTree, WriteOutcome};
use prisma_engine::group_fader::{GroupFader, PresetPlan};

fn master(tree: &ParameterTree, name: &str) -> ParamHandle {
    tree.register(
        &format!("/group/{name}/dimmer"),
        ParamDescriptor::unit_level("group.dimmer", 1.0),
    )
}

fn preset(
    tree: &ParameterTree,
    i: usize,
    levels: &[(&ParamHandle, f32)],
    fade_ms: u64,
) -> PresetPlan {
    PresetPlan {
        trigger: tree.register(
            &format!("/group-preset/{i}/apply"),
            ParamDescriptor::trigger("group_preset.apply"),
        ),
        levels: levels.iter().map(|(h, v)| ((*h).clone(), *v)).collect(),
        fade: Duration::from_millis(fade_ms),
    }
}

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

fn midi() -> SourceId {
    SourceId::new(priority::MIDI, 1)
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_preset_fades_its_masters_and_lands_exactly() {
    let tree = Arc::new(ParameterTree::with_defaults());
    let a = master(&tree, "a");
    let b = master(&tree, "b");
    let other = master(&tree, "other");
    let p = preset(&tree, 0, &[(&a, 0.0), (&b, 0.25)], 300);
    let trigger = p.trigger.clone();
    let _fader = GroupFader::start(Arc::clone(&tree), vec![p]);

    let fired = Instant::now();
    tree.fire(&trigger, ui());
    // Midway it is between the start and the target, then it lands exactly.
    std::thread::sleep(Duration::from_millis(150));
    let mid = a.read_norm().unwrap();
    assert!(mid > 0.05 && mid < 0.95, "fading, got {mid}");
    wait_for("lands", || {
        a.read_norm() == Some(0.0) && b.read_norm() == Some(0.25)
    });
    assert!(
        fired.elapsed() >= Duration::from_millis(280),
        "took the fade time"
    );
    assert_eq!(
        other.read_norm(),
        Some(1.0),
        "a group the preset does not store is untouched"
    );
}

#[test]
fn a_preset_with_no_fade_cuts() {
    let tree = Arc::new(ParameterTree::with_defaults());
    let a = master(&tree, "a");
    let p = preset(&tree, 0, &[(&a, 0.4)], 0);
    let trigger = p.trigger.clone();
    let _fader = GroupFader::start(Arc::clone(&tree), vec![p]);
    tree.fire(&trigger, ui());
    wait_for("cut", || a.read_norm() == Some(0.4));
}

#[test]
fn a_preset_lands_while_a_controller_owns_the_master() {
    let tree = Arc::new(ParameterTree::with_defaults());
    let a = master(&tree, "a");
    // The APC just moved the master: it owns it, a UI write is refused...
    assert_eq!(tree.write(&a, 0.8, midi()), WriteOutcome::Accepted);
    assert!(matches!(
        tree.write(&a, 0.1, ui()),
        WriteOutcome::Rejected { .. }
    ));
    // ...but the preset still lands.
    let p = preset(&tree, 0, &[(&a, 0.2)], 100);
    let trigger = p.trigger.clone();
    let _fader = GroupFader::start(Arc::clone(&tree), vec![p]);
    tree.fire(&trigger, ui());
    wait_for("lands over the owner", || a.read_norm() == Some(0.2));
}

#[test]
fn a_hand_on_a_master_stops_that_groups_fade_only() {
    let tree = Arc::new(ParameterTree::with_defaults());
    let a = master(&tree, "a");
    let b = master(&tree, "b");
    let p = preset(&tree, 0, &[(&a, 0.0), (&b, 0.0)], 400);
    let trigger = p.trigger.clone();
    let _fader = GroupFader::start(Arc::clone(&tree), vec![p]);
    tree.fire(&trigger, ui());
    std::thread::sleep(Duration::from_millis(120));
    // The operator grabs master a mid-fade.
    assert_eq!(tree.write(&a, 0.9, ui()), WriteOutcome::Accepted);
    // b finishes its fade; a stays where the hand left it.
    wait_for("b lands", || b.read_norm() == Some(0.0));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.read_norm(), Some(0.9), "the fade let go of a");
}

#[test]
fn the_fader_thread_stops_when_dropped() {
    let tree = Arc::new(ParameterTree::with_defaults());
    let a = master(&tree, "a");
    let p = preset(&tree, 0, &[(&a, 0.0)], 10_000);
    let trigger = p.trigger.clone();
    let fader = GroupFader::start(Arc::clone(&tree), vec![p]);
    tree.fire(&trigger, ui());
    std::thread::sleep(Duration::from_millis(60));
    let t = Instant::now();
    drop(fader); // mid-fade: joins promptly
    assert!(t.elapsed() < Duration::from_millis(500));
    let v = a.read_norm().unwrap();
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(a.read_norm(), Some(v), "nothing writes after the drop");
}

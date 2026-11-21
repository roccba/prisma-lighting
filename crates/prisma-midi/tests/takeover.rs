//! Soft takeover (pickup) for continuous controls, and value feedback to the controls
//! that can display it (knob LED rings) — driven through the byte seams, no hardware.
//!
//! The reported failure: reconnecting the APC40 makes it report every knob/fader
//! position (it does so on entering Ableton mode); under plain absolute mapping the
//! knobs, parked at zero, slammed every zone's speed to full reverse.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::{
    priority, ControlKind, ControlMode, ControllerMappings, Curve, ManualClock, MapTarget,
    MidiControl, MidiMapping, ParamDescriptor, ParameterTree, RealRange, ResponseCurve, SourceId,
    Takeover, TreeConfig, WriteOutcome, ZoneSub,
};
use prisma_midi::{DeviceProfile, MidiOut, MidiRuntime};
use uuid::Uuid;

/// Zone dimmer (0..1, linear for readable numbers) and bipolar speed (±10, detent ±0.4,
/// default +1.0), like the engine's.
fn tree(zone: Uuid) -> (Arc<ParameterTree>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.clone(),
        takeover_timeout: 1_000,
    });
    tree.register(
        &format!("/zone/{zone}/dimmer"),
        ParamDescriptor::level(
            "dimmer",
            RealRange { min: 0.0, max: 1.0 },
            Curve::Linear,
            0.0,
        ),
    );
    tree.register(
        &format!("/zone/{zone}/speed"),
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
    (Arc::new(tree), clock)
}

fn cc(channel: u8, n: u8) -> MidiControl {
    MidiControl {
        channel,
        kind: ControlKind::Cc(n),
    }
}

fn mapping(control: MidiControl, sub: ZoneSub, takeover: Takeover) -> MidiMapping {
    MidiMapping {
        control,
        target: MapTarget::ZoneSlot { slot: 0, sub },
        mode: ControlMode::Absolute,
        curve: ResponseCurve::Linear,
        led: None,
        takeover,
    }
}

/// APC40 mkII: track-1 fader (CC 7) → dimmer, track-1 knob (CC 48) → speed.
fn runtime(zone: Uuid, tree: &Arc<ParameterTree>, takeover: Takeover) -> MidiRuntime {
    let mut profile = ControllerMappings::new("t");
    profile.bind(mapping(cc(0, 7), ZoneSub::Dimmer, takeover));
    profile.bind(mapping(cc(0, 48), ZoneSub::Speed, takeover));
    let rt = MidiRuntime::new(profile);
    rt.bind(Arc::clone(tree), vec![zone]);
    rt
}

fn real(tree: &ParameterTree, path: &str) -> f32 {
    tree.resolve(path).unwrap().read_real().unwrap()
}

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

#[test]
fn a_controller_dump_on_connect_moves_nothing() {
    let zone = Uuid::new_v4();
    let (tree, _) = tree(zone);
    let dimmer = format!("/zone/{zone}/dimmer");
    let speed = format!("/zone/{zone}/speed");
    let h = tree.resolve(&dimmer).unwrap();
    tree.write_real(&h, 0.5, ui());
    let rt = runtime(zone, &tree, Takeover::Pickup);

    // The APC40 reports every position on connect: knob and fader parked at zero.
    rt.feed(&[0xB0, 48, 0]);
    rt.feed(&[0xB0, 7, 0]);
    assert_eq!(
        real(&tree, &speed),
        1.0,
        "speed must not jump to full reverse"
    );
    assert_eq!(real(&tree, &dimmer), 0.5, "dimmer must not jump to black");
}

#[test]
fn a_fader_picks_the_value_up_by_crossing_it_then_follows() {
    let zone = Uuid::new_v4();
    let (tree, _) = tree(zone);
    let dimmer = format!("/zone/{zone}/dimmer");
    let h = tree.resolve(&dimmer).unwrap();
    tree.write_real(&h, 0.5, ui());
    let rt = runtime(zone, &tree, Takeover::Pickup);

    rt.feed(&[0xB0, 7, 0]);
    rt.feed(&[0xB0, 7, 30]); // still below 0.5: ignored
    assert_eq!(real(&tree, &dimmer), 0.5);
    rt.feed(&[0xB0, 7, 70]); // 30 → 70 passes through 0.5: picked up
    assert!((real(&tree, &dimmer) - 70.0 / 127.0).abs() < 1e-5);
    rt.feed(&[0xB0, 7, 100]); // and it follows from here
    assert!((real(&tree, &dimmer) - 100.0 / 127.0).abs() < 1e-5);
    rt.feed(&[0xB0, 7, 10]); // even a big jump, once engaged
    assert!((real(&tree, &dimmer) - 10.0 / 127.0).abs() < 1e-5);
}

#[test]
fn a_knob_at_the_value_picks_it_up_at_once() {
    let zone = Uuid::new_v4();
    let (tree, _) = tree(zone);
    let speed = format!("/zone/{zone}/speed");
    let rt = runtime(zone, &tree, Takeover::Pickup);

    // Speed is +1.0 = position 0.55 → CC 70 (what value feedback paints on the ring).
    rt.feed(&[0xB0, 48, 70]);
    let picked = real(&tree, &speed);
    assert!(
        (picked - 1.0).abs() < 0.25,
        "within a step: engaged, got {picked}"
    );
    rt.feed(&[0xB0, 48, 90]);
    assert!(real(&tree, &speed) > 3.0, "follows once engaged");
}

#[test]
fn the_control_lets_go_when_something_else_changes_the_value() {
    let zone = Uuid::new_v4();
    let (tree, clock) = tree(zone);
    let dimmer = format!("/zone/{zone}/dimmer");
    let h = tree.resolve(&dimmer).unwrap();
    let rt = runtime(zone, &tree, Takeover::Pickup);

    rt.feed(&[0xB0, 7, 0]); // dimmer is 0: picked up at once
    rt.feed(&[0xB0, 7, 100]);
    assert!((real(&tree, &dimmer) - 100.0 / 127.0).abs() < 1e-5);

    // The operator takes the UI fader to 0.2 (after MIDI's ownership times out).
    clock.advance(1_000);
    assert_eq!(tree.write_real(&h, 0.2, ui()), WriteOutcome::Accepted);

    // The hardware fader, still up at 100, no longer drives it…
    rt.feed(&[0xB0, 7, 101]);
    assert_eq!(real(&tree, &dimmer), 0.2);
    // …until it comes back down through 0.2.
    rt.feed(&[0xB0, 7, 20]);
    assert!((real(&tree, &dimmer) - 20.0 / 127.0).abs() < 1e-5);
}

#[test]
fn jump_mode_applies_every_position_at_once() {
    let zone = Uuid::new_v4();
    let (tree, _) = tree(zone);
    let speed = format!("/zone/{zone}/speed");
    let rt = runtime(zone, &tree, Takeover::Jump);
    rt.feed(&[0xB0, 48, 0]);
    assert_eq!(real(&tree, &speed), -10.0);
}

#[test]
fn a_profile_without_the_field_reads_as_pickup() {
    let json = r#"{
        "format": "prisma.controller-mappings", "schema_version": 3,
        "mappings": { "id": "6a1b8f3e-3f7c-4f5e-9d1e-2b8a4c6d0e11", "name": "old",
          "mappings": [ { "control": { "channel": 0, "kind": { "cc": 48 } },
                          "target": { "zone_slot": { "slot": 0, "sub": "speed" } },
                          "mode": "absolute" } ] } }"#;
    let profile = prisma_core::load_mappings(json).unwrap();
    assert_eq!(profile.mappings[0].takeover, Takeover::Pickup);
}

/// Everything sent to the controller, in order.
#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);

impl Wire {
    fn sink(&self) -> MidiOut {
        let log = self.0.clone();
        Arc::new(move |b: &[u8]| log.lock().unwrap().push(b.to_vec()))
    }
    fn sent(&self, bytes: [u8; 3]) -> bool {
        self.0.lock().unwrap().iter().any(|b| b[..] == bytes)
    }
    fn wait_for(&self, bytes: [u8; 3]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.sent(bytes) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("never sent {bytes:?}; sent {:?}", self.0.lock().unwrap());
    }
}

#[test]
fn knob_rings_show_prismas_value_on_connect_and_on_change() {
    let zone = Uuid::new_v4();
    let (tree, _) = tree(zone);
    let speed = tree.resolve(&format!("/zone/{zone}/speed")).unwrap();
    let rt = runtime(zone, &tree, Takeover::Pickup);
    rt.set_device(DeviceProfile::apc40_mk2());
    let wire = Wire::default();
    rt.set_output(Some(wire.sink()));

    // On connect the track-1 knob's ring shows speed +1.0 (position 0.55 → CC 70).
    wire.wait_for([0xB0, 48, 70]);
    // A change from anywhere (here the UI) moves the ring with it.
    tree.write_real(&speed, -10.0, ui());
    wire.wait_for([0xB0, 48, 0]);

    // The fader (CC 7) has no motor: it is never sent a value.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !wire
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|b| b.len() == 3 && b[0] == 0xB0 && b[1] == 7),
        "a plain fader gets no value feedback"
    );
}

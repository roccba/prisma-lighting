//! M9 Slice 2 — LED feedback from real state + Toggle mode, driven through the byte
//! seams (input `feed`, output `MidiOut` collector), no `midir`, no hardware (§13).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::{
    priority, ControlKind, ControlMode, ControllerMappings, Curve, ManualClock, MapTarget,
    MidiControl, MidiMapping, ParamDescriptor, ParameterTree, RealRange, SourceId, Takeover,
    TreeConfig, ZoneSub,
};
use prisma_midi::{DeviceProfile, MidiOut, MidiRuntime};
use uuid::Uuid;

fn tree(zones: &[Uuid]) -> (Arc<ParameterTree>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let tree = ParameterTree::new(TreeConfig {
        clock: clock.clone(),
        takeover_timeout: 1_000,
    });
    tree.register(
        "/master/dimmer",
        ParamDescriptor::level(
            "dimmer",
            RealRange { min: 0.0, max: 1.0 },
            Curve::Linear,
            0.0,
        ),
    );
    for id in zones {
        tree.register(
            &format!("/zone/{id}/solo"),
            ParamDescriptor::toggle("solo", false),
        );
        tree.register(
            &format!("/zone/{id}/blackout"),
            ParamDescriptor::toggle("blackout", false),
        );
    }
    (Arc::new(tree), clock)
}

/// Everything the runtime sent, in order.
#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);

impl Wire {
    fn sink(&self) -> MidiOut {
        let log = self.0.clone();
        Arc::new(move |b: &[u8]| log.lock().unwrap().push(b.to_vec()))
    }
    fn take(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    /// Wait (bounded) until `bytes` has been sent — feedback is asynchronous.
    fn wait_for(&self, bytes: [u8; 3]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.0.lock().unwrap().iter().any(|b| b[..] == bytes) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("never sent {bytes:?}; sent {:?}", self.0.lock().unwrap());
    }
}

fn note(channel: u8, n: u8) -> MidiControl {
    MidiControl {
        channel,
        kind: ControlKind::Note(n),
    }
}

fn solo(slot: u16) -> MapTarget {
    MapTarget::ZoneSlot {
        slot,
        sub: ZoneSub::Solo,
    }
}

fn read(tree: &ParameterTree, path: &str) -> bool {
    tree.resolve(path).unwrap().read_bool().unwrap()
}

#[test]
fn learning_a_pad_on_solo_makes_a_toggle_that_latches() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let path = format!("/zone/{}/solo", zones[0]);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), zones.to_vec());

    // APC40 track-1 solo button: note 49, ch 0. Learn captures the press and applies it.
    rt.arm_learn(solo(0));
    rt.feed(&[0x90, 49, 127]);
    assert_eq!(rt.profile().mappings[0].mode, ControlMode::Toggle);
    assert!(read(&tree, &path));

    // The release does nothing; the next press flips it back, and so on.
    rt.feed(&[0x80, 49, 0]);
    assert!(read(&tree, &path));
    rt.feed(&[0x90, 49, 127]);
    assert!(!read(&tree, &path));
    rt.feed(&[0x90, 49, 0]); // note-on velocity 0 is a release
    assert!(!read(&tree, &path));
    rt.feed(&[0x90, 49, 127]);
    assert!(read(&tree, &path));

    // A CC button in toggle mode flips on the press (127), not on the release (0).
    rt.arm_learn(MapTarget::ZoneSlot {
        slot: 0,
        sub: ZoneSub::Blackout,
    });
    rt.feed(&[0xB0, 60, 127]);
    let blackout = format!("/zone/{}/blackout", zones[0]);
    assert!(read(&tree, &blackout));
    rt.feed(&[0xB0, 60, 0]);
    assert!(read(&tree, &blackout));
    rt.feed(&[0xB0, 60, 127]);
    assert!(!read(&tree, &blackout));
}

#[test]
fn a_ui_change_lights_the_pad_and_turning_it_off_sends_a_real_note_off() {
    let zones = [Uuid::new_v4(), Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let wire = Wire::default();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.bind(Arc::clone(&tree), zones.to_vec());

    // Zone 2's solo ← track-2 solo button (note 49, ch 1); learning paints it once.
    rt.arm_learn(solo(1));
    rt.feed(&[0x91, 49, 127]);
    wire.wait_for([0x91, 49, 1]);
    rt.feed(&[0x91, 49, 127]); // back off from the hardware
    wire.wait_for([0x81, 49, 0]);
    wire.take();

    // The UI (another source) toggles it: the LED follows the tree, not the button.
    let h = tree.resolve(&format!("/zone/{}/solo", zones[1])).unwrap();
    let ui = SourceId::new(priority::MIDI, 999); // same tier: no takeover wait in the test
    tree.write_real(&h, 1.0, ui);
    wire.wait_for([0x91, 49, 1]);
    tree.write_real(&h, 0.0, ui);
    wire.wait_for([0x81, 49, 0]);
}

#[test]
fn rgb_grid_pads_light_in_the_profile_colour() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let wire = Wire::default();
    let mut profile = ControllerMappings::new("t");
    profile.bind(MidiMapping {
        led: None,
        control: note(0, 3),
        target: solo(0),
        mode: ControlMode::Toggle,
        curve: Default::default(),
        takeover: Takeover::Jump,
    });
    let rt = MidiRuntime::new(profile);
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.bind(Arc::clone(&tree), zones.to_vec());

    // Binding paints the current (off) state first.
    wire.wait_for([0x80, 3, 0]);
    rt.feed(&[0x90, 3, 127]);
    let colour = DeviceProfile::apc40_mk2().leds.rgb_on_colour;
    wire.wait_for([0x90, 3, colour]);
}

#[test]
fn the_release_reaffirms_the_led_the_apc40_just_reverted() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let wire = Wire::default();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    rt.arm_learn(solo(0));
    rt.feed(&[0x90, 49, 127]);
    wire.wait_for([0x90, 49, 1]);
    wire.take();

    // The release is sent synchronously from the input path, with the current state.
    rt.feed(&[0x80, 49, 0]);
    assert_eq!(wire.take(), vec![vec![0x90, 49, 1]]);

    // A device that does not revert (generic) gets no re-send.
    rt.set_device(DeviceProfile::generic());
    wire.wait_for([0x90, 49, 127]); // the repaint in the new device's language
    wire.take();
    rt.feed(&[0x80, 49, 0]);
    std::thread::sleep(Duration::from_millis(30));
    assert!(wire.take().is_empty());
}

#[test]
fn faders_get_no_feedback_and_unmapping_darkens_the_pad() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let wire = Wire::default();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.bind(Arc::clone(&tree), zones.to_vec());
    wire.take(); // the connect handshake

    // A master fader (CC 14) moving sends nothing back: no motors.
    rt.arm_learn(MapTarget::Path("/master/dimmer".into()));
    rt.feed(&[0xB0, 14, 90]);
    rt.feed(&[0xB0, 14, 20]);
    std::thread::sleep(Duration::from_millis(30));
    assert!(wire.take().is_empty());

    rt.arm_learn(solo(0));
    rt.feed(&[0x90, 49, 127]);
    wire.wait_for([0x90, 49, 1]);
    rt.unmap(&solo(0));
    wire.wait_for([0x80, 49, 0]);
}

#[test]
fn feedback_follows_a_rebind_and_stops_on_unbind() {
    let zones = [Uuid::new_v4()];
    let (old_tree, _) = tree(&zones);
    let wire = Wire::default();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.bind(Arc::clone(&old_tree), zones.to_vec());
    rt.arm_learn(solo(0));
    rt.feed(&[0x90, 49, 127]);
    wire.wait_for([0x90, 49, 1]);

    // Recompile: a new tree whose zone 1 is not soloed. The pad repaints dark.
    rt.unbind();
    let new_zones = [Uuid::new_v4()];
    let (new_tree, _) = tree(&new_zones);
    wire.take();
    rt.bind(Arc::clone(&new_tree), new_zones.to_vec());
    wire.wait_for([0x80, 49, 0]);

    // The old tree no longer drives anything.
    wire.take();
    let old = old_tree
        .resolve(&format!("/zone/{}/solo", zones[0]))
        .unwrap();
    old_tree.write_real(&old, 0.0, SourceId::new(priority::MIDI, 7));
    std::thread::sleep(Duration::from_millis(30));
    assert!(wire.take().is_empty());

    // Exit darkens what was lit.
    rt.feed(&[0x90, 49, 127]);
    wire.wait_for([0x90, 49, 1]);
    wire.take();
    rt.clear_leds();
    assert_eq!(wire.take(), vec![vec![0x80, 49, 0]]);
}

/// The APC40 mkII latches its track buttons in the hardware until told otherwise, so
/// the profile's mode handshake must reach the device first — before any LED is
/// painted (a mode switch would repaint over them).
#[test]
fn the_mode_handshake_goes_out_before_any_led() {
    let zones = [Uuid::new_v4()];
    let (tree, _) = tree(&zones);
    let wire = Wire::default();
    let mut profile = ControllerMappings::new("t");
    profile.bind(MidiMapping {
        led: None,
        control: note(0, 49),
        target: solo(0),
        mode: ControlMode::Toggle,
        curve: Default::default(),
        takeover: Takeover::Jump,
    });
    let rt = MidiRuntime::new(profile);
    rt.bind(Arc::clone(&tree), zones.to_vec());
    rt.set_device(DeviceProfile::apc40_mk2()); // no output yet: nothing sent
    assert!(wire.take().is_empty());

    rt.set_output(Some(wire.sink()));
    wire.wait_for([0x80, 49, 0]);
    let sent = wire.take();
    assert_eq!(sent[0], DeviceProfile::apc40_mk2().on_connect[0]);
    assert_eq!(sent[1], vec![0x80, 49, 0]);

    // A generic device has no handshake.
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_output(Some(wire.sink()));
    assert!(wire.take().is_empty());
}

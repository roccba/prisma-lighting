//! M9 Slice 3 — pad modes on grid cells (latch / trigger / toggle / momentary), the
//! operator's LED style, and cell LEDs showing empty / assigned / running in the
//! zone's colour. Byte seams only: no `midir`, no hardware, no engine (the clip
//! parameters are registered here the way `prisma-engine::clips` registers them).
//!
//! Since the live model a grid pad is a `TrackSlot { slot, Clip(i) }`: clip i of the
//! bank window's track `slot` (`/track/{id}/clip/{i}/…`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::{
    ControlMode, ControllerMappings, Curve, LedAnimation, LedRate, LedStyle, ManualClock,
    MapTarget, ParamDescriptor, ParameterTree, RealRange, SlotIds, SourceId, TrackSub, TreeConfig,
    ZoneSub,
};
use prisma_midi::{DeviceProfile, MidiOut, MidiRuntime};
use uuid::Uuid;

const CELLS: usize = 3;

/// One zone and its track, as the engine binds them.
struct Ids {
    zone: Uuid,
    track: Uuid,
}

impl Ids {
    fn new() -> Ids {
        Ids {
            zone: Uuid::new_v4(),
            track: Uuid::new_v4(),
        }
    }
    fn slots(&self) -> SlotIds {
        SlotIds {
            zones: vec![self.zone],
            tracks: vec![self.track],
            track_zones: vec![Some(0)],
            rows: CELLS as u16,
            ..SlotIds::default()
        }
    }
}

fn tree(ids: &Ids) -> Arc<ParameterTree> {
    let (zone, track) = (ids.zone, ids.track);
    let tree = ParameterTree::new(TreeConfig {
        clock: Arc::new(ManualClock::new()),
        takeover_timeout: 1_000,
    });
    for i in 0..CELLS {
        let base = format!("/track/{track}/clip/{i}");
        tree.register(&format!("{base}/launch"), ParamDescriptor::trigger("l"));
        tree.register(&format!("{base}/trigger"), ParamDescriptor::trigger("t"));
        tree.register(&format!("{base}/hold"), ParamDescriptor::toggle("h", false));
        tree.register(
            &format!("{base}/state"),
            ParamDescriptor::level("s", RealRange { min: 0.0, max: 2.0 }, Curve::Linear, 0.0),
        );
    }
    tree.register(
        &format!("/zone/{zone}/solo"),
        ParamDescriptor::toggle("solo", false),
    );
    Arc::new(tree)
}

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

fn cell(i: u16) -> MapTarget {
    MapTarget::TrackSlot {
        slot: 0,
        sub: TrackSub::Clip(i),
    }
}

fn param(tree: &ParameterTree, track: Uuid, clip: usize, leaf: &str) -> prisma_core::ParamHandle {
    tree.resolve(&format!("/track/{track}/clip/{clip}/{leaf}"))
        .unwrap()
}

#[test]
fn pad_modes_drive_the_cell_parameters() {
    let ids = Ids::new();
    let (zone, track) = (ids.zone, ids.track);
    let tree = tree(&ids);
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), ids.slots());
    let fires = |leaf| param(&tree, track, 1, leaf).fire_count().unwrap();
    let held = || param(&tree, track, 1, "hold").read_bool().unwrap();

    // Learn on a grid pad defaults to latch: a tap fires `launch`, the release nothing.
    rt.arm_learn(cell(1));
    rt.feed(&[0x90, 9, 127]);
    assert_eq!(rt.profile().mappings[0].mode, ControlMode::Latch);
    assert_eq!(fires("launch"), 1);
    rt.feed(&[0x80, 9, 0]);
    rt.feed(&[0x90, 9, 127]);
    assert_eq!(fires("launch"), 2);

    // Trigger: `trigger`, once per press however long it is held.
    rt.configure(&cell(1), ControlMode::Trigger, None).unwrap();
    rt.feed(&[0x90, 9, 127]);
    rt.feed(&[0x80, 9, 0]);
    assert_eq!((fires("launch"), fires("trigger")), (2, 1));

    // Momentary: hold follows the finger.
    rt.configure(&cell(1), ControlMode::Momentary, None)
        .unwrap();
    rt.feed(&[0x90, 9, 127]);
    assert!(held());
    rt.feed(&[0x80, 9, 0]);
    assert!(!held());

    // Toggle: one tap on, the next off; releases ignored.
    rt.configure(&cell(1), ControlMode::Toggle, None).unwrap();
    rt.feed(&[0x90, 9, 127]);
    rt.feed(&[0x80, 9, 0]);
    assert!(held());
    rt.feed(&[0x90, 9, 127]);
    assert!(!held());

    // Configure persists in the profile it returns; an unmapped target is refused.
    let profile = rt
        .configure(&cell(1), ControlMode::Momentary, None)
        .unwrap();
    assert_eq!(profile.mappings[0].mode, ControlMode::Momentary);
    assert!(rt.configure(&cell(2), ControlMode::Latch, None).is_none());

    // Momentary on a toggle parameter (solo while held) works the same way.
    let solo = MapTarget::ZoneSlot {
        slot: 0,
        sub: ZoneSub::Solo,
    };
    rt.arm_learn(solo.clone());
    rt.feed(&[0x90, 49, 127]); // learned as toggle → on
    rt.configure(&solo, ControlMode::Momentary, None).unwrap();
    let solo_on = || {
        tree.resolve(&format!("/zone/{zone}/solo"))
            .unwrap()
            .read_bool()
            .unwrap()
    };
    rt.feed(&[0x80, 49, 0]);
    assert!(!solo_on());
    rt.feed(&[0x90, 49, 127]);
    assert!(solo_on());
    rt.feed(&[0x80, 49, 0]);
    assert!(!solo_on());
}

#[test]
fn cell_leds_show_empty_assigned_running_in_zone_colour_or_the_style() {
    let ids = Ids::new();
    let tree = tree(&ids);
    let wire = Wire::default();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    rt.set_zone_colours(vec![[255, 0, 0]]); // a red zone
    rt.bind(Arc::clone(&tree), ids.slots());
    let engine = SourceId::new(u8::MAX, 0);
    let state = |v: f32| tree.write_real(&param(&tree, ids.track, 0, "state"), v, engine);

    rt.arm_learn(cell(0));
    rt.feed(&[0x90, 0, 127]);
    wire.wait_for([0x80, 0, 0]); // empty cell: dark
    wire.take();

    // Running → bright zone red (palette 5); assigned → a dim red; empty → dark.
    state(2.0);
    wire.wait_for([0x90, 0, 5]);
    state(1.0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let idle = loop {
        let dim = wire
            .take()
            .into_iter()
            .find(|b| b[0] == 0x90 && b[1] == 0 && b[2] != 5 && b[2] != 0);
        if let Some(b) = dim {
            break b[2];
        }
        assert!(Instant::now() < deadline, "no idle colour sent");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_ne!(idle, 5);

    // The operator styles it: green when running, blinking at 1/8; blue idle.
    let style = LedStyle {
        on: Some(21),
        idle: Some(45),
        animation: LedAnimation::Blink(LedRate::N8),
    };
    rt.configure(&cell(0), ControlMode::Momentary, Some(style))
        .unwrap();
    wire.wait_for([0x90, 0, 45]); // repainted idle in the style
    state(2.0);
    wire.wait_for([0x9D, 0, 21]); // blink 1/8 = channel 13
                                  // Releasing the pad reaffirms the styled LED at once.
    wire.take();
    rt.feed(&[0x80, 0, 0]);
    assert!(wire.take().contains(&vec![0x9D, 0, 21]));
}

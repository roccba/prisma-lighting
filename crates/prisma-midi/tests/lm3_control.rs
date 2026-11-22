//! Live model phase 3 (docs/LIVE-MODEL-SPEC.md §8.2) at the MIDI layer: track, scene and
//! selected-parameter targets resolved through the bank window; bank buttons; pad LEDs
//! for the four clip states; the APC40 mkII defaults seeded on first connection only and
//! the "load profile mappings" path never overwriting. Byte seams only: no `midir`, no
//! hardware, no engine (the parameters are registered here the way the engine does).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prisma_core::{
    Bank, ControlKind, ControlMode, ControllerMappings, Curve, ManualClock, MapTarget, MidiControl,
    MidiMapping, ParamDescriptor, ParameterTree, RealRange, ResponseCurve, SlotIds, SourceId,
    Takeover, TrackSub, TreeConfig,
};
use prisma_midi::{DeviceProfile, MidiNotice, MidiOut, MidiRuntime};
use uuid::Uuid;

const TRACKS: usize = 10;
const ROWS: usize = 12;

struct Grid {
    tracks: Vec<Uuid>,
    zones: Vec<Uuid>,
}

impl Grid {
    fn new() -> Grid {
        Grid {
            tracks: (0..TRACKS).map(|_| Uuid::new_v4()).collect(),
            zones: (0..TRACKS).map(|_| Uuid::new_v4()).collect(),
        }
    }

    fn slots(&self) -> SlotIds {
        SlotIds {
            zones: self.zones.clone(),
            tracks: self.tracks.clone(),
            track_zones: (0..TRACKS as u16).map(Some).collect(),
            rows: ROWS as u16,
            bank: Bank::default(),
        }
    }

    fn tree(&self) -> Arc<ParameterTree> {
        let tree = ParameterTree::new(TreeConfig {
            clock: Arc::new(ManualClock::new()),
            takeover_timeout: 1_000,
        });
        for t in &self.tracks {
            for i in 0..ROWS {
                let base = format!("/track/{t}/clip/{i}");
                for leaf in ["launch", "trigger"] {
                    tree.register(&format!("{base}/{leaf}"), ParamDescriptor::trigger(leaf));
                }
                tree.register(&format!("{base}/hold"), ParamDescriptor::toggle("h", false));
                tree.register(
                    &format!("{base}/state"),
                    ParamDescriptor::level(
                        "s",
                        RealRange { min: 0.0, max: 3.0 },
                        Curve::Linear,
                        0.0,
                    ),
                );
            }
            tree.register(
                &format!("/track/{t}/opacity"),
                ParamDescriptor::unit_level("o", 1.0),
            );
            tree.register(&format!("/track/{t}/stop"), ParamDescriptor::trigger("s"));
            tree.register(&format!("/track/{t}/select"), ParamDescriptor::trigger("s"));
            tree.register(
                &format!("/track/{t}/mute"),
                ParamDescriptor::toggle("m", false),
            );
        }
        for i in 0..ROWS {
            tree.register(
                &format!("/scene/{i}/launch"),
                ParamDescriptor::trigger("scene"),
            );
        }
        for n in 0..8 {
            tree.register(
                &format!("/select/param/{n}"),
                ParamDescriptor::unit_level("p", 0.0),
            );
        }
        tree.register("/master/dimmer", ParamDescriptor::unit_level("m", 1.0));
        tree.register("/tracks/stop", ParamDescriptor::trigger("stop"));
        Arc::new(tree)
    }
}

fn fires(tree: &ParameterTree, path: &str) -> u32 {
    tree.resolve(path).unwrap().fire_count().unwrap()
}

fn note(channel: u8, n: u8) -> MidiControl {
    MidiControl {
        channel,
        kind: ControlKind::Note(n),
    }
}

fn mapping(control: MidiControl, target: MapTarget, mode: ControlMode) -> MidiMapping {
    MidiMapping {
        control,
        target,
        mode,
        curve: ResponseCurve::Linear,
        led: None,
        takeover: Takeover::Jump,
    }
}

#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);

impl Wire {
    fn sink(&self) -> MidiOut {
        let log = self.0.clone();
        Arc::new(move |b: &[u8]| log.lock().unwrap().push(b.to_vec()))
    }
    fn wait_for(&self, bytes: [u8; 3]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.0.lock().unwrap().iter().any(|b| b[..] == bytes) {
                self.0.lock().unwrap().clear();
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("never sent {bytes:?}; sent {:?}", self.0.lock().unwrap());
    }
}

/// The APC40's grid pad note for bank-window column `col`, row `row` (row 0 = top).
fn pad(col: u8, row: u8) -> u8 {
    (4 - row) * 8 + col
}

#[test]
fn apc40_defaults_follow_the_bank_window() {
    let grid = Grid::new();
    let tree = grid.tree();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.bind(Arc::clone(&tree), grid.slots());
    let (_, added) = rt
        .seed_device_defaults()
        .expect("an empty profile is seeded");
    assert_eq!(added, DeviceProfile::apc40_mk2().default_mappings.len());

    let clip = |t: usize, i: usize| format!("/track/{}/clip/{i}/launch", grid.tracks[t]);
    let notices: Arc<Mutex<Vec<Bank>>> = Arc::default();
    let log = notices.clone();
    rt.set_listener(Some(Arc::new(move |n| {
        if let MidiNotice::Bank(b) = n {
            log.lock().unwrap().push(b)
        }
    })));

    // Bank 0: the top-left pad is track 0, clip 0; the bottom-right is track 7, clip 4.
    rt.feed(&[0x90, pad(0, 0), 127]);
    rt.feed(&[0x90, pad(7, 4), 127]);
    assert_eq!(fires(&tree, &clip(0, 0)), 1);
    assert_eq!(fires(&tree, &clip(7, 4)), 1);
    // Scene launch 2 → row 1; clip stop on column 3 → track 3; Stop All.
    rt.feed(&[0x90, 83, 127]);
    assert_eq!(fires(&tree, "/scene/1/launch"), 1);
    rt.feed(&[0x93, 52, 127]);
    assert_eq!(fires(&tree, &format!("/track/{}/stop", grid.tracks[3])), 1);
    rt.feed(&[0x90, 81, 127]);
    assert_eq!(fires(&tree, "/tracks/stop"), 1);

    // Shift + bank down moves a whole window (5 rows); bank right alone moves 1 track.
    rt.feed(&[0x90, 98, 127]); // Shift held
    rt.feed(&[0x90, 95, 127]);
    rt.feed(&[0x80, 95, 0]);
    rt.feed(&[0x80, 98, 0]); // Shift released
    rt.feed(&[0x90, 96, 127]);
    assert_eq!(rt.bank(), Bank { track: 1, row: 5 });
    assert_eq!(
        *notices.lock().unwrap(),
        vec![Bank { track: 0, row: 5 }, Bank { track: 1, row: 5 }]
    );
    rt.feed(&[0x90, pad(0, 0), 127]);
    assert_eq!(fires(&tree, &clip(1, 5)), 1);
    rt.feed(&[0x90, 83, 127]);
    assert_eq!(fires(&tree, "/scene/6/launch"), 1);
    // A track fader follows the window too: fader 1 is now track 1's opacity (pickup:
    // it engages at the current value, then follows).
    rt.feed(&[0xB0, 7, 127]);
    rt.feed(&[0xB0, 7, 0]);
    let opacity = |t: usize| {
        tree.resolve(&format!("/track/{}/opacity", grid.tracks[t]))
            .unwrap()
            .read_real()
            .unwrap()
    };
    assert_eq!((opacity(0), opacity(1)), (1.0, 0.0));

    // The window always shows a full 8 × 5: 10 tracks → at most 2 right, 12 rows → at
    // most 7 down. Plain presses move one track / one row.
    for _ in 0..5 {
        rt.feed(&[0x90, 96, 127]);
        rt.feed(&[0x90, 95, 127]);
    }
    assert_eq!(rt.bank(), Bank { track: 2, row: 7 });
    // The bottom-right pad is the grid's last row of track 9.
    rt.feed(&[0x90, pad(7, 4), 127]);
    assert_eq!(fires(&tree, &clip(9, 11)), 1);
    rt.feed(&[0x90, 94, 127]);
    rt.feed(&[0x90, 97, 127]);
    assert_eq!(rt.bank(), Bank { track: 1, row: 6 });
    // Shift + left / up: a whole window, clamped at the start (row 6 − 5 = 1).
    rt.feed(&[0x90, 98, 127]);
    rt.feed(&[0x90, 97, 127]);
    rt.feed(&[0x90, 94, 127]);
    rt.feed(&[0x80, 98, 0]);
    assert_eq!(rt.bank(), Bank { track: 0, row: 1 });

    // Bank buttons and Shift are never learned.
    rt.arm_learn(MapTarget::Path("/master/dimmer".into()));
    rt.feed(&[0x90, 94, 127]);
    rt.feed(&[0x90, 98, 127]);
    rt.feed(&[0x80, 98, 0]);
    assert!(rt.learning().is_some(), "a bank press must not be captured");
    rt.cancel_learn();

    // A rebind onto a smaller grid keeps the window inside it.
    let small = SlotIds {
        tracks: grid.tracks[..3].to_vec(),
        track_zones: vec![Some(0); 3],
        rows: 4,
        ..grid.slots()
    };
    rt.bind(Arc::clone(&tree), small);
    assert_eq!(rt.bank(), Bank::default());
}

#[test]
fn device_knobs_drive_the_selected_parameters_and_never_bank() {
    let grid = Grid::new();
    let tree = grid.tree();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.bind(Arc::clone(&tree), grid.slots());
    rt.seed_device_defaults().unwrap();
    let knob = |n: usize| {
        tree.resolve(&format!("/select/param/{n}"))
            .unwrap()
            .read_real()
            .unwrap()
    };
    // Device knob 3 (CC 18): pickup — it engages from 0 because the value is 0.
    rt.feed(&[0xB0, 18, 0]);
    rt.feed(&[0xB0, 18, 127]);
    assert_eq!(knob(2), 1.0);
    // A bank move changes nothing for it: still engaged, it follows at once.
    rt.feed(&[0x90, 96, 127]); // bank right
    rt.feed(&[0xB0, 18, 64]);
    assert!((knob(2) - 64.0 / 127.0).abs() < 1e-3);
}

#[test]
fn pads_show_the_four_clip_states() {
    let grid = Grid::new();
    let tree = grid.tree();
    let wire = Wire::default();
    let mut profile = ControllerMappings::new("t");
    profile.bind(mapping(
        note(0, 0),
        MapTarget::TrackSlot {
            slot: 0,
            sub: TrackSub::Clip(0),
        },
        ControlMode::Latch,
    ));
    let rt = MidiRuntime::new(profile);
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.set_output(Some(wire.sink()));
    let mut colours = vec![[0, 0, 0]; TRACKS];
    colours[0] = [255, 0, 0]; // track 0 plays in a red zone
    rt.set_zone_colours(colours);
    rt.bind(Arc::clone(&tree), grid.slots());
    let state_h = tree
        .resolve(&format!("/track/{}/clip/0/state", grid.tracks[0]))
        .unwrap();
    let state = |v: f32| tree.write_real(&state_h, v, SourceId::new(u8::MAX, 0));

    wire.wait_for([0x80, 0, 0]); // Vacía: dark
    state(2.0);
    wire.wait_for([0x90, 0, 5]); // En vivo: full zone red (palette 5), static
    state(3.0);
    // Armada: the same red, pulsing (APC40 pulse channels 6–10; 1/8 = channel 8).
    wire.wait_for([0x98, 0, 5]);
    state(1.0);
    // Cargada: a dim red, static (channel 0, a colour that is neither off nor full red).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let sent = wire.0.lock().unwrap().clone();
        if sent
            .iter()
            .any(|b| b[0] == 0x90 && b[1] == 0 && b[2] != 0 && b[2] != 5)
        {
            break;
        }
        assert!(Instant::now() < deadline, "no dim colour: {sent:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    state(0.0);
    wire.wait_for([0x80, 0, 0]);
}

#[test]
fn apc40_defaults_are_seeded_on_the_first_connection_only() {
    let grid = Grid::new();
    let tree = grid.tree();
    let rt = MidiRuntime::new(ControllerMappings::new("t"));
    rt.bind(Arc::clone(&tree), grid.slots());
    // A device without defaults seeds nothing.
    assert!(rt.seed_device_defaults().is_none());

    rt.set_device(DeviceProfile::apc40_mk2());
    let (profile, added) = rt.seed_device_defaults().unwrap();
    assert!(added > 0);
    assert_eq!(profile.seeded, vec!["apc40mk2".to_string()]);

    // Reconnecting does nothing, even after the operator removed every mapping: the
    // device was seeded once already.
    assert!(rt.seed_device_defaults().is_none());
    let mut cleared = rt.profile();
    cleared.mappings.clear();
    rt.set_profile(cleared);
    assert!(rt.seed_device_defaults().is_none());
    assert!(rt.profile().mappings.is_empty());

    // A profile with mappings of its own is never seeded, even for a new device.
    let mut own = ControllerMappings::new("own");
    own.bind(mapping(
        note(0, 32),
        MapTarget::Path("/tracks/stop".into()),
        ControlMode::Trigger,
    ));
    rt.set_profile(own.clone());
    assert!(rt.seed_device_defaults().is_none());
    assert_eq!(rt.profile().mappings, own.mappings);
}

#[test]
fn loading_the_profile_mappings_never_overwrites() {
    let grid = Grid::new();
    let tree = grid.tree();
    // The operator learned the top-left pad (note 32) as Stop All, and track 1's
    // opacity on a knob.
    let mut own = ControllerMappings::new("own");
    own.bind(mapping(
        note(0, 32),
        MapTarget::Path("/tracks/stop".into()),
        ControlMode::Trigger,
    ));
    own.bind(mapping(
        MidiControl {
            channel: 0,
            kind: ControlKind::Cc(48),
        },
        MapTarget::TrackSlot {
            slot: 0,
            sub: TrackSub::Opacity,
        },
        ControlMode::Absolute,
    ));
    let rt = MidiRuntime::new(own.clone());
    rt.set_device(DeviceProfile::apc40_mk2());
    rt.bind(Arc::clone(&tree), grid.slots());

    let (profile, added) = rt.add_device_defaults();
    let defaults = DeviceProfile::apc40_mk2().default_mappings.len();
    // Skipped: the default on note 32 (control taken), fader 1 → track 1 opacity and
    // Stop All → /tracks/stop (targets taken), track knob 1 on CC 48 (control taken).
    assert_eq!(added, defaults - 4);
    for m in &own.mappings {
        assert_eq!(profile.find(m.control), Some(m), "learned mapping changed");
    }
    // The pad still stops everything.
    rt.feed(&[0x90, 32, 127]);
    assert_eq!(fires(&tree, "/tracks/stop"), 1);
    // A second load adds nothing.
    assert_eq!(rt.add_device_defaults().1, 0);
}

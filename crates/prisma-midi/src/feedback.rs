//! Feedback (MIDI-out) from real state (M9-SPEC §8).
//!
//! LEDs show what Prisma **is doing**, not what the controller last sent: a
//! [`Feedback`] engine holds a tree [`Subscription`] filtered to exactly the handles
//! that mapped note controls show, and on every change reads the live value and
//! sends that control's LED bytes. So a solo toggled from the UI, from another
//! controller or from the pad itself all light the pad the same way.
//!
//! # Threading
//!
//! One thread per binding (`prisma-midi-feedback`), parked in `recv` on the tree's
//! async fan-out (§4.5): never polling, never on render/output, newest-wins by
//! construction (the mailbox coalesces per parameter). It never takes the runtime's
//! state lock, so stopping it (close + join) from under that lock cannot deadlock.
//!
//! # Value feedback
//!
//! The same thread also follows the continuous controls the device can DISPLAY (knob
//! LED rings, [`LedSpec::value_rings`](crate::profile::LedSpec::value_rings)): it sends
//! each one its parameter's value as a CC — on connect and on every change. A ring then
//! shows Prisma's value, and since the knob continues from what it shows, turning it
//! picks the value up at once (soft takeover, see [`crate::resolver`]). Plain faders
//! cannot show anything and get nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use prisma_core::{
    ControlKind, Filter, ParamHandle, ParamId, ParamKind, ParameterTree, SubscriptionCloser,
};

use crate::message::cc_bytes;
use crate::profile::{DeviceProfile, LedLevel};
use crate::resolver::{FeedbackTarget, ValueTarget};

/// Where MIDI-out bytes go: the `midir` output connection in the app, a collector in
/// tests. The injectable output seam, mirroring [`MidiRuntime::feed`](crate::MidiRuntime::feed)
/// on the input side. Called from the feedback thread and the input thread.
pub type MidiOut = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// What a handle's current value shows. A grid clip's state has four levels (Vacía →
/// dark, Cargada → dim, En vivo → full, Armada → pulsing); anything else is lit when a
/// toggle is on or a level is above the bottom of its range. `None` for triggers and
/// dead handles.
pub fn led_level(handle: &ParamHandle, cell: bool) -> Option<LedLevel> {
    let lit = |on: bool| if on { LedLevel::On } else { LedLevel::Off };
    match handle.kind() {
        ParamKind::Toggle => handle.read_bool().map(lit),
        ParamKind::Level if cell => handle.read_real().map(|v| {
            if v >= 2.5 {
                LedLevel::Armed
            } else if v >= 1.5 {
                LedLevel::On
            } else if v >= 0.5 {
                LedLevel::Idle
            } else {
                LedLevel::Off
            }
        }),
        ParamKind::Level => handle.read_norm().map(|n| lit(n > 0.0005)),
        ParamKind::Trigger => None,
    }
}

/// Send a target's LED for its current state. `zone_colours` are the running show's
/// zone display colours by slot, for the default cell colour.
pub fn send_led(
    out: &MidiOut,
    device: &DeviceProfile,
    target: &FeedbackTarget,
    zone_colours: &[[u8; 3]],
) {
    let colour = target
        .zone
        .and_then(|z| zone_colours.get(z as usize))
        .copied();
    let bytes = led_level(&target.handle, target.cell)
        .and_then(|level| device.led_bytes(target.control, level, target.led.as_ref(), colour));
    if let Some(bytes) = bytes {
        out(&bytes);
    }
}

/// Send a continuous control its parameter's current value (a knob's LED ring).
pub fn send_value(out: &MidiOut, target: &ValueTarget) {
    let ControlKind::Cc(cc) = target.control.kind else {
        return;
    };
    if let Some(position) = target.position() {
        out(&cc_bytes(target.control.channel, cc, position));
    }
}

/// What one feedback target is.
#[derive(Clone, Copy)]
enum Shown {
    Led(usize),
    Value(usize),
}

/// A running feedback thread. Dropping it stops the thread and waits for it.
pub struct Feedback {
    closer: SubscriptionCloser,
    thread: Option<JoinHandle<()>>,
}

impl Feedback {
    /// Start feedback for `targets` (LEDs) and `values` (controls that display a value):
    /// paint every one from current state once, then follow changes. `None` when there
    /// is nothing to show.
    pub fn start(
        tree: &ParameterTree,
        targets: Vec<FeedbackTarget>,
        values: Vec<ValueTarget>,
        device: Arc<DeviceProfile>,
        zone_colours: Arc<Vec<[u8; 3]>>,
        out: MidiOut,
    ) -> Option<Feedback> {
        if targets.is_empty() && values.is_empty() {
            return None;
        }
        let mut by_param: HashMap<ParamId, Vec<Shown>> = HashMap::new();
        for (i, t) in targets.iter().enumerate() {
            by_param
                .entry(t.handle.id())
                .or_default()
                .push(Shown::Led(i));
        }
        for (i, v) in values.iter().enumerate() {
            by_param
                .entry(v.handle.id())
                .or_default()
                .push(Shown::Value(i));
        }
        // Subscribe BEFORE the initial paint reads state, so a change landing in
        // between is delivered rather than lost.
        let sub = tree.subscribe(Filter::Ids(by_param.keys().copied().collect()));
        let closer = sub.closer();
        let thread = std::thread::Builder::new()
            .name("prisma-midi-feedback".into())
            .spawn(move || {
                for target in &targets {
                    send_led(&out, &device, target, &zone_colours);
                }
                for value in &values {
                    send_value(&out, value);
                }
                loop {
                    let changed = sub.recv();
                    if changed.is_empty() {
                        break; // closed: rebinding, unbinding, or the tree is gone
                    }
                    for handle in &changed {
                        for shown in by_param.get(&handle.id()).into_iter().flatten() {
                            match *shown {
                                Shown::Led(i) => {
                                    send_led(&out, &device, &targets[i], &zone_colours)
                                }
                                Shown::Value(i) => send_value(&out, &values[i]),
                            }
                        }
                    }
                }
            })
            .expect("spawn MIDI feedback thread");
        Some(Feedback {
            closer,
            thread: Some(thread),
        })
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        self.closer.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

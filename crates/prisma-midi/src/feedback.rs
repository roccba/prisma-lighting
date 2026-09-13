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
//! Motorized-fader value echo is deliberately out of scope: CCs have no feedback.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use prisma_core::{Filter, ParamHandle, ParamId, ParamKind, ParameterTree, SubscriptionCloser};

use crate::profile::{DeviceProfile, LedLevel};
use crate::resolver::FeedbackTarget;

/// Where MIDI-out bytes go: the `midir` output connection in the app, a collector in
/// tests. The injectable output seam, mirroring [`MidiRuntime::feed`](crate::MidiRuntime::feed)
/// on the input side. Called from the feedback thread and the input thread.
pub type MidiOut = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// What a handle's current value shows. A grid cell's state has three levels
/// (empty / assigned / running); anything else is lit when a toggle is on or a level
/// is above the bottom of its range. `None` for triggers and dead handles.
pub fn led_level(handle: &ParamHandle, cell: bool) -> Option<LedLevel> {
    let lit = |on: bool| if on { LedLevel::On } else { LedLevel::Off };
    match handle.kind() {
        ParamKind::Toggle => handle.read_bool().map(lit),
        ParamKind::Level if cell => handle.read_real().map(|v| {
            if v >= 1.5 {
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

/// A running feedback thread. Dropping it stops the thread and waits for it.
pub struct Feedback {
    closer: SubscriptionCloser,
    thread: Option<JoinHandle<()>>,
}

impl Feedback {
    /// Start feedback for `targets`: paint every LED from current state once, then
    /// follow changes. `None` when there is nothing to show.
    pub fn start(
        tree: &ParameterTree,
        targets: Vec<FeedbackTarget>,
        device: Arc<DeviceProfile>,
        zone_colours: Arc<Vec<[u8; 3]>>,
        out: MidiOut,
    ) -> Option<Feedback> {
        if targets.is_empty() {
            return None;
        }
        // Subscribe BEFORE the initial paint reads state, so a change landing in
        // between is delivered rather than lost.
        let sub = tree.subscribe(Filter::Ids(targets.iter().map(|t| t.handle.id()).collect()));
        let closer = sub.closer();
        let mut by_param: HashMap<ParamId, Vec<usize>> = HashMap::new();
        for (i, t) in targets.iter().enumerate() {
            by_param.entry(t.handle.id()).or_default().push(i);
        }
        let thread = std::thread::Builder::new()
            .name("prisma-midi-feedback".into())
            .spawn(move || {
                for target in &targets {
                    send_led(&out, &device, target, &zone_colours);
                }
                loop {
                    let changed = sub.recv();
                    if changed.is_empty() {
                        break; // closed: rebinding, unbinding, or the tree is gone
                    }
                    for handle in &changed {
                        for &i in by_param.get(&handle.id()).into_iter().flatten() {
                            send_led(&out, &device, &targets[i], &zone_colours);
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

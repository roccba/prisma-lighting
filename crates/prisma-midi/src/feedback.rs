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

use prisma_core::{
    Filter, MidiControl, ParamHandle, ParamId, ParamKind, ParameterTree, SubscriptionCloser,
};

use crate::profile::DeviceProfile;

/// Where MIDI-out bytes go: the `midir` output connection in the app, a collector in
/// tests. The injectable output seam, mirroring [`MidiRuntime::feed`](crate::MidiRuntime::feed)
/// on the input side. Called from the feedback thread and the input thread.
pub type MidiOut = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Whether a handle's state reads as "lit": a toggle by its bool, a level when above
/// the bottom of its range. `None` for triggers and dead handles.
pub fn led_on(handle: &ParamHandle) -> Option<bool> {
    match handle.kind() {
        ParamKind::Toggle => handle.read_bool(),
        ParamKind::Level => handle.read_norm().map(|n| n > 0.0005),
        ParamKind::Trigger => None,
    }
}

/// Send `control`'s LED for `handle`'s current state.
pub fn send_led(out: &MidiOut, device: &DeviceProfile, control: MidiControl, handle: &ParamHandle) {
    if let Some(bytes) = led_on(handle).and_then(|on| device.led_bytes(control, on)) {
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
        targets: Vec<(MidiControl, ParamHandle)>,
        device: Arc<DeviceProfile>,
        out: MidiOut,
    ) -> Option<Feedback> {
        if targets.is_empty() {
            return None;
        }
        // Subscribe BEFORE the initial paint reads state, so a change landing in
        // between is delivered rather than lost.
        let sub = tree.subscribe(Filter::Ids(targets.iter().map(|(_, h)| h.id()).collect()));
        let closer = sub.closer();
        let mut by_param: HashMap<ParamId, Vec<MidiControl>> = HashMap::new();
        for (control, handle) in &targets {
            by_param.entry(handle.id()).or_default().push(*control);
        }
        let thread = std::thread::Builder::new()
            .name("prisma-midi-feedback".into())
            .spawn(move || {
                for (control, handle) in &targets {
                    send_led(&out, &device, *control, handle);
                }
                loop {
                    let changed = sub.recv();
                    if changed.is_empty() {
                        break; // closed: rebinding, unbinding, or the tree is gone
                    }
                    for handle in &changed {
                        for control in by_param.get(&handle.id()).into_iter().flatten() {
                            send_led(&out, &device, *control, handle);
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

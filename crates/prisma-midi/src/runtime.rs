//! The MIDI runtime: active profile + binding + MIDI Learn (M9-SPEC §4, §7).
//!
//! [`MidiRuntime`] is the hardware-independent heart of the MIDI transport. Bytes
//! enter through [`feed`](MidiRuntime::feed) — the injectable seam (§5, §13): the
//! real `midir` adapter calls it from its input callback, tests call it directly.
//!
//! # Lifecycle across recompiles
//!
//! The engine rebuilds its Parameter Tree on every patch/show recompile, so the
//! runtime is **not** tied to one tree. It holds the operator's profile for the life
//! of the process and is re-[`bind`](MidiRuntime::bind)-ed to each new tree; while
//! unbound (mid-recompile) input is dropped, never queued against a dead tree. Open
//! ports therefore survive a recompile untouched.
//!
//! # Feedback (Slice 2)
//!
//! With an output ([`set_output`](MidiRuntime::set_output)) and a bound tree, every
//! (re)build of the binding also restarts a [`Feedback`] thread for the mapped note
//! controls, which paints their LEDs from live state. A control that stops being
//! mapped is darkened. On a note-off the profile may ask for the LED to be re-sent
//! ([`reaffirm_on_release`](crate::profile::LedSpec::reaffirm_on_release)).
//!
//! # Threading
//!
//! `feed` runs on the MIDI input thread (a transport thread, §3.3). It takes the
//! runtime's short state lock and writes the tree, whose write path never blocks on
//! notification (§4.5). The render and output threads never see this lock. The
//! [`MidiListener`] is called **after** the lock is released, so a listener that
//! persists a file or emits a UI event cannot stall the next message's resolution
//! behind it.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use prisma_core::{
    ControlMode, ControllerMappings, MapTarget, MidiControl, MidiMapping, ParameterTree,
    ResponseCurve,
};
use uuid::Uuid;

use crate::feedback::{send_led, Feedback, MidiOut};
use crate::message::{EventKind, MidiEvent};
use crate::profile::{DeviceProfile, LedLevel};
use crate::resolver::Binding;

/// Something the runtime reports to its host (the app), off the lock.
#[derive(Debug, Clone)]
pub enum MidiNotice {
    /// Learn captured a control. `profile` is the whole updated profile, ready to
    /// persist; `mapping` is the binding just created.
    Learned {
        mapping: MidiMapping,
        profile: ControllerMappings,
    },
    /// A mapped control wrote the tree (the UI refreshes what it shows). Emitted for
    /// accepted writes only; the host coalesces. `zone` is the zone slot the control
    /// drives (`None` for globals like the master), so the UI can follow the operator's
    /// hands to that zone.
    Applied {
        control: MidiControl,
        zone: Option<u16>,
    },
}

/// Host callback for [`MidiNotice`]s.
pub type MidiListener = Arc<dyn Fn(MidiNotice) + Send + Sync>;

struct State {
    profile: ControllerMappings,
    /// The running tree and zone order, kept so a profile change can recompile the
    /// binding without the host re-supplying them.
    tree: Option<(Arc<ParameterTree>, Vec<Uuid>)>,
    binding: Option<Binding>,
    learn: Option<MapTarget>,
    out: Option<MidiOut>,
    device: Arc<DeviceProfile>,
    feedback: Option<Feedback>,
    /// The running show's zone display colours by slot (a cell's default LED colour).
    zone_colours: Arc<Vec<[u8; 3]>>,
    /// Note controls the last feedback drove, kept across an unbind so a rebuild can
    /// darken the ones no longer mapped.
    lit: HashSet<MidiControl>,
}

impl State {
    fn rebuild(&mut self) {
        self.feedback = None; // stop the old thread before its binding goes away
        self.binding = self
            .tree
            .as_ref()
            .map(|(tree, zones)| Binding::compile(Arc::clone(tree), zones, &self.profile));
        self.restart_feedback();
    }

    /// Send the device's connect messages (a mode handshake), before any LED.
    fn handshake(&self) {
        if let Some(out) = &self.out {
            for message in &self.device.on_connect {
                out(message);
            }
        }
    }

    /// (Re)start feedback for the current binding and darken controls that lost
    /// their mapping. A no-op without an output or a bound tree.
    fn restart_feedback(&mut self) {
        self.feedback = None;
        let (Some(out), Some(binding), Some((tree, _))) =
            (self.out.clone(), self.binding.as_ref(), self.tree.as_ref())
        else {
            return;
        };
        let targets = binding.feedback_targets();
        let now: HashSet<MidiControl> = targets.iter().map(|t| t.control).collect();
        for gone in self.lit.difference(&now) {
            if let Some(bytes) = self.device.led_bytes(*gone, LedLevel::Off, None, None) {
                out(&bytes);
            }
        }
        self.lit = now;
        self.feedback = Feedback::start(
            tree,
            targets,
            Arc::clone(&self.device),
            Arc::clone(&self.zone_colours),
            out,
        );
    }
}

/// Cheaply cloneable handle to the shared runtime.
#[derive(Clone)]
pub struct MidiRuntime {
    state: Arc<Mutex<State>>,
    listener: Arc<Mutex<Option<MidiListener>>>,
}

impl MidiRuntime {
    pub fn new(profile: ControllerMappings) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                profile,
                tree: None,
                binding: None,
                learn: None,
                out: None,
                device: Arc::new(DeviceProfile::generic()),
                feedback: None,
                zone_colours: Arc::new(Vec::new()),
                lit: HashSet::new(),
            })),
            listener: Arc::new(Mutex::new(None)),
        }
    }

    /// Install (or replace) the host listener.
    pub fn set_listener(&self, listener: Option<MidiListener>) {
        *self.listener.lock() = listener;
    }

    /// Bind to a running tree, with its zones in slot order. Recompiles every
    /// mapping's target against it (slot → uuid → handle, once).
    pub fn bind(&self, tree: Arc<ParameterTree>, zone_ids: Vec<Uuid>) {
        let mut s = self.state.lock();
        s.tree = Some((tree, zone_ids));
        s.rebuild();
    }

    /// Drop the tree reference. Input is ignored until the next [`bind`](Self::bind).
    /// The engine calls this before tearing a tree down, so the runtime never keeps
    /// an old tree (and its notifier thread) alive.
    pub fn unbind(&self) {
        let mut s = self.state.lock();
        s.feedback = None;
        s.tree = None;
        s.binding = None;
    }

    /// Set (or remove) where feedback bytes go. Repaints every mapped LED.
    /// A new output gets the device's connect handshake first.
    pub fn set_output(&self, out: Option<MidiOut>) {
        let mut s = self.state.lock();
        s.feedback = None;
        s.out = out;
        if s.out.is_none() {
            s.lit.clear(); // nothing to darken on a port that is gone
        }
        s.handshake();
        s.restart_feedback();
    }

    /// Set the connected device's profile (LED semantics). Repaints every mapped LED.
    pub fn set_device(&self, device: DeviceProfile) {
        let mut s = self.state.lock();
        s.feedback = None;
        s.device = Arc::new(device);
        s.handshake();
        s.restart_feedback();
    }

    /// The running show's zone display colours, by slot: a grid cell's LED defaults
    /// to its zone's colour. Set before [`bind`](Self::bind) to avoid a double repaint.
    pub fn set_zone_colours(&self, colours: Vec<[u8; 3]>) {
        let mut s = self.state.lock();
        s.feedback = None;
        s.zone_colours = Arc::new(colours);
        s.restart_feedback();
    }

    /// Change how a mapped control behaves and lights (the Learn right-click menu).
    /// Returns the updated profile to persist, or `None` if `target` is not mapped.
    pub fn configure(
        &self,
        target: &MapTarget,
        mode: ControlMode,
        led: Option<prisma_core::LedStyle>,
    ) -> Option<ControllerMappings> {
        let mut s = self.state.lock();
        let mapping = s
            .profile
            .mappings
            .iter_mut()
            .find(|m| &m.target == target)?;
        mapping.mode = mode;
        mapping.led = led;
        s.rebuild();
        Some(s.profile.clone())
    }

    /// The connected device's profile.
    pub fn device(&self) -> DeviceProfile {
        (*self.state.lock().device).clone()
    }

    /// Darken every LED feedback lit and stop following state — for app exit, so the
    /// controller is not left showing a show that is no longer running.
    pub fn clear_leds(&self) {
        let mut s = self.state.lock();
        s.feedback = None;
        if let Some(out) = s.out.clone() {
            for control in std::mem::take(&mut s.lit) {
                if let Some(bytes) = s.device.led_bytes(control, LedLevel::Off, None, None) {
                    out(&bytes);
                }
            }
        }
    }

    pub fn is_bound(&self) -> bool {
        self.state.lock().binding.is_some()
    }

    /// Mappings that resolved against the bound tree (0 when unbound).
    pub fn resolved_count(&self) -> usize {
        self.state
            .lock()
            .binding
            .as_ref()
            .map_or(0, Binding::resolved_count)
    }

    /// A copy of the active profile.
    pub fn profile(&self) -> ControllerMappings {
        self.state.lock().profile.clone()
    }

    /// Replace the active profile (e.g. loaded from disk) and rebind.
    pub fn set_profile(&self, profile: ControllerMappings) {
        let mut s = self.state.lock();
        s.profile = profile;
        s.rebuild();
    }

    /// Remove the mapping for `target`. Returns the updated profile if one was
    /// removed, for the host to persist.
    pub fn unmap(&self, target: &MapTarget) -> Option<ControllerMappings> {
        let mut s = self.state.lock();
        if !s.profile.unbind_target(target) {
            return None;
        }
        s.rebuild();
        Some(s.profile.clone())
    }

    /// Arm Learn for `target`: the next note-on or CC captured becomes its control.
    /// Re-arming replaces the previous armed target.
    pub fn arm_learn(&self, target: MapTarget) {
        self.state.lock().learn = Some(target);
    }

    pub fn cancel_learn(&self) {
        self.state.lock().learn = None;
    }

    /// The target currently waiting for a control, if any.
    pub fn learning(&self) -> Option<MapTarget> {
        self.state.lock().learn.clone()
    }

    /// The input seam: one complete MIDI message from any source.
    pub fn feed(&self, bytes: &[u8]) {
        let Some(event) = MidiEvent::parse(bytes) else {
            return;
        };
        let notice = {
            let mut s = self.state.lock();
            reaffirm_on_release(&s, &event);
            match learn_capture(&mut s, &event) {
                Some(notice) => Some(notice),
                None => s.binding.as_ref().and_then(|b| {
                    let accepted = b.apply(&event)? == prisma_core::WriteOutcome::Accepted;
                    let control = event.control().filter(|_| accepted)?;
                    Some(MidiNotice::Applied {
                        control,
                        zone: b.zone_of(control),
                    })
                }),
            }
        };
        if let Some(notice) = notice {
            let listener = self.listener.lock().clone();
            if let Some(listener) = listener {
                listener(notice);
            }
        }
    }
}

/// The mode a learned control gets: `Toggle` for a latching parameter (a pad on solo
/// latches it; `Absolute` would switch it back off on the pad's next velocity-less
/// event), `Absolute` for everything else.
fn learn_mode(s: &State, target: &MapTarget) -> ControlMode {
    // A grid pad starts as a latch: a tap runs the cell, like clicking it on screen.
    if target.is_cell() {
        return ControlMode::Latch;
    }
    let path = s
        .tree
        .as_ref()
        .and_then(|(_, zones)| target.resolve_path(zones));
    // A strobe engage defaults to a flash: strobing while held.
    if path.as_deref().is_some_and(|p| p.ends_with("/strobe/on")) {
        return ControlMode::Momentary;
    }
    let kind = s
        .tree
        .as_ref()
        .zip(path)
        .and_then(|((tree, _), path)| tree.resolve(&path).map(|h| h.kind()));
    match kind {
        Some(prisma_core::ParamKind::Toggle) => ControlMode::Toggle,
        _ => ControlMode::Absolute,
    }
}

/// The APC40 mkII reverts a pad's LED on the physical release, whatever the host
/// last sent: on a note-off of a mapped control, re-send its LED from state.
fn reaffirm_on_release(s: &State, event: &MidiEvent) {
    if !matches!(event.kind, EventKind::NoteOff { .. }) || !s.device.leds.reaffirm_on_release {
        return;
    }
    let (Some(out), Some(binding), Some(control)) = (&s.out, &s.binding, event.control()) else {
        return;
    };
    if let Some(target) = binding.feedback_of(control) {
        send_led(out, &s.device, &target, &s.zone_colours);
    }
}

/// If Learn is armed and `event` is a capturable gesture, bind it, rebuild, apply
/// the event (so the fader that was just touched takes effect at once) and return
/// the notice. Note-offs are never captured: releasing the pad that armed nothing
/// must not bind, and a note's key is already taken on its note-on.
fn learn_capture(s: &mut State, event: &MidiEvent) -> Option<MidiNotice> {
    let capturable = matches!(event.kind, EventKind::NoteOn { .. } | EventKind::Cc { .. });
    if s.learn.is_none() || !capturable {
        return None;
    }
    let target = s.learn.take()?;
    let mapping = MidiMapping {
        led: None,
        control: event.control()?,
        mode: learn_mode(s, &target),
        target,
        curve: ResponseCurve::Linear,
    };
    s.profile.bind(mapping.clone());
    s.rebuild();
    if let Some(b) = s.binding.as_ref() {
        b.apply(event);
    }
    Some(MidiNotice::Learned {
        mapping,
        profile: s.profile.clone(),
    })
}

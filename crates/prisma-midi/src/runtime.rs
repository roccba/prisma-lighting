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
//! # Banks
//!
//! A device whose profile declares bank buttons ([`BankSpec`]) sees the track grid
//! through a window (8 tracks × 5 rows on an APC40). The bank buttons move it a step at
//! a time, or a whole window with the device's Shift held; they belong to the device and
//! are never learned or mapped. The window position lives
//! here, per runtime (the operator's hands, not the show): it survives recompiles,
//! clamped to the new grid, and every move rebinds so track/scene targets re-resolve.
//!
//! # Device defaults
//!
//! A device profile may ship default mappings. [`seed_device_defaults`] applies them
//! automatically only the first time that device connects to an **empty** profile;
//! [`add_device_defaults`] (the "Cargar mapeos del perfil" button) adds the ones whose
//! control and target are both free, never overwriting.
//!
//! [`seed_device_defaults`]: MidiRuntime::seed_device_defaults
//! [`add_device_defaults`]: MidiRuntime::add_device_defaults
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
    Bank, ControlMode, ControllerMappings, KeyBinding, MapTarget, MidiControl, MidiMapping,
    ParameterTree, ResponseCurve, SlotIds, Takeover,
};

use crate::feedback::{send_led, Feedback, MidiOut};
use crate::message::{EventKind, MidiEvent};
use crate::profile::{BankMove, DeviceProfile, LedLevel};
use crate::resolver::Binding;

/// Something the runtime reports to its host (the app), off the lock.
#[derive(Debug, Clone)]
pub enum MidiNotice {
    /// Learn captured a control. `profile` is the whole updated profile, ready to
    /// persist; `mapping` is the binding just created.
    Learned {
        mapping: MidiMapping,
        profile: Box<ControllerMappings>,
    },
    /// A mapped control wrote the tree (the UI refreshes what it shows). Emitted for
    /// accepted writes only; the host coalesces. `zone` is the zone slot the control
    /// drives (`None` for globals like the master), so the UI can follow the operator's
    /// hands to that zone.
    Applied {
        control: MidiControl,
        zone: Option<u16>,
    },
    /// A bank button moved the controller's grid window.
    Bank(Bank),
}

/// Host callback for [`MidiNotice`]s.
pub type MidiListener = Arc<dyn Fn(MidiNotice) + Send + Sync>;

struct State {
    profile: ControllerMappings,
    /// The running tree and zone order, kept so a profile change can recompile the
    /// binding without the host re-supplying them.
    tree: Option<(Arc<ParameterTree>, SlotIds)>,
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
    /// Where the controller's grid window sits (see the module docs).
    bank: Bank,
    /// The device's Shift button is held: bank presses move a whole window.
    shift: bool,
}

impl State {
    fn rebuild(&mut self) {
        self.feedback = None; // stop the old thread before its binding goes away
        self.bank = self.clamped(self.bank);
        let bank = self.bank;
        let prev = self.binding.take();
        self.binding = self.tree.as_mut().map(|(tree, ids)| {
            ids.bank = bank;
            let mut next = Binding::compile(Arc::clone(tree), ids, &self.profile);
            if let Some(prev) = &prev {
                next.carry_pickup_from(prev);
            }
            next
        });
        self.restart_feedback();
    }

    /// `bank` kept inside the grid: the window never shows fewer tracks or rows than it
    /// fits (unless the grid has fewer).
    fn clamped(&self, bank: Bank) -> Bank {
        let Some((_, ids)) = &self.tree else {
            return bank;
        };
        let (cols, rows) = self.device.window();
        let max_track = (ids.tracks.len() as u16).saturating_sub(cols);
        let max_row = ids.rows.saturating_sub(rows);
        Bank {
            track: bank.track.min(max_track),
            row: bank.row.min(max_row),
        }
    }

    /// Apply a bank button: a step, or a whole window with Shift held. Returns the new
    /// window if it moved.
    fn move_bank(&mut self, m: BankMove) -> Option<Bank> {
        let spec = self.device.surface.bank.as_ref()?;
        let (t, r) = if self.shift {
            self.device.window()
        } else {
            (spec.track_step.max(1), spec.row_step.max(1))
        };
        let b = self.bank;
        let next = match m {
            BankMove::Up => Bank {
                row: b.row.saturating_sub(r),
                ..b
            },
            BankMove::Down => Bank {
                row: b.row.saturating_add(r),
                ..b
            },
            BankMove::Left => Bank {
                track: b.track.saturating_sub(t),
                ..b
            },
            BankMove::Right => Bank {
                track: b.track.saturating_add(t),
                ..b
            },
        };
        let next = self.clamped(next);
        if next == b {
            return None;
        }
        self.bank = next;
        self.rebuild();
        Some(next)
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
        // Continuous controls the device can display (knob rings) get their value too.
        let values = binding
            .value_targets()
            .into_iter()
            .filter(|v| self.device.shows_value(v.control))
            .collect();
        self.feedback = Feedback::start(
            tree,
            targets,
            values,
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
                bank: Bank::default(),
                shift: false,
            })),
            listener: Arc::new(Mutex::new(None)),
        }
    }

    /// Install (or replace) the host listener.
    pub fn set_listener(&self, listener: Option<MidiListener>) {
        *self.listener.lock() = listener;
    }

    /// Bind to a running tree, with its zones and tracks in slot order. Recompiles
    /// every mapping's target against it (slot → uuid → handle, once). The bank window
    /// is kept, clamped to the new grid.
    pub fn bind(&self, tree: Arc<ParameterTree>, ids: impl Into<SlotIds>) {
        let mut s = self.state.lock();
        s.tree = Some((tree, ids.into()));
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

    /// Set the connected device's profile (LED semantics, bank buttons). Rebinds (the
    /// window size may differ) and repaints every mapped LED.
    pub fn set_device(&self, device: DeviceProfile) {
        let mut s = self.state.lock();
        s.feedback = None;
        s.device = Arc::new(device);
        s.handshake();
        s.rebuild();
    }

    /// Where the controller's grid window sits.
    pub fn bank(&self) -> Bank {
        self.state.lock().bank
    }

    /// First connection of a device to an empty profile: apply the device's default
    /// mappings and remember the device, so it never happens again by itself. Returns
    /// the updated profile (to persist) and how many mappings were added; `None` when
    /// nothing was done — the device has no defaults, the profile already has mappings,
    /// or this device was seeded before.
    pub fn seed_device_defaults(&self) -> Option<(ControllerMappings, usize)> {
        let mut s = self.state.lock();
        let name = s.device.name.clone();
        let fresh = s.profile.mappings.is_empty() && !s.profile.seeded.contains(&name);
        if !fresh || s.device.default_mappings.is_empty() {
            return None;
        }
        let added = add_defaults(&mut s);
        Some((s.profile.clone(), added))
    }

    /// Add the device's default mappings whose control and target are both still free
    /// ("Cargar mapeos del perfil"). Never overwrites a learned mapping. Returns the
    /// updated profile and how many were added.
    pub fn add_device_defaults(&self) -> (ControllerMappings, usize) {
        let mut s = self.state.lock();
        let added = add_defaults(&mut s);
        (s.profile.clone(), added)
    }

    /// The operator's keyboard shortcuts.
    pub fn keys(&self) -> Vec<KeyBinding> {
        self.state.lock().profile.keys.clone()
    }

    /// Set one shortcut (`None` = the action has no key), moving the combo off any other
    /// action. Returns the updated profile to persist, or `None` for a menu key.
    pub fn rebind_key(
        &self,
        path: &str,
        combo: Option<prisma_core::KeyCombo>,
    ) -> Option<ControllerMappings> {
        let mut s = self.state.lock();
        s.profile.rebind_key(path, combo).then(|| s.profile.clone())
    }

    /// Back to the default shortcuts. Returns the updated profile to persist.
    pub fn reset_keys(&self) -> ControllerMappings {
        let mut s = self.state.lock();
        s.profile.keys = prisma_core::keys::default_bindings();
        for path in prisma_core::keys::default_actions() {
            if !s.profile.keys_known.contains(&path) {
                s.profile.keys_known.push(path);
            }
        }
        s.profile.clone()
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

    /// Set how a mapped continuous control takes over its value (pickup / jump). Returns
    /// the updated profile to persist, or `None` if `target` is not mapped.
    pub fn set_takeover(
        &self,
        target: &MapTarget,
        takeover: Takeover,
    ) -> Option<ControllerMappings> {
        let mut s = self.state.lock();
        let mapping = s
            .profile
            .mappings
            .iter_mut()
            .find(|m| &m.target == target)?;
        mapping.takeover = takeover;
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
            // Bank buttons belong to the device: never learned, never mapped.
            if let Some(input) = bank_button(&s, &event) {
                match input {
                    BankInput::Move(m) => s.move_bank(m).map(MidiNotice::Bank),
                    BankInput::Shift(held) => {
                        s.shift = held;
                        None
                    }
                }
            } else {
                reaffirm_on_release(&s, &event);
                match learn_capture(&mut s, &event) {
                    Some(notice) => Some(notice),
                    None => s.binding.as_mut().and_then(|b| {
                        let accepted = b.apply(&event)? == prisma_core::WriteOutcome::Accepted;
                        let control = event.control().filter(|_| accepted)?;
                        Some(MidiNotice::Applied {
                            control,
                            zone: b.zone_of(control),
                        })
                    }),
                }
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
    // A grid pad starts as a latch: a tap runs the clip, like playing it on screen.
    if target.is_clip() {
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

/// Add the device's defaults that are free, mark the device seeded, rebind.
fn add_defaults(s: &mut State) -> usize {
    let defaults = s.device.default_mappings.clone();
    let added = defaults
        .into_iter()
        .filter(|m| s.profile.add_if_free(m.clone()))
        .count();
    let name = s.device.name.clone();
    if !s.profile.seeded.contains(&name) {
        s.profile.seeded.push(name);
    }
    if added > 0 {
        s.rebuild();
    }
    added
}

/// What a bank-section button does.
enum BankInput {
    Move(BankMove),
    /// Shift pressed (`true`) or released.
    Shift(bool),
}

/// The bank input a note asks for, if the device has bank buttons and it is one: a bank
/// press (consumed even at the edge of the grid, where nothing moves), or Shift going
/// down or up. A bank button's release needs no handling: Learn never captures a release
/// and a bank note is never mapped.
fn bank_button(s: &State, event: &MidiEvent) -> Option<BankInput> {
    let spec = s.device.surface.bank.as_ref()?;
    let c = event.control()?;
    let prisma_core::ControlKind::Note(n) = c.kind else {
        return None;
    };
    let pressed = match event.kind {
        EventKind::NoteOn { .. } => true,
        EventKind::NoteOff { .. } => false,
        _ => return None,
    };
    if c.channel == spec.channel && spec.shift_note == Some(n) {
        return Some(BankInput::Shift(pressed));
    }
    if !pressed {
        return None;
    }
    spec.move_for(c.channel, n).map(BankInput::Move)
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
/// the event (so a pad that was just pressed takes effect at once) and return the
/// notice. A continuous control learns under soft takeover like any other message: if
/// the fader was touched far from the value it does NOT jump it there — it picks the
/// value up as soon as it meets it. Note-offs are never captured: releasing the pad that armed nothing
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
        takeover: Takeover::Pickup,
    };
    s.profile.bind(mapping.clone());
    s.rebuild();
    if let Some(b) = s.binding.as_mut() {
        b.apply(event);
    }
    Some(MidiNotice::Learned {
        mapping,
        profile: Box::new(s.profile.clone()),
    })
}

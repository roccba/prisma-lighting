//! Mapping resolution: a [`ControllerMappings`] compiled against one running tree
//! (M9-SPEC §7).
//!
//! Targets resolve to [`ParamHandle`]s **once**, when a [`Binding`] is compiled —
//! at mapping load, on a Learn, or when the zone set changes — never per message.
//! Per message the input path is a hash lookup on the control and one tree write.
//!
//! # Soft takeover
//!
//! A continuous control (fader / knob, `Absolute` mode, on a level parameter) under
//! [`Takeover::Pickup`] is ignored until it meets the parameter's current value: within
//! [`PICKUP_STEPS`] MIDI steps, or by passing through it between two messages. Until then
//! its messages are dropped, so a controller that reports every position on connect (the
//! APC40 entering Ableton mode), or a fader left somewhere else while the UI moved the
//! value, never makes the rig jump. Once engaged the control follows; if anything else
//! changes the value afterwards (the UI, a show load, another control), it lets go and
//! must pick the value up again. The state lives in the [`Binding`]; a rebind keeps it
//! for controls that still drive the same parameter of the same tree (a bank move, a
//! Learn elsewhere), and a recompile starts every control disengaged — the values may
//! have changed.

use std::collections::HashMap;
use std::sync::Arc;

use crate::message::{EventKind, MidiEvent};
use prisma_core::{
    priority, ControlKind, ControlMode, ControllerMappings, LedStyle, MidiControl, ParamHandle,
    ParamKind, ParameterTree, ResponseCurve, SlotIds, SourceId, Takeover, WriteOutcome,
};

/// One mapping with its target already resolved.
struct Resolved {
    handle: ParamHandle,
    /// What the control's LED shows: a grid clip's `state`, otherwise `handle` itself.
    feedback: ParamHandle,
    /// Whether `feedback` is a clip state (empty / loaded / live / armed).
    cell: bool,
    led: Option<LedStyle>,
    mode: ControlMode,
    curve: ResponseCurve,
    takeover: Takeover,
    /// The zone slot this target belongs to, if any — resolved once here so the host
    /// can follow a control to its zone (UI focus) without re-parsing per message.
    zone: Option<u16>,
}

/// One note control's LED, as the feedback engine needs it.
#[derive(Clone)]
pub struct FeedbackTarget {
    pub control: MidiControl,
    /// The parameter whose value the LED shows.
    pub handle: ParamHandle,
    /// `handle` is a grid clip's state (four levels), not an on/off value.
    pub cell: bool,
    pub led: Option<LedStyle>,
    /// The zone slot the control drives (a clip: its track's first destination zone),
    /// for a default colour.
    pub zone: Option<u16>,
}

/// One continuous control whose position the device can display (a knob's LED ring),
/// as the value-feedback engine needs it.
#[derive(Clone)]
pub struct ValueTarget {
    pub control: MidiControl,
    /// The parameter whose value the control shows.
    pub handle: ParamHandle,
    /// The mapping's curve: feedback sends the position that produces the value.
    pub curve: ResponseCurve,
}

impl ValueTarget {
    /// The 7-bit position that shows the parameter's current value, if it can be read.
    pub fn position(&self) -> Option<u8> {
        let d = self.handle.descriptor();
        let span = d.range.max - d.range.min;
        if span <= 0.0 {
            return None;
        }
        let real = self.handle.read_real()?;
        let pos = self.curve.invert((real - d.range.min) / span);
        Some((pos * 127.0).round().clamp(0.0, 127.0) as u8)
    }
}

/// How close (in MIDI steps of the control) a disengaged control must come to the
/// current value to pick it up without crossing it. A step and a half absorbs the
/// rounding between the 7-bit position and a value set elsewhere (the UI, a show).
pub const PICKUP_STEPS: f32 = 1.5;

/// One control's soft-takeover state.
#[derive(Debug, Default, Clone, Copy)]
struct Pickup {
    /// The control drives the value (it met it, and nothing else moved it since).
    engaged: bool,
    /// The real value the control's previous message stood for, engaged or not — two
    /// consecutive positions on either side of the value mean the control crossed it.
    last_pos: Option<f32>,
    /// The real value this control last wrote; the parameter no longer holding it means
    /// something else changed it, and the control lets go.
    last_written: Option<f32>,
}

impl Pickup {
    /// Whether a message standing for `real` may write a parameter currently at `cur`
    /// (range `span`). Updates the state either way.
    fn allows(&mut self, real: f32, cur: f32, span: f32) -> bool {
        // Float round-trips through the tree's normalized storage: far below a MIDI step.
        let moved_elsewhere = self
            .last_written
            .is_none_or(|w| (w - cur).abs() > span * 1e-3);
        if self.engaged && moved_elsewhere {
            self.engaged = false;
        }
        if !self.engaged {
            let near = (real - cur).abs() <= span * PICKUP_STEPS / 127.0;
            let crossed = self
                .last_pos
                .is_some_and(|prev| (prev - cur) * (real - cur) <= 0.0);
            self.engaged = near || crossed;
        }
        self.last_pos = Some(real);
        self.engaged
    }
}

/// A profile compiled against a specific tree and zone order.
pub struct Binding {
    tree: Arc<ParameterTree>,
    table: HashMap<MidiControl, Resolved>,
    /// Soft-takeover state per continuous control (see the module docs).
    pickup: HashMap<MidiControl, Pickup>,
}

/// The arbitration identity of a control: MIDI priority, and a tag that encodes the
/// control so the UI can say which one holds a parameter (§4.5). Layout:
/// bit 16 = note (else CC), bits 8..12 = channel, bits 0..7 = number.
pub fn source_for(control: MidiControl) -> SourceId {
    let (is_note, number) = match control.kind {
        ControlKind::Note(n) => (1u32, n),
        ControlKind::Cc(n) => (0u32, n),
    };
    SourceId::new(
        priority::MIDI,
        is_note << 16 | (control.channel as u32) << 8 | number as u32,
    )
}

impl Binding {
    /// Resolve every mapping against `tree`. Mappings whose target does not exist in
    /// this tree (a zone slot past the end, a path nothing registered) are skipped —
    /// they stay in the profile and bind again on a Show where they make sense.
    pub fn compile(
        tree: Arc<ParameterTree>,
        ids: &SlotIds,
        mappings: &ControllerMappings,
    ) -> Binding {
        let mut table = HashMap::with_capacity(mappings.mappings.len());
        for m in &mappings.mappings {
            let Some(path) = m.target.resolve_path_for(ids, m.mode) else {
                continue;
            };
            let Some(handle) = tree.resolve(&path) else {
                continue;
            };
            let state = m.target.feedback_path(ids).and_then(|p| tree.resolve(&p));
            table.insert(
                m.control,
                Resolved {
                    cell: state.is_some(),
                    feedback: state.unwrap_or_else(|| handle.clone()),
                    led: m.led,
                    handle,
                    mode: m.mode,
                    curve: m.curve,
                    takeover: m.takeover,
                    zone: m.target.zone_slot(ids),
                },
            );
        }
        Binding {
            tree,
            table,
            pickup: HashMap::new(),
        }
    }

    /// Keep `prev`'s soft-takeover state for every control that still drives the same
    /// parameter of the same tree — a bank move or a Learn elsewhere must not make a
    /// knob that did not change target pick its value up again. A new tree (a
    /// recompile) or a new target starts disengaged, as the values may differ.
    pub fn carry_pickup_from(&mut self, prev: &Binding) {
        if !Arc::ptr_eq(&self.tree, &prev.tree) {
            return;
        }
        for (control, state) in &prev.pickup {
            let same = match (self.table.get(control), prev.table.get(control)) {
                (Some(now), Some(before)) => now.handle.id() == before.handle.id(),
                _ => false,
            };
            if same {
                self.pickup.insert(*control, *state);
            }
        }
    }

    /// The zone slot `control` drives, if it is mapped to a zone's parameter.
    pub fn zone_of(&self, control: MidiControl) -> Option<u16> {
        self.table.get(&control).and_then(|r| r.zone)
    }

    /// A button press/release in `mode`. Every mode fires a trigger target on press;
    /// they differ on held values:
    /// - toggle: press flips; release nothing.
    /// - latch: press sets on; release nothing.
    /// - trigger: press is one event — flips a toggle, sets a level to max.
    /// - momentary: press sets on, release sets off.
    fn button(
        &self,
        r: &Resolved,
        control: MidiControl,
        mode: ControlMode,
        pressed: bool,
    ) -> Option<WriteOutcome> {
        let src = source_for(control);
        let range = r.handle.descriptor().range;
        if r.handle.kind() == ParamKind::Trigger {
            return pressed.then(|| self.tree.fire(&r.handle, src));
        }
        match (mode, pressed) {
            (ControlMode::Toggle, true) => self.toggle(r, control),
            (ControlMode::Trigger, true) if r.handle.kind() == ParamKind::Toggle => {
                self.toggle(r, control)
            }
            (ControlMode::Latch | ControlMode::Trigger | ControlMode::Momentary, true) => {
                Some(self.tree.write_real(&r.handle, range.max, src))
            }
            (ControlMode::Momentary, false) => {
                Some(self.tree.write_real(&r.handle, range.min, src))
            }
            _ => None,
        }
    }

    /// Flip the target between its range ends: a toggle by its bool, a level by which
    /// half it sits in (so a pad can switch a dimmer full/off). Triggers fire.
    fn toggle(&self, r: &Resolved, control: MidiControl) -> Option<WriteOutcome> {
        let src = source_for(control);
        let d = r.handle.descriptor();
        let on = match r.handle.kind() {
            ParamKind::Trigger => return Some(self.tree.fire(&r.handle, src)),
            ParamKind::Toggle => r.handle.read_bool()?,
            ParamKind::Level => r.handle.read_norm()? >= 0.5,
        };
        let real = if on { d.range.min } else { d.range.max };
        Some(self.tree.write_real(&r.handle, real, src))
    }

    /// The mapped **note** controls with the handle each one shows, for LED feedback.
    /// CC controls (faders, knobs) are left out: without motors they have nothing to
    /// display. Triggers are left out too — a fire has no lasting state to light.
    pub fn feedback_targets(&self) -> Vec<FeedbackTarget> {
        self.table
            .keys()
            .filter_map(|c| self.feedback_of(*c))
            .collect()
    }

    /// A control's LED target, if it has one (see [`feedback_targets`](Self::feedback_targets)).
    pub fn feedback_of(&self, control: MidiControl) -> Option<FeedbackTarget> {
        let r = self.table.get(&control)?;
        let shows =
            matches!(control.kind, ControlKind::Note(_)) && r.feedback.kind() != ParamKind::Trigger;
        shows.then(|| FeedbackTarget {
            control,
            handle: r.feedback.clone(),
            cell: r.cell,
            led: r.led,
            zone: r.zone,
        })
    }

    /// The mapped continuous controls (CC in `Absolute` mode on a level parameter), for
    /// value feedback — the host keeps the ones its device can display (knob rings).
    pub fn value_targets(&self) -> Vec<ValueTarget> {
        self.table
            .iter()
            .filter(|(c, r)| {
                matches!(c.kind, ControlKind::Cc(_))
                    && r.mode == ControlMode::Absolute
                    && r.handle.kind() == ParamKind::Level
            })
            .map(|(c, r)| ValueTarget {
                control: *c,
                handle: r.handle.clone(),
                curve: r.curve,
            })
            .collect()
    }

    /// How many mappings resolved against this tree.
    pub fn resolved_count(&self) -> usize {
        self.table.len()
    }

    /// Apply one event. `None` when nothing is mapped to its control, the event
    /// carries no value in the mapping's mode (e.g. a note-off under `Absolute`), or a
    /// continuous control has not picked up its value yet (soft takeover).
    pub fn apply(&mut self, event: &MidiEvent) -> Option<WriteOutcome> {
        let control = event.control()?;
        let r = self.table.get(&control)?;
        let raw = match (r.mode, event.kind) {
            (ControlMode::Absolute, EventKind::Cc { value, .. }) => value,
            (ControlMode::Absolute, EventKind::NoteOn { velocity, .. }) => velocity,
            (ControlMode::Absolute, EventKind::NoteOff { .. } | EventKind::PitchBend { .. }) => {
                return None
            }
            // The button modes act on press and (momentary only) release. A CC button
            // presses above 0 and releases at 0.
            (mode, kind) => {
                let pressed = match kind {
                    EventKind::NoteOn { .. } => true,
                    EventKind::Cc { value, .. } => value > 0,
                    EventKind::NoteOff { .. } => false,
                    EventKind::PitchBend { .. } => return None,
                };
                return self.button(r, control, mode, pressed);
            }
        };
        let position = r.curve.apply(raw as f32 / 127.0);
        let src = source_for(control);
        Some(match r.handle.kind() {
            // A trigger has no position: any non-zero value is one fire.
            ParamKind::Trigger if raw == 0 => return None,
            ParamKind::Trigger => self.tree.fire(&r.handle, src),
            // The control's position maps LINEARLY onto the parameter's real range —
            // the same way the UI fader writes (and a show seeds) it. Writing `position`
            // as the tree's normalized value instead would push it through the
            // descriptor curve (x^2.2 for dimmers), so a hardware fader at 50% gave a
            // 22% dimmer while the UI fader at 50% gave 50%: the low half of the
            // physical fader did almost nothing. Any shaping is the mapping's `curve`.
            ParamKind::Level | ParamKind::Toggle => {
                let d = r.handle.descriptor();
                let span = d.range.max - d.range.min;
                let mut real = d.range.min + position * span;
                // 7-bit has no exact centre (63 → -0.08, 64 → +0.08 on a ±10 speed), so a
                // bipolar parameter's detent is what makes "fader in the middle" static.
                if d.centre_detent.is_some_and(|w| real.abs() < w) {
                    real = 0.0;
                }
                // Soft takeover: a level waits until the control meets its value.
                let pickup = r.takeover == Takeover::Pickup && r.handle.kind() == ParamKind::Level;
                if pickup {
                    let cur = r.handle.read_real().unwrap_or(real);
                    let state = self.pickup.entry(control).or_default();
                    if !state.allows(real, cur, span) {
                        return None;
                    }
                }
                let outcome = self.tree.write_real(&r.handle, real, src);
                if pickup {
                    let state = self.pickup.entry(control).or_default();
                    // A refused write (another source owns the value) means it is not ours.
                    state.last_written = (outcome == WriteOutcome::Accepted).then_some(real);
                }
                outcome
            }
        })
    }
}

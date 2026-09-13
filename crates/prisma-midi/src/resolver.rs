//! Mapping resolution: a [`ControllerMappings`] compiled against one running tree
//! (M9-SPEC §7).
//!
//! Targets resolve to [`ParamHandle`]s **once**, when a [`Binding`] is compiled —
//! at mapping load, on a Learn, or when the zone set changes — never per message.
//! Per message the input path is a hash lookup on the control and one tree write.

use std::collections::HashMap;
use std::sync::Arc;

use prisma_core::{
    priority, ControlKind, ControlMode, ControllerMappings, LedStyle, MapTarget, MidiControl,
    ParamHandle, ParamKind, ParameterTree, ResponseCurve, SourceId, WriteOutcome,
};
use uuid::Uuid;

use crate::message::{EventKind, MidiEvent};

/// One mapping with its target already resolved.
struct Resolved {
    handle: ParamHandle,
    /// What the control's LED shows: a grid cell's `state`, otherwise `handle` itself.
    feedback: ParamHandle,
    /// Whether `feedback` is a cell state (empty / assigned / running).
    cell: bool,
    led: Option<LedStyle>,
    mode: ControlMode,
    curve: ResponseCurve,
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
    /// `handle` is a grid cell's state (three levels), not an on/off value.
    pub cell: bool,
    pub led: Option<LedStyle>,
    /// The zone slot the control drives, for a default colour.
    pub zone: Option<u16>,
}

/// A profile compiled against a specific tree and zone order.
pub struct Binding {
    tree: Arc<ParameterTree>,
    table: HashMap<MidiControl, Resolved>,
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

/// The zone slot a target belongs to: a [`MapTarget::ZoneSlot`] directly, or a literal
/// `/zone/{uuid}/…` path by that uuid's position. Globals (`/master/*`, `/clock/*`)
/// belong to no zone.
fn zone_slot_of(target: &MapTarget, zone_ids: &[Uuid]) -> Option<u16> {
    match target {
        MapTarget::ZoneSlot { slot, .. } => Some(*slot),
        MapTarget::Path(path) => {
            let id = path.strip_prefix("/zone/")?.split('/').next()?;
            let id = Uuid::parse_str(id).ok()?;
            zone_ids.iter().position(|z| *z == id).map(|i| i as u16)
        }
    }
}

impl Binding {
    /// Resolve every mapping against `tree`. Mappings whose target does not exist in
    /// this tree (a zone slot past the end, a path nothing registered) are skipped —
    /// they stay in the profile and bind again on a Show where they make sense.
    pub fn compile(
        tree: Arc<ParameterTree>,
        zone_ids: &[Uuid],
        mappings: &ControllerMappings,
    ) -> Binding {
        let mut table = HashMap::with_capacity(mappings.mappings.len());
        for m in &mappings.mappings {
            let Some(path) = m.target.resolve_path_for(zone_ids, m.mode) else {
                continue;
            };
            let Some(handle) = tree.resolve(&path) else {
                continue;
            };
            let state = m
                .target
                .feedback_path(zone_ids)
                .and_then(|p| tree.resolve(&p));
            table.insert(
                m.control,
                Resolved {
                    cell: state.is_some(),
                    feedback: state.unwrap_or_else(|| handle.clone()),
                    led: m.led,
                    handle,
                    mode: m.mode,
                    curve: m.curve,
                    zone: zone_slot_of(&m.target, zone_ids),
                },
            );
        }
        Binding { tree, table }
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

    /// How many mappings resolved against this tree.
    pub fn resolved_count(&self) -> usize {
        self.table.len()
    }

    /// Apply one event. `None` when nothing is mapped to its control or the event
    /// carries no value in the mapping's mode (e.g. a note-off under `Absolute`).
    pub fn apply(&self, event: &MidiEvent) -> Option<WriteOutcome> {
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
                let mut real = d.range.min + position * (d.range.max - d.range.min);
                // 7-bit has no exact centre (63 → -0.08, 64 → +0.08 on a ±10 speed), so a
                // bipolar parameter's detent is what makes "fader in the middle" static.
                if d.centre_detent.is_some_and(|w| real.abs() < w) {
                    real = 0.0;
                }
                self.tree.write_real(&r.handle, real, src)
            }
        })
    }
}

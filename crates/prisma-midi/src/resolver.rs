//! Mapping resolution: a [`ControllerMappings`] compiled against one running tree
//! (M9-SPEC §7).
//!
//! Targets resolve to [`ParamHandle`]s **once**, when a [`Binding`] is compiled —
//! at mapping load, on a Learn, or when the zone set changes — never per message.
//! Per message the input path is a hash lookup on the control and one tree write.

use std::collections::HashMap;
use std::sync::Arc;

use prisma_core::{
    priority, ControlKind, ControlMode, ControllerMappings, MapTarget, MidiControl, ParamHandle,
    ParamKind, ParameterTree, ResponseCurve, SourceId, WriteOutcome,
};
use uuid::Uuid;

use crate::message::{EventKind, MidiEvent};

/// One mapping with its target already resolved.
struct Resolved {
    handle: ParamHandle,
    mode: ControlMode,
    curve: ResponseCurve,
    /// The zone slot this target belongs to, if any — resolved once here so the host
    /// can follow a control to its zone (UI focus) without re-parsing per message.
    zone: Option<u16>,
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
            let Some(path) = m.target.resolve_path(zone_ids) else {
                continue;
            };
            let Some(handle) = tree.resolve(&path) else {
                continue;
            };
            table.insert(
                m.control,
                Resolved {
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
    pub fn feedback_targets(&self) -> Vec<(MidiControl, ParamHandle)> {
        self.table
            .iter()
            .filter(|(c, r)| {
                matches!(c.kind, ControlKind::Note(_)) && r.handle.kind() != ParamKind::Trigger
            })
            .map(|(c, r)| (*c, r.handle.clone()))
            .collect()
    }

    /// The handle a control drives, if mapped.
    pub fn handle_of(&self, control: MidiControl) -> Option<&ParamHandle> {
        self.table.get(&control).map(|r| &r.handle)
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
            // A press flips; the release (note-off / CC back to 0) does nothing.
            (ControlMode::Toggle, EventKind::NoteOn { .. }) => return self.toggle(r, control),
            (ControlMode::Toggle, EventKind::Cc { value, .. }) if value > 0 => {
                return self.toggle(r, control)
            }
            (ControlMode::Toggle, _) => return None,
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

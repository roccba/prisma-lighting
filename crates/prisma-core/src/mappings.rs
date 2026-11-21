//! The Controller Mappings document (CLAUDE.md §4.6, M9-SPEC §6).
//!
//! The third top-level document, beside the Patch and the Show. It binds physical
//! control-surface controls to Parameter Tree targets and belongs to the
//! **operator and their hardware**, not to any Show: one profile follows the person
//! at the console across every Show, and opening someone else's Show never clobbers
//! the controller (§3.1 principle 6).
//!
//! It lives in `prisma-core` because it references parameter paths — a core concept
//! — and mirrors how the Patch and the Show are documents here; `prisma-midi`
//! consumes it (M9-SPEC §6).
//!
//! # Target portability (M9-SPEC §6.1)
//!
//! A raw `/zone/{uuid}/dimmer` path embeds a Show-specific UUID and would not
//! survive opening another Show, so a target is one of:
//!
//! - [`MapTarget::Path`] — a literal, Show-agnostic path (`/master/dimmer`,
//!   `/clock/tap`). Globals map this way: their ids are stable.
//! - [`MapTarget::ZoneSlot`] — positional: slot 0 is the *first* zone of whichever
//!   Show is running, resolved to its uuid at bind time. "Fader 1 = zone 1" keeps
//!   meaning the first zone across Shows.
//!
//! # What Slice 1 carries, and what is additive later
//!
//! [`ControlMode`] has `Absolute`, `Toggle` (Slice 2) and `Latch`/`Trigger`/`Momentary`
//! (Slice 3); [`ZoneSub`] covers the zone controls and grid cells, and a mapping may
//! carry an [`LedStyle`]. Relative mode, colour and effect-parameter slots arrive in later slices as new
//! enum variants / defaulted fields — each is a schema *addition*, readable by this
//! loader's successors without a migration. Variants are not declared ahead of the
//! code that honours them, so a profile never contains a mode nothing implements.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The `format` tag every controller-mappings file carries.
pub const MAPPINGS_FORMAT_ID: &str = "prisma.controller-mappings";

/// The newest schema version this build writes and can read up to.
///
/// - **v1** — M9 Slice 1: absolute mappings to literal paths and zone slots.
/// - **v2** — M9 Slice 2: `toggle` mode. Purely additive (every v1 file is a valid v2
///   file), so there is no migration; the bump only stops an older build from
///   misreading a toggle mapping it cannot honour.
/// - **v3** — M9 Slice 3: grid cells as targets (`ZoneSub::Cell`), `latch` /
///   `trigger` / `momentary` modes, zone strobe targets, and a per-mapping LED style.
///   Additive again.
/// - **v4** — soft takeover: a per-mapping [`Takeover`] (`pickup` / `jump`). Additive: a
///   file without it reads as `pickup`, the new default for every continuous control.
pub const MAPPINGS_SCHEMA_VERSION: u32 = 4;

/// A per-operator/machine controller profile (CLAUDE.md §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControllerMappings {
    pub id: Uuid,
    /// Human name, e.g. "Bauti — APC40". Operator-authored data, not a UI string.
    pub name: String,
    /// Which device profile (`/profiles/<name>.json`) seeds and decodes this
    /// device, if any. `None` for a purely learned, device-agnostic profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<String>,
    #[serde(default)]
    pub mappings: Vec<MidiMapping>,
}

/// One binding: a physical control → a tree target, with how its value is read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    /// The physical source.
    pub control: MidiControl,
    /// Where it writes in the tree.
    pub target: MapTarget,
    #[serde(default)]
    pub mode: ControlMode,
    /// Shaping of the control's position before it is mapped onto the target's real
    /// range. Distinct from `Layer::MappingMode` (texture → geometry). With `Linear`
    /// the hardware control responds exactly like the UI control for the same target
    /// (fader at 50% = dimmer 0.5): the position maps linearly onto the real range,
    /// it is not fed through the descriptor's curve a second time.
    #[serde(default)]
    pub curve: ResponseCurve,
    /// How this control's LED shows its target, chosen by the operator. `None` =
    /// the device profile's defaults (a cell lights in its zone's colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led: Option<LedStyle>,
    /// How a continuous control (fader / knob in `Absolute` mode) takes over a
    /// parameter whose value it does not match. Ignored by buttons and pads.
    #[serde(default)]
    pub takeover: Takeover,
}

/// Soft takeover for continuous controls (Ableton's "Takeover Mode").
///
/// A physical fader or knob and the value it drives can disagree: the controller was
/// just connected (the APC40 sends every position when it enters Ableton mode), the UI
/// moved the value, a show was loaded. With `Jump` the next message from the control
/// slams the parameter to the control's position — a visible jump on the rig. With
/// `Pickup` the control is ignored until it reaches the parameter's current value
/// (within a step) or passes through it; from then on it follows. If something else
/// changes the value afterwards, the control lets go and must pick it up again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
    /// Wait until the control meets the current value (the default).
    #[default]
    Pickup,
    /// The control's position always wins at once.
    Jump,
}

/// An operator-chosen LED look for one control (M9 Slice 3). Colours are indices
/// into the device's palette (the APC40 mkII's 128 RGB colours); a device without
/// one ignores them and just lights or darkens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LedStyle {
    /// Colour while the target is on / the cell is running. `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<u8>,
    /// Colour while a cell holds an effect but is not running. `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<u8>,
    /// How the "on" colour moves.
    #[serde(default)]
    pub animation: LedAnimation,
}

/// LED motion for the "on" state. The rate is a note value of the device's tempo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedAnimation {
    #[default]
    Solid,
    Pulse(LedRate),
    Blink(LedRate),
}

/// Animation rate as a note value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedRate {
    /// 1/24 note
    N24,
    /// 1/16 note
    N16,
    /// 1/8 note
    N8,
    /// 1/4 note
    N4,
    /// 1/2 note
    N2,
}

/// A physical MIDI control: channel (0-based, `0..=15`) + what it sends.
///
/// Device identity is not part of the key yet: Slice 1 opens one input. Multiple
/// simultaneous controllers are a later concern (M9-SPEC §15) and would add a
/// defaulted `device` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MidiControl {
    pub channel: u8,
    pub kind: ControlKind,
}

/// What a control sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// A note (pad / button): note number `0..=127`.
    Note(u8),
    /// A control change (fader / knob): controller number `0..=127`.
    Cc(u8),
}

/// Where a mapping writes (M9-SPEC §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapTarget {
    /// A literal tree path. Stable for globals; also the "learn to any exact path"
    /// escape hatch.
    Path(String),
    /// A zone control by position in the running Show's zone order.
    ZoneSlot { slot: u16, sub: ZoneSub },
}

/// The per-zone controls addressable by slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneSub {
    Dimmer,
    Speed,
    Solo,
    Blackout,
    /// The zone strobe's engage toggle (`strobe/on`).
    StrobeOn,
    /// The zone strobe's speed (`strobe/rate`, 1–25 Hz).
    StrobeRate,
    /// One cell of the zone's scene grid (M9 Slice 3) — since the live model,
    /// clip `i` of the zone's home track (phase 1 bridge, see [`SlotIds`]). Which of
    /// the clip's parameters a control drives depends on its mode — see
    /// [`MapTarget::resolve_path_for`].
    Cell(u16),
}

impl ZoneSub {
    /// The leaf under `/zone/{uuid}/` this control writes. `None` for
    /// [`ZoneSub::Cell`], which lives under the zone's home track instead (see
    /// [`MapTarget::resolve_path_for`]).
    pub fn leaf(self) -> Option<&'static str> {
        match self {
            ZoneSub::Dimmer => Some("dimmer"),
            ZoneSub::Speed => Some("speed"),
            ZoneSub::Solo => Some("solo"),
            ZoneSub::Blackout => Some("blackout"),
            ZoneSub::StrobeOn => Some("strobe/on"),
            ZoneSub::StrobeRate => Some("strobe/rate"),
            ZoneSub::Cell(_) => None,
        }
    }
}

/// The running Show's slot coordinates, resolved at bind time: zone ids in slot
/// order and, per zone slot, the id of that zone's home track
/// ([`Show::home_track`](crate::show::Show::home_track)).
///
/// **Phase 1 bridge** (docs/LIVE-MODEL-SPEC.md §8): `ZoneSub::Cell(i)` on zone slot
/// `n` drives clip `i` of `tracks[n]`. Phase 3 replaces it with track-slot targets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotIds {
    pub zones: Vec<Uuid>,
    /// Parallel to `zones`; `None` when a zone has no home track.
    pub tracks: Vec<Option<Uuid>>,
}

impl From<Vec<Uuid>> for SlotIds {
    /// Zones only, no tracks: cell targets resolve to nothing.
    fn from(zones: Vec<Uuid>) -> SlotIds {
        SlotIds {
            zones,
            tracks: Vec::new(),
        }
    }
}

/// The cell parameter a pad in `mode` drives: latch/absolute → `launch`, trigger →
/// `trigger` (restarts), toggle/momentary → `hold` (returns to what ran before).
fn cell_leaf(mode: ControlMode) -> &'static str {
    match mode {
        ControlMode::Latch | ControlMode::Absolute => "launch",
        ControlMode::Trigger => "trigger",
        ControlMode::Toggle | ControlMode::Momentary => "hold",
    }
}

impl MapTarget {
    /// The concrete tree path this target addresses in the running Show. `None`
    /// when a slot is past the end (a mapping for zone 8 on a 4-zone Show simply
    /// does nothing) or a cell's zone has no home track.
    pub fn resolve_path(&self, ids: &SlotIds) -> Option<String> {
        self.resolve_path_for(ids, ControlMode::Absolute)
    }

    /// Like [`resolve_path`](Self::resolve_path), for a control in `mode` (a grid
    /// cell's parameter depends on it).
    pub fn resolve_path_for(&self, ids: &SlotIds, mode: ControlMode) -> Option<String> {
        match self {
            MapTarget::Path(p) => Some(p.clone()),
            MapTarget::ZoneSlot {
                slot,
                sub: ZoneSub::Cell(i),
            } => {
                let track = ids.tracks.get(*slot as usize).copied().flatten()?;
                Some(format!("/track/{track}/clip/{i}/{}", cell_leaf(mode)))
            }
            MapTarget::ZoneSlot { slot, sub } => {
                let zone = ids.zones.get(*slot as usize)?;
                Some(format!("/zone/{zone}/{}", sub.leaf()?))
            }
        }
    }

    /// The path this target's LED shows, if it differs from what it writes (a grid
    /// cell shows its clip's `state`).
    pub fn feedback_path(&self, ids: &SlotIds) -> Option<String> {
        match self {
            MapTarget::ZoneSlot {
                slot,
                sub: ZoneSub::Cell(i),
            } => {
                let track = ids.tracks.get(*slot as usize).copied().flatten()?;
                Some(format!("/track/{track}/clip/{i}/state"))
            }
            _ => None,
        }
    }

    /// Whether this is a scene-grid cell.
    pub fn is_cell(&self) -> bool {
        matches!(
            self,
            MapTarget::ZoneSlot {
                sub: ZoneSub::Cell(_),
                ..
            }
        )
    }
}

/// How a control's value is interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// The control's position *is* the value: `0..=127` → `0.0..=1.0`.
    #[default]
    Absolute,
    /// Each press (note-on, or a CC rising above 0) flips the target between off and
    /// on; releases are ignored. The mode for a pad/button driving a latching
    /// parameter (zone solo / mute), whose LED then shows the state (Slice 2). On a
    /// grid cell: one tap runs it, the next returns to what ran before.
    Toggle,
    /// A press makes the target on and it stays: on a grid cell, the cell becomes the
    /// zone's content, exactly like clicking it on screen (Slice 3).
    Latch,
    /// A press is one instantaneous event; how long it is held does not matter. On a
    /// grid cell it runs the cell and restarts its effect from the beginning.
    Trigger,
    /// On while held, off on release. On a grid cell: runs while held, and the zone
    /// returns to what ran before on release (a "flash").
    Momentary,
}

/// Shaping of the incoming normalized value (`0..=1` → `0..=1`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseCurve {
    #[default]
    Linear,
    /// `x^gamma` — more travel at the low end.
    Exponential { gamma: f32 },
    /// `x^(1/gamma)` — more travel at the high end.
    Logarithmic { gamma: f32 },
}

impl ResponseCurve {
    /// The control position that produces `y` — the inverse of [`apply`](Self::apply),
    /// so feedback can show a parameter's value on a control (a knob's LED ring).
    pub fn invert(self, y: f32) -> f32 {
        let y = y.clamp(0.0, 1.0);
        match self {
            ResponseCurve::Linear => y,
            ResponseCurve::Exponential { gamma } if gamma > 0.0 => y.powf(1.0 / gamma),
            ResponseCurve::Logarithmic { gamma } if gamma > 0.0 => y.powf(gamma),
            _ => y,
        }
    }

    /// Apply the curve to a normalized value (clamped to `0..=1`).
    pub fn apply(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            ResponseCurve::Linear => x,
            ResponseCurve::Exponential { gamma } if gamma > 0.0 => x.powf(gamma),
            ResponseCurve::Logarithmic { gamma } if gamma > 0.0 => x.powf(1.0 / gamma),
            // A non-positive gamma is a malformed file; fall back to linear rather
            // than produce NaN/inf on the write path.
            _ => x,
        }
    }
}

impl ControllerMappings {
    /// An empty profile with a fresh id.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            device_profile: None,
            mappings: Vec::new(),
        }
    }

    /// The mapping driven by `control`, if any.
    pub fn find(&self, control: MidiControl) -> Option<&MidiMapping> {
        self.mappings.iter().find(|m| m.control == control)
    }

    /// The mapping writing `target`, if any.
    pub fn for_target(&self, target: &MapTarget) -> Option<&MidiMapping> {
        self.mappings.iter().find(|m| &m.target == target)
    }

    /// Insert `mapping`, replacing whatever previously used **its control or its
    /// target**. This is MIDI Learn's rule: one physical control drives one target,
    /// and re-learning a target moves it to the newly touched control instead of
    /// leaving two controls fighting over it.
    pub fn bind(&mut self, mapping: MidiMapping) {
        self.mappings
            .retain(|m| m.control != mapping.control && m.target != mapping.target);
        self.mappings.push(mapping);
    }

    /// Remove the mapping writing `target`. Returns whether one existed.
    pub fn unbind_target(&mut self, target: &MapTarget) -> bool {
        let before = self.mappings.len();
        self.mappings.retain(|m| &m.target != target);
        self.mappings.len() != before
    }
}

/// Errors loading a controller-mappings file.
#[derive(Debug)]
pub enum MappingsError {
    /// The `format` tag was not [`MAPPINGS_FORMAT_ID`].
    BadFormat {
        found: String,
    },
    /// The file is newer than this build reads — refused rather than half-read.
    NewerThanReader {
        file: u32,
        reader: u32,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for MappingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MappingsError::BadFormat { found } => write!(
                f,
                "not a Prisma controller-mappings file (format tag was {found:?})"
            ),
            MappingsError::NewerThanReader { file, reader } => write!(
                f,
                "controller-mappings schema v{file} is newer than this build reads (v{reader})"
            ),
            MappingsError::Json(e) => write!(f, "controller-mappings JSON error: {e}"),
        }
    }
}

impl std::error::Error for MappingsError {}

impl From<serde_json::Error> for MappingsError {
    fn from(e: serde_json::Error) -> Self {
        MappingsError::Json(e)
    }
}

/// On-disk wrapper, the same envelope shape as the patch and show files.
#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    schema_version: u32,
    #[serde(default)]
    written_by: String,
    #[serde(default)]
    mappings: serde_json::Value,
}

/// Serialize a profile to pretty JSON with the current envelope.
pub fn save_mappings(mappings: &ControllerMappings) -> Result<String, MappingsError> {
    let envelope = Envelope {
        format: MAPPINGS_FORMAT_ID.to_string(),
        schema_version: MAPPINGS_SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        mappings: serde_json::to_value(mappings)?,
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Load a profile, refusing foreign or newer files. v1 is the first schema, so
/// there is no migration chain yet; the first breaking change adds one here, as
/// `show::version` does.
pub fn load_mappings(json: &str) -> Result<ControllerMappings, MappingsError> {
    let envelope: Envelope = serde_json::from_str(json)?;
    if envelope.format != MAPPINGS_FORMAT_ID {
        return Err(MappingsError::BadFormat {
            found: envelope.format,
        });
    }
    if envelope.schema_version > MAPPINGS_SCHEMA_VERSION {
        return Err(MappingsError::NewerThanReader {
            file: envelope.schema_version,
            reader: MAPPINGS_SCHEMA_VERSION,
        });
    }
    Ok(serde_json::from_value(envelope.mappings)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, n: u8) -> MidiControl {
        MidiControl {
            channel,
            kind: ControlKind::Cc(n),
        }
    }

    fn mapping(control: MidiControl, target: MapTarget) -> MidiMapping {
        MidiMapping {
            control,
            target,
            mode: ControlMode::Absolute,
            curve: ResponseCurve::Linear,
            led: None,
            takeover: Takeover::Pickup,
        }
    }

    #[test]
    fn roundtrips_through_the_envelope() {
        let mut m = ControllerMappings::new("Test — APC40");
        m.device_profile = Some("apc40mk2".into());
        m.bind(mapping(cc(0, 14), MapTarget::Path("/master/dimmer".into())));
        m.bind(MidiMapping {
            curve: ResponseCurve::Exponential { gamma: 2.0 },
            ..mapping(
                cc(3, 7),
                MapTarget::ZoneSlot {
                    slot: 3,
                    sub: ZoneSub::Dimmer,
                },
            )
        });
        let json = save_mappings(&m).unwrap();
        assert!(json.contains(MAPPINGS_FORMAT_ID));
        assert_eq!(load_mappings(&json).unwrap(), m);
    }

    #[test]
    fn refuses_foreign_and_newer_files() {
        let foreign = r#"{"format":"prisma.show","schema_version":1,"mappings":{}}"#;
        assert!(matches!(
            load_mappings(foreign),
            Err(MappingsError::BadFormat { .. })
        ));
        let newer = format!(
            r#"{{"format":"{MAPPINGS_FORMAT_ID}","schema_version":{},"mappings":{{}}}}"#,
            MAPPINGS_SCHEMA_VERSION + 1
        );
        assert!(matches!(
            load_mappings(&newer),
            Err(MappingsError::NewerThanReader { .. })
        ));
    }

    #[test]
    fn bind_replaces_same_control_and_same_target() {
        let mut m = ControllerMappings::new("t");
        let master = MapTarget::Path("/master/dimmer".into());
        m.bind(mapping(cc(0, 14), master.clone()));
        // Re-learn the master onto another fader: the old control no longer drives it.
        m.bind(mapping(cc(0, 15), master.clone()));
        assert_eq!(m.mappings.len(), 1);
        assert_eq!(m.for_target(&master).unwrap().control, cc(0, 15));
        // Reuse that fader for a zone: the master mapping goes away.
        let zone = MapTarget::ZoneSlot {
            slot: 0,
            sub: ZoneSub::Dimmer,
        };
        m.bind(mapping(cc(0, 15), zone.clone()));
        assert_eq!(m.mappings.len(), 1);
        assert!(m.for_target(&master).is_none());
        assert!(m.unbind_target(&zone));
        assert!(m.mappings.is_empty());
    }

    #[test]
    fn zone_slot_resolves_positionally_and_past_the_end_is_none() {
        let ids = SlotIds::from(vec![Uuid::new_v4(), Uuid::new_v4()]);
        let t = MapTarget::ZoneSlot {
            slot: 1,
            sub: ZoneSub::Speed,
        };
        assert_eq!(
            t.resolve_path(&ids).unwrap(),
            format!("/zone/{}/speed", ids.zones[1])
        );
        let gone = MapTarget::ZoneSlot {
            slot: 2,
            sub: ZoneSub::Dimmer,
        };
        assert!(gone.resolve_path(&ids).is_none());
    }

    #[test]
    fn curves_are_bounded_and_monotone_at_the_ends() {
        for c in [
            ResponseCurve::Linear,
            ResponseCurve::Exponential { gamma: 2.2 },
            ResponseCurve::Logarithmic { gamma: 2.2 },
            ResponseCurve::Exponential { gamma: 0.0 },
        ] {
            assert_eq!(c.apply(0.0), 0.0);
            assert!((c.apply(1.0) - 1.0).abs() < 1e-6);
            assert_eq!(c.apply(2.0), c.apply(1.0));
        }
        assert!(ResponseCurve::Exponential { gamma: 2.0 }.apply(0.5) < 0.5);
        assert!(ResponseCurve::Logarithmic { gamma: 2.0 }.apply(0.5) > 0.5);
    }

    #[test]
    fn cells_resolve_to_the_zones_home_track_by_mode_and_show_their_state() {
        // Phase 1 bridge: Cell(i) on zone slot n → clip i of the zone's home track.
        let track = Uuid::new_v4();
        let ids = SlotIds {
            zones: vec![Uuid::new_v4()],
            tracks: vec![Some(track)],
        };
        let cell = MapTarget::ZoneSlot {
            slot: 0,
            sub: ZoneSub::Cell(4),
        };
        let at = |mode| cell.resolve_path_for(&ids, mode).unwrap();
        let t = track;
        assert_eq!(at(ControlMode::Latch), format!("/track/{t}/clip/4/launch"));
        assert_eq!(
            at(ControlMode::Trigger),
            format!("/track/{t}/clip/4/trigger")
        );
        assert_eq!(at(ControlMode::Toggle), format!("/track/{t}/clip/4/hold"));
        assert_eq!(
            at(ControlMode::Momentary),
            format!("/track/{t}/clip/4/hold")
        );
        assert_eq!(
            cell.feedback_path(&ids).unwrap(),
            format!("/track/{t}/clip/4/state")
        );
        assert!(MapTarget::Path("/master/dimmer".into())
            .feedback_path(&ids)
            .is_none());
    }

    #[test]
    fn a_cell_on_a_zone_without_a_home_track_resolves_to_nothing() {
        let ids = SlotIds {
            zones: vec![Uuid::new_v4()],
            tracks: vec![None],
        };
        let cell = MapTarget::ZoneSlot {
            slot: 0,
            sub: ZoneSub::Cell(0),
        };
        assert!(cell.resolve_path(&ids).is_none());
        assert!(cell.feedback_path(&ids).is_none());
    }

    #[test]
    fn led_style_and_cell_targets_roundtrip() {
        let mut m = ControllerMappings::new("t");
        m.bind(MidiMapping {
            control: MidiControl {
                channel: 0,
                kind: ControlKind::Note(8),
            },
            target: MapTarget::ZoneSlot {
                slot: 1,
                sub: ZoneSub::Cell(2),
            },
            mode: ControlMode::Momentary,
            curve: ResponseCurve::Linear,
            led: Some(LedStyle {
                on: Some(5),
                idle: Some(7),
                animation: LedAnimation::Blink(LedRate::N8),
            }),
            takeover: Takeover::Pickup,
        });
        let json = save_mappings(&m).unwrap();
        assert!(json.contains("\"blink\": \"n8\""), "{json}");
        assert_eq!(load_mappings(&json).unwrap(), m);
    }
}

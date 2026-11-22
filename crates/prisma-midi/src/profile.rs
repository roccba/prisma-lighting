//! Device profiles (M9-SPEC §9): what a controller's surface is and how its LEDs
//! are driven, as **data** (CLAUDE.md §5.6: "profiles are data, not code").
//!
//! A profile never binds anything — bindings live in the operator's
//! [`ControllerMappings`](prisma_core::ControllerMappings), so Learn can override
//! any default. It only tells the feedback engine how to light a control, and later
//! slices where the grid and bank buttons are. A new controller is a new JSON file
//! under `/profiles`, no engine change.
//!
//! The APC40 mkII is built in (the reference profile, ported from makai). Any other
//! device gets [`DeviceProfile::generic`]: note-on velocity 127 = lit, note-off = dark,
//! which is what most pad controllers understand.

use serde::Deserialize;

use prisma_core::{ControlKind, LedAnimation, LedRate, LedStyle, MidiControl, MidiMapping};

use crate::message::{note_off_bytes, note_on_bytes};

/// The `format` tag every device-profile file carries.
pub const DEVICE_PROFILE_FORMAT_ID: &str = "prisma.device-profile";
/// The newest device-profile schema this build reads.
pub const DEVICE_PROFILE_SCHEMA_VERSION: u32 = 1;

const APC40_MK2_JSON: &str = include_str!("../../../profiles/apc40mk2.json");

/// A controller's surface and LED semantics.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DeviceProfile {
    /// Stable id, e.g. `apc40mk2` (what `ControllerMappings::device_profile` names).
    pub name: String,
    /// Product name for the device manager. Data about hardware, not a UI string.
    pub display_name: String,
    /// Case-insensitive substrings of a port name that identify this device.
    #[serde(default)]
    pub match_keywords: Vec<String>,
    /// Messages sent once when the device's output opens, before any LED — a mode
    /// handshake. The APC40 mkII powers up in *Generic* mode, where the track buttons
    /// (activator, solo, record arm, A|B) latch **in the hardware**: one message per
    /// press, alternating note-on / note-off, and nothing on release. Prisma cannot
    /// own a state the controller toggles by itself, so the profile switches it to
    /// *Ableton Live* mode (`F0 47 7F 29 60 00 04 41 …`, protocol v1.2 "Introduction
    /// message"): every button becomes momentary (note-on press / note-off release)
    /// and every LED is the host's — the same contract as the grid pads.
    #[serde(default)]
    pub on_connect: Vec<Vec<u8>>,
    #[serde(default)]
    pub surface: Surface,
    pub leds: LedSpec,
    /// The mappings this device ships with (LIVE-MODEL-SPEC §6: the APC40 mkII's grid →
    /// clips, faders → track opacity, …). Data in the profile, never code. They seed an
    /// empty operator profile on the device's first connection and can be added later
    /// without overwriting anything ("Cargar mapeos del perfil").
    #[serde(default)]
    pub default_mappings: Vec<MidiMapping>,
}

/// Where the structured parts of the surface are. Read by later slices (grid →
/// cells, bank navigation); carried now so the reference profile is complete data.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Surface {
    pub grid: Option<GridSpec>,
    pub bank: Option<BankSpec>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GridSpec {
    pub channel: u8,
    pub note_base: u8,
    pub rows: u8,
    pub cols: u8,
    /// Which physical row note row 0 is: `"bottom"` on the APC40 mkII.
    pub row0: String,
}

/// The bank buttons: they move the controller's grid window over the track grid
/// (LIVE-MODEL-SPEC §8.2) — by a step, or with Shift held by a whole window (8 tracks /
/// 5 rows on an APC40). Reserved by the device, Shift included: never learned, never
/// mapped.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BankSpec {
    #[serde(default)]
    pub channel: u8,
    pub up_note: u8,
    pub down_note: u8,
    pub left_note: u8,
    pub right_note: u8,
    /// The device's Shift button: held, a bank press moves a whole window.
    #[serde(default)]
    pub shift_note: Option<u8>,
    /// Tracks a left/right press moves the window by.
    #[serde(default = "one")]
    pub track_step: u16,
    /// Rows an up/down press moves the window by.
    #[serde(default = "one")]
    pub row_step: u16,
}

fn one() -> u16 {
    1
}

/// Which way a bank button moves the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankMove {
    Up,
    Down,
    Left,
    Right,
}

impl BankSpec {
    /// The move a note-on on `channel` / `note` asks for, if it is a bank button.
    pub fn move_for(&self, channel: u8, note: u8) -> Option<BankMove> {
        if channel != self.channel {
            return None;
        }
        [
            (self.up_note, BankMove::Up),
            (self.down_note, BankMove::Down),
            (self.left_note, BankMove::Left),
            (self.right_note, BankMove::Right),
        ]
        .into_iter()
        .find(|(n, _)| *n == note)
        .map(|(_, m)| m)
    }
}

/// How LEDs are lit.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LedSpec {
    /// Note ranges whose LEDs are RGB: lit with a palette index as velocity.
    #[serde(default)]
    pub rgb: Vec<NoteRange>,
    /// Palette index an RGB pad shows when its target is on.
    #[serde(default)]
    pub rgb_on_colour: u8,
    /// Velocity that lights a single-colour LED.
    pub on_velocity: u8,
    /// The RGB palette velocities index, if the device has one (`"apc40mk2-128"`).
    #[serde(default)]
    pub palette: Option<String>,
    /// MIDI channel of a pulsing / blinking RGB LED message, one per [`LedRate`] from
    /// 1/24 to 1/2 (APC40 mkII: channels 6–10 pulse, 11–15 blink; 0 is static). Empty
    /// = the device cannot animate, the LED is static.
    #[serde(default)]
    pub pulse_channels: Vec<u8>,
    #[serde(default)]
    pub blink_channels: Vec<u8>,
    /// Re-send a control's LED on every note-off: the APC40 mkII reverts a pad's LED
    /// on the physical release, whatever the host last sent.
    #[serde(default)]
    pub reaffirm_on_release: bool,
    /// CC controls that DISPLAY a value sent back to them — knobs with an LED ring, or
    /// motorized faders. Prisma sends each mapped one its parameter's value on connect
    /// and on every change, so the control shows (and continues from) Prisma's value.
    /// Plain faders are left out: they cannot show anything. Empty = none.
    #[serde(default)]
    pub value_rings: Vec<CcRange>,
}

/// A run of CC numbers on one channel.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CcRange {
    pub channel: u8,
    pub first_cc: u8,
    pub last_cc: u8,
}

impl CcRange {
    fn contains(&self, channel: u8, cc: u8) -> bool {
        channel == self.channel && (self.first_cc..=self.last_cc).contains(&cc)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct NoteRange {
    pub channel: u8,
    pub first_note: u8,
    pub last_note: u8,
}

impl NoteRange {
    fn contains(&self, channel: u8, note: u8) -> bool {
        channel == self.channel && (self.first_note..=self.last_note).contains(&note)
    }
}

/// Errors loading a device-profile file.
#[derive(Debug)]
pub enum DeviceProfileError {
    BadFormat { found: String },
    NewerThanReader { file: u32, reader: u32 },
    Json(serde_json::Error),
}

impl std::fmt::Display for DeviceProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadFormat { found } => {
                write!(f, "not a Prisma device profile (format tag was {found:?})")
            }
            Self::NewerThanReader { file, reader } => write!(
                f,
                "device profile schema v{file} is newer than this build reads (v{reader})"
            ),
            Self::Json(e) => write!(f, "device profile JSON error: {e}"),
        }
    }
}

impl std::error::Error for DeviceProfileError {}

#[derive(Deserialize)]
struct Header {
    format: String,
    schema_version: u32,
}

/// Parse a device profile, refusing foreign or newer files.
pub fn load_device_profile(json: &str) -> Result<DeviceProfile, DeviceProfileError> {
    let header: Header = serde_json::from_str(json).map_err(DeviceProfileError::Json)?;
    if header.format != DEVICE_PROFILE_FORMAT_ID {
        return Err(DeviceProfileError::BadFormat {
            found: header.format,
        });
    }
    if header.schema_version > DEVICE_PROFILE_SCHEMA_VERSION {
        return Err(DeviceProfileError::NewerThanReader {
            file: header.schema_version,
            reader: DEVICE_PROFILE_SCHEMA_VERSION,
        });
    }
    serde_json::from_str(json).map_err(DeviceProfileError::Json)
}

impl DeviceProfile {
    /// The built-in APC40 mkII reference profile.
    pub fn apc40_mk2() -> DeviceProfile {
        load_device_profile(APC40_MK2_JSON).expect("the bundled APC40 mkII profile is valid")
    }

    /// Any device without a profile: velocity 127 lights, note-off darkens.
    pub fn generic() -> DeviceProfile {
        DeviceProfile {
            name: "generic".into(),
            display_name: "Generic MIDI".into(),
            match_keywords: Vec::new(),
            on_connect: Vec::new(),
            surface: Surface::default(),
            leds: LedSpec {
                rgb: Vec::new(),
                rgb_on_colour: 0,
                on_velocity: 127,
                palette: None,
                pulse_channels: Vec::new(),
                blink_channels: Vec::new(),
                reaffirm_on_release: false,
                value_rings: Vec::new(),
            },
            default_mappings: Vec::new(),
        }
    }

    /// The grid window's size (tracks × rows), from the surface's grid; 8 × 5 when the
    /// device declares none.
    pub fn window(&self) -> (u16, u16) {
        self.surface
            .grid
            .as_ref()
            .map_or((8, 5), |g| (g.cols as u16, g.rows as u16))
    }

    /// Whether `control` displays a value sent back to it (see [`LedSpec::value_rings`]).
    pub fn shows_value(&self, control: MidiControl) -> bool {
        match control.kind {
            ControlKind::Cc(cc) => self
                .leds
                .value_rings
                .iter()
                .any(|r| r.contains(control.channel, cc)),
            ControlKind::Note(_) => false,
        }
    }

    /// The built-in profile whose keywords match `port_name`, else the generic one.
    pub fn for_port(port_name: &str) -> DeviceProfile {
        let lowered = port_name.to_lowercase();
        [DeviceProfile::apc40_mk2()]
            .into_iter()
            .find(|p| {
                p.match_keywords
                    .iter()
                    .any(|k| lowered.contains(&k.to_lowercase()))
            })
            .unwrap_or_else(DeviceProfile::generic)
    }

    /// The palette this device's RGB LEDs index, if any.
    pub fn palette(&self) -> Option<&'static [u32]> {
        match self.leds.palette.as_deref() {
            Some("apc40mk2-128") => Some(&APC40_MK2_PALETTE),
            _ => None,
        }
    }

    /// The bytes that show `level` on `control`'s LED. `None` for controls without an
    /// LED to drive (CCs).
    ///
    /// On an RGB pad, `On` uses the style's colour, else the zone's colour matched to
    /// the palette, else the profile default, moving as the style says; `Idle` (a cell
    /// with an effect, not running) uses the style's idle colour, else a dim zone
    /// colour; `Armed` is the `On` colour pulsing (the style's motion if it has one).
    /// A single-colour LED is lit for `On` / `Armed` and dark otherwise. Dark is always
    /// a real note-off: the APC40 protocol notes a velocity-0 note-on does not
    /// reliably turn a LED off (makai).
    pub fn led_bytes(
        &self,
        control: MidiControl,
        level: LedLevel,
        style: Option<&LedStyle>,
        zone_colour: Option<[u8; 3]>,
    ) -> Option<[u8; 3]> {
        let ControlKind::Note(note) = control.kind else {
            return None;
        };
        let channel = control.channel;
        let dark = Some(note_off_bytes(channel, note));
        let rgb =
            self.palette().is_some() && self.leds.rgb.iter().any(|r| r.contains(channel, note));
        if !rgb {
            return match level {
                LedLevel::On | LedLevel::Armed => {
                    Some(note_on_bytes(channel, note, self.leds.on_velocity))
                }
                LedLevel::Idle | LedLevel::Off => dark,
            };
        }
        let colour = match level {
            LedLevel::Off => return dark,
            LedLevel::Idle => style
                .and_then(|s| s.idle)
                .or_else(|| zone_colour.map(|c| nearest_palette_index(dim(c)))),
            LedLevel::On | LedLevel::Armed => style
                .and_then(|s| s.on)
                .or_else(|| zone_colour.map(nearest_palette_index))
                .or(Some(self.leds.rgb_on_colour)),
        };
        let Some(colour) = colour.filter(|&c| c > 0) else {
            return dark;
        };
        let animation = match level {
            LedLevel::On => style.map(|s| s.animation).unwrap_or_default(),
            LedLevel::Armed => match style.map(|s| s.animation) {
                Some(moving @ (LedAnimation::Pulse(_) | LedAnimation::Blink(_))) => moving,
                _ => LedAnimation::Pulse(ARMED_RATE),
            },
            _ => LedAnimation::Solid,
        };
        let pick = |channels: &[u8], rate: LedRate| channels.get(rate_index(rate)).copied();
        let out_channel = match animation {
            LedAnimation::Solid => None,
            LedAnimation::Pulse(r) => pick(&self.leds.pulse_channels, r),
            LedAnimation::Blink(r) => pick(&self.leds.blink_channels, r),
        }
        .unwrap_or(channel);
        Some(note_on_bytes(out_channel, note, colour))
    }
}

/// What an LED should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedLevel {
    Off,
    /// A cell that holds an effect but is not running.
    Idle,
    On,
    /// A clip armed for GO: lit and pulsing.
    Armed,
}

/// How fast an armed clip's pad pulses when its style does not say.
pub const ARMED_RATE: LedRate = LedRate::N8;

fn rate_index(rate: LedRate) -> usize {
    match rate {
        LedRate::N24 => 0,
        LedRate::N16 => 1,
        LedRate::N8 => 2,
        LedRate::N4 => 3,
        LedRate::N2 => 4,
    }
}

/// A zone colour dimmed for an idle cell, before matching it to the palette.
fn dim(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| (v as u16 * 3 / 10) as u8)
}

/// The APC40 mkII's 128-colour RGB pad palette, `0xRRGGBB`; the index is the velocity
/// that paints it (0 = off). From the "RGB LEDs" table of *Akai APC40 Mk2
/// Communications Protocol v1.2*, ported verbatim from makai `state/palette.js`.
pub const APC40_MK2_PALETTE: [u32; 128] = [
    0x000000, 0x1E1E1E, 0x7F7F7F, 0xFFFFFF, 0xFF4C4C, 0xFF0000, 0x590000, 0x190000, 0xFFBD6C,
    0xFF5400, 0x591D00, 0x271B00, 0xFFFF4C, 0xFFFF00, 0x595900, 0x191900, 0x88FF4C, 0x54FF00,
    0x1D5900, 0x142B00, 0x4CFF4C, 0x00FF00, 0x005900, 0x001900, 0x4CFF5E, 0x00FF19, 0x00590D,
    0x001902, 0x4CFF88, 0x00FF55, 0x00591D, 0x001F12, 0x4CFFB7, 0x00FF99, 0x005935, 0x001912,
    0x4CC3FF, 0x00A9FF, 0x004152, 0x001019, 0x4C88FF, 0x0055FF, 0x001D59, 0x000819, 0x4C4CFF,
    0x0000FF, 0x000059, 0x000019, 0x874CFF, 0x5400FF, 0x190064, 0x0F0030, 0xFF4CFF, 0xFF00FF,
    0x590059, 0x190019, 0xFF4C87, 0xFF0054, 0x59001D, 0x220013, 0xFF1500, 0x993500, 0x795100,
    0x436400, 0x033900, 0x005735, 0x00547F, 0x0000FF, 0x00454F, 0x2500CC, 0x7F7F7F, 0x202020,
    0xFF0000, 0xBDFF2D, 0xAFED06, 0x64FF09, 0x108B00, 0x00FF87, 0x00A9FF, 0x002AFF, 0x3F00FF,
    0x7A00FF, 0xB21A7D, 0x402100, 0xFF4A00, 0x88E106, 0x72FF15, 0x00FF00, 0x3BFF26, 0x59FF71,
    0x38FFCC, 0x5B8AFF, 0x3151C6, 0x877FE9, 0xD31DFF, 0xFF005D, 0xFF7F00, 0xB9B000, 0x90FF00,
    0x835D07, 0x392B00, 0x144C10, 0x0D5038, 0x15152A, 0x16205A, 0x693C1C, 0xA8000A, 0xDE513D,
    0xD86A1C, 0xFFE126, 0x9EE12F, 0x67B50F, 0x1E1E30, 0xDCFF6B, 0x80FFBD, 0x9A99FF, 0x8E66FF,
    0x404040, 0x757575, 0xE0FFFF, 0xA00000, 0x350000, 0x1AD000, 0x074200, 0xB9B000, 0x3F3100,
    0xB35F00, 0x4B1502,
];

/// The palette index closest to an sRGB colour (squared distance), skipping 0 (off)
/// so a dark colour still lights. For showing a zone's colour on a pad.
pub fn nearest_palette_index(rgb: [u8; 3]) -> u8 {
    let dist = |c: u32| {
        let d = |shift: u32, v: u8| (((c >> shift) & 0xFF) as i32 - v as i32).pow(2);
        d(16, rgb[0]) + d(8, rgb[1]) + d(0, rgb[2])
    };
    (1..128u8)
        .min_by_key(|&i| dist(APC40_MK2_PALETTE[i as usize]))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(channel: u8, n: u8) -> MidiControl {
        MidiControl {
            channel,
            kind: ControlKind::Note(n),
        }
    }

    #[test]
    fn apc40_profile_matches_the_hardware_facts() {
        let p = DeviceProfile::apc40_mk2();
        assert_eq!(p.name, "apc40mk2");
        let grid = p.surface.grid.as_ref().unwrap();
        assert_eq!(
            (grid.channel, grid.note_base, grid.rows, grid.cols),
            (0, 0, 5, 8)
        );
        assert_eq!(grid.row0, "bottom");
        let bank = p.surface.bank.as_ref().unwrap();
        assert_eq!(
            (
                bank.up_note,
                bank.down_note,
                bank.right_note,
                bank.left_note
            ),
            (94, 95, 96, 97)
        );
        assert_eq!((bank.track_step, bank.row_step), (1, 1));
        assert_eq!(bank.shift_note, Some(98));
        assert_eq!(bank.move_for(0, 97), Some(BankMove::Left));
        assert_eq!(bank.move_for(1, 97), None);
        assert_eq!(p.window(), (8, 5));
        assert!(p.leds.reaffirm_on_release);
        // The Ableton Live mode handshake (manufacturer 0x47, model 0x29, mode 0x41).
        assert_eq!(p.on_connect.len(), 1);
        let sysex = &p.on_connect[0];
        assert_eq!(
            &sysex[..8],
            &[0xF0, 0x47, 0x7F, 0x29, 0x60, 0x00, 0x04, 0x41]
        );
        assert_eq!(sysex.last(), Some(&0xF7));
        assert_eq!(sysex[5..7], [0x00, sysex.len() as u8 - 8]);
        assert_eq!(APC40_MK2_PALETTE.len(), 128);
        assert_eq!(APC40_MK2_PALETTE[0], 0x000000);
        assert_eq!(APC40_MK2_PALETTE[5], 0xFF0000);
    }

    #[test]
    fn rgb_pads_get_a_colour_single_leds_get_on_velocity_off_is_note_off() {
        let p = DeviceProfile::apc40_mk2();
        // Grid pad 12 (ch 0): palette colour.
        let on = |c| p.led_bytes(c, LedLevel::On, None, None);
        assert_eq!(on(note(0, 12)), Some([0x90, 12, 9]));
        // Track-3 solo button (note 49, ch 2): single-colour, velocity 1.
        assert_eq!(on(note(2, 49)), Some([0x92, 49, 1]));
        // A note in the grid range on another channel is not a grid pad.
        assert_eq!(on(note(1, 12)), Some([0x91, 12, 1]));
        assert_eq!(
            p.led_bytes(note(2, 49), LedLevel::Off, None, None),
            Some([0x82, 49, 0])
        );
        let fader = MidiControl {
            channel: 0,
            kind: ControlKind::Cc(7),
        };
        assert_eq!(on(fader), None);
    }

    #[test]
    fn ports_pick_their_profile() {
        assert_eq!(
            DeviceProfile::for_port("APC40 mkII:APC40 mkII MIDI 1 24:0").name,
            "apc40mk2"
        );
        assert_eq!(DeviceProfile::for_port("nanoPAD2").name, "generic");
        assert_eq!(
            DeviceProfile::generic().led_bytes(note(0, 36), LedLevel::On, None, None),
            Some([0x90, 36, 127])
        );
    }

    #[test]
    fn foreign_and_newer_profiles_are_refused() {
        assert!(matches!(
            load_device_profile(r#"{"format":"prisma.show","schema_version":1}"#),
            Err(DeviceProfileError::BadFormat { .. })
        ));
        assert!(matches!(
            load_device_profile(r#"{"format":"prisma.device-profile","schema_version":99}"#),
            Err(DeviceProfileError::NewerThanReader { .. })
        ));
    }

    #[test]
    fn cells_light_in_zone_colour_or_the_operators_style() {
        let p = DeviceProfile::apc40_mk2();
        let pad = note(0, 3);
        let red = Some([255, 0, 0]);
        // Default: the zone colour, bright when running, dim when idle.
        assert_eq!(
            p.led_bytes(pad, LedLevel::On, None, red),
            Some([0x90, 3, 5])
        );
        let [_, _, idle] = p.led_bytes(pad, LedLevel::Idle, None, red).unwrap();
        assert!(idle != 5 && idle != 0, "a dimmer red, got {idle}");
        assert_eq!(
            p.led_bytes(pad, LedLevel::Off, None, red),
            Some([0x80, 3, 0])
        );
        // The operator's style wins, and the animation picks the message channel.
        let style = LedStyle {
            on: Some(21),
            idle: Some(23),
            animation: LedAnimation::Blink(LedRate::N8),
        };
        assert_eq!(
            p.led_bytes(pad, LedLevel::On, Some(&style), red),
            Some([0x9D, 3, 21])
        );
        assert_eq!(
            p.led_bytes(pad, LedLevel::Idle, Some(&style), red),
            Some([0x90, 3, 23])
        );
        let pulse = LedStyle {
            animation: LedAnimation::Pulse(LedRate::N24),
            ..style
        };
        assert_eq!(
            p.led_bytes(pad, LedLevel::On, Some(&pulse), red),
            Some([0x96, 3, 21])
        );
        // A device without a palette ignores colours: idle is dark, on is plain lit.
        let g = DeviceProfile::generic();
        assert_eq!(
            g.led_bytes(pad, LedLevel::Idle, Some(&style), red),
            Some([0x80, 3, 0])
        );
        assert_eq!(
            g.led_bytes(pad, LedLevel::On, Some(&style), red),
            Some([0x90, 3, 127])
        );
    }

    #[test]
    fn nearest_palette_colour_never_picks_off() {
        assert_eq!(nearest_palette_index([255, 0, 0]), 5);
        assert_ne!(nearest_palette_index([0, 0, 0]), 0);
    }
}

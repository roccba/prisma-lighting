//! MIDI wire parsing and encoding (M9-SPEC §5), ported from makai's
//! `src/midi/messages.js`.
//!
//! Parsing is a pure function of the bytes, so everything above it — resolution,
//! Learn, feedback — is testable by feeding byte slices, with no `midir` and no
//! virtual port (§13).

use prisma_core::{ControlKind, MidiControl};

const NOTE_OFF: u8 = 0x80;
const NOTE_ON: u8 = 0x90;
const CONTROL_CHANGE: u8 = 0xB0;
const PITCH_BEND: u8 = 0xE0;

/// One decoded channel message. System realtime (clock) is added with Slice 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    /// 0-based channel, `0..=15`.
    pub channel: u8,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Velocity is always `1..=127`: a note-on with velocity 0 is a note-off.
    NoteOn {
        note: u8,
        velocity: u8,
    },
    NoteOff {
        note: u8,
    },
    Cc {
        control: u8,
        value: u8,
    },
    /// 14-bit, `0..=16383`, centre 8192.
    PitchBend {
        value: u16,
    },
}

impl MidiEvent {
    /// Decode one complete message. `None` for anything that is not a channel
    /// note/CC/pitch-bend message, or that is truncated. midir delivers whole
    /// messages (running status already expanded), so no parser state is needed.
    pub fn parse(bytes: &[u8]) -> Option<MidiEvent> {
        let (&status, rest) = bytes.split_first()?;
        if status < 0x80 || status >= 0xF0 {
            return None;
        }
        let channel = status & 0x0F;
        let d1 = *rest.first()? & 0x7F;
        let d2 = *rest.get(1)? & 0x7F;
        let kind = match status & 0xF0 {
            NOTE_ON if d2 == 0 => EventKind::NoteOff { note: d1 },
            NOTE_ON => EventKind::NoteOn {
                note: d1,
                velocity: d2,
            },
            NOTE_OFF => EventKind::NoteOff { note: d1 },
            CONTROL_CHANGE => EventKind::Cc {
                control: d1,
                value: d2,
            },
            PITCH_BEND => EventKind::PitchBend {
                value: (d2 as u16) << 7 | d1 as u16,
            },
            _ => return None,
        };
        Some(MidiEvent { channel, kind })
    }

    /// The physical control this event came from, as a mapping key. Note-on and
    /// note-off of the same note are the same control. Pitch bend has no mapping
    /// key yet (no APC40 control sends it).
    pub fn control(&self) -> Option<MidiControl> {
        let kind = match self.kind {
            EventKind::NoteOn { note, .. } | EventKind::NoteOff { note } => ControlKind::Note(note),
            EventKind::Cc { control, .. } => ControlKind::Cc(control),
            EventKind::PitchBend { .. } => return None,
        };
        Some(MidiControl {
            channel: self.channel,
            kind,
        })
    }
}

/// `note_on` bytes (feedback, Slice 2).
pub fn note_on_bytes(channel: u8, note: u8, velocity: u8) -> [u8; 3] {
    [NOTE_ON | (channel & 0x0F), note & 0x7F, velocity & 0x7F]
}

/// `note_off` bytes (feedback, Slice 2).
pub fn note_off_bytes(channel: u8, note: u8) -> [u8; 3] {
    [NOTE_OFF | (channel & 0x0F), note & 0x7F, 0]
}

/// `control_change` bytes (value echo, Slice 2).
pub fn cc_bytes(channel: u8, control: u8, value: u8) -> [u8; 3] {
    [
        CONTROL_CHANGE | (channel & 0x0F),
        control & 0x7F,
        value & 0x7F,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_messages() {
        assert_eq!(
            MidiEvent::parse(&[0x93, 36, 100]),
            Some(MidiEvent {
                channel: 3,
                kind: EventKind::NoteOn {
                    note: 36,
                    velocity: 100
                }
            })
        );
        assert_eq!(
            MidiEvent::parse(&[0x80, 36, 64]).unwrap().kind,
            EventKind::NoteOff { note: 36 }
        );
        assert_eq!(
            MidiEvent::parse(&[0xB7, 7, 127]),
            Some(MidiEvent {
                channel: 7,
                kind: EventKind::Cc {
                    control: 7,
                    value: 127
                }
            })
        );
        assert_eq!(
            MidiEvent::parse(&[0xE0, 0x00, 0x40]).unwrap().kind,
            EventKind::PitchBend { value: 8192 }
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert_eq!(
            MidiEvent::parse(&[0x90, 5, 0]).unwrap().kind,
            EventKind::NoteOff { note: 5 }
        );
    }

    #[test]
    fn rejects_truncated_system_and_data_bytes() {
        assert_eq!(MidiEvent::parse(&[]), None);
        assert_eq!(MidiEvent::parse(&[0xB0, 7]), None);
        assert_eq!(MidiEvent::parse(&[0xF8]), None); // clock: Slice 5
        assert_eq!(MidiEvent::parse(&[0xF0, 0x47, 0xF7]), None); // sysex
        assert_eq!(MidiEvent::parse(&[0x40, 1, 2]), None); // stray data byte
        assert_eq!(MidiEvent::parse(&[0xC0, 1]), None); // program change: unmapped
    }

    #[test]
    fn note_on_and_off_share_a_control_key() {
        let on = MidiEvent::parse(&[0x90, 12, 127]).unwrap().control();
        let off = MidiEvent::parse(&[0x80, 12, 0]).unwrap().control();
        assert_eq!(on, off);
        assert_ne!(on, MidiEvent::parse(&[0xB0, 12, 0]).unwrap().control());
    }

    #[test]
    fn encoders_mask_and_roundtrip() {
        let b = note_on_bytes(0, 39, 5);
        assert_eq!(
            MidiEvent::parse(&b).unwrap().kind,
            EventKind::NoteOn {
                note: 39,
                velocity: 5
            }
        );
        assert_eq!(note_off_bytes(17, 200), [0x81, 72, 0]);
        assert_eq!(cc_bytes(0, 14, 255), [0xB0, 14, 127]);
    }
}

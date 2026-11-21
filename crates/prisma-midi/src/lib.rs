//! MIDI transport: generic mapping layer, MIDI Learn, LED feedback, device profiles
//! and (later) clock (CLAUDE.md §5.6, milestone M9 — see docs/M9-SPEC.md).
//!
//! MIDI is a **transport onto the Parameter Tree** (§3.1 principle 2): it never
//! talks to effects, the compositor or the output stage. A physical control writes a
//! tree target at `priority::MIDI`, and everything downstream already reacts.
//!
//! # Layers
//!
//! - [`message`] — pure byte parsing/encoding.
//! - [`resolver`] — a [`ControllerMappings`](prisma_core::ControllerMappings)
//!   compiled against one tree: targets resolved to handles once, never per message.
//! - [`runtime`] — [`MidiRuntime`]: the active profile, its binding across engine
//!   recompiles, and MIDI Learn. Its [`feed`](MidiRuntime::feed) is the injectable
//!   seam every test drives, with no `midir` and no hardware.
//! - [`profile`] — device profiles as JSON data: LED semantics, surface layout.
//! - [`feedback`] — LEDs painted from live tree state on a subscription thread.
//! - `io` (feature `hardware`) — the thin `midir` adapter that calls `feed`.
//!
//! This crate depends on `prisma-core` only; never on render/output (M9-SPEC §4).

pub mod feedback;
pub mod message;
pub mod ports;
pub mod profile;
pub mod resolver;
pub mod runtime;

#[cfg(feature = "hardware")]
pub mod io;

pub use feedback::{led_level, MidiOut};
pub use message::{EventKind, MidiEvent};
pub use ports::{find_port_by_keywords, APC40_KEYWORDS};
pub use profile::{
    load_device_profile, nearest_palette_index, DeviceProfile, LedLevel, APC40_MK2_PALETTE,
};
pub use resolver::{source_for, Binding, FeedbackTarget, ValueTarget, PICKUP_STEPS};
pub use runtime::{MidiListener, MidiNotice, MidiRuntime};

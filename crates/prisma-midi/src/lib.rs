//! MIDI transport: generic mapping layer, MIDI Learn and (later) feedback, clock
//! and device profiles (CLAUDE.md §5.6, milestone M9 — see docs/M9-SPEC.md).
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
//! - `io` (feature `hardware`) — the thin `midir` adapter that calls `feed`.
//!
//! This crate depends on `prisma-core` only; never on render/output (M9-SPEC §4).

pub mod message;
pub mod ports;
pub mod resolver;
pub mod runtime;

#[cfg(feature = "hardware")]
pub mod io;

pub use message::{EventKind, MidiEvent};
pub use ports::{find_port_by_keywords, APC40_KEYWORDS};
pub use resolver::{source_for, Binding};
pub use runtime::{MidiListener, MidiNotice, MidiRuntime};

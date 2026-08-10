//! Engine model: clock, zones, compositor, parameter tree, show model (CLAUDE.md §7).
//!
//! `prisma-core` is the headlessly-testable engine model. It must never depend on
//! `prisma-render` (§7) — the value here is a model that can be exercised with no
//! GPU.
//!
//! # Parameter Tree (milestone M3)
//!
//! The tree ([`ParameterTree`]) is the single place every controllable value
//! lives (§3.1 principle 2). MIDI, OSC, DMX-in and the UI are all *transports*
//! that write into it; effects and outputs read from it. Its guarantees:
//!
//! - **Addressable & normalized.** Every value has a path and an external
//!   `0.0..=1.0` representation with a per-parameter [`Curve`] to a real range
//!   ([`descriptor`]).
//! - **Wait-free reads.** The render/output threads read parameters through a
//!   [`ParamHandle`] with no lock ([`slot`]).
//! - **Priority arbitration.** Writes resolve by priority with takeover after an
//!   idle timeout, off an injected clock so it is testable without wall-clock
//!   ([`arbitration`], [`time`]).
//! - **Async observable.** Writes enqueue; a notifier thread fans changes out to
//!   subscribers that drain on their own threads. Slow/hung subscribers are
//!   contained ([`notify`]).
//! - **Introspectable.** The UI/MIDI-learn build themselves by walking the tree
//!   ([`ParameterTree::walk`]).

pub mod arbitration;
pub mod clock;
pub mod descriptor;
pub mod notify;
pub mod overlap;
pub mod show;
pub mod slot;
pub mod source;
pub mod time;
pub mod transition;
pub mod tree;
pub mod trigger;
pub mod zone;

pub use clock::{ClockShared, ClockSnapshot, MasterClock, MAX_BPM, MIN_BPM};
pub use descriptor::{Curve, ParamDescriptor, ParamKind, RealRange};
pub use notify::{Filter, Subscription};
pub use overlap::{OverlapSnapshot, NO_CELL, NO_ZONE};
pub use show::{
    load_show, save_show, Cell, Layer, MasterState, ParamSet, Show, ShowError, Source, ZoneLive,
    ZoneShow,
};
pub use slot::{ParamHandle, ParamId};
pub use source::{priority, SourceId};
pub use time::{ManualClock, SystemClock, Tick, TimeSource};
pub use transition::{Easing, TimeBase, Transition};
pub use tree::{NodeInfo, ParameterTree, TreeConfig, WriteOutcome};
pub use trigger::{TriggerCursor, MAX_TRIGGER_REPLAY};
pub use zone::{ZoneLiveState, ZoneStates, DIMMER_UP_FALL, DIMMER_UP_RISE};

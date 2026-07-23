//! Write sources and their priority tiers (CLAUDE.md §4.5).
//!
//! Every write into the Parameter Tree carries a [`SourceId`]: a **priority
//! tier** and an opaque **tag**. Arbitration decisions use only the priority;
//! the tag identifies *which* transport currently owns a parameter, for
//! observability and feedback routing (e.g. the UI showing "this fader is held
//! by MIDI").
//!
//! The tier values are a **configurable policy**, not a fixed enum — §4.5 says
//! the mechanism is fixed but the tiers (UI vs MIDI vs OSC vs automation) are
//! configurable. They are therefore plain `u8`s; the constants below are the
//! default policy, and higher wins.

/// Identifies a writer to the Parameter Tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SourceId {
    /// Priority tier; a higher value wins arbitration (CLAUDE.md §4.5).
    pub priority: u8,
    /// Opaque transport identity, for observability/feedback routing only. Not
    /// used in the arbitration decision.
    pub tag: u32,
}

impl SourceId {
    pub const fn new(priority: u8, tag: u32) -> Self {
        Self { priority, tag }
    }
}

/// Default priority tiers. Higher wins. These are policy defaults, overridable
/// by configuration — the arbitration *mechanism* does not depend on them.
pub mod priority {
    /// Automation / scripts (M11) — lowest, yields to any human input.
    pub const AUTOMATION: u8 = 10;
    /// OSC / WebSocket remote (M13).
    pub const REMOTE: u8 = 20;
    /// The desktop UI.
    pub const UI: u8 = 30;
    /// Physical MIDI control surface — highest by default: the operator's hands
    /// on hardware take a parameter from a UI drag or an automation curve.
    pub const MIDI: u8 = 40;
}

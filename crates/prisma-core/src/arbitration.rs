//! Write arbitration: priority with takeover-by-timeout (CLAUDE.md §4.5).
//!
//! Every parameter has one [`ArbState`]. A higher-priority source that writes
//! takes **ownership** and holds it; a lower-priority source cannot override an
//! owned parameter until the owner has been idle on it for a configurable
//! timeout, at which point ownership is released and can be taken over. Equal
//! priority is last-write-wins. This stops a UI drag and a physical fader from
//! fighting frame-by-frame while still letting either take clean control.
//!
//! [`ArbState::decide`] is a **pure function of state + time**: it never reads a
//! clock itself. The caller passes `now` (from the injected [`TimeSource`](crate::time::TimeSource))
//! and the configured `timeout`. That is what makes the whole rule testable with
//! a [`ManualClock`](crate::time::ManualClock) and no wall-clock sleeps.

use crate::source::SourceId;
use crate::time::Tick;

/// Per-parameter arbitration state. Lives behind the slot's mutex; only writes
/// touch it, never the wait-free read path.
#[derive(Debug, Clone, Copy)]
pub struct ArbState {
    /// The current owner, or `None` if the parameter has never been written.
    owner: Option<SourceId>,
    /// Engine time of the owner's most recent accepted write. Idle is measured
    /// from here.
    last_write: Tick,
}

impl ArbState {
    pub fn new() -> Self {
        Self {
            owner: None,
            last_write: 0,
        }
    }

    /// The priority of the current owner, or `None` if unowned. For the write
    /// outcome / UI ("held by …").
    pub fn owner(&self) -> Option<SourceId> {
        self.owner
    }

    /// Decide whether `src` may write at time `now`, given `timeout`.
    ///
    /// Rules, in order:
    /// 1. No owner yet → accept.
    /// 2. Owner idle for `>= timeout` → ownership released, accept.
    /// 3. `src.priority > owner.priority` → takeover, accept.
    /// 4. `src.priority == owner.priority` → last-write-wins, accept.
    /// 5. Otherwise (lower priority, owner not idle) → reject.
    ///
    /// Pure: same inputs always give the same answer. [`commit`](Self::commit)
    /// applies the state change after an accept.
    pub fn decide(&self, src: SourceId, now: Tick, timeout: Tick) -> bool {
        match self.owner {
            None => true,
            Some(owner) => {
                let idle = now.saturating_sub(self.last_write);
                idle >= timeout || src.priority >= owner.priority
            }
        }
    }

    /// Record an accepted write: `src` becomes (or stays) the owner and the idle
    /// clock resets to `now`. Call only after [`decide`](Self::decide) returned
    /// `true`.
    pub fn commit(&mut self, src: SourceId, now: Tick) {
        self.owner = Some(src);
        self.last_write = now;
    }
}

impl Default for ArbState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::priority;

    const TIMEOUT: Tick = 1000;

    fn src(prio: u8, tag: u32) -> SourceId {
        SourceId::new(prio, tag)
    }

    /// A fresh parameter accepts anyone.
    #[test]
    fn unowned_accepts_any() {
        let s = ArbState::new();
        assert!(s.decide(src(priority::AUTOMATION, 1), 0, TIMEOUT));
    }

    /// Higher priority takes over immediately, no waiting.
    #[test]
    fn higher_priority_takes_over_immediately() {
        let mut s = ArbState::new();
        s.commit(src(priority::UI, 1), 0);
        assert!(s.decide(src(priority::MIDI, 2), 1, TIMEOUT));
    }

    /// Lower priority is rejected while the owner is still active…
    #[test]
    fn lower_priority_rejected_while_owner_active() {
        let mut s = ArbState::new();
        s.commit(src(priority::MIDI, 1), 0);
        // Just before the timeout: still owned.
        assert!(!s.decide(src(priority::UI, 2), TIMEOUT - 1, TIMEOUT));
    }

    /// …and accepted once the owner has been idle for the full timeout.
    #[test]
    fn lower_priority_takes_over_after_idle_timeout() {
        let mut s = ArbState::new();
        s.commit(src(priority::MIDI, 1), 0);
        assert!(!s.decide(src(priority::UI, 2), TIMEOUT - 1, TIMEOUT));
        assert!(s.decide(src(priority::UI, 2), TIMEOUT, TIMEOUT));
    }

    /// Each accepted write from the owner resets the idle clock, so a steadily
    /// active high-priority owner locks out a lower-priority source forever.
    #[test]
    fn active_owner_keeps_resetting_idle() {
        let mut s = ArbState::new();
        s.commit(src(priority::MIDI, 1), 0);
        // Owner writes again at t=999, resetting idle.
        assert!(s.decide(src(priority::MIDI, 1), 999, TIMEOUT));
        s.commit(src(priority::MIDI, 1), 999);
        // Now the lower-priority source needs another full timeout from 999.
        assert!(!s.decide(src(priority::UI, 2), 999 + TIMEOUT - 1, TIMEOUT));
        assert!(s.decide(src(priority::UI, 2), 999 + TIMEOUT, TIMEOUT));
    }

    /// Equal priority is last-write-wins, and ownership follows the latest
    /// writer (its tag becomes the owner).
    #[test]
    fn equal_priority_last_write_wins_and_transfers_ownership() {
        let mut s = ArbState::new();
        s.commit(src(priority::UI, 1), 0);
        assert!(s.decide(src(priority::UI, 2), 1, TIMEOUT));
        s.commit(src(priority::UI, 2), 1);
        assert_eq!(s.owner().unwrap().tag, 2);
    }
}

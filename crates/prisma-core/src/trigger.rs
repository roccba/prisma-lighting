//! Consumer-side trigger handling: turning a monotonic fire count into "how many
//! times should I act" — with the counter-wrap rule pinned down explicitly.
//!
//! A [`ParamKind::Trigger`](crate::descriptor::ParamKind::Trigger) has no held
//! value; each write increments a `u32` fire counter. Coalescing in the fan-out
//! (see `notify.rs`) keeps only the latest counter, so a consumer recovers the
//! number of fires it missed by **diffing** the counter since it last looked.
//! That diff is the classic once-every-six-months bug if done naively, so the
//! rule is fixed here, in one place, and tested across the wrap boundary.
//!
//! ## The rule
//!
//! - **Width: `u32`, wrapping.** Reuses the slot's value cell; ~4.3e9 fires
//!   before wrap — unreachable in normal use, but handled correctly anyway
//!   because the repro *is* "consumer offline a long time".
//! - **Diff via [`u32::wrapping_sub`], never signed subtraction.** Modular
//!   arithmetic gives the exact number of missed fires for any real gap
//!   (`< 2^32` fires), even across the wrap. Signed subtraction is what produces
//!   the "absurd or negative" number.
//! - **Replay threshold [`MAX_TRIGGER_REPLAY`].** `delta == 0` → nothing.
//!   `1..=MAX` → acted `delta` times (a count-consumer replays `delta`; an
//!   edge-consumer collapses to one). `> MAX` → "many": clamp to `MAX` (count)
//!   or `1` (edge). Past the threshold, 70 vs 70 000 fires is indistinguishable
//!   for any real lighting/musical action.
//! - **First observation adopts a baseline and fires ZERO.** A subscriber that
//!   connects mid-show must not retro-fire the entire history — that is the
//!   "plugged in the tablet and every scene fired at once" bug.

/// Above this many missed fires we stop replaying pulse-for-pulse and treat it
/// as "many" (see module docs). 64 is comfortably more than any real burst a
/// consumer would want to replay after a hiccup.
pub const MAX_TRIGGER_REPLAY: u32 = 64;

/// Tracks the last-seen fire count of one trigger for one consumer. Each consumer
/// keeps its own cursor; nothing is shared, so a slow consumer never affects
/// another's edge accounting.
#[derive(Debug, Default, Clone)]
pub struct TriggerCursor {
    last: Option<u32>,
}

impl TriggerCursor {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Count-consumer poll: how many times to act, given the trigger's current
    /// fire count. Adopts a baseline (returns 0) on first observation, diffs with
    /// [`u32::wrapping_sub`] afterwards, and clamps to [`MAX_TRIGGER_REPLAY`].
    pub fn poll(&mut self, current: u32) -> u32 {
        match self.last {
            None => {
                // First observation for this consumer: adopt the baseline, do
                // not replay history.
                self.last = Some(current);
                0
            }
            Some(prev) => {
                let delta = current.wrapping_sub(prev);
                self.last = Some(current);
                delta.min(MAX_TRIGGER_REPLAY)
            }
        }
    }

    /// Edge-consumer poll: did it fire at all since the last poll? Collapses any
    /// positive delta (including a clamped "many") to a single edge.
    pub fn poll_edge(&mut self, current: u32) -> bool {
        self.poll(current) > 0
    }

    /// Whether this cursor has adopted a baseline yet. Mostly for tests.
    pub fn is_primed(&self) -> bool {
        self.last.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_fires_zero() {
        let mut c = TriggerCursor::new();
        // Even if the counter is already large (subscriber joined mid-show),
        // the first poll adopts it as baseline and fires nothing.
        assert_eq!(c.poll(9_000_000), 0);
        assert!(c.is_primed());
    }

    #[test]
    fn counts_fires_since_last_poll() {
        let mut c = TriggerCursor::new();
        assert_eq!(c.poll(10), 0); // baseline
        assert_eq!(c.poll(13), 3); // three fires
        assert_eq!(c.poll(13), 0); // none since
    }

    #[test]
    fn edge_consumer_collapses_to_bool() {
        let mut c = TriggerCursor::new();
        assert!(!c.poll_edge(5)); // baseline
        assert!(c.poll_edge(8)); // fired
        assert!(!c.poll_edge(8)); // idle
    }

    #[test]
    fn wrap_gives_exact_small_delta_not_absurd() {
        let mut c = TriggerCursor::new();
        let before = u32::MAX - 2;
        assert_eq!(c.poll(before), 0); // baseline near the wrap
                                       // Five more fires wrap the counter past u32::MAX.
        let after = before.wrapping_add(5); // == 2
        assert_eq!(c.poll(after), 5); // exact, not ~4 billion
    }

    #[test]
    fn huge_gap_clamps_to_max_for_count_consumer() {
        let mut c = TriggerCursor::new();
        assert_eq!(c.poll(0), 0);
        // Consumer was gone for a million fires: replay is clamped, not literal.
        assert_eq!(c.poll(1_000_000), MAX_TRIGGER_REPLAY);
    }

    #[test]
    fn huge_gap_is_still_a_single_edge() {
        let mut c = TriggerCursor::new();
        assert!(!c.poll_edge(0));
        assert!(c.poll_edge(1_000_000)); // one edge, not a storm
    }
}

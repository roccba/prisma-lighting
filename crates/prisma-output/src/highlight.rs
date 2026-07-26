//! Highlight override for the output stage (M7 §7 / S9).
//!
//! The patch editor identifies a fixture on the real rig by lighting it up. That is a
//! pure **output-stage** override — it forces the selected pixels to a bright value on
//! the wire, over whatever the master buffer holds — so it does **not** touch the render
//! thread or the clock: content stays frozen (§7) while the highlight blinks.
//!
//! # How it reaches the output thread
//!
//! Wait-free, exactly like the flat parameters. The engine owns a [`HighlightState`]
//! (which pixels, whether it blinks) and publishes it through a triple-buffer handoff;
//! the output loop reads the latest each tick. The state changes only when the operator
//! changes the selection — rare — but it must never make the output thread block, hence
//! the same lock-free handoff the frame path uses.
//!
//! # Colour / pattern
//!
//! A highlighted pixel emits **full white** (every component at 255). It either holds
//! solid or **blinks** (a ~2 Hz square wave), the operator's choice: solid to compare
//! two fixtures side by side without flicker, blink to spot one among many. The blink
//! phase is driven by **wall-clock** in the output loop — never the show clock — so it
//! keeps blinking while content is frozen.

use std::time::Duration;

use crate::frame::{triple_buffer, Consumer, Producer};

/// Half of the blink period: 250 ms → a 2 Hz square wave (250 ms on, 250 ms off).
pub const BLINK_HALF_PERIOD: Duration = Duration::from_millis(250);

/// The published highlight selection: which master-buffer components are highlighted,
/// whether any are, and whether the highlight blinks or holds solid. `mask` is indexed
/// by **component offset** in the canonical master buffer — the same coordinate the
/// output stage reads a channel's source from — so the check is O(1) per channel.
pub struct HighlightState {
    pub mask: Vec<bool>,
    pub active: bool,
    pub blink: bool,
}

impl HighlightState {
    /// A cleared state sized to `master_len` components (nothing highlighted).
    pub fn cleared(master_len: usize) -> Self {
        Self {
            mask: vec![false; master_len],
            active: false,
            blink: false,
        }
    }
}

/// The borrowed view the output stage consults while encoding one frame: the mask plus
/// the resolved blink phase (`on = true` emits white, `on = false` emits dark).
pub struct HighlightFrame<'a> {
    pub mask: &'a [bool],
    pub on: bool,
}

/// Whether the highlight is in its "on" (lit) half of the blink cycle at `elapsed` wall
/// time. A solid highlight passes `blink = false` and is always on; a blinking one
/// toggles every [`BLINK_HALF_PERIOD`].
#[inline]
pub fn blink_on(elapsed: Duration, blink: bool) -> bool {
    if !blink {
        return true;
    }
    (elapsed.as_millis() / BLINK_HALF_PERIOD.as_millis()).is_multiple_of(2)
}

/// Build a highlight handoff whose state holds `master_len` component flags. The engine
/// keeps the producer (publishes on selection change); the output loop keeps the
/// consumer (reads the latest each tick, wait-free).
pub fn highlight_handoff(
    master_len: usize,
) -> (Producer<HighlightState>, Consumer<HighlightState>) {
    triple_buffer(|| HighlightState::cleared(master_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_is_always_on() {
        assert!(blink_on(Duration::from_millis(0), false));
        assert!(blink_on(Duration::from_millis(9999), false));
    }

    #[test]
    fn blink_toggles_each_half_period() {
        assert!(blink_on(Duration::from_millis(0), true));
        assert!(blink_on(Duration::from_millis(249), true));
        assert!(!blink_on(Duration::from_millis(250), true));
        assert!(!blink_on(Duration::from_millis(499), true));
        assert!(blink_on(Duration::from_millis(500), true));
    }

    #[test]
    fn handoff_publishes_a_new_selection() {
        let (mut p, mut c) = highlight_handoff(4);
        assert!(!c.current().active);
        {
            let s = p.input();
            s.mask[1] = true;
            s.active = true;
            s.blink = true;
        }
        p.publish();
        assert!(c.advance());
        assert!(c.current().active);
        assert!(c.current().blink);
        assert_eq!(c.current().mask, vec![false, true, false, false]);
    }
}

//! Per-track clip playback state: which clip runs, and where temporary clips return
//! to (M9 Slice 3 — pad modes on the scene grid). Named `ZoneDeck` for history; since
//! the live model (docs/LIVE-MODEL-SPEC.md §5) there is one per **track**.
//!
//! A track has one **active** clip. Cells are started four
//! ways, which differ only in what happens *after*:
//!
//! - **latch** — the cell becomes the zone's new base content and stays, exactly as
//!   clicking it on screen. Any temporary cells are forgotten.
//! - **trigger** — like latch, and it always **restarts** the effect from its
//!   beginning, even if that cell is already running (a one-shot hit).
//! - **press / release** — a *temporary* cell: while held it runs, and on release
//!   the zone returns to what ran before it. Momentary pads press on note-on and
//!   release on note-off; toggle pads press on one tap and release on the next.
//!
//! Temporary cells stack: hold A, then B, release B → A; release A → the base.
//! Releasing one that is not on top (A while B is still held) splices it out, so B
//! returns straight to what A would have returned to. A latch or trigger makes a
//! new base and clears the stack.
//!
//! A track can also hold one **armed** clip (LIVE-MODEL-SPEC §5): arming readies a
//! clip without playing it, and GO ([`take_armed`](ZoneDeck::take_armed)) plays every
//! armed clip together. Arming the armed clip again disarms it. Playing the armed clip
//! directly also disarms it (it is no longer waiting for GO).
//!
//! This is the decision logic only — pure, no tree, no GPU — so the engine's cell
//! controller and its tests share one definition.

/// One zone's playback state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneDeck {
    active: Option<usize>,
    holds: Vec<Hold>,
    armed: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hold {
    cell: usize,
    back_to: Option<usize>,
}

/// What the zone's content must do after a deck operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckChange {
    /// Nothing to do: the running content is unchanged.
    None,
    /// Run this cell (`None` = nothing playing). `restart` asks for the effect to
    /// start from its beginning even if it is the one already running.
    Play { cell: Option<usize>, restart: bool },
}

impl ZoneDeck {
    pub fn new(active: Option<usize>) -> Self {
        Self {
            active,
            holds: Vec::new(),
            armed: None,
        }
    }

    /// The running cell.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// The armed clip, waiting for GO.
    pub fn armed(&self) -> Option<usize> {
        self.armed
    }

    /// Arm `cell` (replacing any armed clip), or disarm it if it already is. Returns
    /// the armed clip afterwards. Never changes what plays.
    pub fn arm(&mut self, cell: usize) -> Option<usize> {
        self.armed = if self.armed == Some(cell) {
            None
        } else {
            Some(cell)
        };
        self.armed
    }

    /// Disarm (Esc, `/arm/clear`). Returns whether something was armed.
    pub fn disarm(&mut self) -> bool {
        self.armed.take().is_some()
    }

    /// GO: play the armed clip as a latch (a new base, temporary clips forgotten) and
    /// disarm. [`DeckChange::None`] if nothing is armed — or if it is already the base.
    pub fn take_armed(&mut self) -> DeckChange {
        match self.armed.take() {
            Some(cell) => self.latch(cell),
            None => DeckChange::None,
        }
    }

    /// Stop: nothing plays, temporary clips are forgotten. The armed clip stays armed.
    pub fn stop(&mut self) -> DeckChange {
        self.set_base(None)
    }

    /// Whether `cell` is currently held as a temporary cell.
    pub fn is_held(&self, cell: usize) -> bool {
        self.holds.iter().any(|h| h.cell == cell)
    }

    /// Make `cell` the new base content.
    pub fn latch(&mut self, cell: usize) -> DeckChange {
        self.holds.clear();
        self.play(Some(cell), false)
    }

    /// Make `cell` the new base content and restart it.
    pub fn trigger(&mut self, cell: usize) -> DeckChange {
        self.holds.clear();
        self.active = Some(cell);
        DeckChange::Play {
            cell: Some(cell),
            restart: true,
        }
    }

    /// Set the base content from outside the pads (a show load, a grid edit that
    /// assigns and runs a cell). Clears temporary cells like a latch.
    pub fn set_base(&mut self, cell: Option<usize>) -> DeckChange {
        self.holds.clear();
        self.play(cell, false)
    }

    /// Start `cell` temporarily. Pressing a cell that is already held does nothing.
    pub fn press(&mut self, cell: usize) -> DeckChange {
        if self.is_held(cell) {
            return DeckChange::None;
        }
        self.holds.push(Hold {
            cell,
            back_to: self.active,
        });
        self.play(Some(cell), false)
    }

    /// End a temporary `cell`: the zone returns to what ran before it. A cell that is
    /// not held (never pressed, or cleared by a latch meanwhile) does nothing.
    pub fn release(&mut self, cell: usize) -> DeckChange {
        let Some(pos) = self.holds.iter().position(|h| h.cell == cell) else {
            return DeckChange::None;
        };
        let released = self.holds.remove(pos);
        if pos < self.holds.len() {
            // Not on top: whoever was held after it now returns where it would have.
            self.holds[pos].back_to = released.back_to;
            DeckChange::None
        } else {
            self.play(released.back_to, false)
        }
    }

    fn play(&mut self, cell: Option<usize>, restart: bool) -> DeckChange {
        if cell.is_some() && self.armed == cell {
            self.armed = None; // played directly: no longer waiting for GO
        }
        if self.active == cell && !restart {
            return DeckChange::None;
        }
        self.active = cell;
        DeckChange::Play { cell, restart }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(cell: Option<usize>) -> DeckChange {
        DeckChange::Play {
            cell,
            restart: false,
        }
    }

    #[test]
    fn momentary_returns_to_the_base() {
        let mut d = ZoneDeck::new(Some(0));
        assert_eq!(d.press(3), play(Some(3)));
        assert_eq!(d.active(), Some(3));
        assert_eq!(d.press(3), DeckChange::None); // already held
        assert_eq!(d.release(3), play(Some(0)));
        assert_eq!(d.release(3), DeckChange::None); // not held any more
                                                    // From nothing playing, a flash returns to nothing.
        let mut d = ZoneDeck::new(None);
        d.press(2);
        assert_eq!(d.release(2), play(None));
    }

    #[test]
    fn nested_holds_unwind_in_any_order() {
        let mut d = ZoneDeck::new(Some(0));
        d.press(1);
        d.press(2);
        assert_eq!(d.release(2), play(Some(1)));
        assert_eq!(d.release(1), play(Some(0)));

        // Release the lower one first: nothing changes now, and the top one then
        // returns straight to the base.
        d.press(1);
        d.press(2);
        assert_eq!(d.release(1), DeckChange::None);
        assert_eq!(d.active(), Some(2));
        assert_eq!(d.release(2), play(Some(0)));
    }

    #[test]
    fn latch_and_trigger_make_a_new_base() {
        let mut d = ZoneDeck::new(Some(0));
        d.press(1);
        assert_eq!(d.latch(4), play(Some(4)));
        // The held cell was forgotten: its release no longer returns anywhere.
        assert_eq!(d.release(1), DeckChange::None);
        assert_eq!(d.active(), Some(4));
        // Latching the running cell again is a no-op; triggering it restarts.
        assert_eq!(d.latch(4), DeckChange::None);
        assert_eq!(
            d.trigger(4),
            DeckChange::Play {
                cell: Some(4),
                restart: true
            }
        );
    }

    #[test]
    fn arming_readies_without_playing_and_toggles() {
        let mut d = ZoneDeck::new(Some(0));
        assert_eq!(d.arm(2), Some(2));
        assert_eq!(d.active(), Some(0), "arming never plays");
        assert_eq!(d.arm(3), Some(3), "a new arm replaces the old one");
        assert_eq!(d.arm(3), None, "arming the armed clip disarms it");
        assert_eq!(d.armed(), None);
    }

    #[test]
    fn go_plays_the_armed_clip_as_a_latch_and_disarms() {
        let mut d = ZoneDeck::new(Some(0));
        d.press(1);
        d.arm(4);
        assert_eq!(d.take_armed(), play(Some(4)));
        assert_eq!(d.armed(), None);
        assert_eq!(
            d.release(1),
            DeckChange::None,
            "GO latches: holds forgotten"
        );
        assert_eq!(
            d.take_armed(),
            DeckChange::None,
            "nothing armed: GO does nothing"
        );
        // Arming the running clip and GO: already the base, nothing to do.
        d.arm(4);
        assert_eq!(d.take_armed(), DeckChange::None);
    }

    #[test]
    fn playing_the_armed_clip_directly_disarms_it() {
        let mut d = ZoneDeck::new(None);
        d.arm(2);
        d.latch(2);
        assert_eq!(d.armed(), None);
        // Playing another clip leaves the arm alone.
        d.arm(3);
        d.latch(1);
        assert_eq!(d.armed(), Some(3));
    }

    #[test]
    fn stop_clears_the_base_and_holds_but_keeps_the_arm() {
        let mut d = ZoneDeck::new(Some(0));
        d.press(1);
        d.arm(2);
        assert_eq!(d.stop(), play(None));
        assert_eq!(d.active(), None);
        assert_eq!(d.release(1), DeckChange::None);
        assert_eq!(d.armed(), Some(2));
        assert_eq!(d.stop(), DeckChange::None, "already stopped");
    }

    #[test]
    fn disarm_clears_the_arm_and_reports_it() {
        let mut d = ZoneDeck::new(Some(0));
        assert!(!d.disarm(), "nothing armed");
        d.arm(3);
        assert!(d.disarm());
        assert_eq!(d.armed(), None);
        assert_eq!(d.active(), Some(0), "disarming never touches what plays");
    }
}

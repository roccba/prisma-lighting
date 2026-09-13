//! Per-zone cell playback state: which cell runs, and where temporary cells return
//! to (M9 Slice 3 — pad modes on the scene grid).
//!
//! A zone's grid (CLAUDE.md §4.3) has one **active** cell. Cells are started four
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
//! This is the decision logic only — pure, no tree, no GPU — so the engine's cell
//! controller and its tests share one definition.

/// One zone's playback state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneDeck {
    active: Option<usize>,
    holds: Vec<Hold>,
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
        }
    }

    /// The running cell.
    pub fn active(&self) -> Option<usize> {
        self.active
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
}

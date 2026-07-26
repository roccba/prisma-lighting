//! [`PatchDraft`] — the editable patch plus its undo/redo history.
//!
//! This is the editor's single source of truth (Part A decision): the app holds a
//! `PatchDraft` in Rust, mutates it only through [`apply`](PatchDraft::apply), and
//! renders the returned state. The methods here are exactly the "structural command
//! surface" the Tauri IPC layer wraps (`patch_apply_op`, `patch_undo`,
//! `patch_redo`, `patch_validate`, …).

use prisma_patch::{Diagnostic, Patch, PatchError};

use crate::op::{self, EditError, EditOp};

/// Identifies a continuous user gesture (a drag, a slider sweep). Ops applied under
/// the same `GestureId` collapse into one undo entry so a whole drag undoes at once
/// (M7 spec §8). The app mints a fresh id per gesture (e.g. on mouse-down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureId(pub u64);

/// One undo/redo entry: the op that reverses the applied edit, plus the gesture it
/// belonged to (for coalescing).
struct Entry {
    inverse: EditOp,
    gesture: Option<GestureId>,
}

/// The editable patch and its history. Never mutate the patch directly — go through
/// [`apply`](PatchDraft::apply) so every change is undoable.
pub struct PatchDraft {
    patch: Patch,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// Set on any successful mutation, cleared by [`mark_saved`](PatchDraft::mark_saved).
    dirty: bool,
}

impl PatchDraft {
    /// Start editing `patch` with an empty history.
    pub fn new(patch: Patch) -> Self {
        Self {
            patch,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        }
    }

    /// The current patch. The canvas and the properties panel render from this.
    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    /// Apply an edit. `gesture` groups it with adjacent edits of the same gesture
    /// into a single undo step; pass `None` for a discrete edit.
    ///
    /// On a structural error the patch is untouched and the history unchanged. On
    /// success the redo stack is cleared (a new edit forks history) and the draft is
    /// marked dirty.
    pub fn apply(&mut self, op: EditOp, gesture: Option<GestureId>) -> Result<(), EditError> {
        let key = op.coalesce_key();
        let inverse = op::apply(&mut self.patch, op)?;

        // Coalesce: if this op continues the same gesture on the same target as the
        // top undo entry, keep that entry's inverse (it already restores to the
        // pre-gesture state) and drop this one. Snapshot inverses make this exact.
        if let (Some(g), Some(k)) = (gesture, key) {
            if let Some(top) = self.undo.last() {
                if top.gesture == Some(g) && top.inverse.coalesce_key() == Some(k) {
                    self.redo.clear();
                    self.dirty = true;
                    return Ok(());
                }
            }
        }

        self.undo.push(Entry { inverse, gesture });
        self.redo.clear();
        self.dirty = true;
        Ok(())
    }

    /// Undo the most recent edit (or coalesced gesture). Returns `false` if there is
    /// nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(entry) => {
                // Applying the inverse yields the original forward op — push it to
                // redo. The inverse was built from valid state, so this cannot fail;
                // a failure here would be an internal invariant break.
                let redo_op = op::apply(&mut self.patch, entry.inverse)
                    .expect("undo: applying a stored inverse must succeed");
                self.redo.push(Entry {
                    inverse: redo_op,
                    gesture: entry.gesture,
                });
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Redo the most recently undone edit. Returns `false` if there is nothing to
    /// redo.
    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(entry) => {
                let undo_op = op::apply(&mut self.patch, entry.inverse)
                    .expect("redo: applying a stored inverse must succeed");
                self.undo.push(Entry {
                    inverse: undo_op,
                    gesture: entry.gesture,
                });
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Whether there is an edit to undo / redo (for greying out toolbar buttons).
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Re-run the compile gate on the current draft: the first structural error, or
    /// `Ok(())`. Kept for callers that only need a yes/no; the editor's discreet
    /// notice uses [`wire_faults`](Self::wire_faults) instead.
    pub fn validate(&self) -> Result<(), PatchError> {
        self.patch.validate()
    }

    /// Collect the current draft's **physical DMX wire faults** (§1, §4.3, S6 replan) —
    /// address collisions and out-of-range channels — each annotated with the pixels it
    /// points at, for the editor's discreet notice and canvas marks. Never blocking:
    /// the editor shows these but the edit that produced them still applied. Out-of-
    /// matrix positions and shared cells are valid choices and are deliberately **not**
    /// reported (§2.4, §4.4).
    pub fn wire_faults(&self) -> Vec<Diagnostic> {
        self.patch.wire_faults()
    }

    /// Whether the draft has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the draft as saved (clears the dirty flag). Called after a successful
    /// write to disk (S8).
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
}

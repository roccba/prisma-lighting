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
    /// Start editing `patch` with an empty history. The world `matrix_size` is fitted
    /// to the rig on load (a derived fact, not a stored knob — see
    /// [`Patch::fit_matrix_size`]); this normalises any patch saved before auto-fit
    /// without marking the draft dirty.
    pub fn new(mut patch: Patch) -> Self {
        patch.fit_matrix_size();
        // The editor works purely in fixture→zone OWNERSHIP space (§2.2): `Zone::members`
        // — the engine's DERIVED overlap membership — is not the editor's to hold. A show
        // patch arrives here with members already filled (a previous apply-to-live ran
        // `fill_zone_members_from_ownership`), and a `.prisma`/`.prismap` may carry them
        // too. Keeping them is a latent bug: `RemoveFixtures` (and any fixture deletion)
        // deletes the fixture but NOT the members that point at it, leaving DANGLING refs.
        // `fill_zone_members_from_ownership` only fills zones whose members are EMPTY, so
        // it would skip those polluted zones — the dangling refs survive into
        // `validate_zone_refs`, fail compile, and silently abort the next apply-to-live,
        // freezing the running show on the pre-delete layout. Clearing members on seed
        // keeps every edit in ownership space and lets the apply path re-derive membership
        // fresh from what actually remains.
        for zone in &mut patch.zones {
            zone.members.clear();
        }
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
        // The world size is derived from where the pixels are, so re-fit it after any
        // edit that may have added or moved them. `matrix_size` is not part of any op's
        // diff, so undo/redo re-fit too (below), keeping it a pure derived fact.
        self.patch.fit_matrix_size();

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
                self.patch.fit_matrix_size(); // derived; re-fit after the geometry moves
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
                self.patch.fit_matrix_size(); // derived; re-fit after the geometry moves
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::wedding_rig;

    /// Seeding a draft from a show patch (which carries DERIVED `Zone::members`) must
    /// leave the draft in ownership space — members empty — so fixture edits never leave
    /// dangling refs. Regression: previously members survived the seed, and deleting a
    /// fixture left members pointing at it.
    #[test]
    fn new_clears_zone_members_from_a_show_patch() {
        let mut show_patch = wedding_rig();
        // A show patch has members filled by a prior apply-to-live.
        show_patch.fill_zone_members_from_ownership();
        assert!(show_patch.zones.iter().any(|z| !z.members.is_empty()));

        let draft = PatchDraft::new(show_patch);
        assert!(
            draft.patch().zones.iter().all(|z| z.members.is_empty()),
            "the editor draft must hold no overlap membership"
        );
    }

    /// The reported bug end-to-end: open a show-derived patch in the editor, delete EVERY
    /// fixture, then run the apply path's derive+compile. It must succeed (an empty rig),
    /// not fail with a dangling-fixture ref — which is what froze the running show.
    #[test]
    fn deleting_all_fixtures_then_applying_compiles() {
        let mut show_patch = wedding_rig();
        show_patch.fill_zone_members_from_ownership();

        let mut draft = PatchDraft::new(show_patch);
        let all: Vec<_> = draft.patch().fixtures.iter().map(|f| f.id).collect();
        draft
            .apply(EditOp::RemoveFixtures { ids: all }, None)
            .expect("removing all fixtures is a valid edit");
        assert!(draft.patch().fixtures.is_empty());

        // Reproduce what `apply_editor_draft` does before touching the engine.
        let mut candidate = draft.patch().clone();
        candidate.fill_zone_members_from_ownership();
        candidate.fit_matrix_size();
        candidate
            .compile()
            .expect("an empty rig must compile, not fail on a dangling zone ref");
    }

    /// Deleting SOME fixtures must also re-derive membership only over what remains.
    #[test]
    fn deleting_some_fixtures_then_applying_compiles() {
        let mut show_patch = wedding_rig();
        show_patch.fill_zone_members_from_ownership();

        let mut draft = PatchDraft::new(show_patch);
        let victims: Vec<_> = draft
            .patch()
            .fixtures
            .iter()
            .take(3)
            .map(|f| f.id)
            .collect();
        draft
            .apply(EditOp::RemoveFixtures { ids: victims }, None)
            .expect("removing a subset is a valid edit");

        let mut candidate = draft.patch().clone();
        candidate.fill_zone_members_from_ownership();
        candidate.fit_matrix_size();
        let index = candidate
            .compile()
            .expect("the remaining rig must compile cleanly");
        // Membership now covers only surviving fixtures' pixels.
        let remaining_pixels: usize = candidate.fixtures.iter().map(|f| f.pixels.len()).sum();
        assert_eq!(index.pixel_count() as usize, remaining_pixels);
    }
}

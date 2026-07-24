//! The patch-editor backend (CLAUDE.md milestone M7 / S2).
//!
//! Owns the Rust-side editable draft of a patch and the invertible edit operations
//! that mutate it. This is the editor's single source of truth (Part A decision):
//! the M7 editor makes **structural** edits as typed [`EditOp`]s over a
//! [`PatchDraft`], not as Parameter-Tree traffic. Every op is exactly reversible
//! ([`op::apply`] returns its inverse), so undo/redo re-produce the patch
//! byte-for-byte and a whole drag collapses to one undo step via gesture coalescing.
//!
//! The Tauri app wraps [`PatchDraft`]'s methods as IPC commands; keeping the logic
//! in this leaf crate (it depends only on `prisma-patch`) makes it testable
//! headlessly and independent of the UI.
//!
//! # Scope boundary
//!
//! - **Structural editing only.** Live show values (dimmers, colours) are
//!   Parameter-Tree concerns, not draft edits.
//! - **Validation does not block edits.** The draft may be transiently invalid; the
//!   editor surfaces problems visually (§4.3/§4.4). See [`PatchDraft::validate`].
//! - **Compile/apply to the live engine** (recompile on editor close, §7) is a
//!   bridge to `prisma-engine`, not part of this crate.

pub mod draft;
pub mod op;
pub mod placement;
pub mod samples;
pub mod selection;

pub use draft::{GestureId, PatchDraft};
pub use op::{CoalesceKey, EditError, EditOp, PixelAddress};
pub use placement::{
    builtin_fixture_types, next_free_address, plan, FixtureType, Orientation, PlacementError,
    PlacementSpec,
};
pub use selection::{aggregate, SelectionAggregate};

//! Patch model + serialization (CLAUDE.md §4.1, milestone M2).
//!
//! The [`Patch`] is the fixed physical/logical wiring of a rig: where every
//! pixel lives, how it is addressed on the wire, and how pixels group into
//! [`Zone`]s. It holds **no performable state** (no layers, no dimmers) — that
//! belongs to the Show. One Patch backs many Shows (§3.1 principle 6).
//!
//! # What this crate owns
//!
//! - The serialized model: [`Patch`], [`Fixture`], [`Pixel`], [`Zone`] and the
//!   supporting enums, with JSON serialization and a versioned envelope
//!   ([`version`]).
//! - The runtime-derived [`PatchIndex`]: global pixel indexing, the canonical
//!   master-buffer byte layout, and resolved zone membership — the inputs the
//!   render/compositor (M5) and the output-plan compiler (in `prisma-output`)
//!   build on. **The global pixel index is never serialized** (see [`index`]).
//!
//! # What this crate does not own
//!
//! The compilation of a patch into a wire `OutputPlan` (color-order permutation,
//! gamma LUT, and slicing into universes) lives in `prisma-output`, which
//! depends on this crate. Keeping it out here leaves the model dependency-free
//! and headlessly testable (§7).

pub mod error;
pub mod fixture;
pub mod geometry;
pub mod index;
pub mod patch;
pub mod version;
pub mod zone;

/// Maximum DMX channels in one universe. A protocol constant (owned here as the
/// leaf crate; `prisma-output` consumes the same fact).
pub const CHANNELS_PER_UNIVERSE: usize = 512;

pub use error::PatchError;
pub use fixture::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Pixel, WireOrder,
};
pub use geometry::{cell_of, Rect, WorldTransform};
pub use index::{LayoutGeneration, PatchIndex, ZoneMembership};
pub use patch::Patch;
pub use version::{load_patch, save_patch, CURRENT_SCHEMA_VERSION, FORMAT_ID};
pub use zone::{default_display_color, BlendMode, DisplayColor, PixelRef, Zone};

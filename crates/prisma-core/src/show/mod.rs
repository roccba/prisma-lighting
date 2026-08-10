//! The Show model and its `.prisma` persistence (CLAUDE.md §4.3, M8-SPEC §5).
//!
//! The Show is the performable document: per-zone grids of assignable cells, each
//! zone's always-live state, and the master state, over an **embedded** Patch.
//! See [`model`] for the data model and [`version`] for the on-disk envelope,
//! versioning and migration (mirroring `prisma_patch::version`).
//!
//! This is the M8 "show model" the repo layout (§7) and the crate docs place in
//! `prisma-core`. It depends on `prisma-patch` (the embedded [`Patch`]), which is
//! allowed — §7 forbids only a dependency on `prisma-render`.
//!
//! [`Patch`]: prisma_patch::Patch

mod error;
mod model;
mod version;

pub use error::ShowError;
pub use model::{
    Cell, Layer, MasterState, NetworkConfig, ParamSet, Show, Source, ZoneLive, ZoneShow,
};
pub use version::{load_show, save_show, CURRENT_SCHEMA_VERSION, FORMAT_ID};

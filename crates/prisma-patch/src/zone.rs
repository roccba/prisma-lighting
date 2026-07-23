//! Zones: logical groupings of pixels, decoupled from physical layout
//! (CLAUDE.md §3.1 principle 5). A pixel MAY belong to more than one zone; that
//! overlap is what a global strobe/flash zone is built on (§4.1).
//!
//! This module holds only the **static** membership and compositing metadata.
//! The dynamic per-pixel active-zone counter (§5.2.1) is runtime state owned by
//! the render/compositor milestone (M5); it is fed by the resolved membership in
//! [`crate::index`], not stored here.

use crate::geometry::Rect;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a zone (or a layer) blends onto whatever is below it. The same set is
/// reused for intra-zone layer blending and inter-zone overlap resolution
/// (CLAUDE.md §4.1, §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Mask,
    Max,
}

/// A stable reference to one pixel: the owning fixture plus the pixel's index
/// **within that fixture**. Deliberately not a global index — global indices are
/// a runtime derivative that reordering fixtures would silently invalidate
/// (see [`crate::index`]). Referencing by `(fixture id, local index)` stays valid
/// and diffable across edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRef {
    pub fixture: Uuid,
    pub pixel: u32,
}

/// A zone: a named set of pixels with compositing order and blend mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub id: Uuid,
    pub name: String,
    /// The pixels this zone controls. A pixel may appear in several zones;
    /// duplicate references to the *same* pixel within one zone are collapsed
    /// when the membership is resolved (otherwise the overlap counter would
    /// double-count it — see [`crate::index`]).
    pub members: Vec<PixelRef>,
    /// Cached 2D bounding box for effect mapping. Recomputable from `members`.
    #[serde(default)]
    pub bounds: Option<Rect>,
    /// Compositing order for overlap resolution. Higher `z_order` composites on
    /// top; ties are broken by declaration order in the patch (documented and
    /// deterministic).
    pub z_order: i32,
    /// How this zone blends onto the zones below it (§5.2).
    pub blend_mode: BlendMode,
}

//! Zones: logical groupings of pixels, decoupled from physical layout
//! (CLAUDE.md §3.1 principle 5). A pixel MAY belong to more than one zone; that
//! overlap is what a global strobe/flash zone is built on (§4.1).
//!
//! This module holds only the **static** membership and compositing metadata.
//! The dynamic per-pixel active-zone counter (§5.2.1) is runtime state owned by
//! the render/compositor milestone (M5); it is fed by the resolved membership in
//! [`crate::index`], not stored here.

use crate::geometry::{Rect, WorldTransform};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A zone's display colour on the patch-editor canvas (M7 spec §2.3): identifies
/// the zone at a glance. Stored as 8-bit sRGB, serialized as `[r, g, b]` — this
/// is a UI label colour, not a light value, so it needs no linear-light precision.
pub type DisplayColor = [u8; 3];

/// Neutral mid-grey, the `#[serde(default)]` for a zone that omits its display
/// colour (a partial hand-written file). The migration and the editor always set
/// a real colour; this only keeps such a file loadable.
pub fn default_display_color() -> DisplayColor {
    [128, 128, 128]
}

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
///
/// # M7 editor fields (`mapping_size`, `display_color`, `world_transform`)
///
/// These three describe the zone as the patch editor works with it (M7 spec
/// §2.3–§2.4). `mapping_size` is the zone's **own** local matrix — you edit each
/// zone at a comfortable scale, without one zone's geometry constraining
/// another's. `world_transform` is its **optional** placement in the shared venue
/// space, needed only for cross-zone effects.
///
/// *Not yet consumed by render.* The render gather still samples one global
/// `Patch::matrix_size` (CLAUDE.md §5.2 step 5). Per-zone mapping spaces are
/// stored and edited in M7 but wired through the gather in M8 — see the M7
/// explicit-debt note in CLAUDE.md. Until then pixel positions are validated and
/// sampled against `Patch::matrix_size`, not against this field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub id: Uuid,
    pub name: String,
    /// The pixels this zone controls. A pixel may appear in several zones;
    /// duplicate references to the *same* pixel within one zone are collapsed
    /// when the membership is resolved (otherwise the overlap counter would
    /// double-count it — see [`crate::index`]).
    ///
    /// This is the engine's **overlap** membership. It is *not* how the editor
    /// expresses which zone a fixture belongs to — that is fixture→zone ownership
    /// ([`Fixture::zone`](crate::fixture::Fixture::zone), M7 spec §2.2). The editor
    /// never writes overlapping membership here; the field stays for a later
    /// global-effect zone (M8+) and the overlap machinery already built in M5.
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
    /// The zone's own local mapping matrix (M7 spec §2.4). Every axis is `>= 1`.
    /// **Required** — a v1 patch (one global matrix) gains this via the v1→v2
    /// migration, which seeds it from the old `Patch::matrix_size`.
    pub mapping_size: [u32; 3],
    /// Colour identifying this zone on the editor canvas (§2.3).
    #[serde(default = "default_display_color")]
    pub display_color: DisplayColor,
    /// Optional placement of this zone's local space in the world/venue (§2.4).
    /// `None` = default arrangement; only cross-zone effects need it set.
    #[serde(default)]
    pub world_transform: Option<WorldTransform>,
}

//! Spatial primitives for the patch: positions in the virtual matrix and cached
//! 2D bounding boxes for zones.
//!
//! Positions are `glam::Vec3` (Z = 0 for the current 2D matrix, non-zero when
//! 3D patching lands — a Deferred item). `glam`'s `serde` feature serializes a
//! `Vec3` as a `[x, y, z]` array, which stays human-readable and diffable
//! (CLAUDE.md §4.1).

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Map a position in the virtual matrix to the **cell** it samples — the single
/// definition of that rule, shared by patch validation and the render's gather
/// (CLAUDE.md §5.2 step 5) so the two can never disagree.
///
/// # Nearest cell, deliberately
///
/// Each integer coordinate is a cell centre, so position `1.4` samples cell `1`
/// and `1.6` samples cell `2`. There is **no filtering**: a pixel reads exactly
/// one texel of the virtual matrix, never a blend of neighbours. LEDs are discrete
/// emitters; interpolating between cells would smear an effect's hard edges (a
/// chase, a strobe boundary) across fixtures that should switch cleanly.
///
/// # Several pixels in one cell is INTENTIONAL — do not "fix" it
///
/// When the virtual matrix is coarser than the rig (many LEDs per cell), every
/// pixel in a cell reads the same texel and therefore shows the same colour. That
/// is the designed behaviour, not a resolution bug: `matrix_size` **is** the
/// effect's resolution, deliberately decoupled from the physical pixel count
/// (§3.1 principle 5). An operator who wants neighbouring LEDs to differ raises
/// `matrix_size` — the remedy is a higher-resolution matrix, never a change to
/// this sampling rule.
///
/// Returns `None` if any component is not finite (NaN/±inf), which callers treat
/// as an invalid position rather than silently sampling cell 0.
pub fn cell_of(position: Vec3) -> Option<[i64; 3]> {
    if !position.is_finite() {
        return None;
    }
    Some([
        position.x.round() as i64,
        position.y.round() as i64,
        position.z.round() as i64,
    ])
}

/// A cached 2D axis-aligned bounding box (`Zone::bounds`, CLAUDE.md §4.1). Stored
/// as `min`/`max` corners so it round-trips exactly and is trivial to grow when
/// accumulating over a zone's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    /// The bounding box of a set of 2D points, or `None` if the iterator is empty.
    /// Uses only the X/Y of each position — Z is layout depth, not part of the
    /// 2D effect-mapping box.
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut min = first.truncate();
        let mut max = min;
        for p in iter {
            let p = p.truncate();
            min = min.min(p);
            max = max.max(p);
        }
        Some(Self { min, max })
    }
}

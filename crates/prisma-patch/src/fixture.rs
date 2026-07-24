//! Fixtures and pixels: the physical/electrical description of the rig.
//!
//! A [`Fixture`] carries a [`FixtureProfile`] (component set + gamma/color
//! calibration) shared by all its pixels; each [`Pixel`] carries its own DMX
//! address and [`ColorOrder`]. The color order and gamma are **not** applied
//! here — they are compiled into the wire `OutputPlan` (in `prisma-output`).
//! This crate only describes them (CLAUDE.md §4.1, §5.2).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The set of color components a fixture drives per pixel. Determines how many
/// bytes a pixel occupies in the canonical (linear-light) master buffer and how
/// many DMX channels it consumes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentSet {
    /// Red, Green, Blue — 3 components.
    Rgb,
    /// Red, Green, Blue, White — 4 components. White is canonical index 3.
    Rgbw,
}

impl ComponentSet {
    /// Number of components (and therefore canonical bytes and DMX channels) per
    /// pixel.
    #[inline]
    pub const fn count(self) -> u8 {
        match self {
            ComponentSet::Rgb => 3,
            ComponentSet::Rgbw => 4,
        }
    }
}

/// Wire order of a pixel's RGB channels. The canonical order in the master
/// buffer is always `[R, G, B(, W)]`; `ColorOrder` describes the permutation the
/// physical fixture expects on the wire. Because it is compiled into which
/// canonical byte each DMX channel pulls, color order costs nothing at runtime
/// (CLAUDE.md §5.2, point 4 of the M2 design).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorOrder {
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
}

impl ColorOrder {
    /// Maps wire position → canonical RGB index (`0 = R`, `1 = G`, `2 = B`).
    /// `perm[k]` is the canonical component emitted on the pixel's k-th RGB wire
    /// channel.
    #[inline]
    pub const fn rgb_permutation(self) -> [u8; 3] {
        match self {
            ColorOrder::Rgb => [0, 1, 2],
            ColorOrder::Rbg => [0, 2, 1],
            ColorOrder::Grb => [1, 0, 2],
            ColorOrder::Gbr => [1, 2, 0],
            ColorOrder::Brg => [2, 0, 1],
            ColorOrder::Bgr => [2, 1, 0],
        }
    }
}

/// Wire → canonical component permutation for a whole pixel, including White.
/// Only the first [`len`](WireOrder::len) entries are meaningful.
///
/// Built by [`FixtureProfile::wire_order`]. Consumed by the output-plan compiler
/// to decide, per DMX channel, which byte of the canonical master buffer to pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireOrder {
    entries: [u8; 4],
    len: u8,
}

impl WireOrder {
    /// `wire_channel_offset → canonical_component_index`, length equal to the
    /// pixel's component count.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.entries[..self.len as usize]
    }
}

/// Per-fixture gamma and color calibration. Real LED fixtures are not
/// color-accurate and installations mix batches (CLAUDE.md §5.2), so this is
/// per-fixture, not global. The actual LUT is built at output-plan compile time;
/// this only stores the parameters.
///
/// `gamma` defaults to `1.0` (identity): an uncalibrated patch applies no gamma,
/// which keeps a trivial patch byte-identical to the M1 direct path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GammaCalibration {
    /// Encoding exponent applied at the output stage (`out = in^gamma`). `1.0`
    /// is identity.
    #[serde(default = "default_gamma")]
    pub gamma: f32,
}

fn default_gamma() -> f32 {
    1.0
}

impl Default for GammaCalibration {
    fn default() -> Self {
        Self {
            gamma: default_gamma(),
        }
    }
}

/// The channel layout and calibration a group of pixels shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureProfile {
    /// Human-readable profile name (e.g. `"WS2812 GRB"`); UI/label only.
    pub name: String,
    pub components: ComponentSet,
    #[serde(default)]
    pub gamma: GammaCalibration,
}

impl FixtureProfile {
    /// Number of components (canonical bytes / DMX channels) per pixel.
    #[inline]
    pub fn component_count(&self) -> u8 {
        self.components.count()
    }

    /// The full wire → canonical permutation for a pixel with this profile and
    /// the given `order`. For `Rgbw`, White is canonical index 3 and — for M2 —
    /// always the last wire channel (arbitrary White placement is deferred).
    pub fn wire_order(&self, order: ColorOrder) -> WireOrder {
        let rgb = order.rgb_permutation();
        match self.components {
            ComponentSet::Rgb => WireOrder {
                entries: [rgb[0], rgb[1], rgb[2], 0],
                len: 3,
            },
            ComponentSet::Rgbw => WireOrder {
                entries: [rgb[0], rgb[1], rgb[2], 3],
                len: 4,
            },
        }
    }
}

/// One controllable pixel: where it lives in space and how it is addressed on
/// the wire. Its `index` is the position **within its fixture**, not a global
/// index — the global master-buffer index is a runtime derivative and is never
/// stored (see [`crate::index`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pixel {
    /// Position of this pixel within its fixture's `pixels` vector. Used by
    /// [`PixelRef`](crate::zone::PixelRef) to name a pixel stably across edits.
    pub index: u32,
    /// Position in the virtual matrix. `Z = 0` for the current 2D rig.
    pub position: Vec3,
    /// Art-Net/DMX universe (15-bit port address for Art-Net).
    pub universe: u16,
    /// First DMX channel of this pixel, `1..=512`.
    pub start_channel: u16,
    pub color_order: ColorOrder,
}

/// A fixture: a group of pixels sharing one profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
    pub id: Uuid,
    pub name: String,
    pub profile: FixtureProfile,
    pub pixels: Vec<Pixel>,
    /// The zone this fixture belongs to — **fixture→zone ownership**, the single
    /// source of truth for zone membership as the editor sees it (M7 spec §2.2,
    /// §3.1). A fixture belongs to exactly one zone; the editor manipulates this
    /// field ("move this tube to the Booth zone"), and it is what selects the
    /// [`Zone::mapping_size`](crate::zone::Zone::mapping_size) a pixel's position
    /// is read in.
    ///
    /// Distinct from [`Zone::members`](crate::zone::Zone::members), which is the
    /// engine's *overlap* membership (a pixel may be in several zones). Ownership
    /// is one-to-one; overlap membership is many-to-many.
    ///
    /// `#[serde(default)]` → `Uuid::nil()` ("unassigned") so a partial or
    /// programmatic patch loads; the v1→v2 migration derives a real owner for
    /// legacy files, and the editor keeps it valid. Not consumed by the render or
    /// output pipeline yet (§ M7 debt), so a nil owner is tolerated there.
    #[serde(default)]
    pub zone: Uuid,
}

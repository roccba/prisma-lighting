//! Fixture placement (M7 spec §4): instantiate many fixtures in one action from a
//! [`PlacementSpec`] — the "count X/Y + offset" grid the placement dialog drives,
//! plus Prisma's addition of **diagonal stepped** runs (§4.2).
//!
//! [`plan`] is a pure function (spec → fixtures); it does not touch the patch, so
//! the placement math — grid positions, the diagonal disjointness requirement,
//! addressing with universe wrap and the per-universe limit — is unit-testable. The
//! resulting fixtures are handed to an [`InsertFixtures`](crate::EditOp::InsertFixtures)
//! op so placement is undoable like any other edit.

use glam::Vec3;
use prisma_patch::{ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Pixel};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A reusable fixture definition (§3.2, minimal for M7). The full fixture library —
/// a separate reusable file, with intra-fixture pixel roles (the hexagon case) —
/// is later; the model here must not preclude it, so richer arrangement/roles can
/// be added without breaking this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureType {
    pub name: String,
    pub pixel_count: u32,
    pub components: ComponentSet,
    pub color_order: ColorOrder,
}

/// How a fixture's pixels run from its origin. Prisma adds the two diagonals to
/// professional consoles's horizontal/vertical (§4.2); diagonals are **stepped** — each pixel
/// advances one cell on both axes, an honest staircase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    Horizontal,
    Vertical,
    DiagonalDown,
    DiagonalUp,
}

impl Orientation {
    /// The per-pixel cell step.
    pub fn step(self) -> (i32, i32) {
        match self {
            Orientation::Horizontal => (1, 0),
            Orientation::Vertical => (0, 1),
            Orientation::DiagonalDown => (1, 1),
            Orientation::DiagonalUp => (1, -1),
        }
    }
}

/// Everything the placement dialog collects (§4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementSpec {
    pub fixture_type: FixtureType,
    /// Name prefix; instances are `"{prefix} {n}"` from `start_number`.
    pub name_prefix: String,
    pub start_number: u32,
    /// Instances along X and Y (2D; Z is deferred with 3D).
    pub count: [u32; 2],
    /// Spacing in cells between instance origins, per axis.
    pub offset: [i32; 2],
    /// Origin of the first instance, in cells.
    pub start_position: [i32; 2],
    pub orientation: Orientation,
    /// Snake mode: alternate rows are addressed in reverse, so the signal path
    /// wraps back and forth instead of jumping across the matrix each row (§4.1).
    pub snake: bool,
    pub start_universe: u16,
    pub start_channel: u16,
    /// Maximum fixtures per universe before spilling to the next (§4.1). `None` =
    /// only the 512-channel limit applies.
    pub fixtures_per_universe: Option<u32>,
    /// The zone placed fixtures belong to — the **active zone** (§4.1, §6). This is
    /// what makes the zone selector the editor's organising control.
    pub zone: Uuid,
}

/// Why a placement could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// Zero instances, or a zero-pixel fixture — nothing to place.
    Empty,
    /// One fixture's channel span exceeds a whole universe (512), so it can never
    /// be addressed.
    FixtureTooBig { span: u16 },
    /// Addressing ran past the last Art-Net universe (65,535).
    OutOfUniverses,
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementError::Empty => write!(f, "nothing to place (zero count or zero pixels)"),
            PlacementError::FixtureTooBig { span } => {
                write!(
                    f,
                    "a fixture needs {span} channels, more than one universe (512)"
                )
            }
            PlacementError::OutOfUniverses => write!(f, "placement ran out of DMX universes"),
        }
    }
}

impl std::error::Error for PlacementError {}

/// Plan a placement: compute the fixtures it produces, with positions and DMX
/// addresses. Pure — does not consult the patch.
pub fn plan(spec: &PlacementSpec) -> Result<Vec<Fixture>, PlacementError> {
    let (cx, cy) = (spec.count[0], spec.count[1]);
    let n = spec.fixture_type.pixel_count;
    if cx == 0 || cy == 0 || n == 0 {
        return Err(PlacementError::Empty);
    }
    let comps = spec.fixture_type.components.count() as u16;
    let span = n as u16 * comps;
    if span > 512 {
        return Err(PlacementError::FixtureTooBig { span });
    }

    let (sdx, sdy) = spec.orientation.step();
    let mut fixtures = Vec::with_capacity((cx * cy) as usize);

    // Addressing cursor, advanced per fixture in signal-path (snake) order.
    let mut universe = spec.start_universe;
    let mut channel = spec.start_channel.max(1);
    let mut in_universe = 0u32;
    let mut number = spec.start_number;

    for iy in 0..cy {
        // Snake: reverse the X order on odd rows so the run folds back.
        let row: Vec<u32> = if spec.snake && iy % 2 == 1 {
            (0..cx).rev().collect()
        } else {
            (0..cx).collect()
        };
        for ix in row {
            // Per-universe fixture limit.
            if let Some(lim) = spec.fixtures_per_universe {
                if lim > 0 && in_universe >= lim {
                    universe = universe
                        .checked_add(1)
                        .ok_or(PlacementError::OutOfUniverses)?;
                    channel = 1;
                    in_universe = 0;
                }
            }
            // Channel fit within the universe.
            if channel + span > 513 {
                universe = universe
                    .checked_add(1)
                    .ok_or(PlacementError::OutOfUniverses)?;
                channel = 1;
                in_universe = 0;
            }

            let ox = spec.start_position[0] + ix as i32 * spec.offset[0];
            let oy = spec.start_position[1] + iy as i32 * spec.offset[1];
            let pixels = (0..n)
                .map(|i| Pixel {
                    index: i,
                    position: Vec3::new(
                        (ox + sdx * i as i32) as f32,
                        (oy + sdy * i as i32) as f32,
                        0.0,
                    ),
                    universe,
                    start_channel: channel + i as u16 * comps,
                    color_order: spec.fixture_type.color_order,
                })
                .collect();

            fixtures.push(Fixture {
                id: Uuid::new_v4(),
                name: format!("{} {}", spec.name_prefix, number),
                profile: FixtureProfile {
                    name: spec.fixture_type.name.clone(),
                    components: spec.fixture_type.components,
                    gamma: GammaCalibration::default(),
                },
                pixels,
                zone: spec.zone,
            });

            channel += span;
            in_universe += 1;
            number += 1;
        }
    }

    Ok(fixtures)
}

/// Suggest the next free DMX address: just past the highest-addressed pixel in the
/// patch (append semantics, like professional consoles). `(1, 1)` for an empty patch.
pub fn next_free_address(patch: &prisma_patch::Patch) -> (u16, u16) {
    let mut best: Option<(u16, u16)> = None; // (universe, end_channel exclusive)
    for f in &patch.fixtures {
        let comps = f.profile.component_count() as u16;
        for p in &f.pixels {
            let end = p.start_channel + comps;
            let key = (p.universe, end);
            if best.is_none_or(|b| key > b) {
                best = Some(key);
            }
        }
    }
    match best {
        None => (1, 1),
        Some((u, end)) if end <= 512 => (u, end),
        Some((u, _)) => (u.saturating_add(1), 1),
    }
}

/// A small built-in set of fixture types for the placement dialog until the fixture
/// library (§3.2) lands as its own file.
pub fn builtin_fixture_types() -> Vec<FixtureType> {
    let tube = |name: &str, px: u32| FixtureType {
        name: name.into(),
        pixel_count: px,
        components: ComponentSet::Rgb,
        color_order: ColorOrder::Grb,
    };
    vec![
        tube("Tube 30px (WS2812 GRB)", 30),
        tube("Tube 60px (WS2812 GRB)", 60),
        tube("Bar 12px (WS2812 GRB)", 12),
        FixtureType {
            name: "Tube 60px RGBW (GRB)".into(),
            pixel_count: 60,
            components: ComponentSet::Rgbw,
            color_order: ColorOrder::Grb,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PlacementSpec {
        PlacementSpec {
            fixture_type: FixtureType {
                name: "t".into(),
                pixel_count: 10,
                components: ComponentSet::Rgb,
                color_order: ColorOrder::Rgb,
            },
            name_prefix: "Tube".into(),
            start_number: 1,
            count: [1, 1],
            offset: [1, 1],
            start_position: [0, 0],
            orientation: Orientation::Vertical,
            snake: false,
            start_universe: 1,
            start_channel: 1,
            fixtures_per_universe: None,
            zone: Uuid::from_u128(0x1),
        }
    }

    #[test]
    fn count_and_offset_place_a_grid() {
        let mut s = spec();
        s.count = [3, 2];
        s.offset = [5, 4];
        s.orientation = Orientation::Horizontal;
        let f = plan(&s).unwrap();
        assert_eq!(f.len(), 6);
        // Row-major origins: (0,0),(5,0),(10,0),(0,4),(5,4),(10,4).
        assert_eq!(f[0].pixels[0].position, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(f[2].pixels[0].position, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(f[3].pixels[0].position, Vec3::new(0.0, 4.0, 0.0));
        // Names run from start_number.
        assert_eq!(f[0].name, "Tube 1");
        assert_eq!(f[5].name, "Tube 6");
    }

    #[test]
    fn diagonal_pixels_step_on_both_axes() {
        let mut s = spec();
        s.orientation = Orientation::DiagonalDown;
        let f = plan(&s).unwrap();
        let p = &f[0].pixels;
        assert_eq!(p[0].position, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(p[1].position, Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(p[9].position, Vec3::new(9.0, 9.0, 0.0));
    }

    #[test]
    fn parallel_diagonals_do_not_share_cells() {
        // The §4.2 requirement: parallel diagonal runs must stay disjoint.
        let mut s = spec();
        s.orientation = Orientation::DiagonalDown;
        s.count = [4, 1];
        s.offset = [3, 0]; // horizontal spacing between the parallel runs
        let f = plan(&s).unwrap();
        let mut seen = std::collections::HashSet::new();
        for fixture in &f {
            for p in &fixture.pixels {
                let cell = (p.position.x as i32, p.position.y as i32);
                assert!(
                    seen.insert(cell),
                    "two parallel diagonal runs shared cell {cell:?}"
                );
            }
        }
    }

    #[test]
    fn addressing_is_sequential_and_wraps_universes() {
        let mut s = spec();
        // 10 RGB px = 30 channels/fixture. 17 fixtures = 510 ch fit in universe 1;
        // the 18th spills to universe 2.
        s.count = [18, 1];
        let f = plan(&s).unwrap();
        assert_eq!(f[0].pixels[0].universe, 1);
        assert_eq!(f[0].pixels[0].start_channel, 1);
        assert_eq!(f[1].pixels[0].start_channel, 31);
        assert_eq!(f[16].pixels[0].universe, 1);
        assert_eq!(f[16].pixels[0].start_channel, 481); // 16*30 + 1
        assert_eq!(f[17].pixels[0].universe, 2);
        assert_eq!(f[17].pixels[0].start_channel, 1);
    }

    #[test]
    fn fixtures_per_universe_limit_spills_early() {
        let mut s = spec();
        s.count = [5, 1];
        s.fixtures_per_universe = Some(2);
        let f = plan(&s).unwrap();
        assert_eq!(f[0].pixels[0].universe, 1);
        assert_eq!(f[1].pixels[0].universe, 1);
        assert_eq!(f[2].pixels[0].universe, 2); // limit 2 → third spills
        assert_eq!(f[4].pixels[0].universe, 3);
    }

    #[test]
    fn snake_reverses_addressing_on_odd_rows() {
        let mut s = spec();
        s.count = [3, 2];
        s.offset = [2, 2];
        s.orientation = Orientation::Horizontal;
        s.snake = true;
        let f = plan(&s).unwrap();
        // Row 0 addressed left→right; row 1 right→left. So the lowest channel of
        // row 1 (the 4th fixture placed) sits at the RIGHTMOST origin x = 4.
        assert_eq!(f[3].pixels[0].position.x, 4.0);
        assert_eq!(f[5].pixels[0].position.x, 0.0);
        // Addresses stay monotonically increasing in placement order.
        assert!(f[3].pixels[0].start_channel < f[4].pixels[0].start_channel);
    }

    #[test]
    fn rejects_empty_and_oversized() {
        let mut s = spec();
        s.count = [0, 1];
        assert_eq!(plan(&s), Err(PlacementError::Empty));

        let mut big = spec();
        big.fixture_type.pixel_count = 200; // 600 ch > 512
        assert!(matches!(
            plan(&big),
            Err(PlacementError::FixtureTooBig { .. })
        ));
    }

    #[test]
    fn next_free_after_the_last_used_address() {
        use crate::samples::wedding_rig;
        let patch = wedding_rig();
        let (u, c) = next_free_address(&patch);
        // The rig fills into universe 2; the suggestion is past its last pixel.
        assert!(u >= 2);
        assert!((1..=512).contains(&c));
    }
}

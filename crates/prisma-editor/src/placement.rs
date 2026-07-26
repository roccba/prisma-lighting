//! Fixture placement (M7 spec §4): instantiate many fixtures in one action from a
//! [`PlacementSpec`] — the "count X/Y + offset" grid the placement dialog drives,
//! plus Prisma's diagonal stepped runs and the full eight signal directions (§4.1).
//!
//! [`plan`] is a pure function (spec → fixtures); it does not touch the patch, so
//! the placement math — grid positions, the eight directions, the diagonal
//! disjointness requirement, addressing with universe wrap and offsets — is
//! unit-testable. [`plan_preview`] returns the same layout as lightweight runs for
//! the dialog's live signal-path preview. The fixtures are handed to an
//! [`InsertFixtures`](crate::EditOp::InsertFixtures) op so placement is undoable.

use glam::Vec3;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Pixel, PixelRole,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a fixture type's pixels are physically laid out (M7 spec §3.2). Our whole
/// domain is pixel LED — tubes, bars, matrices, custom pixel shapes — so this stays
/// deliberately small: **no DMX personalities, channel modes or macros**, just the
/// pixel geometry.
///
/// M7 fixtures are 1-D strips (§4.1), so `Strip` is the only variant now; `Matrix` and
/// `Custom` are the reserved shapes (a matrix is where [`Rotation`] stops being inert).
/// Kept as an enum from day one so adding them later does not reopen the type model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FixtureArrangement {
    /// `pixel_count` pixels in a single line, in signal order. The only M7 shape.
    #[default]
    Strip,
    // Reserved (not implemented): Matrix { cols, rows }, Custom { cells: Vec<[i32; 2]> }.
}

/// A reusable fixture definition — a fixture-library entry (§3.2). Our domain is only
/// pixel LED, so a type is exactly: a name, a pixel count, a physical arrangement, a
/// component set and a colour order — nothing about arbitrary DMX channels or modes.
///
/// The editor's fixture library (`crate::library`) is a collection of these in a
/// separate reusable file. Placement snapshots the resolved profile into each fixture,
/// so a `.prismap` never references the library (`crate::library` docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureType {
    pub name: String,
    pub pixel_count: u32,
    pub components: ComponentSet,
    pub color_order: ColorOrder,
    /// Physical pixel layout. `#[serde(default)]` = [`FixtureArrangement::Strip`] so a
    /// library written before this field loads unchanged.
    #[serde(default)]
    pub arrangement: FixtureArrangement,
    /// Intra-fixture pixel roles (§3.2, the hexagon case). **Forward-proof, empty and
    /// unconsumed in M7** — see [`PixelRole`]. `#[serde(default)]` for older libraries.
    #[serde(default)]
    pub roles: Vec<PixelRole>,
}

/// **Signal direction** — which way a fixture's pixels run from its origin, i.e.
/// the order the data is wired (§4.1). This is deliberately distinct from mere
/// orientation: a horizontal tube wired left-to-right and right-to-left are
/// different fixtures (a chase runs the other way), and the model captures that as
/// the stored pixel order. All eight are reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Right,     // →
    Left,      // ←
    Down,      // ↓
    Up,        // ↑
    DownRight, // ↘
    DownLeft,  // ↙
    UpRight,   // ↗
    UpLeft,    // ↖
}

impl Direction {
    /// The per-pixel cell step (x, y), y increasing downward.
    pub fn step(self) -> (i32, i32) {
        match self {
            Direction::Right => (1, 0),
            Direction::Left => (-1, 0),
            Direction::Down => (0, 1),
            Direction::Up => (0, -1),
            Direction::DownRight => (1, 1),
            Direction::DownLeft => (-1, 1),
            Direction::UpRight => (1, -1),
            Direction::UpLeft => (-1, -1),
        }
    }
}

/// Fixture rotation (§4.1). Present from day one but **inert for 1-D tubes** —
/// rotation re-orients a fixture *type*'s internal 2-D pixel map, which M7's
/// fixture types do not yet have. Kept so adding 2-D fixtures later does not reopen
/// the placement dialog, exactly like the Z axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

/// Everything the placement dialog collects (§4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementSpec {
    pub fixture_type: FixtureType,
    /// Name prefix; instances are `"{prefix} {n}"` from `start_number`.
    pub name_prefix: String,
    pub start_number: u32,
    /// Present but inert for 1-D fixtures (see [`Rotation`]).
    pub rotation: Rotation,
    /// Instances along X, Y, Z. **Z exists from day one** though M7 is 2-D — it is
    /// fixed at 1 in the UI while 2-D, so 3-D does not reopen this dialog.
    pub count: [u32; 3],
    /// Spacing in cells between instance origins, per axis.
    pub offset: [i32; 3],
    /// Origin of the first instance, in cells.
    pub start_position: [i32; 3],
    /// Signal direction the pixels run (§4.1) — eight options.
    pub direction: Direction,
    /// Snake mode: alternate rows are addressed in reverse, folding the path (§4.1).
    pub snake: bool,
    pub start_universe: u16,
    pub start_channel: u16,
    /// Extra channels left between consecutive fixtures (professional consoles "Desfase"). `0` =
    /// tight packing.
    pub channel_offset: u16,
    /// Extra universes skipped each time addressing wraps to a new universe. `0` =
    /// the next universe.
    pub universe_offset: u16,
    /// Maximum fixtures per universe before spilling to the next (§4.1). `None` =
    /// only the 512-channel limit applies.
    pub fixtures_per_universe: Option<u32>,
    /// The zone placed fixtures belong to — the **active zone** (§4.1, §6).
    pub zone: Uuid,
}

/// Why a placement could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    Empty,
    FixtureTooBig { span: u16 },
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

/// Instance origins in addressing (snake) order.
fn instance_origins(spec: &PlacementSpec) -> Vec<(i32, i32, i32)> {
    let [cx, cy, cz] = spec.count;
    let mut origins = Vec::with_capacity((cx * cy * cz.max(1)) as usize);
    for iz in 0..cz.max(1) {
        for iy in 0..cy {
            // Snake: reverse X on odd rows so the run folds back.
            let row: Vec<u32> = if spec.snake && iy % 2 == 1 {
                (0..cx).rev().collect()
            } else {
                (0..cx).collect()
            };
            for ix in row {
                origins.push((
                    spec.start_position[0] + ix as i32 * spec.offset[0],
                    spec.start_position[1] + iy as i32 * spec.offset[1],
                    spec.start_position[2] + iz as i32 * spec.offset[2],
                ));
            }
        }
    }
    origins
}

/// Plan a placement: compute the fixtures it produces, with positions and DMX
/// addresses. Pure — does not consult the patch.
pub fn plan(spec: &PlacementSpec) -> Result<Vec<Fixture>, PlacementError> {
    let n = spec.fixture_type.pixel_count;
    if spec.count[0] == 0 || spec.count[1] == 0 || n == 0 {
        return Err(PlacementError::Empty);
    }
    let comps = spec.fixture_type.components.count() as u16;
    let span = n as u16 * comps;
    if span > 512 {
        return Err(PlacementError::FixtureTooBig { span });
    }

    let (sdx, sdy) = spec.direction.step();
    let origins = instance_origins(spec);
    let mut fixtures = Vec::with_capacity(origins.len());

    let mut universe = spec.start_universe;
    let mut channel = spec.start_channel.max(1);
    let mut in_universe = 0u32;

    let wrap = |universe: &mut u16, channel: &mut u16, in_universe: &mut u32| {
        let step = 1 + spec.universe_offset;
        *universe = universe
            .checked_add(step)
            .ok_or(PlacementError::OutOfUniverses)?;
        *channel = 1;
        *in_universe = 0;
        Ok::<(), PlacementError>(())
    };

    for (k, (ox, oy, oz)) in origins.into_iter().enumerate() {
        let number = spec.start_number + k as u32;
        if let Some(lim) = spec.fixtures_per_universe {
            if lim > 0 && in_universe >= lim {
                wrap(&mut universe, &mut channel, &mut in_universe)?;
            }
        }
        if channel + span > 513 {
            wrap(&mut universe, &mut channel, &mut in_universe)?;
        }

        let pixels = (0..n)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new(
                    (ox + sdx * i as i32) as f32,
                    (oy + sdy * i as i32) as f32,
                    oz as f32,
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

        channel += span + spec.channel_offset;
        in_universe += 1;
    }

    Ok(fixtures)
}

/// One instance's run for the dialog's signal-path preview: where its pixels start
/// and end, and its position in the signal order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewRun {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub order: u32,
}

/// The layout a spec would produce, as lightweight runs — for the live preview
/// before applying (§4.1). Empty if the spec places nothing.
pub fn plan_preview(spec: &PlacementSpec) -> Vec<PreviewRun> {
    let n = spec.fixture_type.pixel_count;
    if spec.count[0] == 0 || spec.count[1] == 0 || n == 0 {
        return Vec::new();
    }
    let (sdx, sdy) = spec.direction.step();
    instance_origins(spec)
        .into_iter()
        .enumerate()
        .map(|(order, (ox, oy, _oz))| PreviewRun {
            start: [ox as f32, oy as f32],
            end: [
                (ox + sdx * (n as i32 - 1)) as f32,
                (oy + sdy * (n as i32 - 1)) as f32,
            ],
            order: order as u32,
        })
        .collect()
}

/// Suggest the next free DMX address: just past the highest-addressed pixel in the
/// patch (append semantics). `(1, 1)` for an empty patch.
pub fn next_free_address(patch: &prisma_patch::Patch) -> (u16, u16) {
    let mut best: Option<(u16, u16)> = None;
    for f in &patch.fixtures {
        let comps = f.profile.component_count() as u16;
        for p in &f.pixels {
            let key = (p.universe, p.start_channel + comps);
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

/// A small set of generic pixel-LED fixture types. Used to **seed a fresh fixture
/// library** the first time the app runs (`crate::library::seed`), so the placement
/// dialog is never empty; the operator edits the library from there (§3.2).
pub fn builtin_fixture_types() -> Vec<FixtureType> {
    let tube = |name: &str, px: u32| FixtureType {
        name: name.into(),
        pixel_count: px,
        components: ComponentSet::Rgb,
        color_order: ColorOrder::Grb,
        arrangement: FixtureArrangement::Strip,
        roles: Vec::new(),
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
            arrangement: FixtureArrangement::Strip,
            roles: Vec::new(),
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
                arrangement: FixtureArrangement::Strip,
                roles: Vec::new(),
            },
            name_prefix: "Tube".into(),
            start_number: 1,
            rotation: Rotation::R0,
            count: [1, 1, 1],
            offset: [1, 1, 1],
            start_position: [0, 0, 0],
            direction: Direction::Down,
            snake: false,
            start_universe: 1,
            start_channel: 1,
            channel_offset: 0,
            universe_offset: 0,
            fixtures_per_universe: None,
            zone: Uuid::from_u128(0x1),
        }
    }

    #[test]
    fn count_and_offset_place_a_grid() {
        let mut s = spec();
        s.count = [3, 2, 1];
        s.offset = [5, 4, 0];
        s.direction = Direction::Right;
        let f = plan(&s).unwrap();
        assert_eq!(f.len(), 6);
        assert_eq!(f[0].pixels[0].position, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(f[2].pixels[0].position, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(f[3].pixels[0].position, Vec3::new(0.0, 4.0, 0.0));
        assert_eq!(f[0].name, "Tube 1");
        assert_eq!(f[5].name, "Tube 6");
    }

    #[test]
    fn all_eight_directions_set_the_pixel_order() {
        for (dir, end) in [
            (Direction::Right, (9.0, 0.0)),
            (Direction::Left, (-9.0, 0.0)),
            (Direction::Down, (0.0, 9.0)),
            (Direction::Up, (0.0, -9.0)),
            (Direction::DownRight, (9.0, 9.0)),
            (Direction::DownLeft, (-9.0, 9.0)),
            (Direction::UpRight, (9.0, -9.0)),
            (Direction::UpLeft, (-9.0, -9.0)),
        ] {
            let mut s = spec();
            s.start_position = [0, 0, 0];
            s.direction = dir;
            let f = plan(&s).unwrap();
            let last = f[0].pixels[9].position;
            assert_eq!((last.x, last.y), end, "direction {dir:?} end pixel");
            // First pixel is always the origin — the signal STARTS there.
            assert_eq!(f[0].pixels[0].position, Vec3::new(0.0, 0.0, 0.0));
        }
    }

    #[test]
    fn parallel_diagonals_do_not_share_cells() {
        let mut s = spec();
        s.direction = Direction::DownRight;
        s.count = [4, 1, 1];
        s.offset = [3, 0, 0];
        let f = plan(&s).unwrap();
        let mut seen = std::collections::HashSet::new();
        for fixture in &f {
            for p in &fixture.pixels {
                let cell = (p.position.x as i32, p.position.y as i32);
                assert!(seen.insert(cell), "parallel diagonals shared {cell:?}");
            }
        }
    }

    #[test]
    fn addressing_is_sequential_and_wraps_universes() {
        let mut s = spec();
        s.count = [18, 1, 1];
        let f = plan(&s).unwrap();
        assert_eq!(f[1].pixels[0].start_channel, 31);
        assert_eq!(f[16].pixels[0].universe, 1);
        assert_eq!(f[16].pixels[0].start_channel, 481);
        assert_eq!(f[17].pixels[0].universe, 2);
        assert_eq!(f[17].pixels[0].start_channel, 1);
    }

    #[test]
    fn channel_offset_leaves_a_gap_and_universe_offset_skips() {
        let mut s = spec();
        s.count = [3, 1, 1];
        s.channel_offset = 10; // 30 span + 10 gap = 40 stride
        let f = plan(&s).unwrap();
        assert_eq!(f[0].pixels[0].start_channel, 1);
        assert_eq!(f[1].pixels[0].start_channel, 41);
        assert_eq!(f[2].pixels[0].start_channel, 81);

        // With a per-universe limit and a universe offset, wraps skip universes.
        let mut u = spec();
        u.count = [4, 1, 1];
        u.fixtures_per_universe = Some(1);
        u.universe_offset = 2; // wrap advances by 1 + 2 = 3 universes
        let g = plan(&u).unwrap();
        assert_eq!(g[0].pixels[0].universe, 1);
        assert_eq!(g[1].pixels[0].universe, 4);
        assert_eq!(g[2].pixels[0].universe, 7);
    }

    #[test]
    fn snake_reverses_addressing_on_odd_rows() {
        let mut s = spec();
        s.count = [3, 2, 1];
        s.offset = [2, 2, 0];
        s.direction = Direction::Right;
        s.snake = true;
        let f = plan(&s).unwrap();
        assert_eq!(f[3].pixels[0].position.x, 4.0);
        assert_eq!(f[5].pixels[0].position.x, 0.0);
        assert!(f[3].pixels[0].start_channel < f[4].pixels[0].start_channel);
    }

    #[test]
    fn preview_matches_the_planned_layout() {
        let mut s = spec();
        s.count = [3, 1, 1];
        s.offset = [4, 0, 0];
        s.direction = Direction::Down;
        let runs = plan_preview(&s);
        let f = plan(&s).unwrap();
        assert_eq!(runs.len(), f.len());
        for (r, fix) in runs.iter().zip(&f) {
            let first = fix.pixels[0].position;
            let last = fix.pixels[fix.pixels.len() - 1].position;
            assert_eq!(r.start, [first.x, first.y]);
            assert_eq!(r.end, [last.x, last.y]);
        }
    }

    #[test]
    fn rejects_empty_and_oversized() {
        let mut s = spec();
        s.count = [0, 1, 1];
        assert_eq!(plan(&s), Err(PlacementError::Empty));

        let mut big = spec();
        big.fixture_type.pixel_count = 200;
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
        assert!(u >= 2);
        assert!((1..=512).contains(&c));
    }
}

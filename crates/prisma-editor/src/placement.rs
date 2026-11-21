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

/// **Fixture rotation** — which way a fixture's pixels run from its first pixel (the
/// signal input), i.e. the order the data is wired *inside* the fixture (§4.1).
///
/// `R0` ("Original") is the fixture as it was created in the library: a strip runs
/// left → right. Angles are clockwise on screen (y grows downward), so `R90` runs top →
/// bottom and `R270` bottom → top. The four 45° steps are Prisma's diagonal stepped runs
/// (professional consoles has no diagonals) — all eight directions stay reachable.
///
/// This is deliberately separate from the *addressing order between fixtures*
/// ([`StartCorner`] + [`MainOrientation`] + snake): a tube's internal direction and which
/// tube gets the first DMX address are two independent facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    R0,
    R45,
    R90,
    R135,
    R180,
    R225,
    R270,
    R315,
}

impl Rotation {
    /// The per-pixel cell step (x, y) of a strip at this rotation, y increasing downward.
    pub fn step(self) -> (i32, i32) {
        match self {
            Rotation::R0 => (1, 0),
            Rotation::R45 => (1, 1),
            Rotation::R90 => (0, 1),
            Rotation::R135 => (-1, 1),
            Rotation::R180 => (-1, 0),
            Rotation::R225 => (-1, -1),
            Rotation::R270 => (0, -1),
            Rotation::R315 => (1, -1),
        }
    }

    /// The same fixture turned half a turn — what snake's "apply fixture rotation" does
    /// to the fixtures on reversed lines.
    pub fn flipped(self) -> Self {
        match self {
            Rotation::R0 => Rotation::R180,
            Rotation::R45 => Rotation::R225,
            Rotation::R90 => Rotation::R270,
            Rotation::R135 => Rotation::R315,
            Rotation::R180 => Rotation::R0,
            Rotation::R225 => Rotation::R45,
            Rotation::R270 => Rotation::R90,
            Rotation::R315 => Rotation::R135,
        }
    }
}

/// Which corner of the placement grid receives the **first DMX address** (the classic
/// "Esquina inicial"). It changes the addressing order only; the grid itself stays put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Whether addressing walks the grid row by row or column by column (the classic
/// "Orientación principal").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MainOrientation {
    Horizontal,
    Vertical,
}

/// Everything the "Añadir instalaciones" dialog collects (§4.1, modelled on professional consoles).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementSpec {
    pub fixture_type: FixtureType,
    /// First fixture ID; instances are named `"{type name} #{id}"`, IDs consecutive in
    /// addressing order.
    pub start_number: u32,
    /// Direction of the pixels inside each fixture (see [`Rotation`]).
    pub rotation: Rotation,
    /// Instances along X, Y, Z. **Z exists from day one** though placement is 2-D — it is
    /// fixed at 1 in the UI, so 3-D does not reopen this dialog.
    pub count: [u32; 3],
    /// Extra cells left **between** neighbouring fixtures, per axis (the classic "Desfase").
    /// `0` = touching: each fixture already occupies its own rotated footprint.
    pub offset: [i32; 3],
    /// Top-left cell of the whole grid (0-based; the dialog shows it 1-based).
    pub start_position: [i32; 3],
    /// Which grid corner is addressed first.
    pub start_corner: StartCorner,
    /// Rows first (`Horizontal`) or columns first (`Vertical`).
    pub orientation: MainOrientation,
    /// Snake: every other line is addressed in reverse, folding the path.
    pub snake: bool,
    /// With `snake`, also turn the fixtures on the reversed lines half a turn, so the
    /// signal *inside* them follows the fold too (the classic "Aplicar rotación de
    /// instalación"). Ignored without `snake`.
    pub snake_rotation: bool,
    pub start_universe: u16,
    pub start_channel: u16,
    /// Extra channels left between consecutive fixtures. `0` = tight packing.
    pub channel_offset: u16,
    /// Extra universes skipped each time addressing wraps to a new universe. `0` =
    /// the next universe.
    pub universe_offset: u16,
    /// On a wrap, restart at `start_channel` instead of channel 1 (the classic "Aplicar
    /// canal inicial DMX a cada universo").
    pub start_channel_every_universe: bool,
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
    FixtureTooBig {
        span: u16,
    },
    /// `start_channel_every_universe` is on, but a fixture does not fit between the
    /// start channel and the end of the universe.
    StartChannelTooHigh {
        start: u16,
        span: u16,
    },
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
            PlacementError::StartChannelTooHigh { start, span } => write!(
                f,
                "a fixture needs {span} channels and does not fit from channel {start}"
            ),
            PlacementError::OutOfUniverses => write!(f, "placement ran out of DMX universes"),
        }
    }
}

impl std::error::Error for PlacementError {}

/// The cell footprint (w, h) of one fixture at `rotation`, and where its first pixel
/// sits inside that box. Diagonals occupy an n×n box.
fn footprint(n: u32, rotation: Rotation) -> ((i32, i32), (i32, i32)) {
    let n = n as i32;
    let (dx, dy) = rotation.step();
    let w = if dx != 0 { n } else { 1 };
    let h = if dy != 0 { n } else { 1 };
    let first = (
        if dx < 0 { w - 1 } else { 0 },
        if dy < 0 { h - 1 } else { 0 },
    );
    ((w, h), first)
}

/// One instance in addressing order: its grid box origin and its rotation.
struct Instance {
    origin: (i32, i32, i32),
    rotation: Rotation,
}

/// Grid indices `0..len`, forward or reversed.
fn axis(len: u32, forward: bool) -> Vec<u32> {
    if forward {
        (0..len).collect()
    } else {
        (0..len).rev().collect()
    }
}

/// Instances in **addressing order**: the start corner picks where the walk begins, the
/// main orientation whether it walks rows or columns, and snake reverses every other line.
fn instances(spec: &PlacementSpec) -> Vec<Instance> {
    let [cx, cy, cz] = spec.count;
    let ((w, h), _) = footprint(spec.fixture_type.pixel_count, spec.rotation);
    let stride_x = w + spec.offset[0];
    let stride_y = h + spec.offset[1];
    let stride_z = 1 + spec.offset[2];
    let left = matches!(
        spec.start_corner,
        StartCorner::TopLeft | StartCorner::BottomLeft
    );
    let top = matches!(
        spec.start_corner,
        StartCorner::TopLeft | StartCorner::TopRight
    );

    let mut out = Vec::with_capacity((cx * cy * cz.max(1)) as usize);
    for iz in 0..cz.max(1) {
        let (outer, inner_len, inner_fwd) = match spec.orientation {
            MainOrientation::Horizontal => (axis(cy, top), cx, left),
            MainOrientation::Vertical => (axis(cx, left), cy, top),
        };
        for (line, o) in outer.into_iter().enumerate() {
            let reversed = spec.snake && line % 2 == 1;
            let rotation = if reversed && spec.snake_rotation {
                spec.rotation.flipped()
            } else {
                spec.rotation
            };
            for i in axis(inner_len, inner_fwd != reversed) {
                let (ix, iy) = match spec.orientation {
                    MainOrientation::Horizontal => (i, o),
                    MainOrientation::Vertical => (o, i),
                };
                out.push(Instance {
                    origin: (
                        spec.start_position[0] + ix as i32 * stride_x,
                        spec.start_position[1] + iy as i32 * stride_y,
                        spec.start_position[2] + iz as i32 * stride_z,
                    ),
                    rotation,
                });
            }
        }
    }
    out
}

/// The name of an instance: the fixture type's name and its ID.
pub fn instance_name(type_name: &str, id: u32) -> String {
    format!("{type_name} #{id}")
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
    let start_channel = spec.start_channel.clamp(1, 512);
    let restart = if spec.start_channel_every_universe {
        start_channel
    } else {
        1
    };
    if restart + span > 513 {
        return Err(PlacementError::StartChannelTooHigh {
            start: restart,
            span,
        });
    }

    let instances = instances(spec);
    let mut fixtures = Vec::with_capacity(instances.len());

    let mut universe = spec.start_universe;
    let mut channel = start_channel;
    let mut in_universe = 0u32;

    let wrap = |universe: &mut u16, channel: &mut u16, in_universe: &mut u32| {
        let step = 1 + spec.universe_offset;
        *universe = universe
            .checked_add(step)
            .ok_or(PlacementError::OutOfUniverses)?;
        *channel = restart;
        *in_universe = 0;
        Ok::<(), PlacementError>(())
    };

    for (k, inst) in instances.into_iter().enumerate() {
        if let Some(lim) = spec.fixtures_per_universe {
            if lim > 0 && in_universe >= lim {
                wrap(&mut universe, &mut channel, &mut in_universe)?;
            }
        }
        if channel + span > 513 {
            wrap(&mut universe, &mut channel, &mut in_universe)?;
        }

        let (sdx, sdy) = inst.rotation.step();
        let (_, (fx, fy)) = footprint(n, inst.rotation);
        let (ox, oy, oz) = inst.origin;
        let pixels = (0..n)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new(
                    (ox + fx + sdx * i as i32) as f32,
                    (oy + fy + sdy * i as i32) as f32,
                    oz as f32,
                ),
                universe,
                start_channel: channel + i as u16 * comps,
                color_order: spec.fixture_type.color_order,
            })
            .collect();

        fixtures.push(Fixture {
            id: Uuid::new_v4(),
            name: instance_name(&spec.fixture_type.name, spec.start_number + k as u32),
            profile: FixtureProfile {
                name: spec.fixture_type.name.clone(),
                components: spec.fixture_type.components,
                gamma: GammaCalibration::default(),
            },
            pixels,
            zone: spec.zone,
        });

        channel = channel.saturating_add(span + spec.channel_offset);
        in_universe += 1;
    }

    Ok(fixtures)
}

/// One instance's run, for a layout preview: where its pixels start and end, and its
/// position in the signal order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewRun {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub order: u32,
}

/// The layout a spec would produce, as lightweight runs. Empty if the spec places
/// nothing (or cannot be addressed).
pub fn plan_preview(spec: &PlacementSpec) -> Vec<PreviewRun> {
    plan(spec)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(order, f)| {
            let a = f.pixels[0].position;
            let b = f.pixels[f.pixels.len() - 1].position;
            PreviewRun {
                start: [a.x, a.y],
                end: [b.x, b.y],
                order: order as u32,
            }
        })
        .collect()
}

/// The next free fixture ID: one past the highest `#N` that ends a fixture name, `1` for
/// a patch with none. Prefills "Iniciar ID de instalación".
pub fn next_fixture_id(patch: &prisma_patch::Patch) -> u32 {
    patch
        .fixtures
        .iter()
        .filter_map(|f| f.name.rsplit_once('#')?.1.trim().parse::<u32>().ok())
        .max()
        .map_or(1, |m| m.saturating_add(1))
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
            start_number: 1,
            rotation: Rotation::R90,
            count: [1, 1, 1],
            offset: [0, 0, 0],
            start_position: [0, 0, 0],
            start_corner: StartCorner::TopLeft,
            orientation: MainOrientation::Horizontal,
            snake: false,
            snake_rotation: false,
            start_universe: 1,
            start_channel: 1,
            channel_offset: 0,
            universe_offset: 0,
            start_channel_every_universe: false,
            fixtures_per_universe: None,
            zone: Uuid::from_u128(0x1),
        }
    }

    /// First-pixel (x, y) of each planned fixture, in addressing order.
    fn firsts(f: &[Fixture]) -> Vec<(i32, i32)> {
        f.iter()
            .map(|f| (f.pixels[0].position.x as i32, f.pixels[0].position.y as i32))
            .collect()
    }

    #[test]
    fn offset_is_the_gap_between_rotated_footprints() {
        // Horizontal 10-px strips: footprint 10×1. Gap 0 → touching, gap 2 → 12 apart.
        let mut s = spec();
        s.rotation = Rotation::R0;
        s.count = [3, 2, 1];
        s.offset = [2, 1, 0];
        let f = plan(&s).unwrap();
        assert_eq!(
            firsts(&f),
            vec![(0, 0), (12, 0), (24, 0), (0, 2), (12, 2), (24, 2)]
        );

        // Vertical strips (R90): footprint 1×10, so X neighbours touch with gap 0.
        let mut v = spec();
        v.count = [3, 1, 1];
        let g = plan(&v).unwrap();
        assert_eq!(firsts(&g), vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn names_are_type_name_and_id() {
        let mut s = spec();
        s.count = [2, 1, 1];
        s.start_number = 2501;
        let f = plan(&s).unwrap();
        assert_eq!(f[0].name, "t #2501");
        assert_eq!(f[1].name, "t #2502");
    }

    #[test]
    fn all_eight_rotations_set_the_pixel_order_inside_the_footprint() {
        for (rot, first, last) in [
            (Rotation::R0, (0, 0), (9, 0)),
            (Rotation::R180, (9, 0), (0, 0)),
            (Rotation::R90, (0, 0), (0, 9)),
            (Rotation::R270, (0, 9), (0, 0)),
            (Rotation::R45, (0, 0), (9, 9)),
            (Rotation::R135, (9, 0), (0, 9)),
            (Rotation::R315, (0, 9), (9, 0)),
            (Rotation::R225, (9, 9), (0, 0)),
        ] {
            let mut s = spec();
            s.rotation = rot;
            let f = plan(&s).unwrap();
            let p0 = f[0].pixels[0].position;
            let p9 = f[0].pixels[9].position;
            assert_eq!((p0.x as i32, p0.y as i32), first, "{rot:?} first pixel");
            assert_eq!((p9.x as i32, p9.y as i32), last, "{rot:?} last pixel");
        }
    }

    #[test]
    fn start_corner_and_orientation_pick_the_addressing_order() {
        // A 3×2 grid of vertical tubes with 1 cell between columns, 0 between rows.
        // Grid cells: columns at x = 0, 2, 4; rows at y = 0, 10.
        let order = |corner, orientation| {
            let mut s = spec();
            s.count = [3, 2, 1];
            s.offset = [1, 0, 0];
            s.start_corner = corner;
            s.orientation = orientation;
            firsts(&plan(&s).unwrap())
        };
        use MainOrientation::*;
        use StartCorner::*;
        assert_eq!(
            order(TopLeft, Horizontal),
            vec![(0, 0), (2, 0), (4, 0), (0, 10), (2, 10), (4, 10)]
        );
        assert_eq!(
            order(TopRight, Horizontal),
            vec![(4, 0), (2, 0), (0, 0), (4, 10), (2, 10), (0, 10)]
        );
        assert_eq!(
            order(BottomLeft, Horizontal),
            vec![(0, 10), (2, 10), (4, 10), (0, 0), (2, 0), (4, 0)]
        );
        assert_eq!(
            order(TopRight, Vertical),
            vec![(4, 0), (4, 10), (2, 0), (2, 10), (0, 0), (0, 10)]
        );
        assert_eq!(
            order(BottomRight, Vertical),
            vec![(4, 10), (4, 0), (2, 10), (2, 0), (0, 10), (0, 0)]
        );
    }

    #[test]
    fn the_grid_does_not_move_with_the_corner() {
        let cells = |corner| {
            let mut s = spec();
            s.count = [3, 2, 1];
            s.start_corner = corner;
            let mut c = firsts(&plan(&s).unwrap());
            c.sort();
            c
        };
        assert_eq!(cells(StartCorner::TopLeft), cells(StartCorner::BottomRight));
    }

    #[test]
    fn snake_folds_the_path_and_can_rotate_reversed_lines() {
        // 4 vertical tubes in one row, walked column by column from the top right with
        // snake: each column holds one tube, so every other tube is a reversed line.
        let mut s = spec();
        s.count = [4, 1, 1];
        s.start_corner = StartCorner::TopRight;
        s.orientation = MainOrientation::Vertical;
        s.snake = true;
        let f = plan(&s).unwrap();
        assert_eq!(firsts(&f), vec![(3, 0), (2, 0), (1, 0), (0, 0)]);
        // Without "apply rotation" every tube still runs down.
        assert!(f
            .iter()
            .all(|t| t.pixels[9].position.y > t.pixels[0].position.y));

        s.snake_rotation = true;
        let g = plan(&s).unwrap();
        // Tubes 1 and 3 run down, 2 and 4 run up — the zigzag wiring.
        let down: Vec<bool> = g
            .iter()
            .map(|t| t.pixels[9].position.y > t.pixels[0].position.y)
            .collect();
        assert_eq!(down, vec![true, false, true, false]);
        // A flipped tube stays in its own footprint (x fixed, y 0..=9).
        assert_eq!(firsts(&g)[1], (2, 9));

        // Snake on rows: the second row is walked the other way.
        let mut r = spec();
        r.count = [3, 2, 1];
        r.offset = [1, 0, 0];
        r.snake = true;
        assert_eq!(
            firsts(&plan(&r).unwrap()),
            vec![(0, 0), (2, 0), (4, 0), (4, 10), (2, 10), (0, 10)]
        );
    }

    #[test]
    fn parallel_diagonals_do_not_share_cells() {
        let mut s = spec();
        s.rotation = Rotation::R45;
        s.count = [4, 2, 1];
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
    fn start_channel_can_apply_to_every_universe() {
        let mut s = spec();
        s.count = [3, 1, 1];
        s.start_channel = 361;
        s.fixtures_per_universe = Some(1);
        let f = plan(&s).unwrap();
        assert_eq!(f[1].pixels[0].universe, 2);
        assert_eq!(f[1].pixels[0].start_channel, 1);

        s.start_channel_every_universe = true;
        let g = plan(&s).unwrap();
        assert_eq!(
            g.iter()
                .map(|f| (f.pixels[0].universe, f.pixels[0].start_channel))
                .collect::<Vec<_>>(),
            vec![(1, 361), (2, 361), (3, 361)]
        );

        // A fixture that cannot fit after the start channel is refused, not looped.
        s.fixtures_per_universe = None;
        s.start_channel = 500;
        assert!(matches!(
            plan(&s),
            Err(PlacementError::StartChannelTooHigh { .. })
        ));
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
    fn preview_matches_the_planned_layout() {
        let mut s = spec();
        s.count = [3, 1, 1];
        s.offset = [3, 0, 0];
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

    #[test]
    fn next_fixture_id_follows_the_highest_hash_number() {
        use crate::samples::wedding_rig;
        let mut patch = wedding_rig();
        let base = next_fixture_id(&patch);
        patch.fixtures[0].name = "Tube #2500".into();
        assert_eq!(next_fixture_id(&patch), 2501.max(base));
        patch.fixtures.clear();
        assert_eq!(next_fixture_id(&patch), 1);
    }
}

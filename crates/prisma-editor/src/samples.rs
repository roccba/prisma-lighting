//! Sample rigs for looking at the editor with something worth judging.
//!
//! [`wedding_rig`] is the motivating example from the spec (§2.1): a wedding venue
//! with physically distinct parts — a DJ booth, a dance-floor ceiling, a reception
//! patio, a bar — each its own zone. It exists so the canvas is exercised against a
//! realistic layout (several zones, fixtures of different lengths, two diagonal
//! runs) rather than a trivial two-tube patch where nothing about the rendering can
//! be judged.
//!
//! The rig is a plain [`Patch`]; a test asserts it compiles cleanly (valid
//! addresses, in-matrix positions), so a broken sample fails in CI, not on screen.

use glam::Vec3;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, DisplayColor, Fixture, FixtureProfile, GammaCalibration,
    Patch, Pixel, Zone,
};
use uuid::Uuid;

/// The virtual matrix the wedding rig lays out in.
const MATRIX: [u32; 3] = [80, 52, 1];

/// A running DMX address: RGB pixels take 3 channels, spilling to the next universe
/// when a triple would cross channel 512.
struct AddrCursor {
    universe: u16,
    channel: u16,
}

impl AddrCursor {
    fn new() -> Self {
        Self {
            universe: 1,
            channel: 1,
        }
    }

    /// Claim the next 3-channel slot, advancing to the next universe if needed.
    fn next_rgb(&mut self) -> (u16, u16) {
        if self.channel > 510 {
            self.universe += 1;
            self.channel = 1;
        }
        let addr = (self.universe, self.channel);
        self.channel += 3;
        addr
    }
}

/// Build one straight/diagonal RGB tube: `count` pixels stepping by `dir` from
/// `origin`, addressed from `cursor`, owned by `zone`.
fn tube(
    id: u128,
    name: &str,
    zone: Uuid,
    origin: (i32, i32),
    dir: (i32, i32),
    count: u32,
    cursor: &mut AddrCursor,
) -> Fixture {
    let pixels = (0..count)
        .map(|i| {
            let (u, c) = cursor.next_rgb();
            Pixel {
                index: i,
                position: Vec3::new(
                    (origin.0 + dir.0 * i as i32) as f32,
                    (origin.1 + dir.1 * i as i32) as f32,
                    0.0,
                ),
                universe: u,
                start_channel: c,
                color_order: ColorOrder::Grb, // common on real WS2812 tubes
            }
        })
        .collect();
    Fixture {
        id: Uuid::from_u128(id),
        name: name.into(),
        profile: FixtureProfile {
            name: "60px WS2812 (GRB)".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
        },
        pixels,
        zone,
    }
}

fn zone(id: Uuid, name: &str, color: DisplayColor, mapping_size: [u32; 3]) -> Zone {
    Zone {
        id,
        name: name.into(),
        members: vec![],
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        // Per-zone mapping space is stored but not yet consumed by render (M7 debt);
        // here it is the zone's own footprint so the editor can frame it (§5.6).
        mapping_size,
        display_color: color,
        world_transform: None,
    }
}

/// The bounding-box size (cells, max−min+1 per axis) of the fixtures owning `zone`.
fn zone_footprint(fixtures: &[Fixture], zone: Uuid) -> [u32; 3] {
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for f in fixtures.iter().filter(|f| f.zone == zone) {
        for p in &f.pixels {
            let c = [
                p.position.x as i32,
                p.position.y as i32,
                p.position.z as i32,
            ];
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
        }
    }
    if lo[0] == i32::MAX {
        return [1, 1, 1];
    }
    [
        (hi[0] - lo[0] + 1).max(1) as u32,
        (hi[1] - lo[1] + 1).max(1) as u32,
        (hi[2] - lo[2] + 1).max(1) as u32,
    ]
}

/// A wedding venue: DJ booth, dance-floor ceiling, reception patio, bar — four
/// independent zones, fifteen fixtures of varied lengths, two diagonal bar runs.
pub fn wedding_rig() -> Patch {
    let booth_id = Uuid::from_u128(0x100);
    let dance_id = Uuid::from_u128(0x101);
    let patio_id = Uuid::from_u128(0x102);
    let bar_id = Uuid::from_u128(0x103);

    let mut cur = AddrCursor::new();
    let mut fixtures = Vec::new();
    let mut next = 0x201u128;
    let mut id = || {
        let v = next;
        next += 1;
        v
    };

    // DJ booth — four vertical 30px tubes, side by side (blue).
    for (k, x) in [3, 5, 7, 9].into_iter().enumerate() {
        fixtures.push(tube(
            id(),
            &format!("Booth Tube {}", k + 1),
            booth_id,
            (x, 3),
            (0, 1),
            30,
            &mut cur,
        ));
    }

    // Dance-floor ceiling — six horizontal 22px tubes stacked into a grid (teal).
    for (k, y) in [6, 9, 12, 15, 18, 21].into_iter().enumerate() {
        fixtures.push(tube(
            id(),
            &format!("Ceiling Row {}", k + 1),
            dance_id,
            (22, y),
            (1, 0),
            22,
            &mut cur,
        ));
    }

    // Reception patio — three vertical tubes of DIFFERENT lengths (gold).
    for (name, x, len) in [
        ("Patio A", 62, 15),
        ("Patio B", 66, 25),
        ("Patio C", 70, 20),
    ] {
        fixtures.push(tube(id(), name, patio_id, (x, 4), (0, 1), len, &mut cur));
    }

    // Bar — two parallel DIAGONAL 14px runs (stepped pixels), offset so they never
    // share a cell (violet). Hand-placed here; the placement dialog computes this in
    // S5.
    fixtures.push(tube(
        id(),
        "Bar Diagonal 1",
        bar_id,
        (48, 34),
        (1, 1),
        14,
        &mut cur,
    ));
    fixtures.push(tube(
        id(),
        "Bar Diagonal 2",
        bar_id,
        (52, 34),
        (1, 1),
        14,
        &mut cur,
    ));

    let zones = vec![
        zone(
            booth_id,
            "DJ Booth",
            [69, 123, 157],
            zone_footprint(&fixtures, booth_id),
        ),
        zone(
            dance_id,
            "Dance Floor",
            [42, 157, 143],
            zone_footprint(&fixtures, dance_id),
        ),
        zone(
            patio_id,
            "Patio",
            [233, 196, 106],
            zone_footprint(&fixtures, patio_id),
        ),
        zone(
            bar_id,
            "Bar",
            [142, 125, 190],
            zone_footprint(&fixtures, bar_id),
        ),
    ];
    Patch {
        id: Uuid::from_u128(0x5EED),
        name: "Wedding — booth · dance floor · patio · bar".into(),
        matrix_size: MATRIX,
        fixtures,
        zones,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wedding_rig_compiles_cleanly() {
        let patch = wedding_rig();
        // The whole point: a broken sample fails here, not on screen.
        patch.compile().expect("wedding rig must be a valid patch");
        assert_eq!(patch.fixtures.len(), 15);
        assert_eq!(patch.zones.len(), 4);
        // Varied fixture lengths (not a uniform grid).
        let lengths: std::collections::BTreeSet<usize> =
            patch.fixtures.iter().map(|f| f.pixels.len()).collect();
        assert!(
            lengths.len() >= 3,
            "the rig should exercise several distinct tube lengths, got {lengths:?}"
        );
        // Every fixture is owned by one of the four zones.
        let zone_ids: std::collections::HashSet<Uuid> = patch.zones.iter().map(|z| z.id).collect();
        assert!(patch.fixtures.iter().all(|f| zone_ids.contains(&f.zone)));
    }

    #[test]
    fn wedding_rig_has_no_wire_faults() {
        // The editor opens on this rig (S6): it must report no DMX wire faults — no
        // address collisions, no out-of-range channels — so the first thing the
        // operator sees is not a false alarm.
        let faults = wedding_rig().wire_faults();
        assert!(
            faults.is_empty(),
            "sample rig must be spotless, got {faults:?}"
        );
    }
}

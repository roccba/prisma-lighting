//! [`EditOp`] — one reversible patch mutation — and [`apply`], which performs it
//! and returns the operation that undoes it.
//!
//! # Snapshot inverses
//!
//! Every op's inverse is **snapshot-style**: `apply` reads the affected old state,
//! writes the new, and returns an op carrying the *old* values. Undo is just
//! `apply(inverse)`, and it yields back the original op — so undo and redo are the
//! same code path, and a round-trip reproduces the patch **exactly** (byte-for-byte
//! `PartialEq`), not approximately. Snapshot inverses are also what make gesture
//! coalescing trivial (see [`crate::draft`]): the first op of a drag already
//! captures the pre-gesture value, so later ops in the gesture need no new undo
//! entry.
//!
//! # apply never partially mutates on error
//!
//! Structural pre-conditions (fixture/zone exists, pixel counts match, indices in
//! range, ids unique) are checked **before** any mutation, so a rejected op leaves
//! the patch untouched and the draft consistent. *Validation* (address collisions,
//! out-of-matrix positions) is deliberately **not** enforced here — the editor
//! shows those visually and allows transiently-invalid states (M7 spec §4.3/§4.4);
//! see [`crate::draft::PatchDraft::validate`].

use glam::Vec3;
use prisma_patch::{DisplayColor, Fixture, Patch};
use uuid::Uuid;

/// A DMX address for one pixel: the pair an edit sets or restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelAddress {
    pub universe: u16,
    pub start_channel: u16,
}

/// One reversible edit. Each variant maps to a user action (M7 spec §8): place,
/// delete, move, re-address, assign to zone, rename, edit properties.
///
/// The op catalog grows as the canvas and placement dialog need specific actions
/// (S3/S5); S2 fixes the *framework* — every op is exactly invertible through
/// [`apply`] — and a representative set across all the action categories.
#[derive(Debug, Clone, PartialEq)]
pub enum EditOp {
    /// Insert fixtures, each at a given index in `Patch::fixtures`. Placement uses
    /// this with the indices at the end of the vector; undo of a removal uses it to
    /// restore fixtures at their *original* positions (so the vector — and thus the
    /// derived global index order — is reproduced exactly). Indices must be
    /// ascending and in range as they are applied in order.
    InsertFixtures { at: Vec<(usize, Fixture)> },
    /// Remove fixtures by id. `apply` captures each removed fixture with its index,
    /// so the inverse ([`InsertFixtures`](EditOp::InsertFixtures)) restores it
    /// exactly.
    RemoveFixtures { ids: Vec<Uuid> },
    /// Set every pixel's position in a fixture (a move/translate is expressed as
    /// the resulting absolute positions, captured old for the inverse). Length must
    /// equal the fixture's pixel count.
    SetFixturePositions { fixture: Uuid, positions: Vec<Vec3> },
    /// Set every pixel's DMX address in a fixture (re-addressing). Length must equal
    /// the fixture's pixel count.
    SetFixtureAddressing {
        fixture: Uuid,
        addressing: Vec<PixelAddress>,
    },
    /// Change which zone a fixture belongs to (§2.2 ownership).
    AssignFixtureZone { fixture: Uuid, zone: Uuid },
    /// Rename a zone.
    RenameZone { zone: Uuid, name: String },
    /// Change a zone's display colour (§2.3).
    SetZoneColor { zone: Uuid, color: DisplayColor },
    /// Change a zone's local mapping-space size (§2.4).
    SetZoneMappingSize { zone: Uuid, size: [u32; 3] },
}

/// Why an op could not be applied. These are **structural** rejections (the op does
/// not fit the patch's shape), distinct from validation problems, and leave the
/// patch untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    FixtureNotFound(Uuid),
    ZoneNotFound(Uuid),
    /// A positions/addressing op's vector length did not match the fixture's pixel
    /// count.
    PixelCountMismatch {
        fixture: Uuid,
        expected: usize,
        got: usize,
    },
    /// An insert index was past the (growing) end of the fixture vector.
    InsertIndexOutOfBounds {
        index: usize,
        len: usize,
    },
    /// An insert would create two fixtures with the same id.
    DuplicateFixtureId(Uuid),
    /// A remove named a fixture id not present.
    RemoveMissingFixture(Uuid),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::FixtureNotFound(id) => write!(f, "fixture {id} not found"),
            EditError::ZoneNotFound(id) => write!(f, "zone {id} not found"),
            EditError::PixelCountMismatch {
                fixture,
                expected,
                got,
            } => write!(
                f,
                "fixture {fixture} has {expected} pixels but the op supplied {got}"
            ),
            EditError::InsertIndexOutOfBounds { index, len } => {
                write!(f, "insert index {index} out of bounds (len {len})")
            }
            EditError::DuplicateFixtureId(id) => write!(f, "duplicate fixture id {id}"),
            EditError::RemoveMissingFixture(id) => {
                write!(f, "cannot remove fixture {id}: not present")
            }
        }
    }
}

impl std::error::Error for EditError {}

/// A stable key identifying *what* an op targets, for gesture coalescing. Two ops
/// with equal `Some` keys touch the same property of the same entity, so a drag
/// that emits many of them collapses to one undo entry. Structural ops (insert /
/// remove) never coalesce and return `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceKey {
    FixturePositions(Uuid),
    FixtureAddressing(Uuid),
    FixtureZone(Uuid),
    ZoneName(Uuid),
    ZoneColor(Uuid),
    ZoneMapping(Uuid),
}

impl EditOp {
    /// The coalescing target, or `None` for ops that must never merge.
    pub fn coalesce_key(&self) -> Option<CoalesceKey> {
        match self {
            EditOp::SetFixturePositions { fixture, .. } => {
                Some(CoalesceKey::FixturePositions(*fixture))
            }
            EditOp::SetFixtureAddressing { fixture, .. } => {
                Some(CoalesceKey::FixtureAddressing(*fixture))
            }
            EditOp::AssignFixtureZone { fixture, .. } => Some(CoalesceKey::FixtureZone(*fixture)),
            EditOp::RenameZone { zone, .. } => Some(CoalesceKey::ZoneName(*zone)),
            EditOp::SetZoneColor { zone, .. } => Some(CoalesceKey::ZoneColor(*zone)),
            EditOp::SetZoneMappingSize { zone, .. } => Some(CoalesceKey::ZoneMapping(*zone)),
            EditOp::InsertFixtures { .. } | EditOp::RemoveFixtures { .. } => None,
        }
    }
}

/// Apply `op` to `patch`, returning the op that undoes it. On error the patch is
/// left untouched (pre-conditions are checked before any mutation).
pub fn apply(patch: &mut Patch, op: EditOp) -> Result<EditOp, EditError> {
    match op {
        EditOp::InsertFixtures { at } => {
            // Validate first: indices ascending & in range against the growing
            // length, and no id collision with existing or inserted fixtures.
            let base = patch.fixtures.len();
            let mut prev: Option<usize> = None;
            for (i, (index, fixture)) in at.iter().enumerate() {
                // The vector length just before this (in-order) insertion.
                let len = base + i;
                if *index > len {
                    return Err(EditError::InsertIndexOutOfBounds { index: *index, len });
                }
                if prev.is_some_and(|p| *index < p) {
                    // Not strictly ascending — reject rather than silently reorder.
                    return Err(EditError::InsertIndexOutOfBounds { index: *index, len });
                }
                if patch.fixtures.iter().any(|f| f.id == fixture.id)
                    || at.iter().filter(|(_, g)| g.id == fixture.id).count() > 1
                {
                    return Err(EditError::DuplicateFixtureId(fixture.id));
                }
                prev = Some(*index);
            }
            let ids: Vec<Uuid> = at.iter().map(|(_, f)| f.id).collect();
            for (index, fixture) in at {
                patch.fixtures.insert(index, fixture);
            }
            Ok(EditOp::RemoveFixtures { ids })
        }

        EditOp::RemoveFixtures { ids } => {
            // Every id must be present before we remove anything.
            for id in &ids {
                if !patch.fixtures.iter().any(|f| f.id == *id) {
                    return Err(EditError::RemoveMissingFixture(*id));
                }
            }
            // Capture (index, fixture) in ascending index order so the inverse
            // re-inserts them exactly where they were.
            let mut removed: Vec<(usize, Fixture)> = Vec::with_capacity(ids.len());
            for (index, fixture) in patch.fixtures.iter().enumerate() {
                if ids.contains(&fixture.id) {
                    removed.push((index, fixture.clone()));
                }
            }
            patch.fixtures.retain(|f| !ids.contains(&f.id));
            Ok(EditOp::InsertFixtures { at: removed })
        }

        EditOp::SetFixturePositions { fixture, positions } => {
            let f = fixture_mut(patch, fixture)?;
            if positions.len() != f.pixels.len() {
                return Err(EditError::PixelCountMismatch {
                    fixture,
                    expected: f.pixels.len(),
                    got: positions.len(),
                });
            }
            let old: Vec<Vec3> = f.pixels.iter().map(|p| p.position).collect();
            for (pixel, pos) in f.pixels.iter_mut().zip(positions) {
                pixel.position = pos;
            }
            Ok(EditOp::SetFixturePositions {
                fixture,
                positions: old,
            })
        }

        EditOp::SetFixtureAddressing {
            fixture,
            addressing,
        } => {
            let f = fixture_mut(patch, fixture)?;
            if addressing.len() != f.pixels.len() {
                return Err(EditError::PixelCountMismatch {
                    fixture,
                    expected: f.pixels.len(),
                    got: addressing.len(),
                });
            }
            let old: Vec<PixelAddress> = f
                .pixels
                .iter()
                .map(|p| PixelAddress {
                    universe: p.universe,
                    start_channel: p.start_channel,
                })
                .collect();
            for (pixel, addr) in f.pixels.iter_mut().zip(addressing) {
                pixel.universe = addr.universe;
                pixel.start_channel = addr.start_channel;
            }
            Ok(EditOp::SetFixtureAddressing {
                fixture,
                addressing: old,
            })
        }

        EditOp::AssignFixtureZone { fixture, zone } => {
            let f = fixture_mut(patch, fixture)?;
            let old = f.zone;
            f.zone = zone;
            Ok(EditOp::AssignFixtureZone { fixture, zone: old })
        }

        EditOp::RenameZone { zone, name } => {
            let z = zone_mut(patch, zone)?;
            let old = std::mem::replace(&mut z.name, name);
            Ok(EditOp::RenameZone { zone, name: old })
        }

        EditOp::SetZoneColor { zone, color } => {
            let z = zone_mut(patch, zone)?;
            let old = std::mem::replace(&mut z.display_color, color);
            Ok(EditOp::SetZoneColor { zone, color: old })
        }

        EditOp::SetZoneMappingSize { zone, size } => {
            let z = zone_mut(patch, zone)?;
            let old = std::mem::replace(&mut z.mapping_size, size);
            Ok(EditOp::SetZoneMappingSize { zone, size: old })
        }
    }
}

fn fixture_mut(patch: &mut Patch, id: Uuid) -> Result<&mut Fixture, EditError> {
    patch
        .fixtures
        .iter_mut()
        .find(|f| f.id == id)
        .ok_or(EditError::FixtureNotFound(id))
}

fn zone_mut(patch: &mut Patch, id: Uuid) -> Result<&mut prisma_patch::Zone, EditError> {
    patch
        .zones
        .iter_mut()
        .find(|z| z.id == id)
        .ok_or(EditError::ZoneNotFound(id))
}

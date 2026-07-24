//! S2 acceptance: the editor's edit framework is exactly reversible.
//!
//! The property the whole undo/redo design rests on: applying a sequence of edits
//! and then undoing them all reproduces the starting patch **exactly** (`PartialEq`,
//! not "close enough"), and redoing rebuilds the edited patch. Plus the framework
//! rules: coalescing collapses a gesture, a new edit forks history (clears redo),
//! validation is re-run but never blocks an edit, and a structural error mutates
//! nothing.

use glam::Vec3;
use prisma_editor::{EditOp, GestureId, PatchDraft, PixelAddress};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchError, Pixel, Zone,
};
use uuid::Uuid;

const FA: u128 = 0x00A;
const FB: u128 = 0x00B;
const FC: u128 = 0x00C;
const ZL: u128 = 0x010;
const ZR: u128 = 0x011;

fn fixture(id: u128, zone: u128, base_channel: u16, x0: f32) -> Fixture {
    Fixture {
        id: Uuid::from_u128(id),
        name: format!("fixture-{id}"),
        profile: FixtureProfile {
            name: "rgb".into(),
            components: ComponentSet::Rgb,
            gamma: GammaCalibration::default(),
        },
        pixels: (0..2)
            .map(|i| Pixel {
                index: i,
                position: Vec3::new(x0 + i as f32, 0.0, 0.0),
                universe: 1,
                start_channel: base_channel + (i as u16) * 3,
                color_order: ColorOrder::Rgb,
            })
            .collect(),
        zone: Uuid::from_u128(zone),
    }
}

fn zone(id: u128, name: &str) -> Zone {
    Zone {
        id: Uuid::from_u128(id),
        name: name.into(),
        members: vec![],
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        mapping_size: [8, 1, 1],
        display_color: [100, 100, 100],
        world_transform: None,
    }
}

/// A valid base patch: two zones, three RGB fixtures, no address collisions.
fn base_patch() -> Patch {
    Patch {
        id: Uuid::from_u128(0x1),
        name: "editor test".into(),
        matrix_size: [8, 1, 1],
        fixtures: vec![
            fixture(FA, ZL, 1, 0.0),
            fixture(FB, ZR, 20, 2.0),
            fixture(FC, ZL, 40, 4.0),
        ],
        zones: vec![zone(ZL, "left"), zone(ZR, "right")],
    }
}

/// A fresh fixture with a unique id (for insertion), addressed clear of the base.
fn new_fixture(id: u128) -> Fixture {
    fixture(id, ZL, 200, 6.0)
}

#[test]
fn a_sequence_of_edits_undoes_to_the_exact_starting_patch() {
    let start = base_patch();
    let mut draft = PatchDraft::new(start.clone());

    let ops = vec![
        EditOp::RenameZone {
            zone: Uuid::from_u128(ZL),
            name: "booth".into(),
        },
        EditOp::SetZoneColor {
            zone: Uuid::from_u128(ZR),
            color: [1, 2, 3],
        },
        EditOp::AssignFixtureZone {
            fixture: Uuid::from_u128(FA),
            zone: Uuid::from_u128(ZR),
        },
        EditOp::SetFixturePositions {
            fixture: Uuid::from_u128(FB),
            positions: vec![Vec3::new(5.0, 1.0, 0.0), Vec3::new(6.0, 1.0, 0.0)],
        },
        EditOp::SetFixtureAddressing {
            fixture: Uuid::from_u128(FC),
            addressing: vec![
                PixelAddress {
                    universe: 2,
                    start_channel: 1,
                },
                PixelAddress {
                    universe: 2,
                    start_channel: 4,
                },
            ],
        },
        EditOp::SetZoneMappingSize {
            zone: Uuid::from_u128(ZL),
            size: [16, 2, 1],
        },
        EditOp::RemoveFixtures {
            ids: vec![Uuid::from_u128(FB)],
        },
        EditOp::InsertFixtures {
            at: vec![(0, new_fixture(0xDEAD))],
        },
    ];
    let n = ops.len();

    for op in ops {
        draft.apply(op, None).expect("op applies");
    }
    let edited = draft.patch().clone();
    assert_ne!(edited, start, "the edits actually changed the patch");

    for _ in 0..n {
        assert!(draft.undo(), "undo available");
    }
    assert_eq!(
        *draft.patch(),
        start,
        "undoing every edit reproduces the exact starting patch"
    );
    assert!(!draft.can_undo(), "undo stack is empty");

    for _ in 0..n {
        assert!(draft.redo(), "redo available");
    }
    assert_eq!(
        *draft.patch(),
        edited,
        "redoing every edit rebuilds the edited patch"
    );
}

#[test]
fn a_gesture_collapses_to_a_single_undo_step() {
    let start = base_patch();
    let mut draft = PatchDraft::new(start.clone());
    let drag = GestureId(1);

    // A drag emitting five successive absolute positions for the same fixture.
    for x in 1..=5 {
        draft
            .apply(
                EditOp::SetFixturePositions {
                    fixture: Uuid::from_u128(FA),
                    positions: vec![
                        Vec3::new(x as f32, 0.0, 0.0),
                        Vec3::new(x as f32 + 1.0, 0.0, 0.0),
                    ],
                },
                Some(drag),
            )
            .unwrap();
    }
    assert_ne!(*draft.patch(), start);

    // One undo returns to before the whole drag.
    assert!(draft.undo());
    assert_eq!(
        *draft.patch(),
        start,
        "the coalesced gesture undoes in a single step"
    );
    assert!(!draft.can_undo(), "the five moves were one undo entry");
}

#[test]
fn different_gestures_do_not_coalesce() {
    let mut draft = PatchDraft::new(base_patch());
    for (g, x) in [(1u64, 1.0f32), (2, 2.0)] {
        draft
            .apply(
                EditOp::SetFixturePositions {
                    fixture: Uuid::from_u128(FA),
                    positions: vec![Vec3::new(x, 0.0, 0.0), Vec3::new(x + 1.0, 0.0, 0.0)],
                },
                Some(GestureId(g)),
            )
            .unwrap();
    }
    // Two distinct gestures → two undo steps.
    assert!(draft.undo());
    assert!(draft.undo());
    assert!(!draft.can_undo());
}

#[test]
fn a_new_edit_clears_the_redo_stack() {
    let mut draft = PatchDraft::new(base_patch());
    draft
        .apply(
            EditOp::RenameZone {
                zone: Uuid::from_u128(ZL),
                name: "a".into(),
            },
            None,
        )
        .unwrap();
    assert!(draft.undo());
    assert!(draft.can_redo(), "there is something to redo");

    // A fresh edit forks history: redo is discarded.
    draft
        .apply(
            EditOp::RenameZone {
                zone: Uuid::from_u128(ZR),
                name: "b".into(),
            },
            None,
        )
        .unwrap();
    assert!(!draft.can_redo(), "the new edit cleared the redo stack");
}

#[test]
fn validation_is_re_run_after_an_edit_but_never_blocks_it() {
    let mut draft = PatchDraft::new(base_patch());
    assert!(draft.validate().is_ok(), "the base patch is valid");

    // Re-address fixture B onto fixture A's channels — a collision. The edit is
    // ACCEPTED (the editor shows the problem, it does not refuse the edit)...
    draft
        .apply(
            EditOp::SetFixtureAddressing {
                fixture: Uuid::from_u128(FB),
                addressing: vec![
                    PixelAddress {
                        universe: 1,
                        start_channel: 1,
                    },
                    PixelAddress {
                        universe: 1,
                        start_channel: 4,
                    },
                ],
            },
            None,
        )
        .expect("the edit is applied even though it is invalid");
    // ...and validation now reports it.
    assert!(
        matches!(draft.validate(), Err(PatchError::AddressCollision { .. })),
        "validation surfaces the collision"
    );

    // Undoing restores validity.
    assert!(draft.undo());
    assert!(draft.validate().is_ok(), "undo cleared the collision");
}

#[test]
fn a_structural_error_leaves_the_patch_and_history_untouched() {
    let start = base_patch();
    let mut draft = PatchDraft::new(start.clone());

    // Wrong pixel count for the fixture (it has 2 pixels).
    let err = draft
        .apply(
            EditOp::SetFixturePositions {
                fixture: Uuid::from_u128(FA),
                positions: vec![Vec3::ZERO],
            },
            None,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        prisma_editor::EditError::PixelCountMismatch { .. }
    ));
    assert_eq!(*draft.patch(), start, "the rejected op did not mutate");
    assert!(!draft.can_undo(), "the rejected op left no history");

    // A missing fixture also rejects cleanly.
    assert!(draft
        .apply(
            EditOp::AssignFixtureZone {
                fixture: Uuid::from_u128(0xF00D),
                zone: Uuid::from_u128(ZL),
            },
            None,
        )
        .is_err());
    assert_eq!(*draft.patch(), start);
}

// ---- Randomised fuzz: many valid ops, then undo all, must equal the start. ----

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn vec3(&mut self) -> Vec3 {
        Vec3::new(
            (self.below(16)) as f32,
            (self.below(4)) as f32,
            (self.below(2)) as f32,
        )
    }
}

/// Build a random op valid against the current patch (so `apply` never errors).
fn random_op(rng: &mut Lcg, patch: &Patch, fresh_id: &mut u128) -> EditOp {
    // Guard: if there are no fixtures, only insertion is possible.
    let has_fixtures = !patch.fixtures.is_empty();
    let kind = if has_fixtures { rng.below(7) } else { 6 };
    match kind {
        0 => {
            let f = &patch.fixtures[rng.below(patch.fixtures.len())];
            EditOp::SetFixturePositions {
                fixture: f.id,
                positions: f.pixels.iter().map(|_| rng.vec3()).collect(),
            }
        }
        1 => {
            let f = &patch.fixtures[rng.below(patch.fixtures.len())];
            EditOp::SetFixtureAddressing {
                fixture: f.id,
                addressing: f
                    .pixels
                    .iter()
                    .map(|_| PixelAddress {
                        universe: rng.below(4) as u16 + 1,
                        start_channel: rng.below(500) as u16 + 1,
                    })
                    .collect(),
            }
        }
        2 => {
            let f = &patch.fixtures[rng.below(patch.fixtures.len())];
            let z = &patch.zones[rng.below(patch.zones.len())];
            EditOp::AssignFixtureZone {
                fixture: f.id,
                zone: z.id,
            }
        }
        3 => {
            let z = &patch.zones[rng.below(patch.zones.len())];
            EditOp::RenameZone {
                zone: z.id,
                name: format!("z{}", rng.next()),
            }
        }
        4 => {
            let z = &patch.zones[rng.below(patch.zones.len())];
            EditOp::SetZoneColor {
                zone: z.id,
                color: [
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                ],
            }
        }
        5 => EditOp::RemoveFixtures {
            ids: vec![patch.fixtures[rng.below(patch.fixtures.len())].id],
        },
        _ => {
            *fresh_id += 1;
            let index = rng.below(patch.fixtures.len() + 1);
            EditOp::InsertFixtures {
                at: vec![(index, new_fixture(0x1000 + *fresh_id))],
            }
        }
    }
}

#[test]
fn randomised_op_sequences_always_undo_to_the_start() {
    for seed in 0..40u64 {
        let start = base_patch();
        let mut draft = PatchDraft::new(start.clone());
        let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1));
        let mut fresh_id = 0u128;

        let steps = 60;
        let mut applied = 0;
        for _ in 0..steps {
            let op = random_op(&mut rng, draft.patch(), &mut fresh_id);
            if draft.apply(op, None).is_ok() {
                applied += 1;
            }
        }
        assert!(applied > 0, "seed {seed}: some ops applied");

        for _ in 0..applied {
            assert!(draft.undo(), "seed {seed}: undo available");
        }
        assert_eq!(
            *draft.patch(),
            start,
            "seed {seed}: undoing {applied} random edits must reproduce the start exactly"
        );
        assert!(!draft.can_undo());
    }
}

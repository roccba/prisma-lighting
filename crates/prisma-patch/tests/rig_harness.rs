//! M2 milestone harness (CLAUDE.md §6, M2 "done when"):
//!
//!   "a hand-written patch file for the real test rig (pixel tubes, multiple
//!    sectors) loads and each zone can be independently lit with a solid color
//!    from a test harness."
//!
//! The golden file (`data/test_rig.patch.json`) is deliberately built to exercise
//! the features M5 will lean on:
//!   - a pixel that belongs to TWO zones (overlapping membership — the thing the
//!     M5 overlap counter counts),
//!   - a fixture whose color order is not RGB (GRB),
//!   - zones with explicit, non-uniform `z_order`, including a tie, to verify the
//!     deterministic declaration-order tie-break.
//!
//! At M2 there is no render engine, so "lit with a solid color" is expressed at
//! the canonical master-buffer level: write a colour into exactly a zone's member
//! pixels (via `PatchIndex` membership → byte offsets). The harness then asserts
//! the product's central invariant — **lighting a zone does not touch any pixel
//! outside that zone** — from M2 rather than waiting for M5.

use prisma_patch::{load_patch, ColorOrder, Patch, PatchIndex};

const GOLDEN: &str = include_str!("data/test_rig.patch.json");

fn rig() -> (Patch, PatchIndex) {
    let patch = load_patch(GOLDEN).expect("golden rig loads");
    let index = patch.compile().expect("golden rig compiles");
    (patch, index)
}

/// A solid RGBW colour; only the first `component_count` bytes of each pixel are
/// written, so RGB pixels take R,G,B and RGBW pixels take R,G,B,W.
const SOLID: [u8; 4] = [200, 150, 100, 50];

/// Light one zone into a canonical master buffer: write `SOLID` into each member
/// pixel's canonical bytes. This is what a zone's solid-colour effect produces at
/// the master-buffer level (no render engine needed at M2).
fn light_zone(index: &PatchIndex, master: &mut [u8], slot: usize) {
    for &global in index.zone_members(slot) {
        let offset = index.pixel_component_offset(global) as usize;
        let n = index.pixel_component_count(global) as usize;
        master[offset..offset + n].copy_from_slice(&SOLID[..n]);
    }
}

/// Global indices covered by a zone, as a boolean mask over all pixels.
fn coverage(index: &PatchIndex, slot: usize) -> Vec<bool> {
    let mut mask = vec![false; index.pixel_count() as usize];
    for &g in index.zone_members(slot) {
        mask[g as usize] = true;
    }
    mask
}

#[test]
fn golden_rig_loads_and_compiles() {
    let (patch, index) = rig();
    assert_eq!(patch.fixtures.len(), 2);
    assert_eq!(patch.zones.len(), 4);
    // 4 RGB (3 bytes) + 4 RGBW (4 bytes) = 8 pixels, 28 canonical bytes.
    assert_eq!(index.pixel_count(), 8);
    assert_eq!(index.master_buffer_len(), 28);
}

#[test]
fn rig_has_a_non_rgb_fixture() {
    let (patch, _) = rig();
    let non_rgb = patch
        .fixtures
        .iter()
        .flat_map(|f| &f.pixels)
        .any(|p| p.color_order != ColorOrder::Rgb);
    assert!(non_rgb, "the rig must exercise a non-RGB color order");
}

#[test]
fn rig_has_a_pixel_in_two_zones() {
    let (_, index) = rig();
    // Count, per pixel, how many zones cover it — the static analogue of M5's
    // active_zone_count. At least one pixel must be multiply-covered.
    let mut counts = vec![0u32; index.pixel_count() as usize];
    for slot in 0..index.zone_count() {
        for &g in index.zone_members(slot) {
            counts[g as usize] += 1;
        }
    }
    let max_overlap = counts.iter().copied().max().unwrap();
    assert!(
        max_overlap >= 2,
        "the rig must exercise overlapping membership (got max coverage {max_overlap})"
    );
    // Concretely: global pixel 0 is in both sector-left and global-strobe.
    assert_eq!(counts[0], 2);
}

#[test]
fn composite_order_breaks_ties_by_declaration_order() {
    let (patch, index) = rig();

    // sector-left (slot 0) and sector-right (slot 1) both have z_order 0. Their
    // ids are set so sector-right's is *lower* — a sort by id would put it first.
    // The stable, declaration-order tie-break must keep sector-left (declared
    // first) ahead of it.
    assert_eq!(patch.zones[0].name, "sector-left");
    assert_eq!(patch.zones[1].name, "sector-right");
    assert_eq!(patch.zones[0].z_order, patch.zones[1].z_order);
    assert!(
        patch.zones[1].id < patch.zones[0].id,
        "test premise: sector-right has the lower id"
    );

    // Bottom-to-top: [sector-left, sector-right, sector-b(z=5), global-strobe(z=10)].
    assert_eq!(index.composite_order(), &[0, 1, 2, 3]);
}

#[test]
fn each_zone_lights_independently_without_touching_pixels_outside_it() {
    // The central product invariant, asserted from M2: lighting a zone affects
    // exactly its own pixels and nothing else — the whole reason zones are
    // first-class (CLAUDE.md §1.1).
    let (_, index) = rig();

    for slot in 0..index.zone_count() {
        let mut master = vec![0u8; index.master_buffer_len()];
        light_zone(&index, &mut master, slot);

        let covered = coverage(&index, slot);
        for global in 0..index.pixel_count() {
            let offset = index.pixel_component_offset(global) as usize;
            let n = index.pixel_component_count(global) as usize;
            let bytes = &master[offset..offset + n];

            if covered[global as usize] {
                assert_eq!(
                    bytes,
                    &SOLID[..n],
                    "zone slot {slot}: member pixel {global} should carry the solid colour"
                );
            } else {
                assert!(
                    bytes.iter().all(|&b| b == 0),
                    "zone slot {slot}: pixel {global} is outside the zone but was modified \
                     (isolation invariant violated)"
                );
            }
        }
    }
}

#[test]
fn lighting_one_sector_leaves_the_other_dark() {
    // A pointed instance of the invariant: lighting sector-left must leave every
    // sector-right pixel black, even though a global-strobe zone spans both.
    let (_, index) = rig();
    let mut master = vec![0u8; index.master_buffer_len()];
    light_zone(&index, &mut master, 0); // sector-left

    for &g in index.zone_members(1) {
        // sector-right members
        let offset = index.pixel_component_offset(g) as usize;
        let n = index.pixel_component_count(g) as usize;
        assert!(
            master[offset..offset + n].iter().all(|&b| b == 0),
            "sector-right pixel {g} was lit by lighting sector-left"
        );
    }
}

//! The dynamic incremental overlap counter (CLAUDE.md §5.2.1).
//!
//! **GPU-free on purpose.** Nothing here touches wgpu; it lives in
//! `prisma-render` only because it needs patch geometry (zone membership, pixel
//! cells), which `prisma-core` deliberately does not depend on. It is therefore
//! fully testable headlessly, which matters because its correctness is a timing
//! and ordering property, not a visual one.
//!
//! # What it maintains
//!
//! Per pixel: how many zones that currently satisfy `contributes_pixels()` cover
//! it (`count`), and the **sum of those zones' slot indices** (`zone_sum`). The
//! sum is what lets the output stage recover *which* zone covers a singly-covered
//! pixel without a reverse index or a search — a sum of one element is that
//! element. See §5.2.1 for the width bound on `zone_sum` (`u32`, ~257x headroom).
//!
//! # Incremental, never recomputed
//!
//! Membership is fixed by the Patch; only *which zones contribute* changes at
//! runtime. A zone flipping costs one linear pass over its own members. Rebuilding
//! the whole mask on every strobe hit would stall exactly when the strobe fires —
//! the worst possible moment — so it is never done.
//!
//! **Do not amortize the solo toggle.** Pressing solo flips every non-soloed
//! zone at once; the total is bounded by the patch's pixel count (each pixel is
//! touched once per covering zone, and per-pixel coverage is small and fixed), so
//! it is a single linear pass — the same order the output stage already does every
//! frame — paid once, at the toggle. Spreading it across frames would trade a
//! one-frame, already-normal cost for a state machine in the hot path.

use std::sync::Arc;

use prisma_core::overlap::{OverlapSnapshot, NO_CELL};
use prisma_core::zone::ZoneStates;
use prisma_patch::{cell_of, LayoutGeneration, Patch, PatchIndex};

/// Per-cell coverage mask for every zone: `out[slot][cell] != 0` iff some member
/// pixel of that zone lands in that cell.
///
/// This is what confines a zone's contribution to its own geometry when it
/// composites into the master framebuffer (§5.2 step 4) — without it a zone's
/// content would bleed over every other zone's pixels, and zones would not be
/// independent, which is the product's entire thesis. Static per layout: build
/// once, upload once.
pub fn zone_cell_coverage(patch: &Patch, index: &PatchIndex) -> Vec<Vec<u32>> {
    let cell_count = (patch.matrix_size[0] as usize) * (patch.matrix_size[1] as usize);
    // Global pixel -> cell, resolved with the shared rule.
    let mut cell_of_pixel = vec![NO_CELL; index.pixel_count() as usize];
    for fixture in &patch.fixtures {
        for pixel in &fixture.pixels {
            let global = index
                .global_index(fixture.id, pixel.index)
                .expect("index built from this patch resolves every pixel")
                as usize;
            cell_of_pixel[global] = match cell_of(pixel.position) {
                Some(c)
                    if (0..3)
                        .all(|axis| c[axis] >= 0 && c[axis] < patch.matrix_size[axis] as i64) =>
                {
                    c[1] as u32 * patch.matrix_size[0] + c[0] as u32
                }
                _ => NO_CELL,
            };
        }
    }

    (0..index.zone_count())
        .map(|slot| {
            let mut mask = vec![0u32; cell_count];
            for &p in index.zone_members(slot) {
                let cell = cell_of_pixel[p as usize];
                if cell != NO_CELL {
                    mask[cell as usize] = 1;
                }
            }
            mask
        })
        .collect()
}

/// Maintains the per-pixel active-zone counters and publishes frame snapshots.
pub struct OverlapCounter {
    count_len: usize,
    cell_count: usize,
    component_count: usize,
    /// Per global pixel: its row-major framebuffer cell, or [`NO_CELL`] if the
    /// position is outside the matrix (such a pixel gathers black — see S3).
    /// Stored as a flat array, not a struct-of-fields, so a snapshot rebuild can
    /// hand it straight to `derive_from_pixels` without allocating.
    cells: Vec<u32>,
    /// Per global pixel: `(component offset, component count)` in the master buffer.
    component_ranges: Vec<(u32, u8)>,
    /// Cached `contributes_pixels()` per zone slot, for flip detection.
    contributing: Vec<bool>,
    /// Zone membership, borrowed shape from the patch index (global pixel ids).
    members: Vec<Box<[u32]>>,
    generation: LayoutGeneration,
    /// Working per-pixel arrays. Live between frames; copied into a snapshot only
    /// when something changed.
    count: Vec<u8>,
    zone_sum: Vec<u32>,
    dirty: bool,
    published: Option<Arc<OverlapSnapshot>>,
}

impl OverlapCounter {
    /// Build a counter for a compiled layout. All per-pixel geometry is resolved
    /// here; nothing below allocates per frame.
    pub fn new(patch: &Patch, index: &PatchIndex) -> OverlapCounter {
        let pixel_count = index.pixel_count() as usize;
        let cell_count = (patch.matrix_size[0] as usize) * (patch.matrix_size[1] as usize);

        // Indexed by global pixel, which is what every array here is indexed by.
        let mut cells = vec![NO_CELL; pixel_count];
        let mut component_ranges = vec![(0u32, 0u8); pixel_count];
        for fixture in &patch.fixtures {
            for pixel in &fixture.pixels {
                let global = index
                    .global_index(fixture.id, pixel.index)
                    .expect("index built from this patch resolves every pixel")
                    as usize;
                cells[global] = match cell_of(pixel.position) {
                    Some(c)
                        if (0..3).all(|axis| {
                            c[axis] >= 0 && c[axis] < patch.matrix_size[axis] as i64
                        }) =>
                    {
                        c[1] as u32 * patch.matrix_size[0] + c[0] as u32
                    }
                    _ => NO_CELL,
                };
                component_ranges[global] = (
                    index.pixel_component_offset(global as u32),
                    index.pixel_component_count(global as u32),
                );
            }
        }

        let members: Vec<Box<[u32]>> = (0..index.zone_count())
            .map(|slot| index.zone_members(slot).to_vec().into_boxed_slice())
            .collect();

        OverlapCounter {
            count_len: pixel_count,
            cell_count,
            component_count: index.master_buffer_len(),
            cells,
            component_ranges,
            contributing: vec![false; index.zone_count()],
            members,
            generation: index.generation(),
            count: vec![0; pixel_count],
            zone_sum: vec![0; pixel_count],
            dirty: true,
            published: None,
        }
    }

    /// The layout this counter was built against; compare before reusing it after
    /// a patch recompile.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// Whether a pixel is currently multiply covered (live counter, not a
    /// snapshot). For tests and introspection — the pipeline reads snapshots.
    #[inline]
    pub fn is_overlapped(&self, pixel: usize) -> bool {
        self.count[pixel] > 1
    }

    #[inline]
    pub fn count(&self, pixel: usize) -> u8 {
        self.count[pixel]
    }

    /// Re-evaluate `contributes_pixels()` for every zone and apply the deltas for
    /// those that flipped. Returns how many zones flipped.
    ///
    /// Call once per frame, at the frame boundary, before [`snapshot`](Self::snapshot).
    /// The scan over zones is O(zones) and trivial; the real work is proportional
    /// to the members of the zones that actually flipped.
    pub fn sync(&mut self, zones: &ZoneStates) -> usize {
        debug_assert_eq!(zones.len(), self.contributing.len());
        let mut flipped = 0;
        for slot in 0..self.contributing.len() {
            let now = zones.contributes_pixels(slot);
            if now == self.contributing[slot] {
                continue;
            }
            self.contributing[slot] = now;
            flipped += 1;
            self.apply_delta(slot, now);
        }
        if flipped > 0 {
            self.dirty = true;
        }
        flipped
    }

    /// One zone's transition: a single linear pass over its own members.
    fn apply_delta(&mut self, slot: usize, contributing_now: bool) {
        let slot_value = slot as u32;
        let members = &self.members[slot];
        if contributing_now {
            for &p in members.iter() {
                let p = p as usize;
                // Saturating: >255 simultaneously-contributing zones on one pixel
                // is not a real configuration (§5.2.1), but must not wrap.
                self.count[p] = self.count[p].saturating_add(1);
                self.zone_sum[p] += slot_value;
            }
        } else {
            for &p in members.iter() {
                let p = p as usize;
                self.count[p] = self.count[p].saturating_sub(1);
                self.zone_sum[p] -= slot_value;
            }
        }
    }

    /// Publish the classification for this frame.
    ///
    /// Returns the previously published snapshot unchanged when nothing flipped —
    /// the common case, costing one refcount bump and no copy. When the counter
    /// changed, the snapshot is rebuilt **in place** if no frame still holds the
    /// previous one (`Arc::get_mut`), so a strobing zone recycles its buffer
    /// instead of churning the allocator; only if a consumer still holds it is a
    /// fresh one allocated.
    pub fn snapshot(&mut self) -> Arc<OverlapSnapshot> {
        if !self.dirty {
            if let Some(published) = &self.published {
                return Arc::clone(published);
            }
        }

        let mut arc = self.published.take().unwrap_or_else(|| {
            Arc::new(OverlapSnapshot::zeroed(
                self.count_len,
                self.cell_count,
                self.component_count,
            ))
        });
        match Arc::get_mut(&mut arc) {
            Some(snapshot) => self.rebuild_into(snapshot),
            None => {
                // A frame in flight still references the old snapshot; leave it
                // alone and build a fresh one.
                let mut fresh =
                    OverlapSnapshot::zeroed(self.count_len, self.cell_count, self.component_count);
                self.rebuild_into(&mut fresh);
                arc = Arc::new(fresh);
            }
        }

        self.dirty = false;
        self.published = Some(Arc::clone(&arc));
        arc
    }

    fn rebuild_into(&self, snapshot: &mut OverlapSnapshot) {
        {
            let (count, zone_sum) = snapshot.pixel_arrays_mut();
            count.copy_from_slice(&self.count);
            zone_sum.copy_from_slice(&self.zone_sum);
        }
        snapshot.derive_from_pixels(&self.cells, &self.component_ranges);
    }
}

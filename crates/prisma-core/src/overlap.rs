//! The frame-boundary overlap snapshot (CLAUDE.md §5.2.1).
//!
//! This is the **immutable payload** that travels render → output attached to a
//! frame. The incremental counter that produces it needs patch geometry and so
//! lives in `prisma-render`; the snapshot itself is plain data with no GPU or
//! patch dependency, so it lives here in the headless model — which is also where
//! its invariant can be tested without a GPU.
//!
//! # What it answers, and for whom
//!
//! | field | indexed by | consumer |
//! |---|---|---|
//! | `count` | global pixel | classification base |
//! | `zone_sum` | global pixel | *which* zone covers a singly-covered pixel |
//! | `cell_overlapped` | framebuffer cell | render, §5.2 step 4 |
//! | `zone_at_output` | master-buffer component | output stage, §5.2 step 6 |
//!
//! The last two are derived from the first two when the snapshot is built, so
//! neither hot path recomputes anything.
//!
//! # The invariant
//!
//! Once published a snapshot never changes. Render step 4 and output step 6 of a
//! single frame read **the same** snapshot, so each pixel's zone dimmer/colour is
//! applied in exactly one stage — never twice (a one-frame darkening) and never
//! zero times (a one-frame full-bright flash) as a pixel crosses between paths.
//! The output stage must read this snapshot and **never the live counter**, which
//! it cannot reach.

/// Sentinel in [`OverlapSnapshot::zone_at_output`]: this component gets no
/// per-zone scaling at the output stage (its cell was dimmed in render, or no
/// zone covers it).
pub const NO_ZONE: u32 = u32::MAX;

/// Sentinel cell index for a pixel whose position fell outside the matrix.
pub const NO_CELL: u32 = u32::MAX;

/// An immutable classification of every pixel for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlapSnapshot {
    count: Vec<u8>,
    zone_sum: Vec<u32>,
    /// `u32` rather than `bool`/`u8` so it uploads straight into a WGSL
    /// `array<u32>` storage buffer with no repacking.
    cell_overlapped: Vec<u32>,
    zone_at_output: Vec<u32>,
}

impl OverlapSnapshot {
    /// An all-zero snapshot sized for a layout. Used as the recyclable buffer the
    /// counter rebuilds in place.
    pub fn zeroed(pixel_count: usize, cell_count: usize, component_count: usize) -> Self {
        Self {
            count: vec![0; pixel_count],
            zone_sum: vec![0; pixel_count],
            cell_overlapped: vec![0; cell_count],
            zone_at_output: vec![NO_ZONE; component_count],
        }
    }

    #[inline]
    pub fn pixel_count(&self) -> usize {
        self.count.len()
    }

    /// How many contributing zones cover this pixel.
    #[inline]
    pub fn count(&self, pixel: usize) -> u8 {
        self.count[pixel]
    }

    /// Is this pixel itself multiply covered? Note the **render** decision uses
    /// [`cell_overlapped`](Self::cell_overlapped), not this — see §5.2.1.
    #[inline]
    pub fn is_pixel_overlapped(&self, pixel: usize) -> bool {
        self.count[pixel] > 1
    }

    /// The single contributing zone covering this pixel, if exactly one does.
    /// Recovered from the sum, which equals its lone term (§5.2.1).
    #[inline]
    pub fn sole_zone(&self, pixel: usize) -> Option<u32> {
        (self.count[pixel] == 1).then(|| self.zone_sum[pixel])
    }

    /// Does this framebuffer cell hold any overlapped pixel? Render applies the
    /// per-zone dimmer/colour before blending exactly where this is true.
    #[inline]
    pub fn cell_overlapped(&self, cell: usize) -> bool {
        self.cell_overlapped[cell] != 0
    }

    /// The cell mask as bytes, for direct upload to a WGSL `array<u32>`.
    #[inline]
    pub fn cell_mask_bytes(&self) -> &[u32] {
        &self.cell_overlapped
    }

    /// For a master-buffer component: the **zone slot** whose dimmer/colour the
    /// output stage must apply, or `None` if this component takes master scaling
    /// only (its cell was dimmed in render, or no zone covers it).
    ///
    /// Which *channel* of that zone's colour filter to use is not stored here —
    /// the compiled `OutputPlan` already resolves each DMX channel to its
    /// canonical component index while baking the colour order, so it is the
    /// natural (and single) home for that fact.
    /// Bounds-safe: an out-of-range component yields `None` rather than panicking,
    /// matching the output path's "never panic in the field" stance.
    #[inline]
    pub fn zone_at_output(&self, component: usize) -> Option<u32> {
        match self.zone_at_output.get(component) {
            Some(&slot) if slot != NO_ZONE => Some(slot),
            _ => None,
        }
    }

    // ---- construction ------------------------------------------------------
    //
    // These are the write side, called only by the incremental counter (which
    // lives in `prisma-render` because it needs patch geometry). They are `pub`
    // for that reason, not because a snapshot is meant to be mutated once it has
    // been published — after publication it is immutable, and the whole
    // "exactly one stage" invariant rests on that.

    /// Mutable access to the per-pixel arrays, for the counter to rebuild in place.
    pub fn pixel_arrays_mut(&mut self) -> (&mut [u8], &mut [u32]) {
        (&mut self.count, &mut self.zone_sum)
    }

    /// Recompute the two derived views (`cell_overlapped`, `zone_at_output`) from
    /// the per-pixel arrays. `cell_of_pixel` maps each global pixel to its
    /// framebuffer cell ([`NO_CELL`] if outside the matrix), and
    /// `component_range_of_pixel` gives each pixel's `(component offset, count)`.
    pub fn derive_from_pixels(
        &mut self,
        cell_of_pixel: &[u32],
        component_range_of_pixel: &[(u32, u8)],
    ) {
        // 1. Promote the per-pixel classification to cells: a cell is overlapped
        //    if ANY pixel in it is. Step 4 composites in cell units, so a cell
        //    cannot hold both a dimmed and an undimmed value (§5.2.1).
        self.cell_overlapped.fill(0);
        for (pixel, &cell) in cell_of_pixel.iter().enumerate() {
            if cell != NO_CELL && self.count[pixel] > 1 {
                self.cell_overlapped[cell as usize] = 1;
            }
        }

        // 2. Project back onto components for the output stage. A pixel takes the
        //    shortcut only if its cell was NOT dimmed in render and exactly one
        //    zone covers it.
        self.zone_at_output.fill(NO_ZONE);
        for (pixel, &(offset, components)) in component_range_of_pixel.iter().enumerate() {
            let cell = cell_of_pixel[pixel];
            let dimmed_in_render = cell != NO_CELL && self.cell_overlapped[cell as usize] != 0;
            if dimmed_in_render || self.count[pixel] != 1 {
                continue;
            }
            let slot = self.zone_sum[pixel];
            for k in 0..components as u32 {
                self.zone_at_output[(offset + k) as usize] = slot;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pixels in one cell: pixel 0 overlapped (count 2), pixel 1 singly
    /// covered (count 1). This is the case the cell promotion exists for.
    #[test]
    fn a_mixed_cell_denies_the_shortcut_to_both_its_pixels() {
        // 2 pixels, 1 cell, 6 components (2 x RGB).
        let mut snap = OverlapSnapshot::zeroed(2, 1, 6);
        {
            let (count, zone_sum) = snap.pixel_arrays_mut();
            count[0] = 2; // covered by zones 0 and 1
            zone_sum[0] = 1; // 0 + 1
            count[1] = 1; // covered by zone 0 only
            zone_sum[1] = 0;
        }
        snap.derive_from_pixels(&[0, 0], &[(0, 3), (3, 3)]);

        assert!(
            snap.cell_overlapped(0),
            "the cell holds an overlapped pixel"
        );
        // Pixel 0 is overlapped: obviously no output-stage scaling.
        for c in 0..3 {
            assert_eq!(snap.zone_at_output(c), None);
        }
        // Pixel 1 has count == 1, yet must ALSO be denied the shortcut: render
        // already dimmed its cell, so dimming again at output would darken it
        // twice.
        for c in 3..6 {
            assert_eq!(
                snap.zone_at_output(c),
                None,
                "component {c} shares an overlapped cell — must not be dimmed twice"
            );
        }
    }

    #[test]
    fn a_clean_cell_keeps_the_shortcut_and_names_its_zone() {
        let mut snap = OverlapSnapshot::zeroed(1, 1, 4);
        {
            let (count, zone_sum) = snap.pixel_arrays_mut();
            count[0] = 1;
            zone_sum[0] = 7; // zone slot 7
        }
        snap.derive_from_pixels(&[0], &[(0, 4)]);

        assert!(!snap.cell_overlapped(0));
        assert_eq!(snap.sole_zone(0), Some(7));
        for k in 0..4usize {
            assert_eq!(snap.zone_at_output(k), Some(7));
        }
    }

    #[test]
    fn an_uncovered_pixel_takes_master_scaling_only() {
        let mut snap = OverlapSnapshot::zeroed(1, 1, 3);
        snap.derive_from_pixels(&[0], &[(0, 3)]);
        assert_eq!(snap.sole_zone(0), None);
        for c in 0..3 {
            assert_eq!(snap.zone_at_output(c), None);
        }
    }
}

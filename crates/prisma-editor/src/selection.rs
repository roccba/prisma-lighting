//! Aggregate properties of a fixture selection (M7 spec §5.4).
//!
//! Before a bulk operation the operator needs to verify *what* is selected —
//! fixture count, position extents, the DMX address span. This computes that from
//! the patch and the selected fixture ids; it crosses the IPC boundary to the
//! properties panel, hence `Serialize`.

use prisma_patch::Patch;
use serde::Serialize;
use uuid::Uuid;

/// Summary of a set of selected fixtures. `None`-returning [`aggregate`] means the
/// selection was empty (or matched no fixtures); a present value always covers at
/// least one pixel.
///
/// Ordinal fixture IDs (a per-fixture install number) are **not**
/// here: the model identifies fixtures by UUID + name, and the user-facing integer
/// ID arrives with the placement dialog's "starting fixture ID" (§4.1). Until then
/// the meaningful ranges are position and DMX address.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectionAggregate {
    pub fixture_count: usize,
    pub pixel_count: usize,
    /// Per-axis minimum / maximum of every selected pixel's position.
    pub position_min: [f32; 3],
    pub position_max: [f32; 3],
    /// Inclusive universe span across the selection.
    pub universe_min: u16,
    pub universe_max: u16,
    /// Inclusive `start_channel` span across the selection.
    pub channel_min: u16,
    pub channel_max: u16,
}

/// Aggregate the selected fixtures. Ids not present in the patch are ignored; if
/// nothing matches (or the selection covers no pixels) returns `None`.
pub fn aggregate(patch: &Patch, selected: &[Uuid]) -> Option<SelectionAggregate> {
    let mut fixture_count = 0usize;
    let mut pixel_count = 0usize;
    let mut pos_min = [f32::INFINITY; 3];
    let mut pos_max = [f32::NEG_INFINITY; 3];
    let mut universe_min = u16::MAX;
    let mut universe_max = u16::MIN;
    let mut channel_min = u16::MAX;
    let mut channel_max = u16::MIN;

    for fixture in patch.fixtures.iter().filter(|f| selected.contains(&f.id)) {
        fixture_count += 1;
        for pixel in &fixture.pixels {
            pixel_count += 1;
            let p = pixel.position.to_array();
            for axis in 0..3 {
                pos_min[axis] = pos_min[axis].min(p[axis]);
                pos_max[axis] = pos_max[axis].max(p[axis]);
            }
            universe_min = universe_min.min(pixel.universe);
            universe_max = universe_max.max(pixel.universe);
            channel_min = channel_min.min(pixel.start_channel);
            channel_max = channel_max.max(pixel.start_channel);
        }
    }

    if pixel_count == 0 {
        return None;
    }
    Some(SelectionAggregate {
        fixture_count,
        pixel_count,
        position_min: pos_min,
        position_max: pos_max,
        universe_min,
        universe_max,
        channel_min,
        channel_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::wedding_rig;

    #[test]
    fn empty_selection_aggregates_to_none() {
        let patch = wedding_rig();
        assert!(aggregate(&patch, &[]).is_none());
        // An id that is not in the patch also yields nothing.
        assert!(aggregate(&patch, &[Uuid::from_u128(0xDEAD)]).is_none());
    }

    #[test]
    fn aggregate_spans_the_selected_fixtures_only() {
        let patch = wedding_rig();
        // Select the first two fixtures (both Booth vertical 30px tubes at x=3 and 5).
        let ids: Vec<Uuid> = patch.fixtures.iter().take(2).map(|f| f.id).collect();
        let agg = aggregate(&patch, &ids).expect("non-empty");

        assert_eq!(agg.fixture_count, 2);
        assert_eq!(agg.pixel_count, 60); // two 30px tubes
                                         // Booth tubes 1 and 2 sit at x = 3 and 5, y = 3..=32.
        assert_eq!(agg.position_min, [3.0, 3.0, 0.0]);
        assert_eq!(agg.position_max, [5.0, 32.0, 0.0]);
        // Their channels start at universe 1, channel 1.
        assert_eq!(agg.universe_min, 1);
        assert!(agg.channel_min >= 1);
    }

    #[test]
    fn selecting_everything_covers_every_pixel() {
        let patch = wedding_rig();
        let ids: Vec<Uuid> = patch.fixtures.iter().map(|f| f.id).collect();
        let agg = aggregate(&patch, &ids).expect("non-empty");
        let total: usize = patch.fixtures.iter().map(|f| f.pixels.len()).sum();
        assert_eq!(agg.pixel_count, total);
        assert_eq!(agg.fixture_count, patch.fixtures.len());
        // Spans more than one universe (the rig is ~340 px > 170 px/universe).
        assert!(agg.universe_max > agg.universe_min);
    }
}

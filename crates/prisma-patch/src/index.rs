//! Runtime-derived index over a [`Patch`](crate::Patch).
//!
//! # The global pixel index is a runtime derivative — never serialized
//!
//! Every pixel gets a dense **global index** `0..N` here, assigned by walking
//! fixtures in order and pixels in order. That index is the single coordinate
//! space shared by the canonical master buffer, the M5 overlap counter, the
//! output plan and the preview.
//!
//! It is **computed, never stored**. Nothing in the on-disk model
//! ([`Patch`](crate::Patch), zones, `PixelRef`) holds a global index; if any
//! serialized type ever gains one, that is a bug. The reason is concrete:
//! reordering fixtures in the file (or the M7 patch editor reordering them)
//! reassigns every global index. A persisted index would silently point at the
//! wrong pixel after such an edit.
//!
//! # Layout generation — how consumers know their caches went stale
//!
//! Because the global index space is rebuilt on every [`Patch::compile`], any
//! consumer that sized an array to `N` pixels (the M5 `active_zone_count`, a
//! cached output plan, the preview buffer) must be able to tell that the layout
//! it was built against is gone. Each [`PatchIndex`] carries a process-unique
//! [`LayoutGeneration`]; consumers store the generation they built against and
//! compare. A mismatch means "rebuild your caches" (§5.2.1: the counter is
//! dimensioned to N).

use crate::error::PatchError;
use crate::patch::Patch;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

/// A process-unique, monotonically increasing tag identifying one build of the
/// pixel-index layout. Two [`PatchIndex`] instances never share a generation
/// (each [`PatchIndex::build`] mints a fresh one), so an equality check is a
/// sound "is my cache still valid?" test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutGeneration(u64);

impl LayoutGeneration {
    /// Sentinel for "no layout yet" — safe to store as the initial cached
    /// generation in a consumer, since a real generation is never `0`.
    pub const NONE: LayoutGeneration = LayoutGeneration(0);

    /// The raw counter value, for logging/telemetry.
    #[inline]
    pub fn get(self) -> u64 {
        self.0
    }

    fn mint() -> Self {
        // Relaxed is sufficient: we only need uniqueness and monotonicity, not
        // ordering against other memory. Starts at 1 so NONE (0) is distinct.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        LayoutGeneration(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Where a fixture's pixels land in the global index space.
#[derive(Debug, Clone, Copy)]
struct FixtureSpan {
    /// Global index of this fixture's first pixel.
    base: u32,
    /// Number of pixels in the fixture.
    count: u32,
}

/// One zone's membership, resolved to global pixel indices, deduplicated and
/// sorted. This is exactly what the M5 overlap counter iterates on a zone's
/// activation transition (`for p in members { active_zone_count[p] ± 1 }`,
/// §5.2.1) — precomputed once here, never per frame.
#[derive(Debug, Clone)]
pub struct ZoneMembership {
    members: Box<[u32]>,
}

impl ZoneMembership {
    /// The zone's member pixels as global indices: sorted ascending, no
    /// duplicates.
    #[inline]
    pub fn members(&self) -> &[u32] {
        &self.members
    }
}

/// The runtime index over a patch: global pixel indexing, the canonical
/// master-buffer byte layout, and resolved zone membership. Built by
/// [`Patch::compile`]; **not** serializable by design (see module docs).
#[derive(Debug, Clone)]
pub struct PatchIndex {
    generation: LayoutGeneration,
    pixel_count: u32,
    /// Prefix sums of per-pixel component counts, length `pixel_count + 1`.
    /// `offsets[g]` is the component (element) offset of global pixel `g` in the
    /// canonical (linear-light `[f16]`) master buffer; `offsets[pixel_count]` is
    /// its total component count. Component index, not byte offset.
    offsets: Box<[u32]>,
    /// Parallel to `Patch::zones`: `zones[i]` is the membership of the i-th zone.
    zones: Box<[ZoneMembership]>,
    /// Zone slots in bottom-to-top compositing order: sorted by `z_order`
    /// ascending, ties broken by declaration order in `Patch::zones`. Precomputed
    /// once here so the M5 compositor has a single, deterministic source of truth
    /// for overlap resolution (§5.2). See [`PatchIndex::composite_order`].
    composite_order: Box<[u32]>,
    /// Fixture id → where its pixels live in the global space. For resolving
    /// `PixelRef`s; not on any hot path.
    spans: HashMap<Uuid, FixtureSpan>,
}

impl PatchIndex {
    /// Build the index from an already-validated patch. Callers should use
    /// [`Patch::compile`], which validates first.
    ///
    /// Assumes structural validity (dense pixel indices, in-range refs) as
    /// guaranteed by [`Patch::validate`]; still returns `Result` so the seam
    /// stays honest if future invariants are checked here.
    pub fn build(patch: &Patch) -> Result<Self, PatchError> {
        let total_pixels: usize = patch.fixtures.iter().map(|f| f.pixels.len()).sum();

        let mut offsets = Vec::with_capacity(total_pixels + 1);
        let mut spans = HashMap::with_capacity(patch.fixtures.len());
        let mut running_bytes: u32 = 0;
        let mut running_global: u32 = 0;

        for fixture in &patch.fixtures {
            let components = fixture.profile.component_count() as u32;
            spans.insert(
                fixture.id,
                FixtureSpan {
                    base: running_global,
                    count: fixture.pixels.len() as u32,
                },
            );
            for _ in &fixture.pixels {
                offsets.push(running_bytes);
                running_bytes += components;
                running_global += 1;
            }
        }
        offsets.push(running_bytes); // sentinel: total master-buffer length

        // Resolve zone membership to global indices, dedup + sort. Validation
        // already guaranteed every ref resolves, so `resolve` cannot miss here.
        let mut zones = Vec::with_capacity(patch.zones.len());
        for zone in &patch.zones {
            let mut members: Vec<u32> = zone
                .members
                .iter()
                .filter_map(|r| resolve(&spans, r.fixture, r.pixel))
                .collect();
            members.sort_unstable();
            members.dedup();
            zones.push(ZoneMembership {
                members: members.into_boxed_slice(),
            });
        }

        // Compositing order: stable sort of zone slots by z_order. A *stable*
        // sort leaves tied zones in their declaration order — that is the
        // documented, deterministic tie-break (§4.1).
        let mut composite_order: Vec<u32> = (0..patch.zones.len() as u32).collect();
        composite_order.sort_by_key(|&slot| patch.zones[slot as usize].z_order);

        Ok(PatchIndex {
            generation: LayoutGeneration::mint(),
            pixel_count: running_global,
            offsets: offsets.into_boxed_slice(),
            zones: zones.into_boxed_slice(),
            composite_order: composite_order.into_boxed_slice(),
            spans,
        })
    }

    /// The generation this index was built with. Consumers compare against a
    /// stored value to detect a stale cache (see module docs).
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// Number of pixels — the dimension of the master buffer (in pixels) and of
    /// the M5 `active_zone_count` array.
    #[inline]
    pub fn pixel_count(&self) -> u32 {
        self.pixel_count
    }

    /// Total number of **components** in the canonical (linear-light) master
    /// pixel buffer — i.e. its length in elements. Each element is one `f16`
    /// linear-light sample (§5.2), so the buffer's size in bytes is twice this;
    /// this count is what sizes the `[f16]` buffer, not a byte length.
    #[inline]
    pub fn master_buffer_len(&self) -> usize {
        self.offsets[self.pixel_count as usize] as usize
    }

    /// Component (element) offset of global pixel `g` in the canonical master
    /// buffer. This indexes the `[f16]` buffer directly — it is a component
    /// index, **not** a byte offset (each component is one 2-byte `f16`).
    ///
    /// # Panics
    /// Panics if `g >= pixel_count()`.
    #[inline]
    pub fn pixel_component_offset(&self, g: u32) -> u32 {
        self.offsets[g as usize]
    }

    /// Component count of global pixel `g` (3 for RGB, 4 for RGBW).
    ///
    /// # Panics
    /// Panics if `g >= pixel_count()`.
    #[inline]
    pub fn pixel_component_count(&self, g: u32) -> u8 {
        (self.offsets[g as usize + 1] - self.offsets[g as usize]) as u8
    }

    /// Number of zones (parallel to `Patch::zones`).
    #[inline]
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    /// Resolved membership of the zone at `slot` (its position in `Patch::zones`).
    ///
    /// # Panics
    /// Panics if `slot >= zone_count()`.
    #[inline]
    pub fn zone_members(&self, slot: usize) -> &[u32] {
        self.zones[slot].members()
    }

    /// Zone slots in bottom-to-top compositing order (§5.2): ascending `z_order`,
    /// ties broken by declaration order. The M5 compositor stacks zones in this
    /// order; exposed now so the tie-break is pinned down and testable from M2.
    #[inline]
    pub fn composite_order(&self) -> &[u32] {
        &self.composite_order
    }

    /// Resolve a `(fixture, local pixel index)` reference to its global index,
    /// or `None` if the fixture is unknown or the local index is out of range.
    #[inline]
    pub fn global_index(&self, fixture: Uuid, pixel: u32) -> Option<u32> {
        resolve(&self.spans, fixture, pixel)
    }
}

/// Resolve `(fixture, local index)` → global index against the span table.
#[inline]
fn resolve(spans: &HashMap<Uuid, FixtureSpan>, fixture: Uuid, pixel: u32) -> Option<u32> {
    let span = spans.get(&fixture)?;
    (pixel < span.count).then_some(span.base + pixel)
}

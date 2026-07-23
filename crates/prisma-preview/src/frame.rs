//! The preview handoff: what the render loop publishes for the preview thread
//! (CLAUDE.md M6).
//!
//! # A second instance of the output handoff, not a second consumer of it
//!
//! The frame handoff in `prisma-output` is a **single-producer / single-consumer**
//! triple buffer, and that is not an accident: the swap protocol guarantees the two
//! sides never address the same buffer, which is exactly what makes its
//! `UnsafeCell` access sound. Adding a second consumer to it would dismantle that.
//!
//! So the preview takes a *second instance of the same generic protocol*
//! ([`prisma_output::frame::triple_buffer`]) with its own payload. Everything that
//! made the original safe — newest-wins, at most one pending frame, never blocks
//! either side — comes along unchanged and already tested.
//!
//! # What it costs the render thread
//!
//! One extra publish per frame: a `f32 → f16` conversion fused into a copy of the
//! master buffer (~4 KB for a 680-pixel rig). It happens **after** the output
//! publish, and the output publish is left exactly as it was, so switching the
//! preview on cannot slow down the path that feeds the wire. When no preview is
//! attached the cost is zero — the producer is an `Option`.
//!
//! # Why the pixels are still the raw master buffer
//!
//! They are pre-flat-parameters, like the output frame, and for the same reason:
//! the flat parameters are read **live** by the consumer (§5.2 step 6), which is
//! what lets a fader move show up in the preview with no re-render. The frame
//! carries the overlap snapshot it was rendered under so the consumer can apply
//! them exactly as the output stage does — see [`crate::plan`].

use half::f16;
use prisma_core::zone::ZoneStates;
use prisma_core::OverlapSnapshot;
use prisma_output::frame::{triple_buffer, Consumer, Producer};
use prisma_patch::LayoutGeneration;
use std::sync::Arc;
use std::time::Instant;

/// Which zones are **active**, one bit per zone slot.
///
/// # `is_active()`, never `contributes_pixels()` — the distinction is the point
///
/// The two canonical predicates (§4.3) differ only by solo, and the preview needs
/// the one **without** it. A zone silenced by *another* zone's solo still has live
/// state: its strip and its pad must keep showing it, not vanish from the console
/// because solo was pressed somewhere else. `contributes_pixels()` answers a
/// different question — "is this zone putting light into the composite right now?"
/// — and belongs to the overlap counter and the compositor, not here.
///
/// # Why this travels with the frame instead of being read live
///
/// Unlike a dimmer or a colour filter, `is_active` is not a plain parameter read:
/// it rests on the hysteretic `dimmer_up` latch, which is **stateful** and evolves
/// as the render loop feeds it dimmer values frame after frame. A preview thread
/// keeping its own `ZoneStates` would be running a *second, independent* latch that
/// can disagree with the canonical one inside the dead band. One latch, one owner —
/// so the render loop stamps its answer onto the frame.
///
/// The accepted cost: with the render thread stopped the mask freezes, so a pad can
/// lag a fader taken to zero. At 44 Hz that is ≤23 ms, and with render stopped
/// there is no show anyway (CLAUDE.md M6, design point 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneActiveMask {
    words: Box<[u64]>,
    zone_count: usize,
}

impl ZoneActiveMask {
    /// An all-inactive mask for `zone_count` zones. All allocation happens here.
    pub fn new(zone_count: usize) -> ZoneActiveMask {
        ZoneActiveMask {
            words: vec![0u64; zone_count.div_ceil(64)].into_boxed_slice(),
            zone_count,
        }
    }

    #[inline]
    pub fn zone_count(&self) -> usize {
        self.zone_count
    }

    /// Is the zone at `slot` active? Out-of-range slots read `false` rather than
    /// panicking: a consumer may hold a mask from another layout generation, and a
    /// preview must degrade, never take anything down.
    #[inline]
    pub fn is_active(&self, slot: usize) -> bool {
        if slot >= self.zone_count {
            return false;
        }
        self.words[slot / 64] & (1u64 << (slot % 64)) != 0
    }

    /// Set one zone's bit. Out-of-range slots are ignored, as in [`is_active`].
    #[inline]
    pub fn set(&mut self, slot: usize, active: bool) {
        if slot >= self.zone_count {
            return;
        }
        let bit = 1u64 << (slot % 64);
        if active {
            self.words[slot / 64] |= bit;
        } else {
            self.words[slot / 64] &= !bit;
        }
    }

    /// Refresh in place from the render loop's canonical zone state. No allocation:
    /// the loop owns one mask for the life of the handoff.
    ///
    /// Reads [`ZoneStates::is_active`] — see the type docs for why that and not
    /// `contributes_pixels`.
    pub fn update_from(&mut self, zones: &ZoneStates) {
        for slot in 0..self.zone_count {
            self.set(slot, slot < zones.len() && zones.is_active(slot));
        }
    }

    /// Overwrite from another mask of the same size, without allocating. Used by
    /// the publish path to stamp the loop's mask onto the staged frame.
    pub fn copy_from(&mut self, other: &ZoneActiveMask) {
        let n = self.words.len().min(other.words.len());
        self.words[..n].copy_from_slice(&other.words[..n]);
        for w in self.words[n..].iter_mut() {
            *w = 0;
        }
    }

    /// The raw bits, least-significant bit = slot 0. The M6 wire format ships these
    /// verbatim, so a consumer needs no per-zone decoding loop.
    #[inline]
    pub fn words(&self) -> &[u64] {
        &self.words
    }
}

/// One frame as the preview sees it: the master pixel buffer plus everything a
/// consumer needs to interpret it.
pub struct PreviewFrame {
    /// The canonical (pixel-domain, linear-light `f16`) master buffer — the very
    /// same content the output frame carries, **before** flat parameters.
    pub pixels: Box<[f16]>,
    /// Monotonic id of this *preview* publish. Gaps are frames the newest-wins
    /// policy dropped — expected, and how a consumer measures its own drop rate.
    /// It counts preview publishes, not render frames; see [`render_frame`](Self::render_frame).
    pub frame_id: u64,
    /// The render frame number this content came from, so a consumer can report
    /// staleness in frames rather than guessing.
    pub render_frame: u64,
    /// When the producer staged this frame.
    pub produced_at: Instant,
    /// The layout generation this buffer was produced against. A consumer whose
    /// static descriptor was built under a different generation is holding stale
    /// geometry and must refuse to draw — the same guard the output router applies.
    pub layout_generation: LayoutGeneration,
    /// The overlap classification this frame was **rendered under** (§5.2.1). The
    /// preview applies the flat parameters against *this*, never a newer one, for
    /// exactly the reason the output stage does: a pixel must be dimmed in exactly
    /// one stage.
    pub overlap: Option<Arc<OverlapSnapshot>>,
    /// Which zones were active when this frame was rendered ([`ZoneActiveMask`]).
    pub zone_active: ZoneActiveMask,
}

/// Writing half of the preview handoff, owned by the render loop.
pub struct PreviewProducer {
    inner: Producer<PreviewFrame>,
    next_id: u64,
    layout_generation: LayoutGeneration,
    overlap: Option<Arc<OverlapSnapshot>>,
}

/// Reading half, owned by the preview thread.
pub struct PreviewConsumer {
    inner: Consumer<PreviewFrame>,
}

/// Build a preview handoff whose buffers each hold `component_len` `f16` elements
/// (`PatchIndex::master_buffer_len`) and `zone_count` zone bits. All three buffers
/// are allocated up front; nothing on the publish path allocates.
pub fn preview_handoff(
    component_len: usize,
    zone_count: usize,
) -> (PreviewProducer, PreviewConsumer) {
    let now = Instant::now();
    let (producer, consumer) = triple_buffer(|| PreviewFrame {
        pixels: vec![f16::ZERO; component_len].into_boxed_slice(),
        frame_id: 0,
        render_frame: 0,
        produced_at: now,
        layout_generation: LayoutGeneration::NONE,
        overlap: None,
        zone_active: ZoneActiveMask::new(zone_count),
    });
    (
        PreviewProducer {
            inner: producer,
            next_id: 1, // 0 marks a buffer that never carried a published frame
            layout_generation: LayoutGeneration::NONE,
            overlap: None,
        },
        PreviewConsumer { inner: consumer },
    )
}

impl PreviewProducer {
    /// Set the layout generation stamped onto subsequently published frames.
    #[inline]
    pub fn set_layout_generation(&mut self, generation: LayoutGeneration) {
        self.layout_generation = generation;
    }

    /// Set the overlap snapshot stamped onto subsequently published frames — the
    /// one the frame was *rendered under*, not the current one.
    #[inline]
    pub fn set_overlap(&mut self, overlap: Option<Arc<OverlapSnapshot>>) {
        self.overlap = overlap;
    }

    /// Stage and publish a preview frame. Never blocks; supersedes any unconsumed
    /// frame (newest-wins).
    ///
    /// `zone_active` is passed per call rather than staged like the generation
    /// because it is per-frame *data* that has to be copied into the buffer, not a
    /// cheap stamp. `fill` must overwrite the whole pixel slice — the buffer may
    /// still hold a frame from two publishes ago.
    #[inline]
    pub fn publish(
        &mut self,
        zone_active: &ZoneActiveMask,
        render_frame: u64,
        fill: impl FnOnce(&mut [f16]),
    ) {
        let frame = self.inner.input();
        fill(&mut frame.pixels);
        frame.zone_active.copy_from(zone_active);
        frame.frame_id = self.next_id;
        frame.render_frame = render_frame;
        frame.produced_at = Instant::now();
        frame.layout_generation = self.layout_generation;
        frame.overlap = self.overlap.clone();
        self.next_id = self.next_id.wrapping_add(1);
        self.inner.publish();
    }
}

impl PreviewConsumer {
    /// Advance to the newest published frame if one exists.
    ///
    /// Unlike the output consumer, a `false` here does **not** mean "resend": the
    /// preview has nothing new to draw, but it may still need to emit, because the
    /// flat parameters it applies are read live and move without a re-render.
    #[inline]
    pub fn advance(&mut self) -> bool {
        self.inner.advance()
    }

    /// The frame currently held. Stable until the next `advance` returns `true`.
    #[inline]
    pub fn current(&self) -> &PreviewFrame {
        self.inner.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_across_a_word_boundary() {
        // 130 zones spans three u64 words, so this exercises the slot/64 indexing.
        let mut m = ZoneActiveMask::new(130);
        assert_eq!(m.words().len(), 3);
        for &slot in &[0usize, 63, 64, 65, 129] {
            assert!(!m.is_active(slot));
            m.set(slot, true);
            assert!(m.is_active(slot), "slot {slot} should be set");
        }
        // A neighbour that was never set stays clear (no accidental spill).
        assert!(!m.is_active(1));
        assert!(!m.is_active(128));

        m.set(64, false);
        assert!(!m.is_active(64));
        assert!(
            m.is_active(65),
            "clearing one slot must not touch its word-mate"
        );
    }

    #[test]
    fn out_of_range_slots_degrade_instead_of_panicking() {
        // A consumer may hold a mask from another generation; a preview must never
        // take anything down over a stale index.
        let mut m = ZoneActiveMask::new(3);
        assert!(!m.is_active(3));
        assert!(!m.is_active(9_999));
        m.set(9_999, true); // ignored, no panic
        assert!(!m.is_active(9_999));
    }

    #[test]
    fn update_from_reflects_is_active_not_contributes_pixels() {
        // Zone 0 soloed, zone 1 up-but-silenced. is_active(1) stays true, so its
        // bit must be set even though it contributes no pixels.
        let mut zones = ZoneStates::new(2);
        zones.set_dimmer(0, 1.0);
        zones.set_dimmer(1, 1.0);
        zones.set_solo(0, true);
        assert!(zones.is_active(1) && !zones.contributes_pixels(1));

        let mut m = ZoneActiveMask::new(2);
        m.update_from(&zones);
        assert!(m.is_active(0));
        assert!(
            m.is_active(1),
            "silenced-by-another-solo zone stays active in the mask"
        );

        // Bring zone 1's dimmer down: now it is genuinely inactive.
        zones.set_dimmer(1, 0.0);
        m.update_from(&zones);
        assert!(!m.is_active(1));
    }

    #[test]
    fn copy_from_overwrites_without_allocating_and_clears_extra_words() {
        let mut src = ZoneActiveMask::new(3);
        src.set(0, true);
        src.set(2, true);
        let mut dst = ZoneActiveMask::new(3);
        dst.set(1, true); // a leftover from a previous frame
        dst.copy_from(&src);
        assert!(dst.is_active(0));
        assert!(!dst.is_active(1), "previous frame's bit must be cleared");
        assert!(dst.is_active(2));
    }
}

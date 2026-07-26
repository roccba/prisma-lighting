//! Output router: turns one canonical master pixel buffer into per-universe
//! Art-Net packets, driven by a compiled [`OutputPlan`].
//!
//! The router owns the per-universe rolling sequence counters and two reusable
//! buffers, so producing a frame's worth of packets allocates nothing on the hot
//! path (CLAUDE.md §3.3). It encodes one universe at a time: the caller sends
//! each packet before asking for the next, which is why single scratch buffers
//! suffice.
//!
//! # Generation guard (the point of the plan/buffer contract)
//!
//! Every [`encode_universe`](OutputRouter::encode_universe) is handed the layout
//! generation the master buffer was produced against. It is checked against the
//! plan's embedded generation **before any bytes are written**, and a mismatch
//! returns [`GenerationMismatch`] instead of encoding. Component offsets from a
//! plan compiled against a different layout would address the wrong pixels (or run
//! out of range); refusing loud here is exactly what [`LayoutGeneration`] is for —
//! far better than discovering it while live-editing a patch (M7). See
//! [`crate::plan`].
//!
//! Art-Net only for M1/M2. sACN/DDP will generalize the encode step behind a
//! protocol trait without changing this per-universe iteration.

use crate::packet::artnet::ArtDmx;
use crate::plan::{FlatParams, OutputPlan};
use crate::universe::Universe;
use prisma_core::OverlapSnapshot;
use prisma_patch::LayoutGeneration;

/// The plan and the master buffer were built against different layout
/// generations. Encoding is refused; the caller must not emit output until the
/// plan and producer agree (rebuild whichever is stale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationMismatch {
    /// Generation the plan was compiled against.
    pub plan: LayoutGeneration,
    /// Generation the master buffer was produced against.
    pub master: LayoutGeneration,
}

impl std::fmt::Display for GenerationMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "output plan/layout generation mismatch: plan was compiled for \
             generation {} but the master buffer is generation {} — refusing to \
             emit with stale offsets",
            self.plan.get(),
            self.master.get()
        )
    }
}

impl std::error::Error for GenerationMismatch {}

/// Encodes a canonical master buffer into Art-Net `ArtDmx` packets, one universe
/// at a time, per a compiled [`OutputPlan`].
pub struct OutputRouter {
    plan: OutputPlan,
    /// Next Art-Net sequence number per universe. Art-Net uses `1..=255` with
    /// `0` meaning "sequencing disabled"; we roll `255 -> 1` and never emit `0`.
    sequences: Vec<u8>,
    /// Reusable channel-data buffer.
    data: Vec<u8>,
    /// Reusable packet buffer, grown once to a full packet and then reused.
    scratch: Vec<u8>,
}

/// Everything the output stage needs for one frame, beyond the compiled plan.
///
/// `overlap` is the classification the master buffer was **rendered under** — it
/// travels with the frame and is never re-read from the live counter, which is
/// what makes the "each pixel dimmed in exactly one stage" invariant structural
/// rather than a matter of discipline (§5.2.1). `flat` is the opposite: read live,
/// once per output frame, so a fader reaches the wire without a re-render.
pub struct OutputFrame<'a> {
    pub master: &'a [half::f16],
    pub generation: LayoutGeneration,
    pub overlap: Option<&'a OverlapSnapshot>,
    pub flat: &'a FlatParams,
    /// The editor's Highlight override (§7 / S9), or `None` when nothing is
    /// highlighted. Applied last, over the fully scaled value: a highlighted pixel's
    /// channels emit white (or dark, on the blink's off phase) regardless of content,
    /// so the operator can identify it on the rig.
    pub highlight: Option<&'a crate::highlight::HighlightFrame<'a>>,
}

impl OutputRouter {
    /// Build a router from a compiled output plan.
    pub fn from_plan(plan: OutputPlan) -> Self {
        let count = plan.universe_count();
        Self {
            plan,
            sequences: vec![1; count],
            data: Vec::with_capacity(crate::universe::CHANNELS_PER_UNIVERSE),
            scratch: Vec::with_capacity(
                crate::packet::artnet::ARTDMX_HEADER_LEN + crate::universe::CHANNELS_PER_UNIVERSE,
            ),
        }
    }

    /// Build a router from M1-style contiguous [`Universe`] maps (the degenerate,
    /// identity, no-gamma plan with generation [`LayoutGeneration::NONE`]). Kept
    /// so M1's tests keep driving the real router.
    pub fn new(universes: Vec<Universe>) -> Self {
        Self::from_plan(OutputPlan::from_universes(universes))
    }

    /// The layout generation this router's plan was compiled against.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.plan.generation()
    }

    /// Number of universes this router emits per frame.
    #[inline]
    pub fn universe_count(&self) -> usize {
        self.plan.universe_count()
    }

    /// The Art-Net port addresses of the universes, in plan order (one transport
    /// per universe, wired in this order).
    pub fn port_addresses(&self) -> Vec<u16> {
        self.plan.port_addresses()
    }

    /// Encode universe `index` from `master` and return the encoded bytes,
    /// advancing that universe's sequence counter.
    ///
    /// `master_generation` is the layout generation the buffer was produced
    /// against; it must equal [`generation`](Self::generation) or this returns
    /// [`GenerationMismatch`] without touching any buffer or sequence counter.
    ///
    /// The returned slice borrows the router until the caller is done with it
    /// (typically: hand it to the socket, then encode the next universe).
    ///
    /// # Panics
    /// Panics if `index >= universe_count()`.
    pub fn encode_universe(
        &mut self,
        index: usize,
        master: &[half::f16],
        master_generation: LayoutGeneration,
    ) -> Result<&[u8], GenerationMismatch> {
        let flat = FlatParams::identity(0);
        self.encode_universe_with(
            index,
            &OutputFrame {
                master,
                generation: master_generation,
                overlap: None,
                flat: &flat,
                highlight: None,
            },
        )
    }

    /// Encode universe `index` applying the frame's flat parameters (§5.2 step 6).
    ///
    /// This is the full output stage: per-zone dimmer/colour for the components the
    /// frame's snapshot marks as taking the shortcut, then master for everything,
    /// then gamma, then the wire. See [`OutputFrame`].
    pub fn encode_universe_with(
        &mut self,
        index: usize,
        frame: &OutputFrame<'_>,
    ) -> Result<&[u8], GenerationMismatch> {
        let plan_generation = self.plan.generation();
        if plan_generation != frame.generation {
            return Err(GenerationMismatch {
                plan: plan_generation,
                master: frame.generation,
            });
        }

        let sequence = self.sequences[index];
        self.sequences[index] = next_sequence(sequence);

        let port_address = self.plan.port_addresses()[index];
        self.plan.encode_into(
            index,
            frame.master,
            frame.overlap,
            frame.flat,
            frame.highlight,
            &mut self.data,
        );
        ArtDmx {
            sequence,
            physical: 0,
            port_address,
            data: &self.data,
        }
        .write_to(&mut self.scratch);

        Ok(&self.scratch)
    }
}

/// Advance an Art-Net sequence number, wrapping `255 -> 1` and skipping `0`
/// (which the spec reserves for "sequencing disabled").
#[inline]
fn next_sequence(current: u8) -> u8 {
    if current == 255 {
        1
    } else {
        current + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::artnet::parse_artdmx;

    // M1's degenerate path uses generation NONE; the buffer is likewise NONE.
    const NONE: LayoutGeneration = LayoutGeneration::NONE;

    /// Old u8 "wire-equivalent" values → normalized linear f16 master samples.
    /// An identity-gamma channel round-trips `norm(&[v])` back to exactly `v`, so
    /// these wire assertions are unchanged from the u8 buffer.
    fn norm(vals: &[u8]) -> Vec<half::f16> {
        vals.iter()
            .map(|&v| half::f16::from_f32(v as f32 / 255.0))
            .collect()
    }

    #[test]
    fn routes_two_universes_from_one_master_buffer() {
        // 6 bytes: universe 1 gets the first 3, universe 2 the next 3.
        let master = norm(&[10, 11, 12, 20, 21, 22]);
        let mut router = OutputRouter::new(vec![Universe::new(1, 0, 3), Universe::new(2, 3, 3)]);
        assert_eq!(router.universe_count(), 2);

        let pkt0 = router.encode_universe(0, &master, NONE).unwrap().to_vec();
        let p0 = parse_artdmx(&pkt0).unwrap();
        assert_eq!(p0.port_address, 1);
        assert_eq!(p0.data, &[10, 11, 12, 0]); // odd length padded to even

        let pkt1 = router.encode_universe(1, &master, NONE).unwrap().to_vec();
        let p1 = parse_artdmx(&pkt1).unwrap();
        assert_eq!(p1.port_address, 2);
        assert_eq!(p1.data, &[20, 21, 22, 0]);
    }

    #[test]
    fn sequence_advances_per_universe_and_wraps() {
        let master = norm(&[0, 0, 0, 0]);
        let mut router = OutputRouter::new(vec![Universe::new(7, 0, 2)]);

        // First three packets carry sequences 1, 2, 3.
        for expected in 1u8..=3 {
            let pkt = router.encode_universe(0, &master, NONE).unwrap().to_vec();
            assert_eq!(parse_artdmx(&pkt).unwrap().sequence, expected);
        }
    }

    #[test]
    fn sequence_wraps_255_to_1_skipping_zero() {
        assert_eq!(next_sequence(254), 255);
        assert_eq!(next_sequence(255), 1);
    }

    #[test]
    fn range_past_end_of_master_is_zero_padded_not_panicking() {
        // Universe claims 4 channels but master only has 2 bytes for it.
        let master = norm(&[5, 6]);
        let mut router = OutputRouter::new(vec![Universe::new(1, 0, 4)]);
        let pkt = router.encode_universe(0, &master, NONE).unwrap().to_vec();
        let p = parse_artdmx(&pkt).unwrap();
        assert_eq!(p.data, &[5, 6, 0, 0]);
    }

    #[test]
    fn offset_entirely_past_end_yields_minimum_padded_packet() {
        let master = norm(&[1, 2]);
        let mut router = OutputRouter::new(vec![Universe::new(1, 100, 3)]);
        let pkt = router.encode_universe(0, &master, NONE).unwrap().to_vec();
        let p = parse_artdmx(&pkt).unwrap();
        // Declared 3 channels, all zero-filled, padded to even -> 4 zero bytes.
        assert_eq!(p.data, &[0, 0, 0, 0]);
    }

    #[test]
    fn generation_mismatch_refuses_to_encode() {
        use prisma_patch::{
            ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
        };
        use uuid::Uuid;

        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![Fixture {
                id: Uuid::from_u128(1),
                name: "f".into(),
                profile: FixtureProfile {
                    name: "p".into(),
                    components: ComponentSet::Rgb,
                    gamma: GammaCalibration::default(),
                },
                pixels: vec![Pixel {
                    index: 0,
                    position: glam::Vec3::ZERO,
                    universe: 1,
                    start_channel: 1,
                    color_order: ColorOrder::Rgb,
                }],
                zone: Uuid::nil(),
            }],
            zones: vec![],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();
        let mut router = OutputRouter::from_plan(plan);

        let master = norm(&[1, 2, 3]);
        // Correct generation encodes.
        assert!(router
            .encode_universe(0, &master, index.generation())
            .is_ok());

        // A different (stale) generation is refused, and the sequence counter is
        // not advanced by the failed call.
        let err = router
            .encode_universe(0, &master, LayoutGeneration::NONE)
            .unwrap_err();
        assert_eq!(err.plan, index.generation());
        assert_eq!(err.master, LayoutGeneration::NONE);
    }
}

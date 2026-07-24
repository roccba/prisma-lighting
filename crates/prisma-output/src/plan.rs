//! Compiled output plan: the bridge from a [`Patch`] to the wire.
//!
//! The master pixel buffer is **pixel-domain canonical** (linear-light RGB/RGBW,
//! one fixed slot per pixel — CLAUDE.md §5.2, M2 pivot). Turning it into DMX
//! means three things, all resolved here **once** at compile time so the hot
//! encode path just reads bytes:
//!
//! 1. **Color order** — each fixture's wire order is baked into *which* canonical
//!    component a given DMX channel pulls. A permutation of source offsets, so
//!    color order costs nothing per frame.
//! 2. **Gamma / calibration** — a per-fixture gamma LUT applied at encode time,
//!    which is also where the linear-light `f16` sample is quantized to the 8-bit
//!    wire — the one and only round in the pipeline (gamma acts on the live,
//!    per-frame value, so it cannot be precomputed). Identical calibrations share
//!    one interned LUT.
//! 3. **Slice into universes** — each pixel's channels land in its universe at
//!    its `start_channel`.
//!
//! # Layout generation is embedded and checked
//!
//! A plan is compiled against one [`PatchIndex`] and carries its
//! [`LayoutGeneration`]. The master buffer is produced against some layout too
//! (the frame carries its generation). If a patch is recompiled and a stale plan
//! keeps running against a new buffer (or vice versa), the byte offsets no longer
//! mean the same pixels — the exact bug [`LayoutGeneration`] exists to catch. The
//! router verifies plan generation == master generation on every encode and
//! refuses to emit on a mismatch (see [`crate::router`]). Failing loud here beats
//! painting wrong pixels or writing out of range while live-editing a patch (M7).

use half::f16;
use prisma_core::OverlapSnapshot;
use prisma_patch::{LayoutGeneration, Patch, PatchIndex};
use std::collections::HashMap;

/// Sentinel source offset for a DMX channel that maps to no pixel (a gap in the
/// universe). Encodes as `0`.
const SRC_NONE: u32 = u32::MAX;

/// Sentinel LUT id meaning "identity" (pass the value through, no gamma).
const LUT_IDENTITY: u16 = u16::MAX;

/// Per-DMX-channel instruction: where to pull the source byte from the canonical
/// master buffer, and which gamma LUT to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelMap {
    /// Component (f16 element) offset into the canonical master buffer, or
    /// [`SRC_NONE`].
    src: u32,
    /// Index into [`OutputPlan::luts`], or [`LUT_IDENTITY`].
    lut: u16,
    /// Canonical component index within its pixel: `0 = R, 1 = G, 2 = B, 3 = W`.
    /// Resolved here because compiling the colour order already computes it. The
    /// output stage indexes the master/zone colour filter with it (§5.2 step 6).
    channel: u8,
}

impl ChannelMap {
    const GAP: ChannelMap = ChannelMap {
        src: SRC_NONE,
        lut: LUT_IDENTITY,
        channel: 0,
    };
}

/// One universe's worth of channel instructions.
#[derive(Debug, Clone)]
struct UniversePlan {
    /// Art-Net 15-bit port address (generalizes to a protocol-tagged address
    /// when sACN/DDP join).
    port_address: u16,
    /// `channel[c]` is the instruction for DMX channel `c` (0-based). Its length
    /// is the number of channels this universe emits.
    channel: Box<[ChannelMap]>,
}

/// Resolution of a gamma/calibration LUT. The index is the linear input value
/// quantized to this many steps ([`quantize_linear`]); each entry is the 8-bit
/// wire code. 4096 (12-bit) keeps the dark end — where a gamma > 1 expands few
/// linear codes into many wire codes — from banding, far finer than the 8-bit
/// output it feeds. The f16 master carries more precision than this; 4096 is past
/// the point where the 8-bit result stops changing.
const GAMMA_LUT_SIZE: usize = 4096;

/// A gamma/calibration lookup table: linear-input (quantized) → 8-bit wire.
type GammaLut = [u8; GAMMA_LUT_SIZE];

/// Round a clamped linear value in `[0, 1]` to an 8-bit wire code — the single
/// quantization to the wire, for an identity-gamma (uncalibrated) channel.
#[inline]
fn linear_to_wire(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Quantize a clamped linear value in `[0, 1]` to a [`GammaLut`] index.
#[inline]
fn quantize_linear(v: f32) -> usize {
    (v.clamp(0.0, 1.0) * (GAMMA_LUT_SIZE - 1) as f32).round() as usize
}

/// Live flat parameters for **one output frame** (CLAUDE.md §5.2 step 6, §1.5).
///
/// The frame's overlap snapshot is frozen at the render boundary; these values are
/// not. They are read once per *output* frame from live state (wait-free parameter
/// reads), which is exactly what makes the shortcut a latency win: a fader move
/// reaches the wire on the next output frame with **no re-render**.
///
/// Each entry is `[d·c_r, d·c_g, d·c_b, d]`, indexed by the canonical channel
/// ([`ChannelMap::channel`]). The dimmer applies to **every** component including
/// a white channel; the colour filter applies only to R/G/B — a colour filter has
/// no meaning for a white emitter, so `W` takes the dimmer alone.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatParams {
    master: [f32; 4],
    zones: Vec<[f32; 4]>,
}

#[inline]
fn scale_of(dimmer: f32, color: [f32; 3]) -> [f32; 4] {
    [
        dimmer * color[0],
        dimmer * color[1],
        dimmer * color[2],
        dimmer,
    ]
}

impl FlatParams {
    /// Neutral parameters for `zone_count` zones: everything at unity, so encoding
    /// is bit-identical to having no flat-parameter stage at all.
    pub fn identity(zone_count: usize) -> Self {
        Self {
            master: [1.0; 4],
            zones: vec![[1.0; 4]; zone_count],
        }
    }

    pub fn set_master(&mut self, dimmer: f32, color: [f32; 3]) {
        self.master = scale_of(dimmer, color);
    }

    /// Set one zone's scale. An out-of-range slot is ignored rather than panicking:
    /// this runs on the output thread, where a mis-sized buffer must degrade, not
    /// take the show down.
    pub fn set_zone(&mut self, slot: usize, dimmer: f32, color: [f32; 3]) {
        if let Some(z) = self.zones.get_mut(slot) {
            *z = scale_of(dimmer, color);
        }
    }

    #[inline]
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    /// A zone's emitted tint `[d·r, d·g, d·b]` — its colour already scaled by its
    /// dimmer. This is what a preview strip swatch should show: it darkens as the
    /// fader drops. Black for an unknown slot (nothing to show). Not used on the
    /// DMX path; exposed for the M6 preview, which reads the same live `FlatParams`.
    #[inline]
    pub fn zone_tint(&self, slot: usize) -> [f32; 3] {
        self.zones
            .get(slot)
            .map_or([0.0; 3], |z| [z[0], z[1], z[2]])
    }

    #[inline]
    fn master_scale(&self, channel: usize) -> f32 {
        self.master[channel.min(3)]
    }

    /// Scale for a zone slot; unity for an unknown slot so a stale snapshot can
    /// never brighten or darken the rig, only fail to attenuate it.
    #[inline]
    fn zone_scale(&self, slot: usize, channel: usize) -> f32 {
        self.zones.get(slot).map_or(1.0, |z| z[channel.min(3)])
    }
}

/// Apply the output stage's flat parameters to one linear-light component
/// (CLAUDE.md §5.2 step 6): per-zone scale where the frame's snapshot allows the
/// shortcut, then master scale unconditionally. Stays in **linear light** — the
/// transfer curve (fixture gamma for the wire, sRGB for the preview) is the
/// caller's, and always comes after.
///
/// # Every consumer of the master buffer MUST call this — do not duplicate it
///
/// Both the wire encoder ([`OutputPlan::encode_into`]) and the M6 preview
/// (`prisma-preview`) apply this stage, and they must apply *exactly* the same
/// one. The reason is not tidiness:
///
/// - The master buffer is the state **before** flat parameters. A consumer that
///   skips this stage shows neither the master fader nor a zone fader on the
///   pixels that take the §1.5 shortcut — while overlapped pixels, already dimmed
///   in render (step 4), *would* move. The §5.2.1 asymmetry then reads as a bug in
///   the engine rather than the design it is.
/// - A consumer that reimplements it drifts. The zone scale is conditional on
///   `zone_at_output`, in *this* precise way, or the "exactly one stage" invariant
///   breaks: dim twice (one-frame darkening) or zero times (one-frame flash).
///
/// So: one function, called from both. If a third consumer of the master buffer
/// appears, it calls this too.
///
/// `src` is the component's offset in the canonical master buffer — the same
/// coordinate [`OverlapSnapshot::zone_at_output`] is indexed by. `channel` is the
/// canonical component index (`0 = R, 1 = G, 2 = B, 3 = W`).
#[inline]
pub fn scale_component(
    lin: f32,
    src: usize,
    channel: usize,
    overlap: Option<&OverlapSnapshot>,
    flat: &FlatParams,
) -> f32 {
    // Per-zone first, and only for the components the frame's snapshot marks as
    // taking the shortcut: everything else was already dimmed in render (step 4),
    // and dimming it again here is precisely the double-dim the "exactly one
    // stage" invariant forbids.
    let mut lin = lin;
    if let Some(snapshot) = overlap {
        if let Some(slot) = snapshot.zone_at_output(src) {
            lin *= flat.zone_scale(slot as usize, channel);
        }
    }
    // Master applies to every pixel, unconditionally.
    lin * flat.master_scale(channel)
}

/// A patch compiled into per-universe, per-channel wire instructions, tagged with
/// the [`LayoutGeneration`] it was built against.
#[derive(Debug, Clone)]
pub struct OutputPlan {
    generation: LayoutGeneration,
    universes: Vec<UniversePlan>,
    /// Interned gamma LUTs, referenced by [`ChannelMap::lut`].
    luts: Vec<GammaLut>,
}

/// Errors from compiling a patch into an [`OutputPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A pixel referenced a fixture/pixel the index could not resolve. Should not
    /// happen for a validated patch compiled against its own index; surfaced so
    /// a mismatched `(patch, index)` pair fails loudly rather than silently.
    UnresolvedPixel { fixture_pixel: u32 },
    /// A pixel's DMX channel span runs past channel 512.
    ChannelOverflow { universe: u16, channel: u32 },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::UnresolvedPixel { fixture_pixel } => {
                write!(f, "pixel {fixture_pixel} did not resolve against the index")
            }
            PlanError::ChannelOverflow { universe, channel } => {
                write!(f, "universe {universe} channel {channel} exceeds 512")
            }
        }
    }
}

impl std::error::Error for PlanError {}

impl OutputPlan {
    /// The layout generation this plan was compiled against. The router compares
    /// it to the master buffer's generation on every encode.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// Number of universes this plan emits.
    #[inline]
    pub fn universe_count(&self) -> usize {
        self.universes.len()
    }

    /// The Art-Net port addresses of the universes, in plan order. The caller
    /// wires one transport per universe in this same order.
    pub fn port_addresses(&self) -> Vec<u16> {
        self.universes.iter().map(|u| u.port_address).collect()
    }

    /// Encode universe `index` from the canonical `master` buffer into `out`
    /// (resized to the universe's channel count): read each channel's source
    /// component, quantize it to the 8-bit wire (identity or gamma LUT), zero-fill
    /// gaps and out-of-range reads.
    ///
    /// Bounds-safe by construction: a source offset past the end of `master`
    /// yields `0` rather than panicking (matches M1's field-safety stance). A
    /// same-generation buffer always has the expected length, so this only
    /// guards genuine misconfiguration.
    pub(crate) fn encode_into(
        &self,
        index: usize,
        master: &[f16],
        overlap: Option<&OverlapSnapshot>,
        flat: &FlatParams,
        out: &mut Vec<u8>,
    ) {
        let plan = &self.universes[index];
        out.clear();
        out.reserve(plan.channel.len());
        for map in plan.channel.iter() {
            if map.src == SRC_NONE {
                out.push(0);
                continue;
            }
            // Linear-light sample (0.0 for an out-of-range read — bounds-safe like
            // M1's field stance).
            let raw = master
                .get(map.src as usize)
                .map(|h| h.to_f32())
                .unwrap_or(0.0);
            let channel = map.channel as usize;

            // --- Flat parameters, in linear light, BEFORE gamma (§5.2 step 6) ---
            //
            // Shared with the preview by construction — see [`scale_component`].
            let lin = scale_component(raw, map.src as usize, channel, overlap, flat);

            // --- Then, and only then, the single quantization to the wire ---
            //
            // Order matters and is load-bearing: gamma is a non-linear curve, so
            // scaling after it would give a different (and wrong) result. Dimming
            // happens in linear light; the curve is applied to the dimmed value.
            let value = if map.lut == LUT_IDENTITY {
                linear_to_wire(lin)
            } else {
                self.luts[map.lut as usize][quantize_linear(lin)]
            };
            out.push(value);
        }
    }

    /// Compile a validated patch and its index into a wire plan.
    ///
    /// `index` must be the one produced from `patch` ([`Patch::compile`]); the
    /// resulting plan carries `index.generation()`.
    pub fn compile(patch: &Patch, index: &PatchIndex) -> Result<OutputPlan, PlanError> {
        // Group channels by universe, growing each universe's channel vector to
        // fit the highest channel actually used. Deterministic universe order
        // (ascending port address) so transport wiring is stable.
        let mut by_universe: HashMap<u16, Vec<ChannelMap>> = HashMap::new();
        let mut lut_interner = LutInterner::new();

        for fixture in &patch.fixtures {
            let lut = lut_interner.intern(fixture.profile.gamma.gamma);
            for pixel in &fixture.pixels {
                let global = index.global_index(fixture.id, pixel.index).ok_or(
                    PlanError::UnresolvedPixel {
                        fixture_pixel: pixel.index,
                    },
                )?;
                let base = index.pixel_component_offset(global);
                let wire = fixture.profile.wire_order(pixel.color_order);
                let channels = by_universe.entry(pixel.universe).or_default();

                for (k, &canonical) in wire.as_slice().iter().enumerate() {
                    // start_channel is 1-based; DMX channel index is 0-based.
                    let channel = pixel.start_channel as u32 - 1 + k as u32;
                    if channel >= prisma_patch::CHANNELS_PER_UNIVERSE as u32 {
                        return Err(PlanError::ChannelOverflow {
                            universe: pixel.universe,
                            channel,
                        });
                    }
                    let c = channel as usize;
                    if c >= channels.len() {
                        channels.resize(c + 1, ChannelMap::GAP);
                    }
                    channels[c] = ChannelMap {
                        src: base + canonical as u32,
                        lut,
                        channel: canonical,
                    };
                }
            }
        }

        let mut universes: Vec<UniversePlan> = by_universe
            .into_iter()
            .map(|(port_address, channel)| UniversePlan {
                port_address,
                channel: channel.into_boxed_slice(),
            })
            .collect();
        universes.sort_by_key(|u| u.port_address);

        Ok(OutputPlan {
            generation: index.generation(),
            universes,
            luts: lut_interner.into_luts(),
        })
    }

    /// Build a degenerate plan from M1-style contiguous [`Universe`] maps:
    /// identity byte mapping (channel `c` ← `source_offset + c`), no gamma,
    /// generation [`LayoutGeneration::NONE`]. This is exactly M1's direct RGB
    /// path expressed as a plan, so M1's tests keep exercising the same wire
    /// output through the generalized router.
    pub fn from_universes(universes: Vec<crate::universe::Universe>) -> OutputPlan {
        let universes = universes
            .into_iter()
            .map(|u| {
                let channel = (0..u.channel_count)
                    .map(|c| ChannelMap {
                        src: (u.source_offset + c) as u32,
                        lut: LUT_IDENTITY,
                        // A layout-less byte range: we cannot know which channel
                        // carries which colour, so mark it `3` — the slot that
                        // takes the master/zone DIMMER but no colour filter.
                        channel: 3,
                    })
                    .collect();
                UniversePlan {
                    port_address: u.port_address,
                    channel,
                }
            })
            .collect();
        OutputPlan {
            generation: LayoutGeneration::NONE,
            universes,
            luts: Vec::new(),
        }
    }
}

/// Interns gamma LUTs so identical calibrations share one table. Keyed on the
/// exact bit pattern of the gamma exponent (identical params → identical key).
struct LutInterner {
    ids: HashMap<u32, u16>,
    luts: Vec<GammaLut>,
}

impl LutInterner {
    fn new() -> Self {
        Self {
            ids: HashMap::new(),
            luts: Vec::new(),
        }
    }

    /// Return the LUT id for a gamma exponent, building the table on first use.
    /// Gamma `1.0` is identity and maps to [`LUT_IDENTITY`] with no table.
    fn intern(&mut self, gamma: f32) -> u16 {
        if gamma == 1.0 {
            return LUT_IDENTITY;
        }
        let key = gamma.to_bits();
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.luts.len() as u16;
        self.luts.push(build_gamma_lut(gamma));
        self.ids.insert(key, id);
        id
    }

    fn into_luts(self) -> Vec<GammaLut> {
        self.luts
    }
}

/// Build a gamma table mapping a quantized linear input `i/(N-1)` in `[0, 1]` to a
/// gamma-encoded 8-bit wire code: `out = round(255 * (i/(N-1))^gamma)`. Applied at
/// the output stage per fixture profile (CLAUDE.md §5.2 color pipeline).
fn build_gamma_lut(gamma: f32) -> GammaLut {
    let mut lut = [0u8; GAMMA_LUT_SIZE];
    let n = (GAMMA_LUT_SIZE - 1) as f32;
    for (i, slot) in lut.iter_mut().enumerate() {
        let normalized = i as f32 / n;
        *slot = (normalized.powf(gamma) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;
    use prisma_patch::{
        BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Pixel,
        PixelRef, Zone,
    };
    use uuid::Uuid;

    /// Old u8 "wire-equivalent" values → normalized linear f16 master samples.
    /// These tests read in familiar 0..255 terms while feeding the f16 buffer; an
    /// identity-gamma channel round-trips `norm(&[v])` back to exactly `v` on the
    /// wire (verified for all 256 values), so the wire assertions are unchanged.
    fn norm(vals: &[u8]) -> Vec<f16> {
        vals.iter()
            .map(|&v| f16::from_f32(v as f32 / 255.0))
            .collect()
    }

    fn rgb_fixture(id: Uuid, universe: u16, start: u16, order: ColorOrder, gamma: f32) -> Fixture {
        Fixture {
            id,
            name: "f".into(),
            profile: FixtureProfile {
                name: "p".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration { gamma },
            },
            pixels: vec![Pixel {
                index: 0,
                position: glam::Vec3::ZERO,
                universe,
                start_channel: start,
                color_order: order,
            }],
            zone: Uuid::nil(),
        }
    }

    #[test]
    fn compiles_color_order_as_offset_permutation() {
        // One GRB pixel at universe 1, channel 1. Canonical bytes are [R,G,B] at
        // offsets 0,1,2. On the wire GRB means channel 0←G(1), 1←R(0), 2←B(2).
        let fixture = rgb_fixture(Uuid::from_u128(1), 1, 1, ColorOrder::Grb, 1.0);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![fixture],
            zones: vec![],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();

        // Canonical master: R=10, G=20, B=30.
        let master = norm(&[10, 20, 30]);
        let mut out = Vec::new();
        plan.encode_into(0, &master, None, &FlatParams::identity(0), &mut out);
        assert_eq!(out, vec![20, 10, 30], "GRB wire order");
    }

    #[test]
    fn applies_gamma_lut() {
        let fixture = rgb_fixture(Uuid::from_u128(1), 1, 1, ColorOrder::Rgb, 2.0);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![fixture],
            zones: vec![],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();

        // gamma 2.0: 128 -> round(255 * (128/255)^2) = round(255*0.2515) = 64.
        let master = norm(&[128, 0, 255]);
        let mut out = Vec::new();
        plan.encode_into(0, &master, None, &FlatParams::identity(0), &mut out);
        assert_eq!(out[0], 64);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 255); // 255 maps to 255 under any gamma
    }

    #[test]
    fn identical_gamma_shares_one_lut() {
        let f1 = rgb_fixture(Uuid::from_u128(1), 1, 1, ColorOrder::Rgb, 2.2);
        let f2 = rgb_fixture(Uuid::from_u128(2), 1, 4, ColorOrder::Rgb, 2.2);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [2, 1, 1],
            fixtures: vec![f1, f2],
            zones: vec![],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();
        assert_eq!(plan.luts.len(), 1, "same gamma must intern to one LUT");
    }

    #[test]
    fn plan_carries_index_generation() {
        let fixture = rgb_fixture(Uuid::from_u128(1), 1, 1, ColorOrder::Rgb, 1.0);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![fixture],
            zones: vec![Zone {
                id: Uuid::from_u128(9),
                name: "z".into(),
                members: vec![PixelRef {
                    fixture: Uuid::from_u128(1),
                    pixel: 0,
                }],
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [1, 1, 1],
                display_color: prisma_patch::default_display_color(),
                world_transform: None,
            }],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();
        assert_eq!(plan.generation(), index.generation());
    }

    #[test]
    fn gaps_between_pixels_emit_zero() {
        // A pixel at channel 5 (1-based) leaves channels 1..4 as gaps.
        let fixture = rgb_fixture(Uuid::from_u128(1), 1, 5, ColorOrder::Rgb, 1.0);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![fixture],
            zones: vec![],
        };
        let index = patch.compile().unwrap();
        let plan = OutputPlan::compile(&patch, &index).unwrap();
        let master = norm(&[7, 8, 9]);
        let mut out = Vec::new();
        plan.encode_into(0, &master, None, &FlatParams::identity(0), &mut out);
        // channels 1..4 (indices 0..3) are gaps → 0; then R,G,B at 5,6,7.
        assert_eq!(out, vec![0, 0, 0, 0, 7, 8, 9]);
    }

    #[test]
    fn rejects_channel_overflow() {
        // RGB pixel at channel 511 needs 511,512,513 → 513 overflows.
        let fixture = rgb_fixture(Uuid::from_u128(1), 1, 511, ColorOrder::Rgb, 1.0);
        let patch = Patch {
            id: Uuid::from_u128(0),
            name: "t".into(),
            matrix_size: [1, 1, 1],
            fixtures: vec![fixture],
            zones: vec![],
        };
        // Note: Patch::validate also catches this; here we exercise the plan's
        // own guard directly by bypassing validate.
        let index = patch.compile().unwrap_err();
        // validate rejects first, so compile() never reaches the plan. Confirm
        // the model-level guard fired.
        assert!(matches!(
            index,
            prisma_patch::PatchError::ChannelOutOfRange { .. }
        ));
    }

    #[test]
    fn degenerate_plan_matches_m1_contiguous_mapping() {
        use crate::universe::Universe;
        let plan = OutputPlan::from_universes(vec![Universe::new(1, 3, 3)]);
        assert_eq!(plan.generation(), LayoutGeneration::NONE);
        // Identity: channel c ← master[source_offset + c].
        let master = norm(&[0, 0, 0, 40, 50, 60]);
        let mut out = Vec::new();
        plan.encode_into(0, &master, None, &FlatParams::identity(0), &mut out);
        assert_eq!(out, vec![40, 50, 60]);
    }
}

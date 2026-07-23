//! Compiled preview plan: the bridge from the canonical master buffer to the
//! screen (CLAUDE.md M6, §5.2 step 6).
//!
//! This is the third sibling of the two plans that already exist, and it helps to
//! see all three together:
//!
//! | plan | crate | maps |
//! |---|---|---|
//! | `SamplePlan` | `prisma-render` | framebuffer → master buffer |
//! | `OutputPlan` | `prisma-output` | master buffer → DMX wire |
//! | [`PreviewPlan`] | here | master buffer → screen |
//!
//! Each is compiled once against a [`PatchIndex`], carries its
//! [`LayoutGeneration`], and leaves the per-frame path doing nothing but reads.
//!
//! # The preview is not a second renderer
//!
//! It consumes the master buffer the output readback already produced — the
//! "third way" of M6: no second GPU pass, no HD framebuffer over IPC. What
//! travels is pixel-domain (thousands of values), which is what the readback
//! hands over anyway.
//!
//! # Why this applies the flat parameters
//!
//! The master buffer is the state **before** the output stage's scalar dimmer and
//! colour (§5.2 step 6). Drawing it raw would produce a preview that ignores the
//! master fader, ignores a zone fader on every pixel taking the §1.5 shortcut, and
//! yet *does* follow it on overlapped pixels (already dimmed in render, step 4) —
//! showing the §5.2.1 asymmetry as if the engine were broken. So the preview runs
//! the same stage, through the very same function the wire encoder calls:
//! [`prisma_output::plan::scale_component`]. Read its docs before touching this.
//!
//! # Linear in, linear out — the transfer curve is not applied here
//!
//! [`PreviewPlan::sample_into`] stops at **linear light**, exactly where the wire
//! encoder is when it reaches its gamma LUT. That is deliberate and it is the
//! contract that makes preview and wire comparable:
//!
//! - the **wire** then applies the *per-fixture gamma* — LED calibration;
//! - the **preview** then applies *sRGB* — the screen's own transfer.
//!
//! They are different curves on purpose, so preview bytes and wire bytes are not
//! equal by design. Linear light is the one place they must agree, and it is where
//! the tests compare them.

use glam::Vec2;
use half::f16;
use prisma_core::OverlapSnapshot;
use prisma_output::plan::{scale_component, FlatParams};
use prisma_patch::{LayoutGeneration, Patch, PatchIndex};

/// Sentinel in [`PreviewPixel::src`]: this fixture does not drive that canonical
/// channel (e.g. `W` on an RGB fixture). Reads as `0.0`.
pub const NO_COMPONENT: u32 = u32::MAX;

/// Everything the preview needs about one pixel, resolved once at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewPixel {
    /// Component (f16 element) offsets into the canonical master buffer, indexed
    /// by **canonical** channel: `[R, G, B, W]`, or [`NO_COMPONENT`].
    ///
    /// Canonical, not wire: the master buffer is always `[R, G, B(, W)]` and
    /// `ColorOrder` is a wire-side permutation compiled into `OutputPlan`. The
    /// preview must never see it — a screen has no colour order.
    pub src: [u32; 4],
}

impl PreviewPixel {
    /// Whether this pixel drives a white emitter (see
    /// [`PreviewPlan::sample_pixel`] for how `W` reaches the screen).
    #[inline]
    pub fn has_white(&self) -> bool {
        self.src[3] != NO_COMPONENT
    }
}

/// A patch compiled into per-pixel preview instructions, tagged with the
/// [`LayoutGeneration`] it was built against.
///
/// The generation is checked the same way the router checks it: a consumer holding
/// a descriptor from another generation is holding stale geometry, and its pixel
/// positions no longer mean the same pixels. Failing loud beats drawing a rig that
/// silently does not match the one on stage (§4.1 / M2).
#[derive(Debug, Clone)]
pub struct PreviewPlan {
    generation: LayoutGeneration,
    matrix_size: [u32; 3],
    pixels: Box<[PreviewPixel]>,
    /// Parallel to `pixels`; static layout, consumed by the descriptor.
    positions: Box<[Vec2]>,
}

impl PreviewPlan {
    /// Compile a validated patch and its index into a preview plan.
    ///
    /// `index` must be the one produced from `patch` ([`Patch::compile`]) — same
    /// contract as `OutputPlan::compile`; the resulting plan carries
    /// `index.generation()`.
    ///
    /// Infallible: unlike the wire plan there is nothing to reject here (no
    /// universes, no channel ranges). Positions were already validated against
    /// `matrix_size` at `Patch::validate`.
    pub fn compile(patch: &Patch, index: &PatchIndex) -> PreviewPlan {
        let count = index.pixel_count() as usize;
        let mut pixels = Vec::with_capacity(count);
        let mut positions = Vec::with_capacity(count);

        // Walk fixtures/pixels in declaration order — the same walk
        // `PatchIndex::build` uses to assign global indices, so `g` stays in step.
        let mut g: u32 = 0;
        for fixture in &patch.fixtures {
            for pixel in &fixture.pixels {
                let base = index.pixel_component_offset(g);
                let components = index.pixel_component_count(g) as u32;
                let at = |c: u32| {
                    if c < components {
                        base + c
                    } else {
                        NO_COMPONENT
                    }
                };
                pixels.push(PreviewPixel {
                    src: [at(0), at(1), at(2), at(3)],
                });
                positions.push(pixel.position.truncate());
                g += 1;
            }
        }

        PreviewPlan {
            generation: index.generation(),
            matrix_size: patch.matrix_size,
            pixels: pixels.into_boxed_slice(),
            positions: positions.into_boxed_slice(),
        }
    }

    /// The layout generation this plan was compiled against. A frame carries the
    /// generation its master buffer was produced under; they must match.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    #[inline]
    pub fn pixel_count(&self) -> usize {
        self.pixels.len()
    }

    /// The virtual matrix this patch renders into — layout context for the
    /// consumer, which draws pixel positions in this coordinate space.
    #[inline]
    pub fn matrix_size(&self) -> [u32; 3] {
        self.matrix_size
    }

    /// Pixel positions in the virtual matrix, indexed by global pixel index.
    #[inline]
    pub fn positions(&self) -> &[Vec2] {
        &self.positions
    }

    /// One pixel's compiled instruction.
    ///
    /// # Panics
    /// Panics if `g >= pixel_count()`.
    #[inline]
    pub fn pixel(&self, g: usize) -> &PreviewPixel {
        &self.pixels[g]
    }

    /// Sample one pixel as **linear-light** RGB, with the output stage's flat
    /// parameters applied (§5.2 step 6) and no transfer curve.
    ///
    /// # White folds into RGB
    ///
    /// A `W` component is added to all three channels: a white emitter puts white
    /// light into the room, and the preview shows light. It is added **after** its
    /// own scaling, which carries the dimmer but not the colour filter — matching
    /// exactly what `W` gets on the wire (a colour filter is meaningless for a
    /// white emitter, §5.2 step 6). Preview and wire therefore agree about white
    /// too, not just about RGB.
    ///
    /// Values are **not clamped**: an `Add`-blended zone or a folded `W` can exceed
    /// `1.0`, and clamping belongs to the single quantization step, exactly as on
    /// the wire. Out-of-range reads yield `0.0` (M1's bounds-safety stance).
    #[inline]
    pub fn sample_pixel(
        &self,
        g: usize,
        master: &[f16],
        overlap: Option<&OverlapSnapshot>,
        flat: &FlatParams,
    ) -> [f32; 3] {
        let pixel = &self.pixels[g];
        let mut rgb = [0.0f32; 3];
        for (channel, out) in rgb.iter_mut().enumerate() {
            *out = scaled(pixel.src[channel], channel, master, overlap, flat);
        }
        let white = scaled(pixel.src[3], 3, master, overlap, flat);
        if white != 0.0 {
            for c in rgb.iter_mut() {
                *c += white;
            }
        }
        rgb
    }

    /// Sample every pixel into `out` (cleared and refilled, so a reused buffer
    /// does not allocate). Mirrors `OutputPlan::encode_into`'s shape.
    pub fn sample_into(
        &self,
        master: &[f16],
        overlap: Option<&OverlapSnapshot>,
        flat: &FlatParams,
        out: &mut Vec<[f32; 3]>,
    ) {
        out.clear();
        out.reserve(self.pixels.len());
        for g in 0..self.pixels.len() {
            out.push(self.sample_pixel(g, master, overlap, flat));
        }
    }
}

/// Read one canonical component and put it through the shared output-stage scale.
#[inline]
fn scaled(
    src: u32,
    channel: usize,
    master: &[f16],
    overlap: Option<&OverlapSnapshot>,
    flat: &FlatParams,
) -> f32 {
    if src == NO_COMPONENT {
        return 0.0;
    }
    let raw = master.get(src as usize).map(|h| h.to_f32()).unwrap_or(0.0);
    scale_component(raw, src as usize, channel, overlap, flat)
}

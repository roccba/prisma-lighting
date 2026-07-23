//! The preview wire: sRGB encoding + the binary per-frame format (CLAUDE.md M6).
//!
//! # Two messages, two formats
//!
//! A preview consumer needs two things, and they change at very different rates:
//!
//! - the **descriptor** — where the pixels are, what the zones are called — which
//!   changes only when the patch is recompiled. That is [`crate::descriptor`],
//!   sent once (and again on a generation change) as JSON.
//! - the **frame** — the colours right now, 30–44 times a second. That is this
//!   module: a compact binary blob, because JSON at that rate for thousands of
//!   pixels is indefensible.
//!
//! # The transfer function is sRGB — deliberately not the fixture gamma
//!
//! The preview shows *light on a screen*, so its final curve is the screen's:
//! linear → sRGB. The wire's final curve is the per-fixture gamma (LED
//! calibration). These are different on purpose (see [`crate::plan`]); the two
//! agree in **linear light**, which is where [`PreviewPlan::sample_pixel`] stops
//! and where the P0 tests compare them. This module applies sRGB *after* that, as
//! the very last step before the byte — exactly as the wire applies its gamma LUT
//! as the last step before its byte.
//!
//! # Generation guard
//!
//! Every frame carries the [`LayoutGeneration`] it was produced under, and the
//! descriptor carries the one it was built under. A consumer holding a descriptor
//! from another generation is holding stale geometry — its positions no longer
//! mean these pixels — so [`encode_frame`] refuses to encode a frame whose
//! generation does not match the plan, the same fail-loud stance the output router
//! takes.

use prisma_output::plan::FlatParams;

use crate::frame::PreviewFrame;
use crate::plan::PreviewPlan;

/// Wire magic: ASCII `PPV1` (Prisma Preview, format v1). A consumer checks this
/// before trusting a byte it received on a binary channel.
pub const MAGIC: [u8; 4] = *b"PPV1";

/// Format version carried in the header. Bump on any wire-layout change so a
/// consumer can reject a frame it does not understand.
pub const FORMAT_VERSION: u16 = 1;

/// Pixel encoding in the payload. Only sRGB 8-bit RGB exists today; the field is
/// here so a future HDR/10-bit preview does not need a version bump to be told
/// apart.
pub const PIXEL_FORMAT_RGB8_SRGB: u8 = 0;

/// Fixed header size in bytes. The zone block starts here, the pixel block after
/// it. 32 bytes keeps both blocks aligned and leaves room reserved for growth
/// without shifting the payload.
pub const HEADER_LEN: usize = 32;

/// Bytes per zone in the zone block: `[flags, r, g, b]`.
pub const ZONE_ENTRY_LEN: usize = 4;

/// Bit 0 of a zone entry's flags byte: the zone is **active** (`is_active()`, not
/// `contributes_pixels()` — a zone silenced by another's solo is still active, so
/// its strip must stay lit). See [`crate::frame::ZoneActiveMask`].
pub const ZONE_FLAG_ACTIVE: u8 = 0b0000_0001;

/// Why a frame could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewEncodeError {
    /// The frame was produced under a different layout than the plan was compiled
    /// for. Encoding it would ship positions and colours that mean different
    /// pixels; the caller should skip this frame (the layout is mid-change).
    GenerationMismatch { plan: u64, frame: u64 },
}

impl std::fmt::Display for PreviewEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewEncodeError::GenerationMismatch { plan, frame } => write!(
                f,
                "preview frame generation {frame} does not match plan generation {plan}"
            ),
        }
    }
}

impl std::error::Error for PreviewEncodeError {}

/// Encode one linear component to an 8-bit sRGB code — the standard sRGB OETF.
///
/// This is the preview's single quantization to the wire, the mirror of the output
/// stage's gamma round. The value is clamped here (an `Add`-blended zone or a
/// folded white can exceed 1.0, exactly as on the DMX wire) so clamping lives in
/// one place, the last step.
#[inline]
pub fn linear_to_srgb8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Encode one preview frame into `out` (cleared and refilled, so a reused buffer
/// never allocates on the hot path).
///
/// Layout, little-endian:
///
/// ```text
///  off  size  field
///   0    4    magic = "PPV1"
///   4    2    format version
///   6    1    pixel format (0 = RGB8 sRGB)
///   7    1    reserved (0)
///   8    8    frame_id
///  16    8    layout_generation
///  24    4    pixel_count
///  28    2    zone_count
///  30    2    reserved (0)
///  32    …    zone block:  zone_count × [flags:u8, r:u8, g:u8, b:u8]
///   …    …    pixel block: pixel_count × [r:u8, g:u8, b:u8]  (global-index order)
/// ```
///
/// Each pixel is sampled through [`PreviewPlan::sample_pixel`] — the *shared*
/// output-stage scale (master + per-zone flat parameters, chosen by the frame's
/// overlap snapshot) — then sRGB-encoded. Each zone entry carries its active flag
/// and its dimmed colour (`FlatParams` already holds `dimmer × colour`), so the
/// consumer can draw a strip swatch that tracks the fader without extra state.
pub fn encode_frame(
    plan: &PreviewPlan,
    frame: &PreviewFrame,
    flat: &FlatParams,
    out: &mut Vec<u8>,
) -> Result<(), PreviewEncodeError> {
    let plan_gen = plan.generation().get();
    let frame_gen = frame.layout_generation.get();
    if plan_gen != frame_gen {
        return Err(PreviewEncodeError::GenerationMismatch {
            plan: plan_gen,
            frame: frame_gen,
        });
    }

    let pixel_count = plan.pixel_count();
    let zone_count = frame.zone_active.zone_count();

    out.clear();
    out.reserve(HEADER_LEN + zone_count * ZONE_ENTRY_LEN + pixel_count * 3);

    // --- header ---
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.push(PIXEL_FORMAT_RGB8_SRGB);
    out.push(0); // reserved
    out.extend_from_slice(&frame.frame_id.to_le_bytes());
    out.extend_from_slice(&(frame_gen).to_le_bytes());
    out.extend_from_slice(&(pixel_count as u32).to_le_bytes());
    out.extend_from_slice(&(zone_count as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    debug_assert_eq!(out.len(), HEADER_LEN);

    // --- zone block: active flag + dimmed swatch colour ---
    for slot in 0..zone_count {
        let flags = if frame.zone_active.is_active(slot) {
            ZONE_FLAG_ACTIVE
        } else {
            0
        };
        // FlatParams stores [d·r, d·g, d·b, d]; the first three are the emitted
        // tint, which is what a strip swatch should show (it darkens with the
        // fader). A zone the plan does not know about scales to unity — harmless
        // for a swatch.
        let tint = flat.zone_tint(slot);
        out.push(flags);
        out.push(linear_to_srgb8(tint[0]));
        out.push(linear_to_srgb8(tint[1]));
        out.push(linear_to_srgb8(tint[2]));
    }

    // --- pixel block: linear sample -> sRGB, global-index order ---
    let overlap = frame.overlap.as_deref();
    for g in 0..pixel_count {
        let rgb = plan.sample_pixel(g, &frame.pixels, overlap, flat);
        out.push(linear_to_srgb8(rgb[0]));
        out.push(linear_to_srgb8(rgb[1]));
        out.push(linear_to_srgb8(rgb[2]));
    }

    Ok(())
}

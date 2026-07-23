//! The static preview descriptor (CLAUDE.md M6).
//!
//! Everything a consumer needs to *interpret* the per-frame binary stream but that
//! does **not** change frame to frame: where each pixel sits, what the matrix is,
//! and what the zones are called. It is sent once when a consumer connects, and
//! again whenever the patch is recompiled (a new [`LayoutGeneration`]).
//!
//! # Why JSON here and binary for frames
//!
//! The descriptor is sent rarely and is small, so JSON wins on being
//! human-inspectable and trivial to parse in the browser (`JSON.parse`). The
//! frames are sent 30–44 times a second for thousands of pixels, so they are
//! hand-rolled binary ([`crate::encode`]). Splitting the two lets each pick the
//! right trade-off.
//!
//! # The generation ties the two together
//!
//! [`generation`](PreviewDescriptor::generation) is the same
//! [`LayoutGeneration`] the frames carry. A consumer keeps the descriptor's
//! generation and checks each frame against it; a mismatch means "your geometry is
//! stale, re-request the descriptor" — the guard that keeps positions and pixels
//! from ever disagreeing while a patch is being edited (M7).

use serde::{Deserialize, Serialize};

use prisma_patch::{Patch, PatchIndex};

use crate::plan::PreviewPlan;

/// One pixel's drawing position in the virtual matrix. The consumer plots a dot
/// here and fills it with the colour the frame gives for the same global index.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DescriptorPixel {
    pub x: f32,
    pub y: f32,
}

/// One zone's identity and membership — enough to draw a strip and to highlight
/// which pixels it owns. Live state (dimmer, colour, active) is **not** here; it
/// travels with the frames, because it changes every frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DescriptorZone {
    /// Stable zone id (the Patch's `Zone::id`), so a consumer can key UI state to
    /// a zone across descriptor reloads.
    pub id: String,
    pub name: String,
    /// Global pixel indices this zone covers — the same coordinate the frame's
    /// pixel block is ordered by, so a consumer can map a zone to its dots.
    pub members: Vec<u32>,
}

/// The full static descriptor. Serializes to JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewDescriptor {
    /// Wire format version, mirroring [`crate::encode::FORMAT_VERSION`] so a
    /// consumer can reject a descriptor it does not understand.
    pub version: u16,
    /// The [`LayoutGeneration`] this descriptor was built under; every frame must
    /// carry the same one. Serialized as a plain integer.
    pub generation: u64,
    /// The virtual matrix `[x, y, z]` the positions live in.
    pub matrix_size: [u32; 3],
    /// Pixel positions, in global-index order — the same order the frame's pixel
    /// block uses, so `pixels[i]` is coloured by frame pixel `i`.
    pub pixels: Vec<DescriptorPixel>,
    pub zones: Vec<DescriptorZone>,
}

impl PreviewDescriptor {
    /// Build the descriptor from a compiled plan and its patch. `plan`, `patch`
    /// and `index` must all come from the same [`Patch::compile`] — the plan
    /// supplies positions and the generation, the patch supplies zone names/ids,
    /// and the index supplies resolved zone membership.
    pub fn build(plan: &PreviewPlan, patch: &Patch, index: &PatchIndex) -> PreviewDescriptor {
        let pixels = plan
            .positions()
            .iter()
            .map(|p| DescriptorPixel { x: p.x, y: p.y })
            .collect();

        let zones = patch
            .zones
            .iter()
            .enumerate()
            .map(|(slot, zone)| DescriptorZone {
                id: zone.id.to_string(),
                name: zone.name.clone(),
                members: index.zone_members(slot).to_vec(),
            })
            .collect();

        PreviewDescriptor {
            version: crate::encode::FORMAT_VERSION,
            generation: plan.generation().get(),
            matrix_size: plan.matrix_size(),
            pixels,
            zones,
        }
    }

    /// Serialize to the JSON text a consumer receives. Compact (no pretty-print):
    /// it is machine-read, and a large rig's positions should not be inflated by
    /// whitespace.
    pub fn to_json(&self) -> String {
        // Infallible in practice: every field is a plain number/string/array with
        // no map keys that could fail. `expect` documents that rather than
        // bubbling an error a caller cannot act on.
        serde_json::to_string(self).expect("PreviewDescriptor is always JSON-serializable")
    }
}

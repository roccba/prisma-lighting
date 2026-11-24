//! The pixel framebuffer's layout (LIVE-MODEL-SPEC §8.6, "composite in pixel space").
//!
//! The per-zone compositor folds tracks and zones in **pixel space**: one texel per
//! patch pixel, the global pixel index laid out in rows. Each pixel is its own unit, so
//! a group mask, a zone's coverage and the overlap classification are exact per pixel,
//! whatever the matrices (a zone's `mapping_size` coarser than its pixels, or two
//! fixtures at the same spot). Effects still render at their zone's `mapping_size`; the
//! fold reads them through a per-zone pixel → texel table.
//!
//! With this layout the gather to the master buffer is a straight copy:
//! [`SamplePlan::pixel_space`](crate::sample::SamplePlan::pixel_space) samples pixel `g`
//! at its own texel.

/// Texels per row of the pixel framebuffer. 43,520 pixels (the design ceiling) take
/// 170 rows; well inside every backend's texture limits.
pub const ROW: u32 = 256;

/// Where each global pixel sits in the pixel framebuffer: `g` at `(g % width, g /
/// width)`, so the row-major texel index of pixel `g` **is** `g`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelLayout {
    pub width: u32,
    pub height: u32,
    /// Patch pixels laid out (the texels past it, in the last row, are unused).
    pub pixels: u32,
}

impl PixelLayout {
    /// The layout for `pixels` patch pixels (at least one texel).
    pub fn new(pixels: u32) -> PixelLayout {
        let width = pixels.clamp(1, ROW);
        let height = pixels.div_ceil(width).max(1);
        PixelLayout {
            width,
            height,
            pixels,
        }
    }

    /// The layout of a compiled patch.
    pub fn of(index: &prisma_patch::PatchIndex) -> PixelLayout {
        PixelLayout::new(index.pixel_count())
    }

    /// Texels in the framebuffer (`width × height`, ≥ `pixels`).
    pub fn texels(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// The framebuffer size, for `create_render_target`.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixels_texel_index_is_its_global_index() {
        let l = PixelLayout::new(43_520);
        assert_eq!((l.width, l.height), (256, 170));
        assert!(l.texels() >= 43_520);
        let small = PixelLayout::new(8);
        assert_eq!((small.width, small.height), (8, 1));
        assert_eq!(PixelLayout::new(0).size(), (1, 1));
        assert_eq!(PixelLayout::new(257).size(), (256, 2));
    }
}

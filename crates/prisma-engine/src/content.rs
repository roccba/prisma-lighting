//! The render content the engine drives when **no show is loaded**.
//!
//! The render loop calls [`FrameContent::render_into`] every frame; something must
//! fill the framebuffer. Until the Show model (M8) supplies the real per-zone
//! compositor content, the engine renders [`IdleContent`], which clears the target
//! to linear-light black. This is not a placeholder to be torn out — "no show
//! loaded" is a real product state (the app has just started, or the operator
//! cleared everything), and it must produce defined output (black) rather than
//! whatever the texture last held.
//!
//! # Seam for M8
//!
//! M8 replaces the content source with the per-zone layer-stack compositor. The
//! engine's *shape* — it renders whatever `FrameContent` is installed — does not
//! change; only which content is installed does. Injecting content on a running
//! render thread (hot-swap vs. restart) is an M8 concern deliberately not built
//! here.

use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, OverlapSnapshot};
use prisma_render::render_loop::FrameContent;
use prisma_render::Gpu;

/// Fills the render target with linear-light black each frame — the engine's
/// "no show loaded" content (see module docs).
pub struct IdleContent;

impl FrameContent for IdleContent {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        _clock: &ClockSnapshot,
        _zones: &ZoneStates,
        _overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        let size = target.size();
        // The canonical framebuffer is `Rgba16Float`: 4 components × 2 bytes each.
        // All-zero bytes are `f16` 0.0 on every component → linear-light black.
        let bytes_per_row = size.width * 4 * 2;
        let zeros = vec![0u8; (bytes_per_row * size.height) as usize];
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &zeros,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size.height),
            },
            size,
        );
    }
}

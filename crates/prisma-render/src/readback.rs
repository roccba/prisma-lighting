//! GPU → CPU readback of an `Rgba16Float` texture into linear `f16` texels.
//!
//! Shared by the S0 bring-up probe and every render step that verifies pixels.
//! The real render loop (S6) reads back the *master pixel buffer* (a storage
//! buffer, not a texture) with the same map machinery but without ever blocking
//! the render thread; this helper blocks on the map and is for setup/test paths
//! only.

use half::f16;

/// One RGBA pixel of `f16` linear-light components, as read back from the GPU.
pub type Rgba16 = [f16; 4];

/// Bytes per `Rgba16Float` texel: 4 components × 2 bytes.
pub const BYTES_PER_TEXEL: u32 = 8;

/// Round `n` up to the next multiple of `align` (a power of two).
#[inline]
pub fn align_up(n: u32, align: u32) -> u32 {
    (n + align - 1) & !(align - 1)
}

/// Create a `width`×`height` `Rgba16Float` texture usable as a render target, a
/// blend/compositor input (`TEXTURE_BINDING`), and a readback source — the
/// canonical linear-light color format (§5.2). One usage set covers every M5
/// intermediate texture (effect output, compositor accumulator).
pub fn create_render_target(gpu: &crate::Gpu, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Copy `texture` (an `Rgba16Float` render target of `width`×`height`) to CPU and
/// return its texels tightly packed in row-major order. Blocks on the map.
pub fn read_rgba16f(
    gpu: &crate::Gpu,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<Rgba16> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let unpadded_row = width * BYTES_PER_TEXEL;
    // wgpu requires each copied row aligned to 256 bytes.
    let padded_row = align_up(unpadded_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("readback map failed"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll failed");

    let mapped = slice.get_mapped_range().expect("get_mapped_range failed");
    let mut out = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        let start = (row * padded_row) as usize;
        let end = start + unpadded_row as usize;
        let texels: &[Rgba16] = bytemuck::cast_slice(&mapped[start..end]);
        out.extend_from_slice(texels);
    }
    drop(mapped);
    readback.unmap();
    out
}

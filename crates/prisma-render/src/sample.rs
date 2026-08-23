//! The geometric gather: virtual-matrix framebuffer → canonical master pixel
//! buffer (CLAUDE.md §5.2 step 5).
//!
//! A [`SamplePlan`] is compiled once per patch layout — the render-side twin of
//! `prisma-output`'s `OutputPlan`. Where `OutputPlan` says "which master component
//! feeds which DMX channel", `SamplePlan` says "which framebuffer cell feeds which
//! master component". Both carry the [`LayoutGeneration`] they were built against,
//! so a stale plan against a recompiled patch is detectable rather than silently
//! addressing the wrong pixels.
//!
//! The sampling rule itself lives in [`prisma_patch::cell_of`] — nearest cell, no
//! filtering, shared with patch validation so runtime and load-time cannot drift.
//! Read its docs before changing anything here; in particular, several pixels
//! landing in one cell is intentional.

use half::f16;
use prisma_patch::{cell_of, LayoutGeneration, Patch, PatchIndex};

/// Per-pixel gather instruction, mirroring `PixelSample` in `shaders/sample.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PixelSample {
    /// Framebuffer cell X, or **negative** to mark an out-of-range pixel (written
    /// black by the gather). Patch validation rejects those at load; this is the
    /// defensive runtime half.
    pub cell_x: i32,
    pub cell_y: i32,
    /// Component offset into the canonical master buffer.
    pub offset: u32,
    /// Component count (3 = RGB, 4 = RGBW). Component `k` takes texel channel `k`.
    pub components: u32,
}

/// Marks a pixel whose position fell outside the virtual matrix.
const OUT_OF_RANGE: i32 = -1;

/// A patch compiled into per-pixel gather instructions, tagged with the layout
/// generation it was built against.
#[derive(Debug, Clone)]
pub struct SamplePlan {
    samples: Vec<PixelSample>,
    generation: LayoutGeneration,
    master_len: usize,
}

impl SamplePlan {
    /// Compile the gather for `patch`/`index`. `index` must be the one produced
    /// from `patch` ([`Patch::compile`]).
    ///
    /// A validated patch has every position inside the matrix, so `OUT_OF_RANGE`
    /// never appears in practice; it is emitted anyway for a patch built
    /// programmatically without `validate`, so the gather degrades to black rather
    /// than reading a clamped (silently wrong) texel.
    pub fn compile(patch: &Patch, index: &PatchIndex) -> SamplePlan {
        let mut samples = Vec::with_capacity(index.pixel_count() as usize);
        for fixture in &patch.fixtures {
            for pixel in &fixture.pixels {
                let global = index
                    .global_index(fixture.id, pixel.index)
                    .expect("index built from this patch resolves every pixel");
                let (cell_x, cell_y) = match cell_of(pixel.position) {
                    Some(cell) => {
                        let inside = (0..3).all(|axis| {
                            cell[axis] >= 0 && cell[axis] < patch.matrix_size[axis] as i64
                        });
                        if inside {
                            (cell[0] as i32, cell[1] as i32)
                        } else {
                            (OUT_OF_RANGE, 0)
                        }
                    }
                    None => (OUT_OF_RANGE, 0),
                };
                samples.push(PixelSample {
                    cell_x,
                    cell_y,
                    offset: index.pixel_component_offset(global),
                    components: index.pixel_component_count(global) as u32,
                });
            }
        }
        SamplePlan {
            samples,
            generation: index.generation(),
            master_len: index.master_buffer_len(),
        }
    }

    /// The layout generation this plan was compiled against.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// Number of components in the master buffer this plan fills.
    #[inline]
    pub fn master_len(&self) -> usize {
        self.master_len
    }

    /// Number of pixels gathered.
    #[inline]
    pub fn pixel_count(&self) -> usize {
        self.samples.len()
    }

    /// The compiled instructions (for tests/introspection).
    #[inline]
    pub fn samples(&self) -> &[PixelSample] {
        &self.samples
    }
}

/// Runs the gather compute pass. Owns the GPU-side plan and the master/staging
/// buffers, all sized once at construction — the dispatch allocates nothing.
pub struct Sampler {
    /// Compute pipeline + bind-group layout, shared via the [`crate::cache`] across
    /// recompiles (the gather shader is layout-independent).
    cached: std::sync::Arc<crate::cache::CachedCompute>,
    samples_buf: wgpu::Buffer,
    /// `array<f32>`, one entry per master component (see the shader docs for why
    /// the gather writes f32 rather than packed f16).
    master_buf: wgpu::Buffer,
    staging: wgpu::Buffer,
    pixel_count: u32,
    master_len: usize,
    generation: LayoutGeneration,
}

impl Sampler {
    /// Build the gather for a compiled plan, uploading the per-pixel instructions.
    pub fn new(gpu: &crate::Gpu, plan: &SamplePlan) -> Sampler {
        let device = &gpu.device;

        // Instructions are immutable for the life of the layout: upload once. A
        // zero-length buffer is invalid, so an empty plan gets one zeroed entry
        // (whose `components == 0` makes the shader write nothing).
        let mut samples_bytes: Vec<u8> = bytemuck::cast_slice(plan.samples()).to_vec();
        if samples_bytes.is_empty() {
            samples_bytes.resize(std::mem::size_of::<PixelSample>(), 0);
        }
        let samples_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sample-plan"),
            size: samples_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        {
            let mut view = samples_buf
                .slice(..)
                .get_mapped_range_mut()
                .expect("freshly mapped buffer");
            view.copy_from_slice(&samples_bytes);
        }
        samples_buf.unmap();

        let master_bytes = (plan.master_len() * std::mem::size_of::<f32>()).max(4) as u64;
        let master_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("master-f32"),
            size: master_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("master-staging"),
            size: master_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // The gather pipeline + layout depend only on the shader, so they are cached and
        // reused across recompiles (M8 live-apply optimisation). The buffers above are
        // per-layout and stay per-instance.
        let cached = gpu.cache.compute("sample", || {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("sample"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../../../shaders/sample.wgsl").into(),
                ),
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("sample-bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        storage_entry(1, true),
                        storage_entry(2, false),
                    ],
                });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sample-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("sample"),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            crate::cache::CachedCompute {
                pipeline,
                bind_group_layout,
            }
        });

        Sampler {
            cached,
            samples_buf,
            master_buf,
            staging,
            pixel_count: plan.pixel_count() as u32,
            master_len: plan.master_len(),
            generation: plan.generation(),
        }
    }

    /// The layout generation this sampler's plan was compiled against — stamp it
    /// onto the frame the master buffer is published in.
    #[inline]
    pub fn generation(&self) -> LayoutGeneration {
        self.generation
    }

    /// Gather `framebuffer` into the master buffer and copy the result into
    /// `destination` (a readback-ring slot), in one submission.
    ///
    /// Safe against the next frame overwriting the master buffer: the copy is
    /// recorded before the following frame's compute pass, and the GPU executes
    /// submissions in order.
    pub fn gather_into(
        &self,
        gpu: &crate::Gpu,
        framebuffer: &wgpu::Texture,
        destination: &wgpu::Buffer,
    ) {
        self.dispatch(gpu, framebuffer, destination);
    }

    /// Gather `framebuffer` (the composited virtual matrix) into the master buffer
    /// and into this sampler's own staging buffer, for the blocking
    /// [`read_master_f16`](Self::read_master_f16) helper. Tests and setup only —
    /// the render loop uses [`gather_into`](Self::gather_into) with a ring slot.
    pub fn gather(&self, gpu: &crate::Gpu, framebuffer: &wgpu::Texture) {
        self.dispatch(gpu, framebuffer, &self.staging);
    }

    /// Record the compute dispatch plus the copy into `destination`, and submit.
    fn dispatch(&self, gpu: &crate::Gpu, framebuffer: &wgpu::Texture, destination: &wgpu::Buffer) {
        let view = framebuffer.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sample-bg"),
            layout: &self.cached.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.samples_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.master_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sample-enc"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sample-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.cached.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            // 64 threads per workgroup, matching @workgroup_size in the shader.
            pass.dispatch_workgroups(self.pixel_count.div_ceil(64).max(1), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.master_buf, 0, destination, 0, destination.size());
        gpu.queue.submit([encoder.finish()]);
    }

    /// Read the gathered master buffer back as canonical linear-light `f16`.
    ///
    /// **Blocks on the GPU map** — this is the S3/test path. The real render loop
    /// (S6) maps asynchronously and never stalls the render thread (§5.2 step 5),
    /// fusing this f32→f16 conversion into the copy that fills `Frame::pixels`.
    pub fn read_master_f16(&self, gpu: &crate::Gpu) -> Vec<f16> {
        let slice = self.staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("master map failed"));
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll failed");

        let mapped = slice.get_mapped_range().expect("get_mapped_range failed");
        let floats: &[f32] = bytemuck::cast_slice(&mapped);
        let out = floats[..self.master_len]
            .iter()
            .map(|&v| f16::from_f32(v))
            .collect();
        drop(mapped);
        self.staging.unmap();
        out
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

//! GPU bring-up: adapter selection and device/queue creation (CLAUDE.md §2).
//!
//! M5 renders **offscreen** — effects and the compositor draw into textures that
//! are read back to the canonical master buffer (§5.2). There is no swapchain and
//! no window here; the Tauri surface is a separate concern (§3.2). So the device
//! is created with `compatible_surface: None` and never needs a presentable
//! format.
//!
//! # Two adapters, on purpose
//!
//! The dev/CI box exposes both a real Vulkan GPU (`radeon`) and **lavapipe**, a
//! CPU Vulkan rasterizer (`libvulkan_lvp`). They serve different jobs and the
//! test suite uses both:
//!
//! - [`AdapterPreference::HighPerformance`] — the real GPU. The only adapter whose
//!   timings mean anything against the §1.5 latency budget; used for the latency
//!   baseline (M5 step S7).
//! - [`AdapterPreference::Software`] — lavapipe. **Deterministic bit-for-bit**
//!   across machines (no vendor-driver rounding), which makes it the better target
//!   for golden-pixel correctness tests than any single vendor GPU. Also the
//!   guaranteed-available fallback when no discrete GPU is present.

use std::fmt;

/// Which adapter to bring up. See the module docs for why both exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterPreference {
    /// Prefer a real hardware GPU (discrete/integrated). Used for latency/jitter
    /// measurement, which is meaningless on a CPU rasterizer.
    HighPerformance,
    /// Force a CPU (software) Vulkan adapter — lavapipe here. Deterministic across
    /// machines; the correctness-test and no-GPU fallback path.
    Software,
}

/// A brought-up GPU: the wgpu instance, the chosen adapter, and its logical
/// device + queue. Held for the life of the render engine.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    /// Pipelines memoised across recompiles (M8 live-apply optimisation). The `Gpu`
    /// outlives every recompile and is dropped only at shutdown, so a cached pipeline
    /// never outlives its device. See [`crate::cache`].
    pub cache: crate::cache::PipelineCache,
}

/// Why bring-up failed. Kept coarse; the caller decides whether to fall back to
/// [`AdapterPreference::Software`] or surface the error.
#[derive(Debug)]
pub enum GpuError {
    /// No adapter matched the requested preference (e.g. `Software` asked for but
    /// no CPU Vulkan adapter is installed).
    NoAdapter(AdapterPreference),
    /// An adapter was found but the logical device could not be created.
    DeviceRequest(wgpu::RequestDeviceError),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter(p) => write!(f, "no Vulkan adapter matched preference {p:?}"),
            GpuError::DeviceRequest(e) => write!(f, "failed to create GPU device: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

impl Gpu {
    /// Bring up the GPU for the given preference. Blocks (setup path, never the
    /// render hot loop). Vulkan-only backend: it is the shared path across Linux
    /// (dev) and Windows (target), and the only one lavapipe implements.
    pub fn new(preference: AdapterPreference) -> Result<Gpu, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        let adapter = select_adapter(&instance, preference)?;

        // Downlevel defaults so the same device request succeeds on lavapipe and
        // on the real GPU; M5's passes stay within this envelope (no bindless, no
        // exotic features). Widen deliberately if a later effect needs it.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("prisma-render device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(GpuError::DeviceRequest)?;

        let info = adapter.get_info();
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            info,
            cache: crate::cache::PipelineCache::default(),
        })
    }

    /// Adapter identity (name, backend, device type) — for logging the chosen GPU
    /// and for tests to assert they actually landed on hardware vs. lavapipe.
    #[inline]
    pub fn info(&self) -> &wgpu::AdapterInfo {
        &self.info
    }

    /// Whether the active adapter is a CPU (software) rasterizer. Latency numbers
    /// from a software adapter are not comparable to the §1.5 budget; callers
    /// recording a baseline check this.
    #[inline]
    pub fn is_software(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }
}

/// Pick an adapter for the preference. `HighPerformance` asks wgpu for the best
/// non-fallback adapter; `Software` enumerates and takes a CPU one (lavapipe).
fn select_adapter(
    instance: &wgpu::Instance,
    preference: AdapterPreference,
) -> Result<wgpu::Adapter, GpuError> {
    match preference {
        AdapterPreference::HighPerformance => {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None::<&wgpu::Surface>,
                apply_limit_buckets: false,
            }))
            .map_err(|_| GpuError::NoAdapter(preference))
        }
        AdapterPreference::Software => {
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
                .into_iter()
                .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
                .ok_or(GpuError::NoAdapter(preference))
        }
    }
}

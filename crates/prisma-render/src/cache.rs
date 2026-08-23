//! Pipeline cache reused across recompiles (M8 live-apply optimisation).
//!
//! A GPU render/compute pipeline depends only on its shader source and fixed formats —
//! never on pixel positions, counts or the layout. Recompiling the whole pipeline set on
//! every editor apply was the dominant apply cost (a shader compile + pipeline-state
//! build, once *per zone* for effects plus the blend/place/sample stages). This cache
//! memoises the pipeline + its bind-group layout by a stable key, on the [`Gpu`] — which
//! outlives every recompile and is dropped only at shutdown, so a cached pipeline never
//! outlives its device.
//!
//! Only the expensive, layout-independent object (the pipeline + its layout) is shared.
//! Per-instance resources — uniform buffers, and bind groups that reference specific
//! textures/buffers — are cheap and stay per-instance; callers build their bind groups
//! against the cached `bind_group_layout`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A cached render pipeline and the bind-group layout callers build their (per-instance)
/// bind groups against.
pub struct CachedRender {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

/// A cached compute pipeline and its bind-group layout.
pub struct CachedCompute {
    pub pipeline: wgpu::ComputePipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

/// Memoises pipelines by a stable string key that fully determines the pipeline (an
/// effect's `effect_id`; the fixed stage names `"blend"` / `"place"` / `"sample"`). The
/// build closure runs at most once per key. Guarded by a mutex so the cache is `Sync`
/// (it lives on the shared [`Gpu`]); it is only ever touched on the single-threaded
/// assemble path, never per frame on the render thread.
#[derive(Default)]
pub struct PipelineCache {
    render: Mutex<HashMap<String, Arc<CachedRender>>>,
    compute: Mutex<HashMap<String, Arc<CachedCompute>>>,
}

impl PipelineCache {
    /// The render pipeline for `key`, building it with `build` on the first request and
    /// returning the shared handle on every subsequent one (this recompile or a later).
    pub fn render(&self, key: &str, build: impl FnOnce() -> CachedRender) -> Arc<CachedRender> {
        let mut map = self.render.lock().unwrap();
        if let Some(hit) = map.get(key) {
            return Arc::clone(hit);
        }
        let built = Arc::new(build());
        map.insert(key.to_string(), Arc::clone(&built));
        built
    }

    /// The compute pipeline for `key` (see [`render`](Self::render)).
    pub fn compute(&self, key: &str, build: impl FnOnce() -> CachedCompute) -> Arc<CachedCompute> {
        let mut map = self.compute.lock().unwrap();
        if let Some(hit) = map.get(key) {
            return Arc::clone(hit);
        }
        let built = Arc::new(build());
        map.insert(key.to_string(), Arc::clone(&built));
        built
    }

    /// How many distinct pipelines are cached (render + compute). For tests/telemetry.
    pub fn len(&self) -> usize {
        self.render.lock().unwrap().len() + self.compute.lock().unwrap().len()
    }

    /// Whether the cache holds nothing yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

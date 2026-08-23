//! wgpu pipeline, shaders, effects (CLAUDE.md §7).
//!
//! `prisma-render` owns all GPU work: effect rendering, the zone compositor, the
//! overlap counter, GPU readback into the canonical master buffer, and the render
//! loop that drives them (M5). It depends on `prisma-core` (clock, params, the
//! zone-state model), `prisma-patch` (geometry/index) and `prisma-output` (the
//! frame handoff it publishes into) — never the reverse: `prisma-core` stays
//! headlessly testable with no GPU (§7).
//!
//! # Build status: M5 in progress
//!
//! - **S0:** GPU bring-up + offscreen `Rgba16Float` render + f16 readback,
//!   verified on both the hardware and software (lavapipe) adapters ([`device`],
//!   [`probe`], [`readback`]).
//! - **S1:** the effect ↔ Parameter Tree seam — an [`effect::EffectManifest`]
//!   registers its params into the tree (M3) and fixes the uniform layout the
//!   shader reads; per frame the render packs the live param values into the
//!   uniform buffer. This is the join all future effects stand on ([`effect`],
//!   [`effects`]).
//! - **S2:** the layer/zone [`blend::Compositor`] — blend modes and opacity in
//!   linear light, reused for zone-over-zone compositing in S4 ([`blend`]).
//! - **S3 (this):** the geometric gather ([`sample`]) — virtual-matrix framebuffer
//!   → canonical linear-light f16 master pixel buffer, via each pixel's patch
//!   position (§5.2 step 5).

pub mod blend;
pub mod cache;
pub mod device;
pub mod effect;
pub mod effects;
pub mod live_params;
pub mod overlap;
pub mod place;
pub mod probe;
pub mod readback;
pub mod readback_ring;
pub mod render_loop;
pub mod sample;

pub use blend::{blend_code, create_cell_mask, Compositor, Layer, ZoneComposite};
pub use cache::{CachedCompute, CachedRender, PipelineCache};
pub use device::{AdapterPreference, Gpu, GpuError};
pub use effect::{ColorChannel, EffectInstance, EffectManifest, ParamRole, ParamSpec};
pub use effects::{catalogue, effect_params, manifest_by_id, EffectListing, EffectParam};
pub use live_params::{LiveParams, MasterHandles, StrobeHandles, ZoneHandles};
pub use overlap::{zone_cell_coverage, OverlapCounter};
pub use place::{footprint_from_coverage, Footprint, ZonePlacer};
pub use readback_ring::{ReadbackRing, ReadyMeta, DEFAULT_SLOTS};
pub use render_loop::{
    run_render_loop, FrameContent, RenderLoop, RenderLoopConfig, RenderStats, TickOutcome,
};
pub use sample::{PixelSample, SamplePlan, Sampler};

//! The pipeline cache reuses GPU pipelines across builds (M8 live-apply optimisation).
//!
//! A recompile used to rebuild every pipeline — a shader compile + pipeline-state build,
//! once *per zone* for effects plus the blend/place/sample stages — which was the
//! dominant editor apply cost. Pipelines depend only on their shader + fixed formats, so
//! they are memoised on the `Gpu` (which outlives recompiles). This proves the reuse:
//! building the same effect twice compiles ONE pipeline, and the blend/place singletons
//! compile once and are handed back on a second build (what a recompile does).
//!
//! Isolated in its own test binary so the per-binary `Gpu`'s cache starts empty and the
//! counts are exact (libtest would otherwise run other cache-touching tests in parallel).

use prisma_core::ParameterTree;
use prisma_render::effect::EffectInstance;
use prisma_render::{effects, Compositor, ZonePlacer};

mod common;
use common::software_gpu;

#[test]
fn pipelines_are_cached_and_reused() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    assert!(gpu.cache.is_empty(), "a fresh Gpu cache starts empty");

    // Two zones running the SAME effect compile ONE pipeline (deduped by effect_id).
    // This is exactly what turns N per-zone compiles per recompile into one.
    let _a = EffectInstance::new(gpu, &tree, "/zone/a/layer/0", &effects::solid());
    assert_eq!(gpu.cache.len(), 1);
    let _b = EffectInstance::new(gpu, &tree, "/zone/b/layer/0", &effects::solid());
    assert_eq!(
        gpu.cache.len(),
        1,
        "the same effect_id must reuse the cached pipeline, not compile a second"
    );

    // A different effect adds exactly one more.
    let _c = EffectInstance::new(gpu, &tree, "/zone/c/layer/0", &effects::gradient());
    assert_eq!(gpu.cache.len(), 2);

    // The blend + place singletons each compile once and are reused on a second build
    // (a recompile reconstructs these — they must hit the cache, not recompile).
    let _comp1 = Compositor::new(gpu);
    let _comp2 = Compositor::new(gpu);
    assert_eq!(gpu.cache.len(), 3, "blend compiled once, reused on rebuild");

    let _place1 = ZonePlacer::new(gpu);
    let _place2 = ZonePlacer::new(gpu);
    assert_eq!(gpu.cache.len(), 4, "place compiled once, reused on rebuild");
}

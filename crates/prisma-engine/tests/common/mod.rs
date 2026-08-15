//! Shared GPU bring-up for the engine's GPU-backed tests.
//!
//! Same rationale as `prisma-render/tests/common`: bring the software (lavapipe)
//! adapter up **once per binary** and share a `'static` [`Gpu`] that is never
//! dropped, so parallel tests in one binary do not race the Vulkan loader's ICD
//! scan/teardown. Correctness tests use the software adapter because it is
//! deterministic bit-for-bit across machines (CLAUDE.md §2.2 test note).

#![allow(dead_code)]

use std::sync::OnceLock;

use prisma_render::{AdapterPreference, Gpu};

/// The process-wide software GPU: created once, never dropped, shared by every test
/// in the binary.
pub fn software_gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| {
        Gpu::new(AdapterPreference::Software)
            .expect("software (lavapipe) adapter must be available")
    })
}

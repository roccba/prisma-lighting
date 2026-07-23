//! Shared helpers for the GPU-backed integration tests.
//!
//! # Why the GPU is shared, not created per test
//!
//! Each `tests/*.rs` file is its own binary, and libtest runs the tests inside it
//! on parallel threads. Creating a wgpu [`Gpu`] per test therefore races the
//! Vulkan loader: one thread dropping its `Gpu` calls `dlclose` on the ICD
//! (`loader_unload_scanned_icd`) while another is mid-`Gpu::new`
//! (`loader_get_icd_and_device`) — which SEGVs. It is a loader bug, not ours, but
//! we provoke it by bringing Vulkan up and tearing it down from many threads at
//! once.
//!
//! The fix is to bring each adapter up **once per binary** and share it: a
//! `'static` [`Gpu`] that is never dropped removes both halves of the race — no
//! concurrent create, and no teardown at all while tests run. Bring-up is also the
//! slowest part of these tests, so paying it once makes the suite much faster.
//!
//! `#![allow(dead_code)]`: not every test binary uses every helper (only the S0
//! bring-up test touches the hardware adapter), and each binary compiles this
//! module independently, so an unused helper is expected rather than a smell.

#![allow(dead_code)]

use std::sync::{Mutex, OnceLock};

use prisma_render::{AdapterPreference, Gpu};

/// Serializes Vulkan bring-up across threads in this process. [`OnceLock`] already
/// serializes each adapter's own initialization, but the software and hardware
/// adapters live in separate `OnceLock`s and could otherwise initialize at the
/// same instant — two loads racing in the loader's ICD scan. Held only during
/// bring-up, never on any hot path.
fn bringup_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// The process-wide software (lavapipe) GPU: created once, never dropped, shared
/// by every test in the binary. The correctness tests all run on this adapter
/// because it is deterministic bit-for-bit across machines (CLAUDE.md §2.2 test
/// note).
pub fn software_gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| {
        let _serialize = bringup_lock().lock().unwrap();
        Gpu::new(AdapterPreference::Software)
            .expect("software (lavapipe) adapter must be available")
    })
}

/// The process-wide hardware GPU, or `None` on a box with no discrete/integrated
/// adapter (CI without a GPU). Created once, never dropped. Only the S0 bring-up
/// test needs it; every other test gates on [`software_gpu`].
pub fn hardware_gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let _serialize = bringup_lock().lock().unwrap();
        Gpu::new(AdapterPreference::HighPerformance).ok()
    })
    .as_ref()
}

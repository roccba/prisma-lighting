//! S0 acceptance: GPU bring-up + offscreen render + readback, on both adapters.
//!
//! The correctness assertions run on **lavapipe** (the software adapter), which is
//! deterministic bit-for-bit, so a `renderD128`-less or headless CI box still gates
//! on them. The hardware adapter is exercised for bring-up but its readback is
//! checked with the same values — the render is a compile-time constant, so both
//! adapters must agree exactly.

mod common;

use half::f16;
use prisma_render::probe::render_solid_and_readback;
use prisma_render::Gpu;

/// The probe color, chosen so every component is *exactly* representable in f16
/// (negative powers of two and their sums) — the readback assertion is bit-exact,
/// with no epsilon, on any conformant adapter.
const COLOR: [f32; 4] = [0.5, 0.25, 0.75, 1.0];

fn expected() -> [f16; 4] {
    [
        f16::from_f32(COLOR[0]),
        f16::from_f32(COLOR[1]),
        f16::from_f32(COLOR[2]),
        f16::from_f32(COLOR[3]),
    ]
}

fn check_readback_is_uniform_color(gpu: &Gpu) {
    let (w, h) = (16, 8); // non-square + row not a multiple of 256B → exercises padding
    let pixels = render_solid_and_readback(gpu, w, h, COLOR);
    assert_eq!(pixels.len(), (w * h) as usize, "readback texel count");
    let want = expected();
    for (i, px) in pixels.iter().enumerate() {
        assert_eq!(
            *px,
            want,
            "texel {i} mismatch on {:?}: got {px:?}, want {want:?}",
            gpu.info().name
        );
    }
}

#[test]
fn software_adapter_renders_and_reads_back_exactly() {
    // Lavapipe must always be present in this environment; the shared accessor
    // panics if it is missing, which is the right response — that is a real
    // environment regression, not a skip.
    let gpu = common::software_gpu();
    assert!(
        gpu.is_software(),
        "Software preference must land on a CPU adapter, got {:?}",
        gpu.info()
    );
    check_readback_is_uniform_color(gpu);
}

#[test]
fn hardware_adapter_renders_and_reads_back_exactly() {
    // On a box with no discrete/integrated GPU this legitimately has nothing to
    // run on; skip rather than fail so CI without hardware stays green on the
    // software test above.
    let Some(gpu) = common::hardware_gpu() else {
        eprintln!("skipping hardware adapter test: no hardware adapter present");
        return;
    };
    check_readback_is_uniform_color(gpu);
}

//! M8 step 2 — the zone placement pass (`prisma_render::place`).
//!
//! # The no-regression gate (run and pass this FIRST)
//!
//! [`identity_placement_is_byte_identical_to_the_m5_direct_render`] is the gate
//! Part A (docs/M8-PARTA.md §2.3) defines: with `mapping_size == footprint` and an
//! identity transform, rendering an effect at `mapping_size` and **placing** it
//! into the world framebuffer must be **byte-for-byte** the same as M5's direct
//! render at world resolution. If this ever fails, M8's per-zone render has
//! perturbed M5 — stop and understand why before building anything else on top.
//!
//! The other two tests prove the pass actually *resamples* (nearest) and *confines*
//! to its footprint — the behaviour the identity case deliberately does not exercise.
//!
//! Correctness runs on lavapipe (deterministic, bit-exact); no timing here.

use half::f16;
use prisma_core::{ClockSnapshot, ParameterTree};
use prisma_render::effect::EffectInstance;
use prisma_render::effects;
use prisma_render::place::{Footprint, ZonePlacer};
use prisma_render::readback::{create_render_target, read_rgba16f, Rgba16};
use prisma_render::Gpu;

mod common;
use common::software_gpu;

/// A fixed, non-trivial clock so the (time-dependent) plasma field is deterministic
/// but not the degenerate t=0 case.
fn still_clock() -> ClockSnapshot {
    ClockSnapshot {
        frame: 7,
        wall_ns: 0,
        show_ns: 500_000_000, // 0.5 s of show time
        beat: 1.0,
        bpm: 120.0,
        playing: false,
    }
}

/// Compare two readbacks by the **exact f16 bit pattern**, not `PartialEq` (which
/// treats `-0.0 == 0.0`). "Byte-identical" is the claim, so assert on the bits.
fn assert_bit_identical(a: &[Rgba16], b: &[Rgba16], msg: &str) {
    let bits =
        |px: &[Rgba16]| -> Vec<[u16; 4]> { px.iter().map(|t| t.map(f16::to_bits)).collect() };
    assert_eq!(bits(a), bits(b), "{msg}");
}

fn view(tex: &wgpu::Texture) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Upload a `w`×`h` `Rgba16Float` source from a per-texel closure. Dyadic values
/// stay exact in f16.
fn upload(gpu: &Gpu, w: u32, h: u32, texel: impl Fn(u32, u32) -> [f32; 4]) -> wgpu::Texture {
    let tex = create_render_target(gpu, w, h);
    let mut data: Vec<f16> = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in texel(x, y) {
                data.push(f16::from_f32(c));
            }
        }
    }
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&data),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4 * 2),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    tex
}

#[test]
fn identity_placement_is_byte_identical_to_the_m5_direct_render() {
    let gpu = software_gpu();
    let tree = ParameterTree::with_defaults();
    let clock = still_clock();
    // Non-square so an X/Y swap in the placement maths could not pass unnoticed.
    let (w, h) = (8u32, 4u32);

    // A real effect (plasma is genuinely 2D and time-dependent).
    let mut effect = EffectInstance::new(gpu, &tree, "/gate/plasma", &effects::plasma());

    // --- M5 path: render the effect directly at world resolution. ---
    let m5 = create_render_target(gpu, w, h);
    effect.render(gpu, &clock, &view(&m5), w, h);
    let m5_px = read_rgba16f(gpu, &m5, w, h);

    // --- M8 path: render at mapping_size (== world) then PLACE with identity. ---
    let zone_tex = create_render_target(gpu, w, h);
    effect.render(gpu, &clock, &view(&zone_tex), w, h);
    let placed = create_render_target(gpu, w, h);
    let placer = ZonePlacer::new(gpu);
    placer.place(gpu, &zone_tex, &view(&placed), Footprint::identity((w, h)));
    let placed_px = read_rgba16f(gpu, &placed, w, h);

    // THE GATE: the placement must not perturb a single bit of M5's output.
    assert_bit_identical(
        &m5_px,
        &placed_px,
        "identity placement (mapping_size == footprint, identity transform) must be \
         byte-identical to the M5 direct render — M8-PARTA §2.3 no-regression gate",
    );
}

#[test]
fn nearest_upscale_replicates_source_texels() {
    let gpu = software_gpu();
    let placer = ZonePlacer::new(gpu);

    // Source: 2×1, texel 0 = red, texel 1 = green (dyadic, exact in f16).
    let src = upload(gpu, 2, 1, |x, _| {
        if x == 0 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0, 1.0]
        }
    });

    // Place across a 4×1 world with an identity (whole-world) footprint. Nearest:
    // world cells 0,1 map to source texel 0; cells 2,3 to source texel 1.
    let out = create_render_target(gpu, 4, 1);
    placer.place(gpu, &src, &view(&out), Footprint::identity((4, 1)));
    let px = read_rgba16f(gpu, &out, 4, 1);

    let red: Rgba16 = [f16::ONE, f16::ZERO, f16::ZERO, f16::ONE];
    let green: Rgba16 = [f16::ZERO, f16::ONE, f16::ZERO, f16::ONE];
    assert_eq!(px, vec![red, red, green, green], "nearest 2→4 upscale");
}

#[test]
fn a_footprint_leaves_cells_outside_it_black() {
    let gpu = software_gpu();
    let placer = ZonePlacer::new(gpu);

    // A 2×2 all-white source placed into a 4×4 world at origin (1,1), size (2,2):
    // only the inner 2×2 block should be white; everything else black.
    let src = upload(gpu, 2, 2, |_, _| [1.0, 1.0, 1.0, 1.0]);
    let out = create_render_target(gpu, 4, 4);
    placer.place(
        gpu,
        &src,
        &view(&out),
        Footprint {
            origin: [1.0, 1.0],
            size: [2.0, 2.0],
        },
    );
    let px = read_rgba16f(gpu, &out, 4, 4);

    let white: Rgba16 = [f16::ONE, f16::ONE, f16::ONE, f16::ONE];
    for y in 0..4u32 {
        for x in 0..4u32 {
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            let got = px[(y * 4 + x) as usize];
            if inside {
                assert_eq!(got, white, "cell ({x},{y}) is inside the footprint");
            } else {
                assert_eq!(
                    got[0],
                    f16::ZERO,
                    "cell ({x},{y}) outside footprint is black"
                );
                assert_eq!(
                    got[1],
                    f16::ZERO,
                    "cell ({x},{y}) outside footprint is black"
                );
                assert_eq!(
                    got[2],
                    f16::ZERO,
                    "cell ({x},{y}) outside footprint is black"
                );
            }
        }
    }
}

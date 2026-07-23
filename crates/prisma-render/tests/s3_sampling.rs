//! S3 acceptance: the geometric gather (framebuffer → canonical master buffer).
//!
//! # Why the framebuffer is uploaded, not rendered
//!
//! The framebuffer here is written from the CPU with a known pattern, so **no
//! shader participates in producing it**. That isolates the gather: any failure is
//! the position→cell→component mapping, never an effect or a blend. Effects and
//! blending are already covered by S1/S2 and get integrated in S4.
//!
//! # The pattern is deliberately asymmetric
//!
//! Every cell gets a unique `(R, G, B, A)` fingerprint, and R varies only with the
//! column while G varies only with the row. A symmetric pattern would let the most
//! common gather bug — a swapped X/Y axis — pass unnoticed; here any wrong cell
//! read produces a wrong B, because B is injective over cells. All pattern values
//! are `k / 2^n`, hence exact in f16, so the assertions are bit-exact with no
//! epsilon.
//!
//! Runs on lavapipe (deterministic, CI gate).

use half::f16;
use prisma_patch::{
    load_patch, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchError, PatchIndex, Pixel,
};
use prisma_render::readback::create_render_target;
use prisma_render::sample::{SamplePlan, Sampler};
mod common;

use common::software_gpu;
use prisma_render::Gpu;
use uuid::Uuid;

/// The same golden rig M2 pinned down: 8 pixels, tube A (RGB/GRB) on row 0 and
/// tube B (RGBW) on row 1, in an 8x2 matrix. Non-square on purpose.
const GOLDEN: &str = include_str!("../../prisma-patch/tests/data/test_rig.patch.json");

// ---- the synthetic pattern -------------------------------------------------

/// Unique per-cell fingerprint. R tracks the column only, G the row only, and B/A
/// mix both with different weights so every cell differs from every other. All
/// values are dyadic (`k / 2^n`) → exact in f16.
fn pattern(x: u32, y: u32) -> [f32; 4] {
    let xa = (x + 1) as f32;
    let ya = (y + 1) as f32;
    [
        xa / 16.0,                     // column only
        ya / 16.0,                     // row only
        (xa + 8.0 * ya) / 256.0,       // injective over cells
        (2.0 * xa + 3.0 * ya) / 256.0, // different mix, so B/A can't be confused
    ]
}

/// Upload the pattern into a `w`x`h` framebuffer. `lit` decides which cells carry
/// the pattern; every other cell stays black.
fn upload_pattern(gpu: &Gpu, w: u32, h: u32, lit: impl Fn(u32, u32) -> bool) -> wgpu::Texture {
    let texture = create_render_target(gpu, w, h);
    let mut texels: Vec<f16> = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if lit(x, y) { pattern(x, y) } else { [0.0; 4] };
            texels.extend(c.iter().map(|&v| f16::from_f32(v)));
        }
    }
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&texels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4 * 2), // 4 components x 2 bytes
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn rig() -> (Patch, PatchIndex) {
    let patch = load_patch(GOLDEN).expect("golden rig loads");
    let index = patch.compile().expect("golden rig compiles");
    (patch, index)
}

/// Gather the golden rig against a framebuffer and return the master buffer.
fn gather_rig(gpu: &Gpu, lit: impl Fn(u32, u32) -> bool) -> (PatchIndex, Vec<f16>) {
    let (patch, index) = rig();
    let fb = upload_pattern(gpu, patch.matrix_size[0], patch.matrix_size[1], lit);
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    sampler.gather(gpu, &fb);
    let master = sampler.read_master_f16(gpu);
    (index, master)
}

#[track_caller]
fn assert_component(master: &[f16], offset: usize, k: usize, expected: f32, ctx: &str) {
    let got = master[offset + k].to_f32();
    assert_eq!(
        got,
        f16::from_f32(expected).to_f32(),
        "{ctx}: component {k} at offset {offset} = {got}, expected {expected}"
    );
}

#[test]
fn each_pixel_gathers_its_own_cell() {
    let gpu = software_gpu();
    let (index, master) = gather_rig(gpu, |_, _| true);
    assert_eq!(master.len(), index.master_buffer_len());

    // Tube A: globals 0..3 on row 0; tube B: globals 4..7 on row 1. Each pixel's
    // cell is (its column, its row) — expected values come from the pattern and
    // the patch geometry, not from what the gather produced.
    for global in 0..index.pixel_count() {
        let (x, y) = if global < 4 {
            (global, 0)
        } else {
            (global - 4, 1)
        };
        let expected = pattern(x, y);
        let offset = index.pixel_component_offset(global) as usize;
        let n = index.pixel_component_count(global) as usize;
        for (k, &want) in expected.iter().take(n).enumerate() {
            assert_component(
                &master,
                offset,
                k,
                want,
                &format!("global {global} (cell {x},{y})"),
            );
        }
    }
}

#[test]
fn a_swapped_axis_would_be_caught() {
    // Documents the guard rather than trusting the pattern by eye: the pixel in
    // cell (1,0) and the one in cell (0,1) must differ. Under a transposed gather
    // they would read each other's cell; a symmetric pattern would make that
    // invisible, which is exactly the bug this asymmetry exists to catch.
    let gpu = software_gpu();
    let (index, master) = gather_rig(gpu, |_, _| true);

    // global 1 is tube A pixel 1 → cell (1,0); global 4 is tube B pixel 0 → (0,1).
    let a = index.pixel_component_offset(1) as usize;
    let b = index.pixel_component_offset(4) as usize;
    assert_ne!(
        (master[a].to_f32(), master[a + 1].to_f32()),
        (master[b].to_f32(), master[b + 1].to_f32()),
        "cells (1,0) and (0,1) must be distinguishable, or an axis swap slips through"
    );
    // And concretely: R follows the column, G follows the row.
    assert_eq!(master[a].to_f32(), f16::from_f32(2.0 / 16.0).to_f32()); // col 1
    assert_eq!(master[a + 1].to_f32(), f16::from_f32(1.0 / 16.0).to_f32()); // row 0
    assert_eq!(master[b].to_f32(), f16::from_f32(1.0 / 16.0).to_f32()); // col 0
    assert_eq!(master[b + 1].to_f32(), f16::from_f32(2.0 / 16.0).to_f32()); // row 1
}

#[test]
fn lighting_one_zone_touches_only_its_member_pixels() {
    // The central M2 invariant, now proven at the *pixel* level through the real
    // gather (M2 asserted it at the model level). Light only the cells covered by
    // sector-left (tube A pixels 0 and 1 → cells (0,0) and (1,0)); every other
    // component of the master buffer must stay exactly zero.
    let gpu = software_gpu();
    let (index, master) = gather_rig(gpu, |x, y| y == 0 && x < 2);

    let members: Vec<u32> = index.zone_members(0).to_vec();
    assert_eq!(members, vec![0, 1], "sector-left is tube A pixels 0 and 1");

    let mut lit_components = std::collections::HashSet::new();
    for &global in &members {
        let offset = index.pixel_component_offset(global) as usize;
        let n = index.pixel_component_count(global) as usize;
        let expected = pattern(global, 0);
        for (k, &want) in expected.iter().take(n).enumerate() {
            assert_component(&master, offset, k, want, "sector-left member");
            lit_components.insert(offset + k);
        }
    }
    for (i, v) in master.iter().enumerate() {
        if !lit_components.contains(&i) {
            assert_eq!(
                v.to_f32(),
                0.0,
                "component {i} is outside sector-left and must be dark"
            );
        }
    }
}

// ---- the deliberate coarse-matrix behaviour --------------------------------

/// Two RGB pixels, both at the same position, in a 2x1 matrix.
fn two_pixels_one_cell() -> Patch {
    let fixture_id = Uuid::from_u128(0xF1);
    Patch {
        id: Uuid::from_u128(0x5001),
        name: "coarse".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "pair".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: vec![
                Pixel {
                    index: 0,
                    position: glam::Vec3::new(1.0, 0.0, 0.0),
                    universe: 1,
                    start_channel: 1,
                    color_order: ColorOrder::Rgb,
                },
                // Same cell as pixel 0 — a coarse matrix with two LEDs per cell.
                Pixel {
                    index: 1,
                    position: glam::Vec3::new(1.0, 0.0, 0.0),
                    universe: 1,
                    start_channel: 4,
                    color_order: ColorOrder::Rgb,
                },
            ],
        }],
        zones: vec![],
    }
}

#[test]
fn pixels_sharing_a_cell_read_the_same_texel_by_design() {
    // NOT a resolution bug — see `prisma_patch::cell_of`. `matrix_size` IS the
    // effect's resolution, decoupled from the physical pixel count; LEDs in one
    // cell move together, and the remedy for wanting them to differ is a larger
    // matrix, never a change to the sampler. This test exists so nobody "fixes"
    // that behaviour without deliberately changing the documented rule.
    let gpu = software_gpu();
    let patch = two_pixels_one_cell();
    let index = patch.compile().expect("valid patch");
    let fb = upload_pattern(gpu, 2, 1, |_, _| true);
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    sampler.gather(gpu, &fb);
    let master = sampler.read_master_f16(gpu);

    let expected = pattern(1, 0);
    for global in 0..2u32 {
        let offset = index.pixel_component_offset(global) as usize;
        for (k, &want) in expected.iter().take(3).enumerate() {
            assert_component(&master, offset, k, want, "shared cell");
        }
    }
}

// ---- out-of-range positions ------------------------------------------------

fn pixel_outside_matrix() -> Patch {
    let mut patch = two_pixels_one_cell();
    patch.name = "out-of-range".into();
    // x = 5 in a matrix only 2 cells wide.
    patch.fixtures[0].pixels[1].position = glam::Vec3::new(5.0, 0.0, 0.0);
    patch
}

#[test]
fn a_position_outside_the_matrix_is_refused_at_compile() {
    // Primary defence: fail loud at load rather than ship one dark fixture to a
    // show. Same stance as the layout-generation guard.
    let err = pixel_outside_matrix()
        .compile()
        .expect_err("a pixel outside the matrix must not validate");
    match err {
        PatchError::PositionOutOfMatrix { pixel, .. } => assert_eq!(pixel, 1),
        other => panic!("expected PositionOutOfMatrix, got {other:?}"),
    }
}

#[test]
fn an_out_of_range_pixel_gathers_black_rather_than_a_clamped_texel() {
    // Defensive half: a patch built programmatically (bypassing validate) must
    // still never read a wrong texel. Black is diagnosable — a clamped edge sample
    // would light plausibly and hide the broken patch.
    let gpu = software_gpu();
    let patch = pixel_outside_matrix();
    // Bypass validate() deliberately to exercise the runtime path.
    let index = PatchIndex::build(&patch).expect("index builds without validation");
    let fb = upload_pattern(gpu, 2, 1, |_, _| true);
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    sampler.gather(gpu, &fb);
    let master = sampler.read_master_f16(gpu);

    // Pixel 0 is in range and gathers normally.
    let in_range = index.pixel_component_offset(0) as usize;
    let expected = pattern(1, 0);
    for (k, &want) in expected.iter().take(3).enumerate() {
        assert_component(&master, in_range, k, want, "in-range pixel");
    }
    // Pixel 1 is outside the matrix: black, not the clamped edge cell.
    let out = index.pixel_component_offset(1) as usize;
    for k in 0..3 {
        assert_eq!(
            master[out + k].to_f32(),
            0.0,
            "out-of-range pixel component {k} must be black"
        );
    }
}

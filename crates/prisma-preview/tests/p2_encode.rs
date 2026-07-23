//! P2 acceptance: the sRGB transfer, the binary frame format, the static
//! descriptor, and the generation guard (CLAUDE.md M6).
//!
//! P0 proved preview and wire agree in **linear light**. P2 is what happens after
//! that split: the preview's last step is **sRGB** (the screen's curve), not the
//! fixture gamma (the LED's). `srgb_is_not_the_wire_transfer` pins that difference
//! so nobody later "unifies" the two encoders.

use std::time::Instant;

use half::f16;
use prisma_output::plan::FlatParams;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, LayoutGeneration, Patch,
    Pixel, PixelRef, Zone,
};
use prisma_preview::descriptor::PreviewDescriptor;
use prisma_preview::encode::{
    encode_frame, linear_to_srgb8, PreviewEncodeError, HEADER_LEN, MAGIC, PIXEL_FORMAT_RGB8_SRGB,
    ZONE_ENTRY_LEN, ZONE_FLAG_ACTIVE,
};
use prisma_preview::frame::{PreviewFrame, ZoneActiveMask};
use prisma_preview::PreviewPlan;
use uuid::Uuid;

/// Two RGB pixels in a 2x1 matrix; pixel 0 in zone A, pixel 1 in zone B.
fn two_zone_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x9A);
    let px = |p: u32| PixelRef {
        fixture: fixture_id,
        pixel: p,
    };
    Patch {
        id: Uuid::from_u128(0x9000),
        name: "m6-p2".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration { gamma: 2.2 },
            },
            pixels: (0..2)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: i as u16 * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
        }],
        zones: vec![
            Zone {
                id: Uuid::from_u128(0xA),
                name: "alpha".into(),
                members: vec![px(0)],
                bounds: None,
                z_order: 0,
                blend_mode: prisma_patch::BlendMode::Normal,
            },
            Zone {
                id: Uuid::from_u128(0xB),
                name: "bravo".into(),
                members: vec![px(1)],
                bounds: None,
                z_order: 1,
                blend_mode: prisma_patch::BlendMode::Normal,
            },
        ],
    }
}

fn master_of(values: &[f32]) -> Box<[f16]> {
    values.iter().map(|&v| f16::from_f32(v)).collect()
}

fn a_frame(generation: LayoutGeneration, pixels: Box<[f16]>, mask: ZoneActiveMask) -> PreviewFrame {
    PreviewFrame {
        pixels,
        frame_id: 42,
        render_frame: 40,
        produced_at: Instant::now(),
        layout_generation: generation,
        overlap: None,
        zone_active: mask,
    }
}

// ---- little-endian header readers -----------------------------------------

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

#[test]
fn srgb_transfer_hits_known_points() {
    assert_eq!(linear_to_srgb8(0.0), 0);
    assert_eq!(linear_to_srgb8(1.0), 255);
    // Standard sRGB midpoints (computed from the OETF, not from the code):
    assert_eq!(linear_to_srgb8(0.5), 188);
    assert_eq!(linear_to_srgb8(0.25), 137);
    // The tiny linear segment (c <= 0.0031308 uses the 12.92 slope):
    assert_eq!(
        linear_to_srgb8(0.002),
        (12.92 * 0.002 * 255.0f32).round() as u8
    );
    // Above 1.0 clamps rather than overflowing (Add zones / folded white can).
    assert_eq!(linear_to_srgb8(2.5), 255);
}

#[test]
fn srgb_is_not_the_wire_transfer() {
    // The whole reason preview and wire are separate encoders: at 0.5 linear, sRGB
    // gives 188, a plain identity-gamma wire gives 128, and a gamma-2.2 wire gives
    // yet another value. If these ever coincide the test is meaningless, so assert
    // they differ.
    let srgb = linear_to_srgb8(0.5);
    let identity_wire = (0.5f32 * 255.0).round() as u8; // 128
    let gamma22_wire = (0.5f32.powf(2.2) * 255.0).round() as u8; // ~55
    assert_ne!(srgb, identity_wire);
    assert_ne!(srgb, gamma22_wire);
    assert_eq!(srgb, 188);
}

#[test]
fn encode_frame_lays_out_header_zones_and_pixels() {
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);

    // Master buffer: pixel 0 = (0.5, 0.25, 1.0), pixel 1 = (0.0, 0.0, 0.0).
    let master = master_of(&[0.5, 0.25, 1.0, 0.0, 0.0, 0.0]);

    // Zone A active, zone B not.
    let mut mask = ZoneActiveMask::new(2);
    mask.set(0, true);
    let frame = a_frame(index.generation(), master, mask);

    // Master identity, zone A dimmed red (0.5, so its swatch reads sRGB(0.5)=188).
    let mut flat = FlatParams::identity(2);
    flat.set_master(1.0, [1.0, 1.0, 1.0]);
    flat.set_zone(0, 0.5, [1.0, 0.0, 0.0]);
    flat.set_zone(1, 1.0, [0.0, 0.0, 1.0]);

    let mut out = Vec::new();
    encode_frame(&plan, &frame, &flat, &mut out).unwrap();

    // --- header ---
    assert_eq!(&out[0..4], &MAGIC);
    assert_eq!(u16_at(&out, 4), 1, "format version");
    assert_eq!(out[6], PIXEL_FORMAT_RGB8_SRGB);
    assert_eq!(out[7], 0, "reserved");
    assert_eq!(u64_at(&out, 8), 42, "frame_id");
    assert_eq!(
        u64_at(&out, 16),
        index.generation().get(),
        "layout generation"
    );
    assert_eq!(u32_at(&out, 24), 2, "pixel_count");
    assert_eq!(u16_at(&out, 28), 2, "zone_count");

    // --- total length ---
    assert_eq!(out.len(), HEADER_LEN + 2 * ZONE_ENTRY_LEN + 2 * 3);

    // --- zone block ---
    let z0 = HEADER_LEN;
    assert_eq!(
        out[z0] & ZONE_FLAG_ACTIVE,
        ZONE_FLAG_ACTIVE,
        "zone A active"
    );
    assert_eq!(out[z0 + 1], 188, "zone A swatch R = sRGB(0.5)");
    assert_eq!(out[z0 + 2], 0, "zone A swatch G");
    assert_eq!(out[z0 + 3], 0, "zone A swatch B");
    let z1 = HEADER_LEN + ZONE_ENTRY_LEN;
    assert_eq!(out[z1] & ZONE_FLAG_ACTIVE, 0, "zone B not active");
    assert_eq!(out[z1 + 3], 255, "zone B swatch B = sRGB(1.0)");

    // --- pixel block (sRGB of the master, master is identity, overlap None) ---
    let p = HEADER_LEN + 2 * ZONE_ENTRY_LEN;
    assert_eq!([out[p], out[p + 1], out[p + 2]], [188, 137, 255], "pixel 0");
    assert_eq!([out[p + 3], out[p + 4], out[p + 5]], [0, 0, 0], "pixel 1");
}

#[test]
fn the_master_fader_moves_the_encoded_pixels() {
    // The preview must react to a live master fader with no re-render: same frame,
    // master pulled to 0.5, every pixel byte drops. This is the §1.5 shortcut seen
    // through the encoder.
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);
    let master = master_of(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    let frame = a_frame(index.generation(), master, ZoneActiveMask::new(2));

    let mut full = Vec::new();
    encode_frame(&plan, &frame, &FlatParams::identity(2), &mut full).unwrap();
    let p = HEADER_LEN + 2 * ZONE_ENTRY_LEN;
    assert_eq!(full[p], 255, "full master -> sRGB(1.0)");

    let mut dimmed_flat = FlatParams::identity(2);
    dimmed_flat.set_master(0.5, [1.0, 1.0, 1.0]);
    let mut dimmed = Vec::new();
    encode_frame(&plan, &frame, &dimmed_flat, &mut dimmed).unwrap();
    assert_eq!(dimmed[p], 188, "half master -> sRGB(0.5), no re-render");
}

#[test]
fn a_generation_mismatch_is_refused() {
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);
    let master = master_of(&[0.0; 6]);

    // A frame stamped with NONE (never-published) — a different generation.
    let frame = a_frame(LayoutGeneration::NONE, master, ZoneActiveMask::new(2));
    let mut out = Vec::new();
    let err = encode_frame(&plan, &frame, &FlatParams::identity(2), &mut out).unwrap_err();
    assert_eq!(
        err,
        PreviewEncodeError::GenerationMismatch {
            plan: index.generation().get(),
            frame: 0,
        }
    );
}

#[test]
fn descriptor_carries_geometry_and_zones_and_round_trips() {
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);

    let desc = PreviewDescriptor::build(&plan, &patch, &index);

    assert_eq!(desc.generation, index.generation().get());
    assert_eq!(desc.matrix_size, [2, 1, 1]);
    assert_eq!(desc.pixels.len(), 2);
    assert_eq!((desc.pixels[0].x, desc.pixels[0].y), (0.0, 0.0));
    assert_eq!((desc.pixels[1].x, desc.pixels[1].y), (1.0, 0.0));

    assert_eq!(desc.zones.len(), 2);
    assert_eq!(desc.zones[0].name, "alpha");
    assert_eq!(desc.zones[0].members, vec![0]);
    assert_eq!(desc.zones[1].name, "bravo");
    assert_eq!(desc.zones[1].members, vec![1]);
    assert_eq!(desc.zones[0].id, Uuid::from_u128(0xA).to_string());

    // JSON round-trips: the browser gets exactly what the engine described.
    let json = desc.to_json();
    let back: PreviewDescriptor = serde_json::from_str(&json).unwrap();
    assert_eq!(back, desc);
}

#[test]
fn descriptor_and_frame_share_the_generation_used_by_the_guard() {
    // The guard the consumer runs: descriptor.generation must equal the frame's
    // layout generation. This ties the two P2 halves together.
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);
    let desc = PreviewDescriptor::build(&plan, &patch, &index);

    let frame = a_frame(
        index.generation(),
        master_of(&[0.0; 6]),
        ZoneActiveMask::new(2),
    );
    assert_eq!(desc.generation, frame.layout_generation.get());

    // And a matching pair encodes without error.
    let mut out = Vec::new();
    encode_frame(&plan, &frame, &FlatParams::identity(2), &mut out).unwrap();
    assert_eq!(u64_at(&out, 16), desc.generation);
}

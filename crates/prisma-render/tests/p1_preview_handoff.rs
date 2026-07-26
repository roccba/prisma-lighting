//! P1 acceptance: the render loop publishes to the preview handoff alongside the
//! output handoff (CLAUDE.md M6).
//!
//! What P1 proves:
//! 1. With a preview attached, the same render produces **both** an output frame
//!    and a preview frame carrying identical master pixels, the right generation,
//!    and the snapshot the frame was rendered under.
//! 2. The preview frame carries the correct `is_active` mask — and it reflects
//!    `is_active()`, **not** `contributes_pixels()`: a zone silenced by another
//!    zone's solo stays active in the mask (the whole reason M6 uses that predicate).
//! 3. A readback carries the snapshot of **its own** frame to the preview too, the
//!    same invariant S6 proved for the output side.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use prisma_core::time::SystemClock;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot};
use prisma_output::frame::frame_handoff;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_preview::preview_handoff;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{
    run_render_loop, FrameContent, RenderLoop, RenderLoopConfig, TickOutcome,
};
use prisma_render::sample::{SamplePlan, Sampler};
mod common;

use common::software_gpu;
use prisma_render::Gpu;
use uuid::Uuid;

/// Two RGB pixels in a 2x1 matrix, in two overlapping zones so solo has something
/// to silence.
fn two_zone_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0xF7);
    let px = |p: u32| PixelRef {
        fixture: fixture_id,
        pixel: p,
    };
    Patch {
        id: Uuid::from_u128(0x57),
        name: "p1".into(),
        matrix_size: [2, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..2)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: Uuid::from_u128(0x1),
        }],
        zones: vec![
            Zone {
                id: Uuid::from_u128(0x1),
                name: "a".into(),
                members: vec![px(0)],
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [2, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            },
            Zone {
                id: Uuid::from_u128(0x2),
                name: "b".into(),
                members: vec![px(1)],
                bounds: None,
                z_order: 1,
                blend_mode: BlendMode::Normal,
                mapping_size: [2, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            },
        ],
    }
}

/// Fills the framebuffer with a flat value from the CPU — no shader, so the test
/// isolates the handoff wiring, not the effect stack.
struct FlatContent {
    value: f32,
}

impl FrameContent for FlatContent {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        _clock: &ClockSnapshot,
        _zones: &ZoneStates,
        _overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        let size = target.size();
        let texels: Vec<f16> = (0..size.width * size.height)
            .flat_map(|_| {
                [
                    f16::from_f32(self.value),
                    f16::from_f32(self.value),
                    f16::from_f32(self.value),
                    f16::from_f32(1.0),
                ]
            })
            .collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.width * 4 * 2),
                rows_per_image: Some(size.height),
            },
            size,
        );
    }
}

struct Setup {
    engine: RenderLoop,
    output: prisma_output::frame::FrameConsumer,
    preview: prisma_preview::PreviewConsumer,
    index: PatchIndex,
    counter: OverlapCounter,
    zones: ZoneStates,
}

fn setup(gpu: &Gpu, zones: ZoneStates) -> Setup {
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    let (producer, output) = frame_handoff(index.master_buffer_len());
    let mut engine = RenderLoop::new(
        gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
        3,
    );
    let (pv_producer, preview) = preview_handoff(index.master_buffer_len(), index.zone_count());
    engine.attach_preview(pv_producer, index.zone_count());
    let counter = OverlapCounter::new(&patch, &index);
    Setup {
        engine,
        output,
        preview,
        index,
        counter,
        zones,
    }
}

/// Drive one rendered+published frame by hand (no timing thread), polling until
/// the readback lands. Returns once the preview has a frame to show.
fn pump_one_frame(s: &mut Setup, gpu: &Gpu, clock: &mut MasterClock, content: &mut FlatContent) {
    s.counter.sync(&s.zones);
    let overlap = s.counter.snapshot();
    let outcome = s
        .engine
        .tick(gpu, &clock.tick(), &s.zones, &overlap, content);
    assert_eq!(outcome, TickOutcome::Rendered);

    // A safety net against a genuinely hung readback, NOT a latency assertion (the
    // preview is not on the §1.5 budget). With the shared GPU (tests/common) there
    // is no bring-up contention, so the readback lands in well under a millisecond;
    // this bound only has to catch a real hang, which is what it is for.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !s.preview.advance() {
        s.engine.poll_and_drain(gpu);
        assert!(Instant::now() < deadline, "preview frame never arrived");
        std::thread::sleep(Duration::from_micros(200));
    }
    // Bring the output consumer up to the same frame.
    while s.output.advance() {}
}

#[test]
fn one_render_feeds_both_output_and_preview() {
    let gpu = software_gpu();
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    let mut s = setup(gpu, zones);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent { value: 0.5 };

    pump_one_frame(&mut s, gpu, &mut clock, &mut content);

    let out = s.output.current();
    let pv = s.preview.current();

    // Same content, both destinations.
    assert_eq!(
        out.pixels, pv.pixels,
        "preview and output carry the same master buffer"
    );
    for (i, v) in pv.pixels.iter().enumerate() {
        assert!(
            (v.to_f32() - 0.5).abs() < 1.0 / 128.0,
            "preview component {i} = {}",
            v.to_f32()
        );
    }
    // Same generation and snapshot, so the preview can apply the flat parameters
    // exactly as the output stage does.
    assert_eq!(pv.layout_generation, s.index.generation());
    assert!(pv.overlap.is_some(), "preview frame carries its snapshot");
    assert_eq!(
        Arc::as_ptr(out.overlap.as_ref().unwrap()),
        Arc::as_ptr(pv.overlap.as_ref().unwrap()),
        "both frames carry the very same snapshot Arc"
    );

    // Both zones up → both active in the mask.
    assert!(pv.zone_active.is_active(0));
    assert!(pv.zone_active.is_active(1));
}

#[test]
fn the_active_mask_uses_is_active_not_contributes_pixels() {
    // Zone 0 soloed, zone 1 up but silenced by the solo. The silenced zone
    // contributes NO pixels, yet is_active() stays true — its strip/pad must not
    // vanish. The preview mask must reflect is_active(), so bit 1 stays set.
    let gpu = software_gpu();
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    zones.set_dimmer(1, 1.0);
    zones.set_solo(0, true);
    let mut s = setup(gpu, zones);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent { value: 0.5 };

    // Sanity: the two predicates really disagree here, or the test proves nothing.
    assert!(s.zones.is_active(1), "silenced zone is still is_active");
    assert!(
        !s.zones.contributes_pixels(1),
        "silenced zone contributes no pixels"
    );

    pump_one_frame(&mut s, gpu, &mut clock, &mut content);
    let pv = s.preview.current();

    assert!(pv.zone_active.is_active(0), "soloed zone is active");
    assert!(
        pv.zone_active.is_active(1),
        "a zone silenced by ANOTHER zone's solo must stay active in the preview"
    );
}

#[test]
fn a_preview_frame_carries_the_snapshot_of_its_own_frame() {
    // Two frames rendered under two different zone configurations. The preview
    // frame that surfaces must carry the mask/snapshot of the frame it came from,
    // not a newer one — the same "exactly one stage" invariant S6 proved for output.
    let gpu = software_gpu();
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    // zone 1 down for the first frame.
    let mut s = setup(gpu, zones);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent { value: 0.5 };

    pump_one_frame(&mut s, gpu, &mut clock, &mut content);
    let first = s.preview.current();
    assert!(first.zone_active.is_active(0));
    assert!(!first.zone_active.is_active(1), "zone 1 was down");
    let first_frame_id = first.frame_id;

    // Bring zone 1 up and render again.
    s.zones.set_dimmer(1, 1.0);
    pump_one_frame(&mut s, gpu, &mut clock, &mut content);
    let second = s.preview.current();
    assert!(second.zone_active.is_active(1), "zone 1 now up");
    assert!(
        second.frame_id > first_frame_id,
        "a newer preview frame was published"
    );
}

#[test]
fn no_preview_attached_costs_nothing_and_still_feeds_output() {
    // The Option is None by default: the loop runs exactly as before M6.
    let gpu = software_gpu();
    let patch = two_zone_patch();
    let index = patch.compile().unwrap();
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    let (producer, mut output) = frame_handoff(index.master_buffer_len());
    let mut engine = RenderLoop::new(
        gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
        3,
    );
    assert!(!engine.has_preview());

    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(2);
    zones.set_dimmer(0, 1.0);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent { value: 0.25 };

    let stats = run_render_loop(
        gpu,
        &mut engine,
        &mut clock,
        &mut counter,
        zones,
        &mut content,
        RenderLoopConfig {
            target_rate_hz: 200,
            run_for: Some(Duration::from_millis(120)),
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
            freeze: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    assert!(
        stats.frames_published > 0,
        "output still fed with no preview"
    );
    assert!(output.advance());
}

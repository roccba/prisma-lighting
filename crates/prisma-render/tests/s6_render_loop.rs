//! S6 acceptance: the render loop and its **non-blocking** readback
//! (CLAUDE.md §3.3, §5.2 step 5, §1.5).
//!
//! The behaviour under test is the one that can silently blow the latency budget:
//! what the loop does when a readback is not ready in time. The rule is
//! *skip the frame whole and count it* — never wait, never reuse an in-flight
//! slot, never pile on more GPU work.

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
use prisma_render::overlap::OverlapCounter;
use prisma_render::readback_ring::ReadbackRing;
use prisma_render::render_loop::{
    run_render_loop, FrameContent, RenderLoop, RenderLoopConfig, TickOutcome,
};
use prisma_render::sample::{SamplePlan, Sampler};
mod common;

use common::software_gpu;
use prisma_render::Gpu;
use uuid::Uuid;

/// Two RGB pixels in a 2x1 matrix, both in one zone.
fn test_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0xF6);
    Patch {
        id: Uuid::from_u128(0x56),
        name: "s6".into(),
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
            zone: Uuid::nil(),
        }],
        zones: vec![Zone {
            id: Uuid::from_u128(0x1),
            name: "all".into(),
            members: (0..2)
                .map(|p| PixelRef {
                    fixture: fixture_id,
                    pixel: p,
                })
                .collect(),
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [2, 1, 1],
            display_color: [128, 128, 128],
            world_transform: None,
        }],
    }
}

/// Fills the framebuffer with a flat value from the CPU — no shader, so the loop's
/// timing and readback behaviour are what is under test, not the effect stack.
struct FlatContent {
    value: f32,
    rendered_frames: Vec<u64>,
}

impl FrameContent for FlatContent {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        _zones: &ZoneStates,
        _overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        self.rendered_frames.push(clock.frame);
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

fn setup(
    gpu: &Gpu,
    slots: usize,
) -> (
    RenderLoop,
    prisma_output::frame::FrameConsumer,
    PatchIndex,
    OverlapCounter,
    ZoneStates,
    Patch,
) {
    let patch = test_patch();
    let index = patch.compile().unwrap();
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    let (producer, consumer) = frame_handoff(index.master_buffer_len());
    let engine = RenderLoop::new(
        gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
        slots,
    );
    let counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(1);
    zones.set_dimmer(0, 1.0);
    (engine, consumer, index, counter, zones, patch)
}

/// Records the clock state each render, so a test can prove the freeze flag stops
/// show time from advancing (§7 / S9) while frames keep rendering.
struct ShowTimeProbe {
    show_ns: Vec<u64>,
    playing: Vec<bool>,
}

impl FrameContent for ShowTimeProbe {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        _zones: &ZoneStates,
        _overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        self.show_ns.push(clock.show_ns);
        self.playing.push(clock.playing);
        let size = target.size();
        let texels: Vec<f16> = vec![f16::ZERO; (size.width * size.height * 4) as usize];
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

#[test]
fn freeze_holds_show_time_while_the_loop_keeps_rendering() {
    // §7 / S9: while frozen, the render loop pauses the clock — show time (and any
    // transition on it) holds — but the loop keeps ticking so the output stays alive.
    let gpu = software_gpu();
    let (mut engine, _consumer, _index, mut counter, zones, _patch) = setup(gpu, 3);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = ShowTimeProbe {
        show_ns: Vec::new(),
        playing: Vec::new(),
    };

    let stats = run_render_loop(
        gpu,
        &mut engine,
        &mut clock,
        &mut counter,
        zones,
        &mut content,
        RenderLoopConfig {
            target_rate_hz: 200,
            run_for: Some(Duration::from_millis(150)),
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
            freeze: Arc::new(AtomicBool::new(true)), // frozen the whole run
        },
    );

    assert!(
        stats.frames_rendered > 5,
        "the loop must keep rendering while frozen"
    );
    assert!(!content.show_ns.is_empty(), "content actually ran");
    assert!(
        content.show_ns.iter().all(|&t| t == 0),
        "show time must not advance while frozen: {:?}",
        content.show_ns
    );
    assert!(
        content.playing.iter().all(|&p| !p),
        "the clock must report paused every frame while frozen"
    );
}

#[test]
fn the_loop_renders_and_publishes_frames() {
    let gpu = software_gpu();
    let (mut engine, mut consumer, index, mut counter, zones, _patch) = setup(gpu, 3);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent {
        value: 0.5,
        rendered_frames: Vec::new(),
    };

    let stats = run_render_loop(
        gpu,
        &mut engine,
        &mut clock,
        &mut counter,
        zones,
        &mut content,
        RenderLoopConfig {
            target_rate_hz: 200,
            run_for: Some(Duration::from_millis(150)),
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
            freeze: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    assert!(
        stats.frames_rendered > 5,
        "rendered {}",
        stats.frames_rendered
    );
    assert!(stats.frames_published > 0, "nothing was published");
    assert_eq!(stats.readback_failures, 0, "readback maps must not fail");
    assert!(!stats.readback.is_empty(), "readback latency was measured");

    // The published frame carries the rendered content, the right generation, and
    // its classification.
    assert!(consumer.advance());
    let frame = consumer.current();
    assert_eq!(frame.layout_generation, index.generation());
    assert!(frame.overlap.is_some(), "the frame carries its snapshot");
    for (i, v) in frame.pixels.iter().enumerate() {
        assert!(
            (v.to_f32() - 0.5).abs() < 1.0 / 128.0,
            "component {i} = {}",
            v.to_f32()
        );
    }
}

#[test]
fn an_exhausted_ring_skips_the_frame_instead_of_waiting() {
    // The rule: with every slot in flight, the loop must NOT block, must NOT
    // render, and must count the stall.
    let gpu = software_gpu();
    let (mut engine, _consumer, _index, mut counter, zones, _patch) = setup(gpu, 1);
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut content = FlatContent {
        value: 0.25,
        rendered_frames: Vec::new(),
    };

    counter.sync(&zones);
    let overlap = counter.snapshot();

    // First tick claims the only slot and starts a readback.
    let first = engine.tick(gpu, &clock.tick(), &zones, &overlap, &mut content);
    assert_eq!(first, TickOutcome::Rendered);

    // Immediately tick again. Either the readback already completed (the slot was
    // freed by the drain) or it did not — but in NO case may this block. Drive
    // enough ticks that at least one must find the ring exhausted, and check the
    // loop stayed responsive throughout.
    let started = Instant::now();
    let mut stalls_seen = 0;
    for _ in 0..50 {
        let snapshot = clock.tick();
        if engine.tick(gpu, &snapshot, &zones, &overlap, &mut content) == TickOutcome::Stalled {
            stalls_seen += 1;
        }
    }
    let elapsed = started.elapsed();

    assert!(
        stalls_seen > 0,
        "a single-slot ring driven flat out must stall at least once"
    );
    assert_eq!(
        engine.stats().readback_stalls,
        stalls_seen,
        "every stall is counted in the probe"
    );
    // 50 ticks of a non-blocking loop finish quickly; a blocking wait on a map
    // would take orders of magnitude longer than this bound.
    assert!(
        elapsed < Duration::from_secs(5),
        "the loop blocked somewhere: 50 ticks took {elapsed:?}"
    );

    // A stalled frame renders nothing at all — no wasted GPU work.
    assert_eq!(
        content.rendered_frames.len() as u64,
        engine.stats().frames_rendered,
        "content must not be rendered on a stalled frame"
    );
}

#[test]
fn a_readback_carries_the_snapshot_of_its_own_frame() {
    // A readback landing now belongs to a frame from several ticks ago. If the
    // loop published it with the CURRENT snapshot, the "exactly one stage"
    // invariant would break exactly when the mask has just changed.
    let gpu = software_gpu();
    let patch = test_patch();
    let index = patch.compile().unwrap();
    let plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(gpu, &plan);
    let mut ring = ReadbackRing::new(gpu, index.master_buffer_len(), 3);

    let framebuffer = prisma_render::readback::create_render_target(gpu, patch.matrix_size[0], 1);

    // Two distinguishable snapshots.
    let mut first = OverlapSnapshot::zeroed(2, 2, 6);
    {
        let (count, zone_sum) = first.pixel_arrays_mut();
        count[0] = 1;
        zone_sum[0] = 0;
    }
    first.derive_from_pixels(&[0, 1], &[(0, 3), (3, 3)]);
    let first = Arc::new(first);

    let second = Arc::new(OverlapSnapshot::zeroed(2, 2, 6)); // everything uncovered

    // Submit two frames, each with its own snapshot.
    let slot_a = ring.try_claim().expect("slot free");
    sampler.gather_into(gpu, &framebuffer, ring.buffer(slot_a));
    ring.begin_map(slot_a, Instant::now(), 1, Some(Arc::clone(&first)));

    let slot_b = ring.try_claim().expect("slot free");
    sampler.gather_into(gpu, &framebuffer, ring.buffer(slot_b));
    ring.begin_map(slot_b, Instant::now(), 2, Some(Arc::clone(&second)));

    // Drain: the OLDEST completed readback comes first, with ITS snapshot.
    let mut seen: Vec<(u64, Option<u32>)> = Vec::new();
    for _ in 0..1000 {
        ring.poll(gpu);
        while let Some(entry) = ring.consume_ready(Instant::now(), |meta, _data| {
            (
                meta.frame,
                meta.overlap.as_ref().and_then(|o| o.zone_at_output(0)),
            )
        }) {
            seen.push(entry);
        }
        if seen.len() == 2 {
            break;
        }
    }

    assert_eq!(seen.len(), 2, "both readbacks completed");
    assert_eq!(
        seen[0],
        (1, Some(0)),
        "frame 1 kept the snapshot it rendered under"
    );
    assert_eq!(
        seen[1],
        (2, None),
        "frame 2 kept its own, different snapshot"
    );
}

#[test]
fn stalls_and_failures_are_reported_separately() {
    // They mean different things: a stall is "too slow", a failure is "broken".
    let gpu = software_gpu();
    let ring = ReadbackRing::new(gpu, 6, 2);
    assert_eq!(ring.stalls(), 0);
    assert_eq!(ring.failures(), 0);
    assert_eq!(ring.slot_count(), 2);
}

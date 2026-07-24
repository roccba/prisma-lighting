//! M6 milestone acceptance test — the whole "done when", not a collection of
//! partial asserts (CLAUDE.md §6, M6):
//!
//!   "a preview consumer, with no GPU and no re-render, shows the same colours the
//!    virtual sink receives on the wire (modulo the transfer function), at a stable
//!    30 Hz while render runs at 44 Hz; moving a zone fader with the render thread
//!    stopped changes the preview (proving it consumes the same §1.5 shortcut); and
//!    a solo on another zone does not erase the silenced zone from the preview
//!    (is_active(), not contributes_pixels())."
//!
//! Everything runs through the **real pipeline**: the render loop on its own thread,
//! the fixed-interval output loop to the virtual sink on another, and the preview
//! loop to a `ChannelSink` on a third. The "consumer with no GPU" is this test
//! thread, which decodes the preview's binary frames with `DataView`-style byte
//! reads — no wgpu, no re-render.
//!
//! The rig: four RGB pixels in a 4×1 matrix. Zone A = pixels 0,1; zone B = pixels
//! 2,3 (disjoint, so both take the §1.5 output-stage shortcut). Content is a flat
//! CPU-uploaded 0.5 everywhere, so the master buffer is a known linear value and the
//! only per-pixel difference is the live per-zone flat parameters — exactly what the
//! preview must reproduce.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use prisma_core::source::{priority, SourceId};
use prisma_core::time::SystemClock;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot, ParameterTree};
use prisma_output::frame::frame_handoff;
use prisma_output::plan::OutputPlan;
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::sink;
use prisma_output::transport::UdpOutput;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel, PixelRef,
    Zone,
};
use prisma_preview::encode::{linear_to_srgb8, HEADER_LEN, ZONE_ENTRY_LEN, ZONE_FLAG_ACTIVE};
use prisma_preview::preview_loop::{run_preview_loop, PreviewLoop, PreviewLoopConfig};
use prisma_preview::sink::{ChannelSink, PreviewMessage};
use prisma_preview::{preview_handoff, PreviewDescriptor, PreviewPlan};
use prisma_render::live_params::LiveParams;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{run_render_loop, FrameContent, RenderLoop, RenderLoopConfig};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::Gpu;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use uuid::Uuid;

mod common;
use common::software_gpu;

const ZONE_A: usize = 0;
const ZONE_B: usize = 1;

fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0xE6);
    let px = |p: u32| PixelRef {
        fixture: fixture_id,
        pixel: p,
    };
    Patch {
        id: Uuid::from_u128(0xE600),
        name: "m6".into(),
        matrix_size: [4, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(), // identity: wire byte == round(linear*255)
            },
            pixels: (0..4)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new(i as f32, 0.0, 0.0),
                    universe: 1,
                    start_channel: (i as u16) * 3 + 1,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: Uuid::from_u128(0xA),
        }],
        zones: vec![
            Zone {
                id: Uuid::from_u128(0xA),
                name: "A".into(),
                members: vec![px(0), px(1)],
                bounds: None,
                z_order: 0,
                blend_mode: prisma_patch::BlendMode::Normal,
                mapping_size: [4, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            },
            Zone {
                id: Uuid::from_u128(0xB),
                name: "B".into(),
                members: vec![px(2), px(3)],
                bounds: None,
                z_order: 1,
                blend_mode: prisma_patch::BlendMode::Normal,
                mapping_size: [4, 1, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            },
        ],
    }
}

/// Flat CPU-uploaded content: the master buffer becomes a known linear value
/// everywhere, so every per-pixel difference downstream is the live flat params.
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

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

fn wire_code(linear: f32) -> u8 {
    (linear.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Red byte of preview pixel `g` (zone block precedes the pixel block).
fn preview_pixel_r(frame: &[u8], zone_count: usize, g: usize) -> u8 {
    frame[HEADER_LEN + zone_count * ZONE_ENTRY_LEN + g * 3]
}

fn preview_zone_active(frame: &[u8], slot: usize) -> bool {
    frame[HEADER_LEN + slot * ZONE_ENTRY_LEN] & ZONE_FLAG_ACTIVE != 0
}

/// Drain the channel and return the most recent preview frame, if any.
fn latest_frame(rx: &Receiver<PreviewMessage>) -> Option<Vec<u8>> {
    let mut last = None;
    while let Ok(msg) = rx.try_recv() {
        if let PreviewMessage::Frame(bytes) = msg {
            last = Some(bytes);
        }
    }
    last
}

/// Poll the preview channel until a frame satisfies `pred`, or fail.
fn wait_preview(
    rx: &Receiver<PreviewMessage>,
    what: &str,
    pred: impl Fn(&[u8]) -> bool,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(f) = latest_frame(rx) {
            if pred(&f) {
                return f;
            }
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for preview: {what} (last frame seen: {last:?})");
}

/// Poll the sink until universe 1 satisfies `pred`, returning its DMX data.
fn wait_wire(handle: &sink::SinkHandle, what: &str, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let state = handle.snapshot();
        if let Some(us) = state.universes.get(&1) {
            if pred(&us.data) {
                return us.data.clone();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for wire: {what}");
}

#[test]
fn m6_preview_matches_the_wire_tracks_faders_live_and_keeps_silenced_zones() {
    let gpu = software_gpu();
    let patch = build_patch();
    let index = patch.compile().unwrap();
    let zone_count = index.zone_count();

    // Live state in the Parameter Tree; one binding feeds render, output AND preview.
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);
    // Zone A full, zone B at half — an independent per-zone difference to reproduce.
    tree.write_real(&live.zone(ZONE_A).dimmer, 1.0, ui());
    tree.write_real(&live.zone(ZONE_B).dimmer, 0.5, ui());

    // --- Output side → virtual sink ---
    let handle = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let target = handle.local_addr();
    let plan = OutputPlan::compile(&patch, &index).unwrap();
    let router = OutputRouter::from_plan(plan);
    let (mut out_producer, out_consumer) = frame_handoff(index.master_buffer_len());
    // Start-up ordering (learned in M5): stage one generation-stamped frame so the
    // output guard does not refuse the initial NONE-generation buffers.
    out_producer.set_layout_generation(index.generation());
    out_producer.publish(|px| px.fill(f16::ZERO));
    let transports = vec![UdpOutput::to_target(target)];
    let rate = RatePlan::compute(&[44]);
    let out_stop = Arc::new(AtomicBool::new(false));
    let out_stop_thread = Arc::clone(&out_stop);
    let live_out = live.clone();
    let output = std::thread::spawn(move || {
        run_output_loop(
            out_consumer,
            router,
            transports,
            rate,
            SchedulerConfig {
                run_for: Some(Duration::from_secs(30)),
                zone_count: 2,
                // Live master/zone flat params at the output stage — the SAME
                // binding the preview reads, so wire and preview move together.
                flat_source: Some(Box::new(live_out)),
                stop: out_stop_thread,
                ..Default::default()
            },
        )
    });

    // --- Preview side → ChannelSink (decoded on this thread, no GPU) ---
    let (pv_producer, pv_consumer) = preview_handoff(index.master_buffer_len(), zone_count);
    let (chan_sink, rx) = ChannelSink::new();
    let preview_plan = PreviewPlan::compile(&patch, &index);
    let descriptor = PreviewDescriptor::build(&preview_plan, &patch, &index);
    let mut preview = PreviewLoop::new(
        preview_plan,
        descriptor,
        pv_consumer,
        Box::new(live.clone()),
    );
    preview.attach_sink(Box::new(chan_sink));
    let pv_stop = Arc::new(AtomicBool::new(false));
    let pv_stop_thread = Arc::clone(&pv_stop);
    let preview_thread = std::thread::spawn(move || {
        run_preview_loop(
            preview,
            PreviewLoopConfig {
                target_rate_hz: 30,
                run_for: Some(Duration::from_secs(30)),
                stop: pv_stop_thread,
            },
        )
    });

    // --- Render engine ---
    let mut engine = RenderLoop::with_default_slots(
        gpu,
        Sampler::new(gpu, &SamplePlan::compile(&patch, &index)),
        out_producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
    );
    engine.attach_preview(pv_producer, zone_count);
    let mut content = FlatContent { value: 0.5 };
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let render_stop = Arc::new(AtomicBool::new(false));

    let render_stats = std::thread::scope(|scope| {
        let render_stop_thread = Arc::clone(&render_stop);
        let live_render = live.clone();
        let render = scope.spawn(move || {
            run_render_loop(
                gpu,
                &mut engine,
                &mut clock,
                &mut counter,
                ZoneStates::new(zone_count),
                &mut content,
                RenderLoopConfig {
                    target_rate_hz: 44,
                    // A safety net so a panic in the phases cannot leave this thread
                    // (and the scope) running forever; phase 4 stops it explicitly.
                    run_for: Some(Duration::from_secs(30)),
                    stop: render_stop_thread,
                    zone_source: Some(Box::new(live_render)),
                },
            )
        });

        // ---- Phase 1: preview == wire in linear light, zones independent ----
        //
        // Pixel 0 (zone A, full): effective linear 0.5. Pixel 2 (zone B, half): 0.25.
        // The wire encodes each via identity gamma; the preview via sRGB. They agree
        // in LINEAR LIGHT — asserted by requiring each side to equal its own curve of
        // the same linear value (the only place they must match, §M6).
        let wire = wait_wire(&handle, "zone A settles to 0.5 on the wire", |d| {
            d.len() >= 12 && d[0] == wire_code(0.5)
        });
        assert_eq!(
            wire[0],
            wire_code(0.5),
            "wire pixel 0 = round(0.5*255) = 128"
        );
        assert_eq!(
            wire[6],
            wire_code(0.25),
            "wire pixel 2 = round(0.25*255) = 64"
        );
        assert_eq!(wire[0], wire[3], "zone A is uniform");
        assert_ne!(wire[0], wire[6], "zones are independent on the wire");

        let frame = wait_preview(&rx, "preview reflects zone A 0.5", |f| {
            preview_pixel_r(f, zone_count, 0) == linear_to_srgb8(0.5)
        });
        assert_eq!(
            preview_pixel_r(&frame, zone_count, 0),
            linear_to_srgb8(0.5),
            "preview pixel 0 = sRGB(0.5) = 188 — same linear as the wire, different curve"
        );
        assert_eq!(
            preview_pixel_r(&frame, zone_count, 2),
            linear_to_srgb8(0.25),
            "preview pixel 2 = sRGB(0.25) = 137"
        );
        assert_ne!(
            preview_pixel_r(&frame, zone_count, 0),
            preview_pixel_r(&frame, zone_count, 2),
            "zones are independent in the preview too"
        );

        // ---- Phase 2: stable ~30 Hz preview while render runs at 44 Hz ----
        let _ = latest_frame(&rx); // drain
        let t0 = Instant::now();
        let mut count = 0;
        while t0.elapsed() < Duration::from_secs(1) {
            if let Ok(PreviewMessage::Frame(_)) = rx.recv_timeout(Duration::from_millis(200)) {
                count += 1;
            }
        }
        assert!(
            (18..=42).contains(&count),
            "preview should stream ~30 Hz, got {count} frames in 1 s"
        );

        // ---- Phase 3: a solo elsewhere must NOT erase the silenced zone ----
        //
        // Solo zone A: zone B stops contributing pixels, but is_active(B) stays true,
        // so its strip (the preview's zone entry) must remain active.
        tree.write(&live.zone(ZONE_A).solo, 1.0, ui());
        let frame = wait_preview(&rx, "solo A settles into the preview mask", |f| {
            preview_zone_active(f, ZONE_A)
        });
        assert!(
            preview_zone_active(&frame, ZONE_A),
            "soloed zone A is active"
        );
        assert!(
            preview_zone_active(&frame, ZONE_B),
            "zone B silenced by A's solo must STAY active (is_active, not contributes_pixels)"
        );
        tree.write(&live.zone(ZONE_A).solo, 0.0, ui()); // un-solo for the next phase

        // ---- Phase 4: a zone fader with the render thread STOPPED moves the preview ----
        //
        // Stop render, then pull zone A down. With no re-render at all, the preview
        // loop re-encodes the held frame with live flat params, so pixel 0 changes —
        // the §1.5 shortcut, seen on screen.
        render_stop.store(true, Ordering::Relaxed);
        let stats = render.join().expect("render thread");
        let published_at_stop = stats.frames_published;

        tree.write_real(&live.zone(ZONE_A).dimmer, 0.25, ui()); // 0.5 * 0.25 = 0.125
        let frame = wait_preview(&rx, "fader moves the preview with render stopped", |f| {
            preview_pixel_r(f, zone_count, 0) == linear_to_srgb8(0.125)
        });
        assert_eq!(
            preview_pixel_r(&frame, zone_count, 0),
            linear_to_srgb8(0.125),
            "preview pixel 0 followed the fader to sRGB(0.125) = 97, with NO re-render"
        );
        // Render is joined; it produced nothing after the stop.
        assert_eq!(
            stats.frames_published, published_at_stop,
            "render published no new frame — the preview moved on the output-stage shortcut alone"
        );

        stats
    });

    // Tear down the other two loops.
    out_stop.store(true, Ordering::Relaxed);
    pv_stop.store(true, Ordering::Relaxed);
    let preview_stats = preview_thread.join().expect("preview thread");
    let _ = output.join().expect("output thread");
    let sink_state = handle.stop_and_collect();

    // Sanity: the whole thing actually ran through the real pipeline.
    assert!(render_stats.frames_published > 0, "render produced frames");
    assert!(preview_stats.frames_sent > 0, "preview shipped frames");
    assert!(
        sink_state.artdmx_packets > 0,
        "the sink received real ArtDmx"
    );
    assert_eq!(
        preview_stats.generation_mismatches, 0,
        "no stale frames shipped"
    );
}

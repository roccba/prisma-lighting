//! `preview-probe` — run the engine with the virtual preview live, and serve it to
//! the browser viewer over WebSocket (CLAUDE.md M6, P4).
//!
//! This is the "see it with your own eyes" tool. The automated tests prove the
//! preview is *correct*; this lets you judge what the tests cannot — whether an
//! effect looks right, whether a fade steps, whether a colour is the one you meant.
//!
//! It wires, by hand (there is no engine-assembly object until the Tauri shell):
//!   GPU → render loop (one animated effect) → preview handoff → preview loop →
//!   WebSocket sink. A small animator drives the master fader in a slow triangle so
//!   you can watch a fade for stepping, and tints each zone differently so per-zone
//!   control is visible.
//!
//! Usage:
//!   cargo run -p prisma-render --bin preview_probe [seconds] [port]
//! then open `tools/preview-viewer.html` in a browser (it defaults to port 8787).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use prisma_core::source::{priority, SourceId};
use prisma_core::time::SystemClock;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot, ParameterTree};
use prisma_output::frame::frame_handoff;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_preview::preview_loop::{run_preview_loop, PreviewLoop, PreviewLoopConfig};
use prisma_preview::{preview_handoff, PreviewDescriptor, PreviewPlan, WebSocketSink};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{run_render_loop, FrameContent, RenderLoop, RenderLoopConfig};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{effects, AdapterPreference, Gpu, LiveParams};
use uuid::Uuid;

/// Three horizontal "tubes" of different lengths, stacked and spaced — a shape you
/// can recognize in the viewer, not a generic grid. Each tube is its own zone.
const TUBES: [u32; 3] = [64, 40, 56];
const ROW_GAP: u32 = 2; // vertical space between tubes, so they read as separate bars
const RATE_HZ: u32 = 44;

fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x9EE7);
    let width = *TUBES.iter().max().unwrap();
    let height = (TUBES.len() as u32 - 1) * ROW_GAP + 1;

    let mut pixels = Vec::new();
    let mut index = 0u32;
    let mut tube_pixel_indices: Vec<Vec<u32>> = Vec::new();
    for (t, &len) in TUBES.iter().enumerate() {
        let mut members = Vec::new();
        let y = t as u32 * ROW_GAP;
        for col in 0..len {
            pixels.push(Pixel {
                index,
                position: glam::Vec3::new(col as f32, y as f32, 0.0),
                universe: 1 + t as u16,
                start_channel: (col * 3 + 1) as u16,
                color_order: ColorOrder::Rgb,
            });
            members.push(index);
            index += 1;
        }
        tube_pixel_indices.push(members);
    }

    let zones = tube_pixel_indices
        .iter()
        .enumerate()
        .map(|(t, members)| Zone {
            id: Uuid::from_u128(0x2000 + t as u128),
            name: format!("tube {}", t + 1),
            members: members
                .iter()
                .map(|&p| PixelRef {
                    fixture: fixture_id,
                    pixel: p,
                })
                .collect(),
            bounds: None,
            z_order: t as i32,
            blend_mode: BlendMode::Normal,
            mapping_size: [width, height, 1],
            display_color: [128, 128, 128],
            world_transform: None,
        })
        .collect();

    Patch {
        id: Uuid::from_u128(0x9E),
        name: "preview probe rig".into(),
        matrix_size: [width, height, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "tubes".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels,
            zone: Uuid::nil(),
        }],
        zones,
    }
}

/// One animated effect across the whole matrix. Per-zone colour/dimmer is applied
/// later (in the preview's output stage), so each tube shows this same pattern
/// tinted and dimmed differently — visible per-zone control over a shared effect.
struct EffectContent {
    effect: EffectInstance,
    width: u32,
    height: u32,
}

impl FrameContent for EffectContent {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        _zones: &ZoneStates,
        _overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        self.effect
            .render(gpu, clock, &view, self.width, self.height);
    }
}

fn ui() -> SourceId {
    SourceId::new(priority::UI, 1)
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(300);
    let port: u16 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8787);

    // Prefer real hardware for a smoother picture, but lavapipe works everywhere.
    let gpu = Gpu::new(AdapterPreference::HighPerformance)
        .or_else(|_| Gpu::new(AdapterPreference::Software))
        .expect("a Vulkan adapter (hardware or lavapipe) is required");
    println!(
        "adapter: {} [{:?}]",
        gpu.info().name,
        gpu.info().device_type
    );

    let patch = build_patch();
    let index = patch.compile().expect("probe rig compiles");
    let tree = ParameterTree::with_defaults();
    let live = LiveParams::register(&tree, &patch);
    let tree = Arc::new(tree);

    // Handoffs: the render loop publishes to both. We never run an output loop
    // here, so the output handoff simply drops its frames (newest-wins) — the
    // producer never blocks. The preview handoff is what we serve.
    let (out_producer, _out_consumer) = frame_handoff(index.master_buffer_len());
    let (pv_producer, pv_consumer) = preview_handoff(index.master_buffer_len(), index.zone_count());

    // Render loop: one plasma effect, publishing to the preview.
    let mut engine = RenderLoop::with_default_slots(
        &gpu,
        Sampler::new(&gpu, &SamplePlan::compile(&patch, &index)),
        out_producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
    );
    engine.attach_preview(pv_producer, index.zone_count());
    let mut content = EffectContent {
        effect: EffectInstance::new(&gpu, &tree, "/probe/plasma", &effects::plasma()),
        width: patch.matrix_size[0],
        height: patch.matrix_size[1],
    };

    // Preview transport: WebSocket for the browser viewer.
    let sink = WebSocketSink::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
        .expect("bind preview WebSocket");
    let bound = sink.local_addr();
    let plan = PreviewPlan::compile(&patch, &index);
    let descriptor = PreviewDescriptor::build(&plan, &patch, &index);
    let mut preview = PreviewLoop::new(plan, descriptor, pv_consumer, Box::new(live.clone()));
    preview.attach_sink(Box::new(sink));

    println!();
    println!(
        "  preview rig: {} pixels in {} tubes, matrix {}x{}",
        index.pixel_count(),
        TUBES.len(),
        patch.matrix_size[0],
        patch.matrix_size[1]
    );
    println!("  serving preview at   ws://127.0.0.1:{}", bound.port());
    println!("  to watch it:         open  tools/preview-viewer.html  in a browser");
    if bound.port() != 8787 {
        println!(
            "                       (set its port field to {})",
            bound.port()
        );
    }
    println!("  running {seconds} s (Ctrl-C to stop early)");
    println!();

    let stop = Arc::new(AtomicBool::new(false));

    // Animator: master fader in a slow triangle (watch it for stepping), and a
    // fixed distinct tint per tube (watch per-zone control). All through the
    // Parameter Tree, exactly as a MIDI fader or the UI would.
    let tints = [[1.0, 0.2, 0.2], [0.2, 1.0, 0.2], [0.2, 0.4, 1.0]];
    for (slot, tint) in tints.iter().enumerate().take(index.zone_count()) {
        tree.write_real(&live.zone(slot).dimmer, 1.0, ui());
        let z = live.zone(slot);
        tree.write_real(&z.color[0], tint[0], ui());
        tree.write_real(&z.color[1], tint[1], ui());
        tree.write_real(&z.color[2], tint[2], ui());
    }
    let animator = {
        let tree = Arc::clone(&tree);
        let live = live.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let start = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                // 6-second triangle 0→1→0.
                let phase = (start.elapsed().as_secs_f32() / 3.0) % 2.0;
                let level = if phase < 1.0 { phase } else { 2.0 - phase };
                tree.write_real(&live.master().dimmer, level, ui());
                std::thread::sleep(Duration::from_millis(16));
            }
        })
    };

    // Preview loop on its own thread (normal priority — see its module docs).
    let preview_thread = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            run_preview_loop(
                preview,
                PreviewLoopConfig {
                    target_rate_hz: 30,
                    run_for: Some(Duration::from_secs(seconds)),
                    stop,
                },
            )
        })
    };

    // Render loop on the main thread.
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(index.zone_count());
    for slot in 0..index.zone_count() {
        zones.set_dimmer(slot, 1.0);
    }
    let render_stats = run_render_loop(
        &gpu,
        &mut engine,
        &mut clock,
        &mut counter,
        zones,
        &mut content,
        RenderLoopConfig {
            target_rate_hz: RATE_HZ,
            run_for: Some(Duration::from_secs(seconds)),
            stop: Arc::clone(&stop),
            zone_source: Some(Box::new(live.clone())),
            freeze: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    stop.store(true, Ordering::Relaxed);
    let preview_stats = preview_thread.join().expect("preview thread");
    animator.join().expect("animator thread");

    println!("\n--- done ---");
    println!(
        "  render: {} frames published, {} stalls",
        render_stats.frames_published, render_stats.readback_stalls
    );
    println!(
        "  preview: {} ticks, {} frames encoded, {} sent, {} skipped (no consumer)",
        preview_stats.ticks,
        preview_stats.frames_encoded,
        preview_stats.frames_sent,
        preview_stats.skipped_no_consumer
    );
    println!("  (skipped == total means nobody opened the viewer — that path costs zero)");
}

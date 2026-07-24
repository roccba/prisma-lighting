//! `render-probe` — measures the render leg of the §1.5 latency budget on real
//! hardware, and chains it with M1's output-side measurement to report the full
//! **tick → wire** path.
//!
//! Run this on a real GPU. On a software adapter (lavapipe) the numbers are
//! meaningless for the budget: it is a CPU rasterizer, so its "readback" never
//! crosses a bus. Correctness is what lavapipe is for; this binary is for timing.
//!
//! What it reports:
//!   - **render**        frame start → submit (CPU cost of building the frame)
//!   - **readback**      submit → readback observed complete (the §1.5 metric)
//!   - **tick→publish**  frame start → frame handed to the output stage
//!   - **produced→wire** M1's output-side number, for the same run
//!   - **tick→wire**     the two legs added: the number the 50 ms budget is about

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use prisma_core::time::SystemClock;
use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, MasterClock, OverlapSnapshot, ParameterTree};
use prisma_output::frame::frame_handoff;
use prisma_output::plan::OutputPlan;
use prisma_output::rate::RatePlan;
use prisma_output::router::OutputRouter;
use prisma_output::scheduler::{run_output_loop, SchedulerConfig};
use prisma_output::transport::UdpOutput;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{
    run_render_loop, FrameContent, LatencyStats, RenderLoop, RenderLoopConfig,
};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{effects, AdapterPreference, Gpu};
use uuid::Uuid;

/// A rig big enough to be representative: 4 full universes of RGB pixels.
const PIXELS_PER_UNIVERSE: u32 = 170; // 170 * 3 = 510 channels
const UNIVERSES: u32 = 4;
const MATRIX_W: u32 = 64;
const RATE_HZ: u32 = 44;

fn build_patch() -> Patch {
    let total = PIXELS_PER_UNIVERSE * UNIVERSES;
    let fixture_id = Uuid::from_u128(0xBEEF);
    let matrix_h = total.div_ceil(MATRIX_W);
    Patch {
        id: Uuid::from_u128(1),
        name: "probe rig".into(),
        matrix_size: [MATRIX_W, matrix_h, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: (0..total)
                .map(|i| Pixel {
                    index: i,
                    position: glam::Vec3::new((i % MATRIX_W) as f32, (i / MATRIX_W) as f32, 0.0),
                    universe: (i / PIXELS_PER_UNIVERSE) as u16 + 1,
                    start_channel: ((i % PIXELS_PER_UNIVERSE) * 3 + 1) as u16,
                    color_order: ColorOrder::Rgb,
                })
                .collect(),
            zone: Uuid::nil(),
        }],
        zones: vec![Zone {
            id: Uuid::from_u128(2),
            name: "all".into(),
            members: (0..total)
                .map(|p| PixelRef {
                    fixture: fixture_id,
                    pixel: p,
                })
                .collect(),
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [MATRIX_W, matrix_h, 1],
            display_color: [128, 128, 128],
            world_transform: None,
        }],
    }
}

/// One real effect pass into the framebuffer, so the measurement includes actual
/// render work rather than a bare texture upload.
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

fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn line(label: &str, s: &LatencyStats) {
    if s.is_empty() {
        println!("  {label:<16} (no samples)");
        return;
    }
    println!(
        "  {label:<16} mean {:>7.3} ms   min {:>7.3} ms   max {:>7.3} ms   n={}",
        ms(s.mean_ns()),
        ms(s.min_ns),
        ms(s.max_ns),
        s.count
    );
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(5);

    let gpu = match Gpu::new(AdapterPreference::HighPerformance) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("no hardware adapter: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "adapter: {} [{:?}] backend={:?}",
        gpu.info().name,
        gpu.info().device_type,
        gpu.info().backend
    );
    if gpu.is_software() {
        println!("WARNING: software adapter — these timings say nothing about the §1.5 budget.");
    }

    let patch = build_patch();
    let index = patch.compile().expect("probe patch compiles");
    println!(
        "rig: {} pixels, {} components, matrix {}x{}, {} universes @ {} Hz, {} s",
        index.pixel_count(),
        index.master_buffer_len(),
        patch.matrix_size[0],
        patch.matrix_size[1],
        UNIVERSES,
        RATE_HZ,
        seconds
    );

    // --- Output side: the real fixed-interval loop, to loopback. ---
    let plan = OutputPlan::compile(&patch, &index).expect("plan compiles");
    let router = OutputRouter::from_plan(plan);
    let (producer, consumer) = frame_handoff(index.master_buffer_len());
    let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454);
    let transports: Vec<UdpOutput> = (0..UNIVERSES)
        .map(|_| UdpOutput::to_target(target))
        .collect();
    let rate = RatePlan::compute(&vec![RATE_HZ; UNIVERSES as usize]);
    let output = std::thread::spawn(move || {
        run_output_loop(
            consumer,
            router,
            transports,
            rate,
            SchedulerConfig {
                run_for: Some(Duration::from_secs(seconds)),
                zone_count: 1,
                ..Default::default()
            },
        )
    });

    // --- Render side ---
    let tree = ParameterTree::with_defaults();
    let mut content = EffectContent {
        effect: EffectInstance::new(&gpu, &tree, "/probe/gradient", &effects::gradient()),
        width: patch.matrix_size[0],
        height: patch.matrix_size[1],
    };
    let sample_plan = SamplePlan::compile(&patch, &index);
    let sampler = Sampler::new(&gpu, &sample_plan);
    let mut engine = RenderLoop::with_default_slots(
        &gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
    );
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(1);
    zones.set_dimmer(0, 1.0);

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
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
        },
    );
    let output_report = output.join().expect("output thread");

    println!("\n--- render side ---");
    println!(
        "  frames rendered {}   published {}   stalls {}   failures {}",
        render_stats.frames_rendered,
        render_stats.frames_published,
        render_stats.readback_stalls,
        render_stats.readback_failures
    );
    line("render", &render_stats.render);
    line("readback", &render_stats.readback);
    line("tick->publish", &render_stats.tick_to_publish);

    println!("\n--- output side (M1) ---");
    println!(
        "  mode {:?}   ticks {}   resends {}   late {}   drops {}",
        output_report.mode,
        output_report.sample_count,
        output_report.resends,
        output_report.late_frames,
        output_report.drops
    );
    println!(
        "  produced->wire   mean {:>7.3} ms   max {:>7.3} ms",
        ms(output_report.latency_mean_ns),
        ms(output_report.latency_max_ns)
    );
    println!(
        "  inter-packet jitter (stddev) {:.3} ms",
        ms(output_report.jitter_stddev_ns)
    );

    println!("\n--- tick -> wire (the §1.5 budget: < 50 ms) ---");
    let mean_total = ms(render_stats.tick_to_publish.mean_ns()) + ms(output_report.latency_mean_ns);
    let max_total = ms(render_stats.tick_to_publish.max_ns) + ms(output_report.latency_max_ns);
    println!("  mean {mean_total:>7.3} ms      worst-case sum {max_total:>7.3} ms");
    println!(
        "  verdict: {}",
        if mean_total < 50.0 {
            "within budget"
        } else {
            "OVER BUDGET"
        }
    );
    let _ = &tree;
    std::process::exit(0);
}

//! `scale-probe` — measure the pipeline across the scale curve before M7.
//!
//! The spec ceiling ("thousands of pixels") is too low. This walks three points so
//! the trend of each segment is visible, not just one number:
//!
//!   -  ~5,000 px /  30 universes  — a small install
//!   - ~18,000 px / 106 universes  — the real target rig (300 tubes × 60 px)
//!   - ~43,000 px / 256 universes  — the industry ceiling (max DMX universes)
//!
//! For each it runs the whole render → output pipeline on real hardware and reports
//! render/readback/tick→publish, output scheduling (jitter, late, drops, produced→
//! wire) with all universes on ONE base tick, the **output thread's CPU**, and the
//! overlap counter's solo-toggle cost. A trend table at the end shows how each
//! segment actually scales.
//!
//! Run on a real GPU with `--release`. Usage:
//!   cargo run -p prisma-render --bin scale_probe --release [seconds_per_point]

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch,
    PatchIndex, Pixel, PixelRef, Zone,
};
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::OverlapCounter;
use prisma_render::render_loop::{
    run_render_loop, FrameContent, LatencyStats, RenderLoop, RenderLoopConfig,
};
use prisma_render::sample::{SamplePlan, Sampler};
use prisma_render::{effects, AdapterPreference, Gpu};
use uuid::Uuid;

const WIDTH: u32 = 60; // pixels per row (a "tube")
const SECTORS: u32 = 15; // sector zones, plus one global strobe zone
const PX_PER_UNIVERSE: u32 = 170; // 170 RGB = 510 channels
const RATE_HZ: u32 = 44;

/// The scale curve, in pixels. 30 / 106 / 256 universes.
const SCALES: [u32; 3] = [5040, 18000, 43520];

/// A 60-wide matrix of `pixels` LEDs (one row per "tube"), grouped into 15 sector
/// zones by row-range plus a global strobe zone (Add) covering everything.
fn build_patch(pixels: u32) -> Patch {
    let fixture_id = Uuid::from_u128(0x5CA1E);
    let height = pixels.div_ceil(WIDTH);

    let pixel_defs: Vec<Pixel> = (0..pixels)
        .map(|i| Pixel {
            index: i,
            position: glam::Vec3::new((i % WIDTH) as f32, (i / WIDTH) as f32, 0.0),
            universe: (i / PX_PER_UNIVERSE) as u16 + 1,
            start_channel: ((i % PX_PER_UNIVERSE) * 3 + 1) as u16,
            color_order: ColorOrder::Rgb,
        })
        .collect();

    let member = |p: u32| PixelRef {
        fixture: fixture_id,
        pixel: p,
    };

    // Sector zones by row-range, so each covers a contiguous block of the rig.
    let mut zones: Vec<Zone> = (0..SECTORS)
        .map(|z| {
            let r0 = z * height / SECTORS;
            let r1 = (z + 1) * height / SECTORS;
            let first = r0 * WIDTH;
            let last = (r1 * WIDTH).min(pixels);
            Zone {
                id: Uuid::from_u128(0x3000 + z as u128),
                name: format!("sector {}", z + 1),
                members: (first..last).map(member).collect(),
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
                mapping_size: [WIDTH, height, 1],
                display_color: [128, 128, 128],
                world_transform: None,
            }
        })
        .collect();

    zones.push(Zone {
        id: Uuid::from_u128(0x3FFF),
        name: "strobe".into(),
        members: (0..pixels).map(member).collect(),
        bounds: None,
        z_order: 100,
        blend_mode: BlendMode::Add,
        mapping_size: [WIDTH, height, 1],
        display_color: [128, 128, 128],
        world_transform: None,
    });

    Patch {
        id: Uuid::from_u128(0x5CA1),
        name: "scale rig".into(),
        matrix_size: [WIDTH, height, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "tubes".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: pixel_defs,
            zone: Uuid::nil(),
        }],
        zones,
    }
}

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

/// This thread's consumed CPU time (user + system), in seconds, from
/// `/proc/thread-self/stat`. Linux-only; returns 0.0 elsewhere (reported as n/a).
fn thread_cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/thread-self/stat") else {
        return 0.0;
    };
    // The comm field (2) may contain spaces/parens; the stable fields start after
    // the last ')'. From there: [0]=state(3) … utime is field 14 → index 11,
    // stime field 15 → index 12, both in clock ticks.
    let Some(rparen) = stat.rfind(')') else {
        return 0.0;
    };
    let rest: Vec<&str> = stat[rparen + 1..].split_whitespace().collect();
    let utime: u64 = rest.get(11).and_then(|x| x.parse().ok()).unwrap_or(0);
    let stime: u64 = rest.get(12).and_then(|x| x.parse().ok()).unwrap_or(0);
    const CLK_TCK: f64 = 100.0; // _SC_CLK_TCK, standard on Linux
    (utime + stime) as f64 / CLK_TCK
}

/// Everything one scale point contributes to the trend table.
struct ScaleResult {
    pixels: u32,
    universes: usize,
    render_mean_ns: u64,
    readback_mean_ns: u64,
    readback_max_ns: u64,
    ttp_mean_ns: u64,
    ttw_mean_ms: f64,
    ttw_max_ms: f64,
    jitter_ns: u64,
    late: usize,
    drops: usize,
    out_cpu_pct: f64,
    pkts_recv: u64,
    pkts_expected: u64,
    solo_on_us: f64,
}

/// Measure the overlap counter's solo-toggle cost: pressing solo flips every
/// non-soloed sector at once (§5.2.1); the snapshot rebuild is O(pixels+components).
fn measure_overlap_toggle(patch: &Patch, index: &PatchIndex) -> (f64, f64, f64, usize) {
    let zone_count = index.zone_count();
    let sectors = zone_count - 1;
    let mut counter = OverlapCounter::new(patch, index);
    let mut zones = ZoneStates::new(zone_count);
    for z in 0..sectors {
        zones.set_dimmer(z, 1.0);
    }
    counter.sync(&zones);
    let _ = counter.snapshot();

    let t = Instant::now();
    for _ in 0..1000 {
        counter.sync(&zones);
        let _ = counter.snapshot();
    }
    let steady_us = t.elapsed().as_nanos() as f64 / 1000.0 / 1000.0;

    let reps = 200;
    let (mut on_total, mut off_total) = (Duration::ZERO, Duration::ZERO);
    let mut flipped = 0;
    for _ in 0..reps {
        zones.set_solo(0, true);
        let t = Instant::now();
        flipped = counter.sync(&zones);
        let _ = counter.snapshot();
        on_total += t.elapsed();

        zones.set_solo(0, false);
        let t = Instant::now();
        counter.sync(&zones);
        let _ = counter.snapshot();
        off_total += t.elapsed();
    }
    let on_us = (on_total / reps).as_nanos() as f64 / 1000.0;
    let off_us = (off_total / reps).as_nanos() as f64 / 1000.0;
    (steady_us, on_us, off_us, flipped)
}

fn run_one(gpu: &Gpu, pixels: u32, seconds: u64) -> ScaleResult {
    let patch = build_patch(pixels);
    let index = patch.compile().expect("scale rig compiles");
    let plan = OutputPlan::compile(&patch, &index).expect("plan compiles");
    let universes = plan.universe_count();
    let zone_count = index.zone_count();

    println!(
        "\n════ {} px / {} universes  (matrix {}x{}, {} zones, {} Hz, {} s) ════",
        index.pixel_count(),
        universes,
        patch.matrix_size[0],
        patch.matrix_size[1],
        zone_count,
        RATE_HZ,
        seconds
    );

    // --- Output side: real fixed-interval loop, all universes to one sink. ---
    let sink = sink::spawn(SocketAddr::from(([127, 0, 0, 1], 0))).expect("sink binds");
    let target = sink.local_addr();
    let router = OutputRouter::from_plan(plan);
    let (producer, consumer) = frame_handoff(index.master_buffer_len());
    let transports: Vec<UdpOutput> = (0..universes)
        .map(|_| UdpOutput::to_target(target))
        .collect();
    let rate = RatePlan::compute(&vec![RATE_HZ; universes]);
    let output = std::thread::spawn(move || {
        let cpu0 = thread_cpu_seconds();
        let w0 = Instant::now();
        let report = run_output_loop(
            consumer,
            router,
            transports,
            rate,
            SchedulerConfig {
                run_for: Some(Duration::from_secs(seconds)),
                zone_count,
                ..Default::default()
            },
        );
        let wall = w0.elapsed().as_secs_f64();
        let cpu_pct = if wall > 0.0 {
            (thread_cpu_seconds() - cpu0) / wall * 100.0
        } else {
            0.0
        };
        (report, cpu_pct)
    });

    // --- Render side ---
    let tree = ParameterTree::with_defaults();
    let mut content = EffectContent {
        effect: EffectInstance::new(gpu, &tree, "/scale/plasma", &effects::plasma()),
        width: patch.matrix_size[0],
        height: patch.matrix_size[1],
    };
    let sampler = Sampler::new(gpu, &SamplePlan::compile(&patch, &index));
    let mut engine = RenderLoop::with_default_slots(
        gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
    );
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(zone_count);
    for z in 0..zone_count - 1 {
        zones.set_dimmer(z, 1.0); // sectors up; strobe down (shortcut applies)
    }

    let render_stats = run_render_loop(
        gpu,
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
    let (report, out_cpu_pct) = output.join().expect("output thread");
    let sink_state = sink.stop_and_collect();

    println!(
        "  published {}   stalls {}",
        render_stats.frames_published, render_stats.readback_stalls
    );
    line("render", &render_stats.render);
    line("readback", &render_stats.readback);
    line("tick->publish", &render_stats.tick_to_publish);

    let expected = report.sample_count as u64 * universes as u64;
    println!(
        "  output: mode {:?}  late {}  drops {}  jitter σ {:.3} ms  CPU {:.1}%",
        report.mode,
        report.late_frames,
        report.drops,
        ms(report.jitter_stddev_ns),
        out_cpu_pct
    );
    println!(
        "  produced->wire mean {:.3} ms  max {:.3} ms   sink recv {}/{} pkts across {} univ",
        ms(report.latency_mean_ns),
        ms(report.latency_max_ns),
        sink_state.artdmx_packets,
        expected,
        sink_state.universes.len()
    );

    let ttw_mean = ms(render_stats.tick_to_publish.mean_ns()) + ms(report.latency_mean_ns);
    let ttw_max = ms(render_stats.tick_to_publish.max_ns) + ms(report.latency_max_ns);
    println!(
        "  tick->wire mean {ttw_mean:.3} ms   worst-case sum {ttw_max:.3} ms   (budget 50 ms)"
    );

    let (steady_us, solo_on_us, solo_off_us, flipped) = measure_overlap_toggle(&patch, &index);
    println!(
        "  overlap: steady {steady_us:.1} µs   solo ON ({flipped} zones) {solo_on_us:.1} µs   solo OFF {solo_off_us:.1} µs   (frame budget {:.0} µs)",
        1_000_000.0 / RATE_HZ as f64
    );

    ScaleResult {
        pixels: index.pixel_count(),
        universes,
        render_mean_ns: render_stats.render.mean_ns(),
        readback_mean_ns: render_stats.readback.mean_ns(),
        readback_max_ns: render_stats.readback.max_ns,
        ttp_mean_ns: render_stats.tick_to_publish.mean_ns(),
        ttw_mean_ms: ttw_mean,
        ttw_max_ms: ttw_max,
        jitter_ns: report.jitter_stddev_ns,
        late: report.late_frames,
        drops: report.drops as usize,
        out_cpu_pct,
        pkts_recv: sink_state.artdmx_packets,
        pkts_expected: expected,
        solo_on_us,
    }
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);

    let gpu = Gpu::new(AdapterPreference::HighPerformance)
        .or_else(|_| Gpu::new(AdapterPreference::Software))
        .expect("a Vulkan adapter is required");
    println!(
        "adapter: {} [{:?}]",
        gpu.info().name,
        gpu.info().device_type
    );
    if gpu.is_software() {
        println!("WARNING: software adapter — latency numbers do not reflect the §1.5 budget.");
    }

    let results: Vec<ScaleResult> = SCALES
        .iter()
        .map(|&px| run_one(&gpu, px, seconds))
        .collect();

    println!("\n════ trend (how each segment actually scales) ════");
    println!(
        "  {:>7} {:>5} | {:>9} {:>10} {:>8} | {:>9} {:>9} {:>8} {:>5} {:>5} {:>7} | {:>9}",
        "pixels",
        "univ",
        "render ms",
        "readbk ms",
        "ttp ms",
        "ttw mean",
        "ttw max",
        "jit µs",
        "late",
        "drop",
        "out CPU",
        "solo ON µs"
    );
    for r in &results {
        let drop_note = if r.pkts_recv < r.pkts_expected {
            "*" // sink-side receive shortfall, not a sender drop
        } else {
            " "
        };
        println!(
            "  {:>7} {:>5} | {:>9.3} {:>10.3} {:>8.3} | {:>8.2}m {:>8.2}m {:>8.0} {:>5} {:>4}{} {:>6.1}% | {:>9.1}",
            r.pixels,
            r.universes,
            ms(r.render_mean_ns),
            ms(r.readback_mean_ns),
            ms(r.ttp_mean_ns),
            r.ttw_mean_ms,
            r.ttw_max_ms,
            r.jitter_ns as f64 / 1000.0,
            r.late,
            r.drops,
            drop_note,
            r.out_cpu_pct,
            r.solo_on_us,
        );
    }
    println!(
        "  readback max at top scale: {:.3} ms",
        ms(results.last().unwrap().readback_max_ns)
    );
    println!("  (* = sink received fewer packets than sent: a receiver-side drain limit, not a sender drop)");
}

//! `scale-probe` — measure the pipeline at the real target scale before M7.
//!
//! The design ceiling in the spec said "thousands of pixels"; the real rig is
//! bigger: 300 tubes of 60 pixels = 18,000 RGB pixels ≈ 106 DMX universes. This
//! binary builds that synthetic rig with realistic zoning and runs the whole
//! render → output pipeline on real hardware, reporting:
//!
//!   - render / readback / tick→publish  (the §1.5 render leg)
//!   - output scheduling mode, jitter, produced→wire, late frames, drops, resends
//!     with ~106 universes on ONE base tick (the M1 "one slow send delays the rest"
//!     question)
//!   - the overlap counter's solo-toggle cost at 18k pixels (27× the 680px baseline)
//!
//! Run on a real GPU with `--release`. Usage:
//!   cargo run -p prisma-render --bin scale_probe --release [seconds]

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

const TUBES: u32 = 300;
const PX_PER_TUBE: u32 = 60;
const TUBES_PER_ZONE: u32 = 20; // 15 sector zones
const PX_PER_UNIVERSE: u32 = 170; // 170 RGB = 510 channels
const RATE_HZ: u32 = 44;

fn total_pixels() -> u32 {
    TUBES * PX_PER_TUBE
}

/// 300 tubes × 60 px on a 60×300 matrix (one row per tube). 15 sector zones of 20
/// tubes each, plus a global strobe zone (Add, on top) covering everything.
fn build_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x5CA1E);
    let total = total_pixels();

    let pixels: Vec<Pixel> = (0..total)
        .map(|i| {
            let tube = i / PX_PER_TUBE;
            let col = i % PX_PER_TUBE;
            Pixel {
                index: i,
                position: glam::Vec3::new(col as f32, tube as f32, 0.0),
                universe: (i / PX_PER_UNIVERSE) as u16 + 1,
                start_channel: ((i % PX_PER_UNIVERSE) * 3 + 1) as u16,
                color_order: ColorOrder::Rgb,
            }
        })
        .collect();

    let member = |p: u32| PixelRef {
        fixture: fixture_id,
        pixel: p,
    };

    // Sector zones: consecutive groups of tubes.
    let sector_count = TUBES / TUBES_PER_ZONE;
    let mut zones: Vec<Zone> = (0..sector_count)
        .map(|z| {
            let first = z * TUBES_PER_ZONE * PX_PER_TUBE;
            let last = first + TUBES_PER_ZONE * PX_PER_TUBE;
            Zone {
                id: Uuid::from_u128(0x3000 + z as u128),
                name: format!("sector {}", z + 1),
                members: (first..last).map(member).collect(),
                bounds: None,
                z_order: 0,
                blend_mode: BlendMode::Normal,
            }
        })
        .collect();

    // Global strobe: covers every pixel, sits on top, adds.
    zones.push(Zone {
        id: Uuid::from_u128(0x3FFF),
        name: "strobe".into(),
        members: (0..total).map(member).collect(),
        bounds: None,
        z_order: 100,
        blend_mode: BlendMode::Add,
    });

    Patch {
        id: Uuid::from_u128(0x5CA1),
        name: "scale rig".into(),
        matrix_size: [PX_PER_TUBE, TUBES, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "tubes".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels,
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

/// Measure the overlap counter's solo-toggle cost at full scale: pressing solo
/// flips every non-soloed sector at once (§5.2.1), then the snapshot rebuild is
/// O(pixels + components). Reported separately from render because it is the cost
/// that scales 27× from the 680px baseline.
fn measure_overlap_toggle(patch: &Patch, index: &prisma_patch::PatchIndex) {
    let zone_count = index.zone_count();
    let sectors = zone_count - 1; // last is strobe
    let mut counter = OverlapCounter::new(patch, index);
    let mut zones = ZoneStates::new(zone_count);
    for z in 0..sectors {
        zones.set_dimmer(z, 1.0); // sectors up (contributing)
    }
    // strobe left down: the common case, so singly-covered pixels take the shortcut.
    counter.sync(&zones);
    let _ = counter.snapshot();

    // Steady state: nothing flips, so a snapshot is a refcount bump.
    let t = Instant::now();
    for _ in 0..1000 {
        counter.sync(&zones);
        let _ = counter.snapshot();
    }
    let steady = t.elapsed() / 1000;

    // Solo toggle: flip solo on sector 0 (silences the other 14 sectors), then off.
    let reps = 200;
    let mut on_total = Duration::ZERO;
    let mut off_total = Duration::ZERO;
    let mut flipped_on = 0;
    for _ in 0..reps {
        zones.set_solo(0, true);
        let t = Instant::now();
        flipped_on = counter.sync(&zones);
        let _ = counter.snapshot();
        on_total += t.elapsed();

        zones.set_solo(0, false);
        let t = Instant::now();
        counter.sync(&zones);
        let _ = counter.snapshot();
        off_total += t.elapsed();
    }

    println!("\n--- overlap counter (18k pixels, {zone_count} zones) ---");
    println!(
        "  steady-state snapshot (no flip)   {:>8.1} µs",
        steady.as_nanos() as f64 / 1000.0
    );
    println!(
        "  solo ON  (flips {flipped_on} zones)          {:>8.1} µs   (sync + snapshot)",
        (on_total / reps).as_nanos() as f64 / 1000.0
    );
    println!(
        "  solo OFF (un-solo)                {:>8.1} µs   (sync + snapshot)",
        (off_total / reps).as_nanos() as f64 / 1000.0
    );
    let frame_budget_us = 1_000_000.0 / RATE_HZ as f64;
    println!(
        "  render frame budget @ {RATE_HZ} Hz         {frame_budget_us:>8.1} µs   (toggle must fit in one)"
    );
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);

    let gpu = Gpu::new(AdapterPreference::HighPerformance)
        .or_else(|_| Gpu::new(AdapterPreference::Software))
        .expect("a Vulkan adapter is required");
    let software = gpu.is_software();
    println!(
        "adapter: {} [{:?}]",
        gpu.info().name,
        gpu.info().device_type
    );
    if software {
        println!("WARNING: software adapter — latency numbers do not reflect the §1.5 budget.");
    }

    let patch = build_patch();
    let index = patch.compile().expect("scale rig compiles");
    let plan = OutputPlan::compile(&patch, &index).expect("plan compiles");
    let universes = plan.universe_count();
    println!(
        "rig: {} pixels, {} components, matrix {}x{}, {} zones, {} universes @ {} Hz, {} s",
        index.pixel_count(),
        index.master_buffer_len(),
        patch.matrix_size[0],
        patch.matrix_size[1],
        index.zone_count(),
        universes,
        RATE_HZ,
        seconds
    );

    // --- Output side: the real fixed-interval loop, all universes to one sink. ---
    let zone_count = index.zone_count();
    let sink = sink::spawn(SocketAddr::from(([127, 0, 0, 1], 0))).expect("sink binds");
    let target = sink.local_addr();
    let router = OutputRouter::from_plan(plan);
    let (producer, consumer) = frame_handoff(index.master_buffer_len());
    let transports: Vec<UdpOutput> = (0..universes)
        .map(|_| UdpOutput::to_target(target))
        .collect();
    let rate = RatePlan::compute(&vec![RATE_HZ; universes]);
    let output = std::thread::spawn(move || {
        run_output_loop(
            consumer,
            router,
            transports,
            rate,
            SchedulerConfig {
                run_for: Some(Duration::from_secs(seconds)),
                zone_count,
                ..Default::default()
            },
        )
    });

    // --- Render side ---
    let tree = ParameterTree::with_defaults();
    let mut content = EffectContent {
        effect: EffectInstance::new(&gpu, &tree, "/scale/plasma", &effects::plasma()),
        width: patch.matrix_size[0],
        height: patch.matrix_size[1],
    };
    let sampler = Sampler::new(&gpu, &SamplePlan::compile(&patch, &index));
    let mut engine = RenderLoop::with_default_slots(
        &gpu,
        sampler,
        producer,
        (patch.matrix_size[0], patch.matrix_size[1]),
        index.master_buffer_len(),
    );
    let mut clock = MasterClock::new(Arc::new(SystemClock::new()));
    let mut counter = OverlapCounter::new(&patch, &index);
    let mut zones = ZoneStates::new(index.zone_count());
    for z in 0..index.zone_count() - 1 {
        zones.set_dimmer(z, 1.0); // sectors up; strobe (last) down
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
            stop: Arc::new(AtomicBool::new(false)),
            zone_source: None,
        },
    );
    let report = output.join().expect("output thread");
    let sink_state = sink.stop_and_collect();

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

    println!("\n--- output side: {universes} universes on ONE base tick ---");
    println!(
        "  mode {:?}   ticks {}   resends {}   late {}   drops {}",
        report.mode, report.sample_count, report.resends, report.late_frames, report.drops
    );
    println!(
        "  produced->wire   mean {:>7.3} ms   max {:>7.3} ms",
        ms(report.latency_mean_ns),
        ms(report.latency_max_ns)
    );
    println!(
        "  inter-packet jitter (stddev) {:.3} ms",
        ms(report.jitter_stddev_ns)
    );
    println!(
        "  sink received: {} ArtDmx packets across {} universes",
        sink_state.artdmx_packets,
        sink_state.universes.len()
    );

    println!("\n--- tick -> wire (the §1.5 budget: < 50 ms) ---");
    let mean_total = ms(render_stats.tick_to_publish.mean_ns()) + ms(report.latency_mean_ns);
    let max_total = ms(render_stats.tick_to_publish.max_ns) + ms(report.latency_max_ns);
    println!("  mean {mean_total:>7.3} ms      worst-case sum {max_total:>7.3} ms");

    measure_overlap_toggle(&patch, &index);
}

//! `show-probe` — the full engine running a real show at the design ceiling.
//!
//! `scale-probe` (prisma-render) measures the render loop with one hand-built effect; it
//! does not go through the per-zone compositor. This probe does: `Engine::start_show`
//! on a show with 15 sector zones plus one global zone (Add) over everything, a plasma
//! track per sector, a wave track across every sector in Add, a solid track on the
//! global zone and a sparkle track confined to a group (every other tube). Everything
//! runs on the real GPU and the real output loop to a local virtual sink; the shutdown
//! report gives the render leg and the output side.
//!
//! Run on a real GPU with `--release`:
//!   cargo run -p prisma-engine --bin show_probe --release [seconds_per_point] [--check]
//!
//! Points: 18,000 px (300 tubes × 60, 106 universes) and 43,500 px (725 tubes, 256).
//!
//! `--check` exits non-zero unless, at both points, the compositor sustains the output
//! rate (frames published ≥ 98 % of 44 Hz × seconds) and produced → wire stays inside
//! the 50 ms budget on every tick. Thresholds and when to run it: LIVE-MODEL-SPEC §8.6.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use glam::Vec3;
use prisma_core::show::{Cell, Destination, Layer, ParamSet, Show, Track};
use prisma_engine::{Engine, EngineConfig};
use prisma_output::sink;
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Group, Patch,
    Pixel, PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use uuid::Uuid;

const TUBE_PX: u32 = 60;
const SECTORS: u32 = 15;
const PX_PER_UNIVERSE: u32 = 170;
const POINTS: [u32; 2] = [300, 725];

fn zone(id: u128, name: &str, rows: u32, z: i32, blend: BlendMode) -> Zone {
    Zone {
        id: Uuid::from_u128(id),
        name: name.into(),
        members: vec![],
        bounds: None,
        z_order: z,
        blend_mode: blend,
        mapping_size: [TUBE_PX, rows.max(1), 1],
        display_color: [128, 128, 128],
        world_transform: None,
    }
}

fn rig(tubes: u32) -> Patch {
    let sector_of = |t: u32| t * SECTORS / tubes;
    let profile = FixtureProfile {
        name: "rgb".into(),
        components: ComponentSet::Rgb,
        gamma: GammaCalibration::default(),
        white: prisma_patch::WhiteProfile::default(),
    };
    let mut global = 0u32;
    let fixtures: Vec<Fixture> = (0..tubes)
        .map(|t| Fixture {
            id: Uuid::from_u128(0x10_0000 + t as u128),
            name: format!("Tubo {}", t + 1),
            profile: profile.clone(),
            pixels: (0..TUBE_PX)
                .map(|i| {
                    let g = global;
                    global += 1;
                    Pixel {
                        index: i,
                        position: Vec3::new(i as f32, t as f32, 0.0),
                        universe: (g / PX_PER_UNIVERSE) as u16 + 1,
                        start_channel: ((g % PX_PER_UNIVERSE) * 3 + 1) as u16,
                        color_order: ColorOrder::Rgb,
                    }
                })
                .collect(),
            zone: Uuid::from_u128(0x3000 + sector_of(t) as u128),
        })
        .collect();
    let mut zones: Vec<Zone> = (0..SECTORS)
        .map(|s| {
            let rows = (0..tubes).filter(|&t| sector_of(t) == s).count() as u32;
            zone(
                0x3000 + s as u128,
                &format!("Sector {}", s + 1),
                rows,
                0,
                BlendMode::Normal,
            )
        })
        .collect();
    // The global zone: explicit overlap membership over every pixel, in Add on top.
    let mut all = zone(0x3FFF, "Global", tubes, 100, BlendMode::Add);
    all.members = fixtures
        .iter()
        .flat_map(|f| {
            f.pixels.iter().map(|p| PixelRef {
                fixture: f.id,
                pixel: p.index,
            })
        })
        .collect();
    zones.push(all);
    let mut patch = Patch {
        id: Uuid::from_u128(0x5CA1),
        name: "show probe".into(),
        matrix_size: [1, 1, 1],
        groups: vec![Group {
            id: Uuid::from_u128(0x6000),
            name: "pares".into(),
            members: fixtures.iter().skip(1).step_by(2).map(|f| f.id).collect(),
            display_color: [1, 2, 3],
        }],
        fixtures,
        zones,
    };
    patch.fill_zone_members_from_ownership();
    patch.fit_matrix_size();
    patch.validate().expect("probe rig is valid");
    patch
}

fn clip(effect: &str) -> Cell {
    Cell::single(Layer::shader(effect, ParamSet::new()))
}

fn track(name: &str, dest: Destination, effect: &str, blend: BlendMode) -> Track {
    let mut t = Track::new(name, dest);
    t.clips = vec![clip(effect)];
    t.active = Some(0);
    t.blend_mode = blend;
    t
}

fn show(tubes: u32) -> Show {
    let patch = rig(tubes);
    let sectors: Vec<Uuid> = patch.zones[..SECTORS as usize]
        .iter()
        .map(|z| z.id)
        .collect();
    let global = patch.zones[SECTORS as usize].id;
    let group = patch.groups[0].id;
    let mut show = Show::new("show probe", patch);
    for zs in &mut show.zones {
        zs.live.dimmer = 1.0;
    }
    let mut tracks: Vec<Track> = sectors
        .iter()
        .map(|&z| track("plasma", Destination::zone(z), "plasma", BlendMode::Normal))
        .collect();
    let mut wave = track(
        "wave",
        Destination::zone(sectors[0]),
        "wave",
        BlendMode::Add,
    );
    wave.destination.zones = sectors.clone();
    tracks.push(wave);
    tracks.push(track(
        "grupo",
        Destination {
            zones: vec![],
            groups: vec![group],
        },
        "sparkle",
        BlendMode::Normal,
    ));
    tracks.push(track(
        "global",
        Destination::zone(global),
        "solid",
        BlendMode::Normal,
    ));
    show.tracks = tracks;
    show.validate().expect("probe show is valid");
    show
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// The output rate the probe's engines run at (EngineConfig default).
const RATE_HZ: f64 = 44.0;
/// `--check`: the share of the output rate the compositor must publish.
const MIN_RATE_SHARE: f64 = 0.98;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let seconds: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(10);
    let mut failures: Vec<String> = Vec::new();
    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    println!("show-probe: full engine, real GPU, {seconds} s per point");
    println!(
        "{:>7} {:>5} | {:>9} {:>9} {:>9} {:>9} | {:>9} {:>9} {:>9} {:>5} | {:>8} {:>4} {:>5} | {:>6} {:>5}",
        "pixels", "univ", "render", "readback", "t→pub", "t→pub mx",
        "prod→wire", "p→w p99", "p→w max", ">50ms", "jitter", "late", "drops", "frames", "Hz"
    );
    // `SHOW_PROBE_TUBES=10,300` overrides the points (e.g. a near-empty rig, to tell the
    // machine's own GPU hiccups from the compositor's).
    let points: Vec<u32> = std::env::var("SHOW_PROBE_TUBES")
        .ok()
        .map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_else(|| POINTS.to_vec());
    for tubes in points {
        let show = show(tubes);
        let pixels = tubes * TUBE_PX;
        let universes = pixels.div_ceil(PX_PER_UNIVERSE);
        let config = EngineConfig::new(AdapterPreference::HighPerformance, dev_sink.local_addr());
        let engine = Engine::start_show(show, config).expect("engine starts");
        std::thread::sleep(Duration::from_secs(seconds));
        let report = engine.shutdown();
        let r = &report.render;
        let o = &report.output;
        let hz = r.frames_published as f64 / seconds as f64;
        println!(
            "{:>7} {:>5} | {:>7.3}ms {:>7.3}ms {:>7.3}ms {:>7.3}ms | {:>7.3}ms {:>7.3}ms {:>7.3}ms {:>5} | {:>6.3}ms {:>4} {:>5} | {:>6} {:>5.1}",
            pixels,
            universes,
            ms(r.render.mean_ns()),
            ms(r.readback.mean_ns()),
            ms(r.tick_to_publish.mean_ns()),
            ms(r.tick_to_publish.max_ns),
            ms(o.latency_mean_ns),
            ms(o.latency_p99_ns),
            ms(o.latency_max_ns),
            o.latency_over_budget,
            ms(o.jitter_stddev_ns),
            o.late_frames,
            o.drops,
            r.frames_published,
            hz,
        );
        if hz < RATE_HZ * MIN_RATE_SHARE {
            failures.push(format!(
                "{pixels} px: {hz:.1} Hz < {:.1}",
                RATE_HZ * MIN_RATE_SHARE
            ));
        }
        if o.latency_over_budget > 0 {
            failures.push(format!(
                "{pixels} px: {} ticks over 50 ms (max {:.1} ms)",
                o.latency_over_budget,
                ms(o.latency_max_ns)
            ));
        }
    }
    let _ = dev_sink.stop_and_collect();
    if check {
        if failures.is_empty() {
            println!("show-probe --check: PASS");
        } else {
            for f in &failures {
                println!("show-probe --check: FAIL {f}");
            }
            std::process::exit(1);
        }
    }
}

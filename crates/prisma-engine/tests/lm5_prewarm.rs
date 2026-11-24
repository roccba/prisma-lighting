//! Every pipeline is built at load (docs/LIVE-MODEL-SPEC.md §8.6, "warm-up"): once a show
//! runs, launching an effect it has never played compiles nothing, and a recompile on
//! the same GPU builds nothing either.
//!
//! The software adapter (lavapipe) keeps it deterministic.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use prisma_core::show::{Cell, Destination, Layer, ParamSet, Show, Source, Track};
use prisma_engine::demo::{demo_show, DEMO_ZONES};
use prisma_engine::{ClipEffect, ClipHost, Engine, EngineConfig};
use prisma_output::sink;
use prisma_render::{catalogue, AdapterPreference};

/// The show's owner as the clip controller sees it.
struct Host(Show);

impl ClipHost for Host {
    fn clip_effect(&self, track: uuid::Uuid, clip: usize) -> Option<ClipEffect> {
        let layer = self
            .0
            .track(track)?
            .clips
            .get(clip)?
            .layers
            .first()?
            .clone();
        let Source::Shader {
            effect_id,
            params,
            palette,
        } = layer.source;
        Some(ClipEffect {
            effect_id,
            params,
            palette,
            fade_time: Duration::ZERO,
        })
    }
    fn active_changed(&self, _track: uuid::Uuid, _active: Option<usize>) {}
}

#[test]
fn launching_a_new_effect_or_recompiling_builds_no_pipeline() {
    // The demo rig with one ceiling track: solid running, clip 1 an effect the show
    // has never played.
    let mut show = demo_show();
    let unused = "fire";
    let mut track = Track::new("techo", Destination::zone(DEMO_ZONES[0]));
    track.clips = vec![
        Cell::single(Layer::shader("solid", ParamSet::new())),
        Cell::single(Layer::shader(unused, ParamSet::new())),
    ];
    track.active = Some(0);
    show.tracks = vec![track];
    show.validate().expect("valid show");
    let id = show.tracks[0].id;

    let dev_sink = sink::spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).expect("sink");
    let config = || EngineConfig::new(AdapterPreference::Software, dev_sink.local_addr());
    let engine = Engine::start_show(show.clone(), config()).expect("engine starts");
    engine.set_clip_host(Arc::new(Host(show.clone())));

    // At load: every effect of the catalogue, the blend and the gather.
    let built = engine.pipelines_built();
    assert_eq!(built, catalogue().len() + 2, "every pipeline built at load");

    // Launch the never-played effect, as a pad or a click does, and wait until it is live.
    assert!(engine.activate_path(&format!("/track/{id}/clip/1/launch")));
    let state = engine
        .tree()
        .resolve(&format!("/track/{id}/clip/1/state"))
        .expect("clip state path");
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.read_real() != Some(2.0) {
        assert!(Instant::now() < deadline, "{unused} never went live");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200)); // a few frames of it
    assert_eq!(
        engine.pipelines_built(),
        built,
        "launching {unused} built a pipeline"
    );

    // A recompile reuses the GPU and its pipelines.
    let engine = engine.recompile_show(show, config()).expect("recompile");
    assert_eq!(
        engine.pipelines_built(),
        built,
        "a recompile built a pipeline"
    );

    let report = engine.shutdown();
    assert_eq!(report.gpu_strong_refs_at_release, 1);
    let _ = dev_sink.stop_and_collect();
}

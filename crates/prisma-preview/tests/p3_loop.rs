//! P3 acceptance: the preview loop (CLAUDE.md M6).
//!
//! Two properties matter most and each has a dedicated test:
//!
//! 1. **Nothing watching ⇒ nothing spent.** With no sink attached, and with a sink
//!    but no consumer connected, the loop must encode *nothing* — not "cheaply",
//!    zero. `no_sink_never_encodes` and `no_consumer_never_encodes` assert the
//!    encoder is never reached (`frames_encoded == 0`) and no frame is shipped.
//! 2. **A fader moves the preview with no re-render.** The loop re-encodes the held
//!    frame with *live* flat parameters every tick, so a master change between two
//!    ticks changes the shipped bytes even though no new frame was rendered — the
//!    §1.5 shortcut, on screen.

use std::sync::{Arc, Mutex};

use half::f16;
use prisma_output::plan::FlatParams;
use prisma_output::scheduler::FlatParamSource;
use prisma_patch::{
    ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel, PixelRef,
    Zone,
};
use prisma_preview::encode::{HEADER_LEN, MAGIC, ZONE_ENTRY_LEN};
use prisma_preview::preview_loop::{PreviewLoop, PreviewOutcome, PreviewSink};
use prisma_preview::{preview_handoff, PreviewDescriptor, PreviewPlan};
use uuid::Uuid;

/// One RGB pixel in a single zone.
fn one_zone_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0xC0);
    Patch {
        id: Uuid::from_u128(0xC000),
        name: "m6-p3".into(),
        matrix_size: [1, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "strip".into(),
            profile: FixtureProfile {
                name: "rgb".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: vec![Pixel {
                index: 0,
                position: glam::Vec3::ZERO,
                universe: 1,
                start_channel: 1,
                color_order: ColorOrder::Rgb,
            }],
        }],
        zones: vec![Zone {
            id: Uuid::from_u128(0x1),
            name: "all".into(),
            members: vec![PixelRef {
                fixture: fixture_id,
                pixel: 0,
            }],
            bounds: None,
            z_order: 0,
            blend_mode: prisma_patch::BlendMode::Normal,
        }],
    }
}

/// Live flat parameters behind a shared mutex, so a test can move a fader between
/// ticks exactly as a transport would.
#[derive(Clone)]
struct SharedFlat(Arc<Mutex<FlatParams>>);

impl FlatParamSource for SharedFlat {
    fn read_into(&self, into: &mut FlatParams) {
        *into = self.0.lock().unwrap().clone();
    }
}

/// A spy transport: records whether it has consumers and captures everything the
/// loop sends, so a test can prove what was (and was not) encoded.
#[derive(Default)]
struct SinkInner {
    consumers: bool,
    descriptors: usize,
    frames: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct SpySink(Arc<Mutex<SinkInner>>);

impl SpySink {
    fn new(consumers: bool) -> (SpySink, Arc<Mutex<SinkInner>>) {
        let inner = Arc::new(Mutex::new(SinkInner {
            consumers,
            ..Default::default()
        }));
        (SpySink(Arc::clone(&inner)), inner)
    }
}

impl PreviewSink for SpySink {
    fn has_consumers(&self) -> bool {
        self.0.lock().unwrap().consumers
    }
    fn send_descriptor(&mut self, _json: &str) {
        self.0.lock().unwrap().descriptors += 1;
    }
    fn send_frame(&mut self, frame: &[u8]) {
        self.0.lock().unwrap().frames.push(frame.to_vec());
    }
}

/// Build a loop whose handoff already holds one published frame (white pixel), and
/// return the loop plus the shared flat handle.
fn loop_with_one_frame() -> (PreviewLoop, SharedFlat) {
    let patch = one_zone_patch();
    let index = patch.compile().unwrap();
    let plan = PreviewPlan::compile(&patch, &index);
    let descriptor = PreviewDescriptor::build(&plan, &patch, &index);

    let (mut producer, consumer) = preview_handoff(index.master_buffer_len(), index.zone_count());
    let mask = prisma_preview::ZoneActiveMask::new(index.zone_count());
    producer.set_layout_generation(index.generation());
    producer.publish(&mask, 1, |px| {
        for v in px.iter_mut() {
            *v = f16::from_f32(1.0);
        }
    });

    let flat = SharedFlat(Arc::new(Mutex::new(FlatParams::identity(
        index.zone_count(),
    ))));
    let engine = PreviewLoop::new(plan, descriptor, consumer, Box::new(flat.clone()));
    (engine, flat)
}

#[test]
fn no_sink_never_encodes() {
    // The strongest form of the zero-cost rule: with no transport attached, a tick
    // does nothing and the encoder is never reached.
    let (mut engine, _flat) = loop_with_one_frame();
    for _ in 0..10 {
        assert_eq!(engine.tick(), PreviewOutcome::NoSink);
    }
    let stats = engine.stats();
    assert_eq!(stats.ticks, 10);
    assert_eq!(stats.frames_encoded, 0, "no sink must encode nothing");
    assert_eq!(stats.frames_sent, 0);
}

#[test]
fn no_consumer_never_encodes() {
    // A sink is attached but nobody is connected: still zero encoding. This is the
    // "nobody is watching costs zero, not cheap" guarantee.
    let (mut engine, _flat) = loop_with_one_frame();
    let (sink, inner) = SpySink::new(false); // no consumers
    engine.attach_sink(Box::new(sink));

    for _ in 0..10 {
        assert_eq!(engine.tick(), PreviewOutcome::NoConsumer);
    }
    let stats = engine.stats();
    assert_eq!(stats.frames_encoded, 0, "no consumer must encode nothing");
    assert_eq!(stats.skipped_no_consumer, 10);

    let inner = inner.lock().unwrap();
    assert!(inner.frames.is_empty(), "nothing was shipped");
    assert_eq!(inner.descriptors, 0, "descriptor not sent to an empty sink");
}

#[test]
fn a_connected_consumer_gets_a_descriptor_then_frames() {
    let (mut engine, _flat) = loop_with_one_frame();
    let (sink, inner) = SpySink::new(true); // one consumer connected
    engine.attach_sink(Box::new(sink));

    assert_eq!(engine.tick(), PreviewOutcome::Sent);
    assert_eq!(engine.tick(), PreviewOutcome::Sent);

    let inner = inner.lock().unwrap();
    assert_eq!(inner.descriptors, 1, "descriptor sent exactly once");
    assert_eq!(inner.frames.len(), 2, "one frame per tick");
    // The shipped bytes are a real preview frame.
    assert_eq!(&inner.frames[0][0..4], &MAGIC);
    let expected_len = HEADER_LEN + ZONE_ENTRY_LEN + 3; // 1 zone, 1 pixel
    assert_eq!(inner.frames[0].len(), expected_len);
}

#[test]
fn a_fader_moves_the_preview_with_no_new_frame() {
    // Only ONE frame is ever published. The loop still ships a frame every tick,
    // and pulling the master fader down between ticks changes the shipped pixel
    // bytes — proving the loop re-encodes with live flat parameters, no re-render.
    let (mut engine, flat) = loop_with_one_frame();
    let (sink, inner) = SpySink::new(true);
    engine.attach_sink(Box::new(sink));

    assert_eq!(engine.tick(), PreviewOutcome::Sent); // master at unity

    flat.0.lock().unwrap().set_master(0.5, [1.0, 1.0, 1.0]);
    assert_eq!(engine.tick(), PreviewOutcome::Sent); // master pulled to half

    let inner = inner.lock().unwrap();
    let pixel_off = HEADER_LEN + ZONE_ENTRY_LEN;
    let full = inner.frames[0][pixel_off]; // sRGB(1.0)
    let dimmed = inner.frames[1][pixel_off]; // sRGB(0.5)
    assert_eq!(full, 255);
    assert_eq!(dimmed, 188);
    assert!(
        dimmed < full,
        "the fader moved the preview without a new render"
    );
}

#[test]
fn the_thread_runs_at_its_target_rate_and_stops() {
    // Drives the real fixed-interval thread (no priority elevation, normal
    // scheduling) for a fixed duration and checks it ticked roughly at rate. Loose
    // bounds: this is a picture, not a timing signal, so exact cadence is a
    // non-goal — we only confirm it runs and stops cleanly.
    use prisma_preview::preview_loop::{run_preview_loop, PreviewLoopConfig};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    let (mut engine, _flat) = loop_with_one_frame();
    let (sink, inner) = SpySink::new(true);
    engine.attach_sink(Box::new(sink));

    let stats = run_preview_loop(
        engine,
        PreviewLoopConfig {
            target_rate_hz: 60,
            run_for: Some(Duration::from_millis(200)),
            stop: Arc::new(AtomicBool::new(false)),
        },
    );

    // ~12 ticks at 60 Hz over 200 ms; allow wide slack for a busy CI box.
    assert!(stats.ticks >= 4, "ticked {} times", stats.ticks);
    assert_eq!(stats.frames_encoded, stats.frames_sent);
    assert_eq!(
        stats.frames_sent as usize,
        inner.lock().unwrap().frames.len()
    );
    assert!(stats.frames_sent > 0, "the running loop shipped frames");
}

#[test]
fn a_late_consumer_still_gets_the_descriptor() {
    // Sink attached while empty, then a consumer connects. The descriptor must go
    // out on the first tick that has a consumer, not be lost because the sink was
    // empty at attach time.
    let (mut engine, _flat) = loop_with_one_frame();
    let (sink, inner) = SpySink::new(false);
    engine.attach_sink(Box::new(sink));

    assert_eq!(engine.tick(), PreviewOutcome::NoConsumer);
    assert_eq!(inner.lock().unwrap().descriptors, 0);

    inner.lock().unwrap().consumers = true; // someone connects
    assert_eq!(engine.tick(), PreviewOutcome::Sent);
    assert_eq!(
        inner.lock().unwrap().descriptors,
        1,
        "descriptor sent once the consumer appeared"
    );
}

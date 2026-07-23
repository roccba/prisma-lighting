//! Latest-frame handoff between the frame producer and the output thread.
//!
//! # Why a triple buffer
//!
//! The output thread must never wait on the producer (CLAUDE.md §3.3: "Must
//! never wait on the render thread; if a frame isn't ready, resend the last
//! one"), and the producer (the render/readback path in M5, a test harness in
//! M1) must never wait on the output thread. Both sides run at independent
//! rates. A lock-free **single-producer / single-consumer triple buffer** is
//! the standard structure for this: three preallocated buffers cycled through
//! one shared atomic index, so each side always has a private buffer to work on
//! and neither ever blocks.
//!
//! # Latency introduced by this handoff — audit this number
//!
//! [`HANDOFF_LATENCY_FRAMES_MAX`] = **1 output frame (worst case)**, ~0 best case.
//!
//! A frame published just after the output thread took its snapshot waits for
//! the next output tick before it goes out; that wait is at most one output
//! period. The triple buffer adds **no buffering latency beyond that** — unlike
//! a queue it holds at most one pending frame, and a superseded pending frame is
//! *dropped*, not delayed (see next section). This 1-frame bound is the
//! irreducible cost of decoupling producer and consumer asynchronously: the only
//! way to remove it is to make the consumer block on the producer, which §3.3
//! forbids. This value feeds directly into the §1.5 latency budget and is the
//! handoff's entire contribution to it.
//!
//! # No backlog — newest-wins guarantee
//!
//! The shared "back" slot has a capacity of exactly one. If the producer
//! publishes faster than the consumer advances, each [`Producer::publish`]
//! swaps the freshly written buffer into the back slot and takes the *previous,
//! unconsumed* buffer back to overwrite — so at most one frame is ever pending
//! and the older one is discarded, never queued. The producer never blocks and
//! a backlog cannot form. The consumer, when it advances, always jumps straight
//! to the most recently published frame. This is verified by
//! `newest_wins_no_backlog` in the tests below.

use half::f16;
use prisma_core::OverlapSnapshot;
use prisma_patch::LayoutGeneration;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Maximum latency, in output frames, that the frame handoff can add to the
/// render→wire path (CLAUDE.md §1.5). See the module docs for the derivation.
/// Exposed as a constant so the latency budget can be audited programmatically
/// without re-reading the implementation.
pub const HANDOFF_LATENCY_FRAMES_MAX: u32 = 1;

// The shared index packs a 2-bit buffer index with a "freshly published" flag.
const INDEX_MASK: u8 = 0b011;
const FRESH_FLAG: u8 = 0b100;

struct Shared<T> {
    // Exactly three buffers. At every instant the producer's `input`, the
    // shared `back`, and the consumer's `output` indices are a permutation of
    // {0,1,2}: no two sides ever address the same buffer, which is what makes
    // the `UnsafeCell` access below sound.
    buffers: [UnsafeCell<T>; 3],
    // Low 2 bits: index of the most recently published buffer. FRESH_FLAG set
    // when the producer has published a frame the consumer has not taken yet.
    back: AtomicU8,
}

// SAFETY: the swap protocol guarantees the producer and consumer never hold the
// same buffer index simultaneously, so the `UnsafeCell` interiors are never
// aliased across threads. The only shared mutable state (`back`) is atomic.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

/// Writing half of the handoff. Owns one buffer at a time; not clonable, so
/// there is only ever a single producer.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    input: u8,
}

/// Reading half of the handoff. Owns one buffer at a time; not clonable, so
/// there is only ever a single consumer.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    output: u8,
}

/// Create a triple-buffer handoff, calling `init` three times for the three
/// preallocated buffers. All allocation happens here, never on the hot path.
pub fn triple_buffer<T>(mut init: impl FnMut() -> T) -> (Producer<T>, Consumer<T>) {
    let shared = Arc::new(Shared {
        buffers: [
            UnsafeCell::new(init()),
            UnsafeCell::new(init()),
            UnsafeCell::new(init()),
        ],
        // input=0 (producer), back=1, output=2 (consumer); nothing fresh yet.
        back: AtomicU8::new(1),
    });
    let producer = Producer {
        shared: Arc::clone(&shared),
        input: 0,
    };
    let consumer = Consumer { shared, output: 2 };
    (producer, consumer)
}

impl<T> Producer<T> {
    /// Mutable access to the buffer currently being staged. The producer writes
    /// the whole frame here, then calls [`publish`](Self::publish).
    #[inline]
    pub fn input(&mut self) -> &mut T {
        // SAFETY: `self.input` is owned exclusively by this producer; the
        // consumer only ever accesses `output`, and the protocol keeps
        // input != output at all times.
        unsafe { &mut *self.shared.buffers[self.input as usize].get() }
    }

    /// Publish the staged buffer as the latest frame. Never blocks. If a
    /// previously published frame was not yet consumed it is discarded here
    /// (newest-wins, no backlog — see module docs).
    #[inline]
    pub fn publish(&mut self) {
        // Release: make the staged writes visible to the consumer's Acquire.
        let prev = self
            .shared
            .back
            .swap(self.input | FRESH_FLAG, Ordering::AcqRel);
        self.input = prev & INDEX_MASK;
    }
}

impl<T> Consumer<T> {
    /// Advance to the most recently published frame if there is one newer than
    /// what is currently held. Returns `true` if a new frame was taken, `false`
    /// if nothing newer was published since the last call — in which case the
    /// caller resends [`current`](Self::current) (timing over freshness, §3.3).
    #[inline]
    pub fn advance(&mut self) -> bool {
        // Relaxed is enough to test the flag; if it is set we do an Acquire swap
        // that synchronizes-with the producer's Release. A missed flag here only
        // defers pickup to the next tick — already within the 1-frame bound.
        if self.shared.back.load(Ordering::Relaxed) & FRESH_FLAG == 0 {
            return false;
        }
        // Acquire: observe the producer's staged writes. We swap in our own
        // (flag-clear) index, so the back slot is marked consumed.
        let prev = self.shared.back.swap(self.output, Ordering::AcqRel);
        self.output = prev & INDEX_MASK;
        true
    }

    /// The frame the consumer currently holds — the last one [`advance`] took,
    /// or the initial buffer. Stable until the next `advance` returns `true`.
    #[inline]
    pub fn current(&self) -> &T {
        // SAFETY: `self.output` is owned exclusively by this consumer; the
        // producer only ever accesses `input`, and the protocol keeps
        // output != input at all times.
        unsafe { &*self.shared.buffers[self.output as usize].get() }
    }
}

/// One master pixel buffer plus the metadata the output stage and the latency
/// probe need. The pixel buffer is a fixed-capacity linear layout (RGB / RGBW…
/// resolved by the patch in M2); for M1 the producer fills it directly.
pub struct Frame {
    /// Canonical (pixel-domain, **linear-light `f16`**) master pixel buffer, fixed
    /// length for the life of the handoff. Rendering keeps full precision in this
    /// buffer; the single quantization to 8-bit wire happens once, at the output
    /// stage's gamma step (§5.2) — never mid-pipeline. One `f16` per component;
    /// its length in elements is [`PatchIndex::master_buffer_len`].
    pub pixels: Box<[f16]>,
    /// Monotonic id. Gaps observed by the consumer are frames dropped by the
    /// newest-wins policy — expected, and counted as drops by the probe (M1 §6).
    /// Id `0` marks a buffer that has never carried a published frame.
    pub frame_id: u64,
    /// When the producer finished staging this frame — the start point for the
    /// produced→wire latency measurement (§1.5).
    pub produced_at: Instant,
    /// The layout generation this buffer's byte layout was produced against. The
    /// output router checks it against its plan's generation and refuses to emit
    /// on a mismatch (see [`crate::router`]). [`LayoutGeneration::NONE`] for the
    /// M1 degenerate path, which has no patch/layout.
    pub layout_generation: LayoutGeneration,
    /// The overlap classification this frame was **rendered under** (§5.2.1).
    /// The output stage reads its zone/master split from here and never from the
    /// live counter, so a pixel can never be dimmed twice or zero times while it
    /// crosses between the render and output paths. `None` on the M1 degenerate
    /// path, which has no zones: then only master scaling applies.
    pub overlap: Option<Arc<OverlapSnapshot>>,
}

/// Producer half specialized for [`Frame`]: it stamps a monotonic `frame_id`
/// and `produced_at` on every publish so the output thread and probe don't have
/// to.
pub struct FrameProducer {
    inner: Producer<Frame>,
    next_id: u64,
    /// Stamped onto every published frame. Set via
    /// [`set_layout_generation`](FrameProducer::set_layout_generation) when the
    /// producer (re)compiles the patch it renders against; defaults to
    /// [`LayoutGeneration::NONE`] for the layout-less M1 path.
    layout_generation: LayoutGeneration,
    /// Stamped onto every published frame; set at the frame boundary from the
    /// counter's snapshot.
    overlap: Option<Arc<OverlapSnapshot>>,
}

/// Consumer half specialized for [`Frame`].
pub struct FrameConsumer {
    inner: Consumer<Frame>,
}

/// Build a frame handoff whose buffers each hold `pixel_len` **components** (f16
/// elements — see [`PatchIndex::master_buffer_len`]). All three buffers are
/// allocated up front.
pub fn frame_handoff(pixel_len: usize) -> (FrameProducer, FrameConsumer) {
    let now = Instant::now();
    let (producer, consumer) = triple_buffer(|| Frame {
        pixels: vec![f16::ZERO; pixel_len].into_boxed_slice(),
        frame_id: 0,
        produced_at: now,
        layout_generation: LayoutGeneration::NONE,
        overlap: None,
    });
    (
        FrameProducer {
            inner: producer,
            next_id: 1, // 0 is reserved for the never-published initial buffers
            layout_generation: LayoutGeneration::NONE,
            overlap: None,
        },
        FrameConsumer { inner: consumer },
    )
}

impl FrameProducer {
    /// Set the layout generation stamped onto subsequently published frames.
    /// Call this whenever the render side (re)compiles its patch, so the router
    /// can detect a stale plan/buffer pairing (see [`crate::router`]).
    #[inline]
    pub fn set_layout_generation(&mut self, generation: LayoutGeneration) {
        self.layout_generation = generation;
    }

    /// The layout generation currently stamped onto published frames.
    #[inline]
    pub fn layout_generation(&self) -> LayoutGeneration {
        self.layout_generation
    }

    /// Set the overlap snapshot stamped onto subsequently published frames. The
    /// render loop calls this at the frame boundary with the counter's snapshot,
    /// so the frame and its classification always travel together.
    #[inline]
    pub fn set_overlap(&mut self, overlap: Option<Arc<OverlapSnapshot>>) {
        self.overlap = overlap;
    }

    /// Stage and publish a frame. `fill` receives the fixed-length pixel buffer
    /// (linear-light `f16` components) and must overwrite the whole thing (the
    /// buffer may still hold data from two frames ago). Never blocks; supersedes
    /// any unconsumed frame.
    #[inline]
    pub fn publish(&mut self, fill: impl FnOnce(&mut [f16])) {
        let frame = self.inner.input();
        fill(&mut frame.pixels);
        frame.frame_id = self.next_id;
        frame.produced_at = Instant::now();
        frame.layout_generation = self.layout_generation;
        frame.overlap = self.overlap.clone();
        self.next_id = self.next_id.wrapping_add(1);
        self.inner.publish();
    }
}

impl FrameConsumer {
    /// Advance to the newest published frame if one exists; see
    /// [`Consumer::advance`].
    #[inline]
    pub fn advance(&mut self) -> bool {
        self.inner.advance()
    }

    /// The frame currently held; see [`Consumer::current`].
    #[inline]
    pub fn current(&self) -> &Frame {
        self.inner.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tag helper: these protocol tests only care about buffer identity/tearing,
    /// not color semantics, so any distinct f16 values serve as markers.
    fn tag(v: u16) -> f16 {
        f16::from_f32(v as f32)
    }

    #[test]
    fn roundtrips_pixel_data() {
        let (mut p, mut c) = frame_handoff(4);
        assert_eq!(c.current().frame_id, 0, "nothing published yet");

        p.publish(|px| px.copy_from_slice(&[tag(1), tag(2), tag(3), tag(4)]));
        assert!(c.advance());
        assert_eq!(c.current().frame_id, 1);
        assert_eq!(&*c.current().pixels, &[tag(1), tag(2), tag(3), tag(4)]);
    }

    #[test]
    fn advance_is_false_without_a_new_frame() {
        let (mut p, mut c) = frame_handoff(2);
        p.publish(|px| px.copy_from_slice(&[tag(9), tag(9)]));
        assert!(c.advance());
        // No new publish: the consumer resends what it holds.
        assert!(!c.advance());
        assert_eq!(&*c.current().pixels, &[tag(9), tag(9)]);
    }

    #[test]
    fn newest_wins_no_backlog() {
        let (mut p, mut c) = frame_handoff(1);
        // Producer runs ahead of the consumer: three publishes, no advance.
        p.publish(|px| px[0] = tag(0xA));
        p.publish(|px| px[0] = tag(0xB));
        p.publish(|px| px[0] = tag(0xC));

        // A single advance jumps straight to the newest; the two older frames
        // were discarded, never queued.
        assert!(c.advance());
        assert_eq!(c.current().pixels[0], tag(0xC));
        assert_eq!(c.current().frame_id, 3);
        // Nothing is backlogged behind it.
        assert!(!c.advance());
    }

    #[test]
    fn producer_never_reuses_the_consumers_buffer() {
        // Stress: producer spams frames while the consumer polls. The consumer
        // must only ever see monotonically non-decreasing frame ids and never
        // observe a torn buffer (id must match its pixel content).
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;

        let (mut p, mut c) = frame_handoff(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_writer = Arc::clone(&stop);

        // Encode the frame id into every element (mod 1024, exact in f16) so the
        // reader can detect a torn/aliased buffer and cross-check it against the
        // observed frame id.
        let id_tag = |id: u64| f16::from_f32((id % 1024) as f32);

        let writer = thread::spawn(move || {
            for id in 1u64..=200_000 {
                p.publish(|px| px.fill(id_tag(id)));
                if stop_writer.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        let mut last = 0u64;
        for _ in 0..2_000_000 {
            if c.advance() {
                let f = c.current();
                assert!(f.frame_id >= last, "frame ids must not go backwards");
                last = f.frame_id;
                let expected = id_tag(f.frame_id);
                assert!(
                    f.pixels.iter().all(|&b| b == expected),
                    "torn buffer: content does not match frame id"
                );
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
}

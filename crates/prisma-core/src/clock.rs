//! The master clock: frame counter, wall/show time, BPM/beat, tap tempo
//! (CLAUDE.md §3.3, milestone M4).
//!
//! # Two times, kept separate on purpose
//!
//! - **Wall time** — a monotonic nanosecond clock that never pauses. Arbitration
//!   idle-timeouts (M3) and jitter/latency measurement read it. Sourced from an
//!   injected [`TimeSource`], so tap tempo and arbitration are both testable with
//!   a [`ManualClock`](crate::time::ManualClock).
//! - **Show time** — a frame counter plus an accumulated `show_ns` and a derived
//!   `beat` position. This is what pauses and what animations/transitions run on
//!   (the per-zone `speed_multiplier` of §4.3 will scale it in render).
//!
//! # The hot read path
//!
//! The render thread and the transition evaluator run on the same thread that
//! advances the clock. They read a [`ClockSnapshot`] — a `Copy` value threaded
//! down from [`MasterClock::tick`] — with **no path resolution, no tree, not even
//! an atomic**. Cross-thread observers (probe, UI, and the tree's arbitration
//! [`TimeSource`]) read the same snapshot through a single-writer **seqlock**
//! ([`ClockShared`]): lock-free, coherent, and — because the only write is once
//! per frame at the boundary — retry-free for a reader inside the frame.
//!
//! Only human-rate controls (`/clock/bpm`, `/clock/playing`, `/clock/tap`) are
//! mirrored into the Parameter Tree. Frame and beat are **never** pushed to the
//! tree per frame — that would be exactly the flood M4 avoids.

use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::time::{SystemClock, Tick, TimeSource};

/// Lowest / highest BPM the clock will hold. Below/above is clamped.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 300.0;

/// A coherent, `Copy` view of the clock at one frame. Passed by value down the
/// render/transition path (no synchronization there) and published to observers
/// via the seqlock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockSnapshot {
    /// Monotonic frame counter, incremented once per [`MasterClock::tick`].
    pub frame: u64,
    /// Wall time (ns, monotonic) at this frame. Never pauses.
    pub wall_ns: Tick,
    /// Accumulated show time (ns). Frozen while paused.
    pub show_ns: u64,
    /// Beat position, accumulated from show time at the current BPM. Continuous
    /// across BPM changes (only its forward rate changes).
    pub beat: f64,
    /// Current tempo in beats per minute.
    pub bpm: f64,
    /// Whether show time is advancing.
    pub playing: bool,
}

impl ClockSnapshot {
    /// Show time in seconds (convenience for second-based transitions).
    pub fn show_seconds(&self) -> f64 {
        self.show_ns as f64 / 1e9
    }
}

/// The cross-thread face of the clock: the wall [`TimeSource`] plus a seqlock the
/// owner publishes each frame. Shared as `Arc<ClockShared>`; hand it to the
/// Parameter Tree as its `TimeSource` and to observers for [`snapshot`](Self::snapshot).
pub struct ClockShared {
    wall: Arc<dyn TimeSource>,
    published: Published,
}

impl ClockShared {
    /// A wait-free, coherent read of the latest published snapshot. Lock-free
    /// (retries only if it races the once-per-frame write, which a reader inside
    /// the frame never does).
    pub fn snapshot(&self) -> ClockSnapshot {
        self.published.load()
    }
}

/// Arbitration and any other transport-thread reader gets wall time here — the
/// same monotonic base the whole engine shares (M3's injected clock is now this).
impl TimeSource for ClockShared {
    fn now(&self) -> Tick {
        self.wall.now()
    }
}

/// Single-writer seqlock over the snapshot fields. The writer is statically
/// single (only [`MasterClock::tick`] via `&mut self`); readers are lock-free.
struct Published {
    /// Even = stable, odd = a write is in progress.
    seq: AtomicU64,
    frame: AtomicU64,
    wall_ns: AtomicU64,
    show_ns: AtomicU64,
    beat_bits: AtomicU64,
    bpm_bits: AtomicU64,
    playing: AtomicBool,
}

impl Published {
    fn new(s: ClockSnapshot) -> Self {
        Self {
            seq: AtomicU64::new(0),
            frame: AtomicU64::new(s.frame),
            wall_ns: AtomicU64::new(s.wall_ns),
            show_ns: AtomicU64::new(s.show_ns),
            beat_bits: AtomicU64::new(s.beat.to_bits()),
            bpm_bits: AtomicU64::new(s.bpm.to_bits()),
            playing: AtomicBool::new(s.playing),
        }
    }

    fn store(&self, s: &ClockSnapshot) {
        let seq0 = self.seq.load(Ordering::Relaxed);
        // Enter write: seq becomes odd.
        self.seq.store(seq0.wrapping_add(1), Ordering::Release);
        fence(Ordering::Release);
        self.frame.store(s.frame, Ordering::Relaxed);
        self.wall_ns.store(s.wall_ns, Ordering::Relaxed);
        self.show_ns.store(s.show_ns, Ordering::Relaxed);
        self.beat_bits.store(s.beat.to_bits(), Ordering::Relaxed);
        self.bpm_bits.store(s.bpm.to_bits(), Ordering::Relaxed);
        self.playing.store(s.playing, Ordering::Relaxed);
        // Publish: seq back to even.
        self.seq.store(seq0.wrapping_add(2), Ordering::Release);
    }

    fn load(&self) -> ClockSnapshot {
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let snap = ClockSnapshot {
                frame: self.frame.load(Ordering::Relaxed),
                wall_ns: self.wall_ns.load(Ordering::Relaxed),
                show_ns: self.show_ns.load(Ordering::Relaxed),
                beat: f64::from_bits(self.beat_bits.load(Ordering::Relaxed)),
                bpm: f64::from_bits(self.bpm_bits.load(Ordering::Relaxed)),
                playing: self.playing.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == s1 {
                return snap;
            }
        }
    }
}

/// If two taps are more than this apart, the tap sequence is treated as a new
/// tempo (the operator stopped and started again) rather than one interval.
const TAP_RESET_NS: u64 = 2_000_000_000;
/// How many recent tap intervals to average.
const TAP_HISTORY: usize = 8;

/// Averages recent tap timestamps into a BPM, discarding outliers. Reads wall
/// time from the injected source, so it is deterministic under [`ManualClock`].
#[derive(Default)]
struct TapDetector {
    taps: std::collections::VecDeque<Tick>,
}

impl TapDetector {
    fn tap(&mut self, now: Tick) {
        if let Some(&last) = self.taps.back() {
            if now.saturating_sub(last) > TAP_RESET_NS {
                self.taps.clear();
            }
        }
        self.taps.push_back(now);
        while self.taps.len() > TAP_HISTORY {
            self.taps.pop_front();
        }
    }

    /// The detected BPM, or `None` with fewer than two taps.
    fn bpm(&self) -> Option<f64> {
        if self.taps.len() < 2 {
            return None;
        }
        let mut intervals: Vec<u64> = self
            .taps
            .iter()
            .zip(self.taps.iter().skip(1))
            .map(|(a, b)| b.saturating_sub(*a))
            .filter(|&d| d > 0)
            .collect();
        if intervals.is_empty() {
            return None;
        }
        intervals.sort_unstable();
        let median = intervals[intervals.len() / 2] as f64;
        // Discard intervals well outside the median (a missed or double tap).
        let kept: Vec<f64> = intervals
            .iter()
            .map(|&d| d as f64)
            .filter(|&d| d >= 0.5 * median && d <= 2.0 * median)
            .collect();
        let avg = if kept.is_empty() {
            median
        } else {
            kept.iter().sum::<f64>() / kept.len() as f64
        };
        Some((60.0e9 / avg).clamp(MIN_BPM, MAX_BPM))
    }
}

/// The engine's master clock. Owned by the engine/render loop; advanced once per
/// frame with [`tick`](Self::tick). Single-writer by construction (`&mut self`).
pub struct MasterClock {
    shared: Arc<ClockShared>,
    frame: u64,
    show_ns: u64,
    beat: f64,
    bpm: f64,
    playing: bool,
    /// Wall time captured at the previous tick, to compute the per-frame delta.
    last_wall_ns: Tick,
    tap: TapDetector,
}

impl MasterClock {
    /// Create a clock reading wall time from `wall`. In production pass a
    /// [`SystemClock`]; in tests a [`ManualClock`](crate::time::ManualClock).
    pub fn new(wall: Arc<dyn TimeSource>) -> Self {
        let now = wall.now();
        let bpm = 120.0;
        let initial = ClockSnapshot {
            frame: 0,
            wall_ns: now,
            show_ns: 0,
            beat: 0.0,
            bpm,
            playing: false,
        };
        Self {
            shared: Arc::new(ClockShared {
                wall,
                published: Published::new(initial),
            }),
            frame: 0,
            show_ns: 0,
            beat: 0.0,
            bpm,
            playing: false,
            last_wall_ns: now,
            tap: TapDetector::default(),
        }
    }

    /// A clock backed by the system monotonic clock.
    pub fn with_system_clock() -> Self {
        Self::new(Arc::new(SystemClock::new()))
    }

    /// The shareable handle: give it to [`ParameterTree`](crate::tree::ParameterTree)
    /// as its `TimeSource`, and to observers for [`snapshot`](ClockShared::snapshot).
    pub fn handle(&self) -> Arc<ClockShared> {
        self.shared.clone()
    }

    /// Advance one frame: fold in the wall-time delta (only while playing),
    /// update beat at the current BPM, publish the snapshot, and return it. The
    /// returned value is what the render/output/transition code consumes — by
    /// value, no synchronization.
    pub fn tick(&mut self) -> ClockSnapshot {
        let wall = self.shared.wall.now();
        let dwall = wall.saturating_sub(self.last_wall_ns);
        self.last_wall_ns = wall;

        if self.playing {
            self.show_ns = self.show_ns.saturating_add(dwall);
            // beat advances by (seconds elapsed) * (beats per second).
            self.beat += (dwall as f64 / 1e9) * (self.bpm / 60.0);
        }
        self.frame += 1;

        let snap = self.current(wall);
        self.shared.published.store(&snap);
        snap
    }

    fn current(&self, wall: Tick) -> ClockSnapshot {
        ClockSnapshot {
            frame: self.frame,
            wall_ns: wall,
            show_ns: self.show_ns,
            beat: self.beat,
            bpm: self.bpm,
            playing: self.playing,
        }
    }

    /// The most recent snapshot without advancing.
    pub fn snapshot(&self) -> ClockSnapshot {
        self.shared.snapshot()
    }

    /// Set the tempo (clamped to [`MIN_BPM`]..=[`MAX_BPM`]). Takes effect on the
    /// next tick; beats already elapsed are unchanged (only the forward rate
    /// changes) — this is why a beat-based fade preserves its length in beats.
    pub fn set_bpm(&mut self, bpm: f64) {
        self.bpm = bpm.clamp(MIN_BPM, MAX_BPM);
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    /// Resume show time. Resets the delta baseline so the paused interval is not
    /// folded in on the next tick.
    pub fn play(&mut self) {
        if !self.playing {
            self.last_wall_ns = self.shared.wall.now();
            self.playing = true;
        }
    }

    /// Freeze show time. In-progress transitions freeze with it (they read show
    /// time); they do not jump to their end.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn beat(&self) -> f64 {
        self.beat
    }

    pub fn show_seconds(&self) -> f64 {
        self.show_ns as f64 / 1e9
    }

    /// Register a tap at the current wall time and, once two taps exist, set BPM
    /// from the averaged interval.
    pub fn tap(&mut self) {
        let now = self.shared.wall.now();
        self.tap.tap(now);
        if let Some(bpm) = self.tap.bpm() {
            self.set_bpm(bpm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::ManualClock;

    const SEC: u64 = 1_000_000_000;

    fn manual() -> (Arc<ManualClock>, MasterClock) {
        let wall = Arc::new(ManualClock::new());
        let clock = MasterClock::new(wall.clone());
        (wall, clock)
    }

    #[test]
    fn frame_counter_is_monotonic() {
        let (_w, mut c) = manual();
        assert_eq!(c.tick().frame, 1);
        assert_eq!(c.tick().frame, 2);
        assert_eq!(c.tick().frame, 3);
    }

    #[test]
    fn show_time_only_advances_while_playing() {
        let (w, mut c) = manual();
        c.play();
        w.advance(SEC);
        let s = c.tick();
        assert!((s.show_seconds() - 1.0).abs() < 1e-9);

        c.pause();
        w.advance(5 * SEC); // paused: this wall time is not show time
        let s = c.tick();
        assert!(
            (s.show_seconds() - 1.0).abs() < 1e-9,
            "paused interval must not accrue"
        );

        c.play();
        w.advance(SEC);
        let s = c.tick();
        assert!(
            (s.show_seconds() - 2.0).abs() < 1e-9,
            "resumes from where it froze"
        );
    }

    #[test]
    fn beat_accumulates_at_bpm() {
        let (w, mut c) = manual();
        c.set_bpm(120.0); // 2 beats/sec
        c.play();
        w.advance(SEC);
        let s = c.tick();
        assert!((s.beat - 2.0).abs() < 1e-6);
    }

    #[test]
    fn bpm_change_rescales_forward_rate_only() {
        let (w, mut c) = manual();
        c.set_bpm(120.0); // 2 beats/sec
        c.play();
        w.advance(SEC);
        let s = c.tick();
        assert!((s.beat - 2.0).abs() < 1e-6);

        // Double the tempo: beats already counted stay, the rate doubles.
        c.set_bpm(240.0); // 4 beats/sec
        w.advance(SEC);
        let s = c.tick();
        assert!(
            (s.beat - 6.0).abs() < 1e-6,
            "2 + 4, not rescaled retroactively"
        );
    }

    #[test]
    fn tap_tempo_sets_bpm_deterministically() {
        let (w, mut c) = manual();
        // Four taps 500 ms apart → 120 BPM.
        c.tap();
        for _ in 0..3 {
            w.advance(SEC / 2);
            c.tap();
        }
        assert!((c.bpm() - 120.0).abs() < 0.5, "got {}", c.bpm());
    }

    #[test]
    fn tap_discards_a_double_tap_outlier() {
        let (w, mut c) = manual();
        c.tap();
        w.advance(SEC / 2);
        c.tap();
        w.advance(SEC / 2);
        c.tap();
        w.advance(20_000_000); // a stray double-tap 20 ms later
        c.tap();
        w.advance(SEC / 2);
        c.tap();
        assert!(
            (c.bpm() - 120.0).abs() < 2.0,
            "outlier skewed it: {}",
            c.bpm()
        );
    }

    #[test]
    fn snapshot_is_coherent_across_threads() {
        let (w, mut c) = manual();
        c.set_bpm(120.0);
        c.play();
        w.advance(SEC);
        c.tick();
        let h = c.handle();
        let s = h.snapshot();
        assert_eq!(s.frame, 1);
        assert!((s.beat - 2.0).abs() < 1e-6);
        assert!(s.playing);
    }
}

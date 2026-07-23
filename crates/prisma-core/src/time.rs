//! The time source arbitration and (later) transitions read.
//!
//! Arbitration's "owner released after idle timeout" rule (CLAUDE.md §4.5) needs
//! a clock, but the arbitration *core* must never call [`std::time::Instant::now`]
//! directly — otherwise its behaviour could only be tested against wall-clock,
//! which is flaky and slow. Instead every time-dependent decision reads a
//! [`TimeSource`], injected at construction. Production uses [`SystemClock`];
//! tests use [`ManualClock`] and advance it by hand, so the timeout table is
//! fully deterministic.
//!
//! Later (M4) the engine's master clock becomes the production [`TimeSource`], so
//! arbitration idle-timeouts and show time share one monotonic base.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A monotonic time value in **nanoseconds** from an unspecified origin. Only
/// differences are meaningful; the origin is arbitrary.
pub type Tick = u64;

/// A monotonic clock. `Send + Sync` because the tree shares one across the
/// transport threads that write parameters.
pub trait TimeSource: Send + Sync {
    /// The current time in [`Tick`]s. Must be monotonic non-decreasing.
    fn now(&self) -> Tick;
}

/// Wall-clock time source for production, backed by [`Instant`] (monotonic).
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for SystemClock {
    fn now(&self) -> Tick {
        // Nanoseconds since origin. u64 nanos overflows after ~584 years of
        // uptime; not a concern.
        self.origin.elapsed().as_nanos() as u64
    }
}

/// Test clock: time only moves when the test moves it. This is what makes the
/// takeover-by-timeout rule testable without sleeping on the wall clock.
#[derive(Default)]
pub struct ManualClock {
    now: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            now: AtomicU64::new(0),
        }
    }

    /// Advance time by `delta` ticks.
    pub fn advance(&self, delta: Tick) {
        self.now.fetch_add(delta, Ordering::SeqCst);
    }

    /// Set the absolute time.
    pub fn set(&self, t: Tick) {
        self.now.store(t, Ordering::SeqCst);
    }
}

impl TimeSource for ManualClock {
    fn now(&self) -> Tick {
        self.now.load(Ordering::SeqCst)
    }
}

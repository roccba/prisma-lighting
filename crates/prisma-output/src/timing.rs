//! Timing probe: the instrumentation that makes "jitter is the enemy"
//! measurable rather than assumed (CLAUDE.md §1.5, §6, §7).
//!
//! The output thread records one sample per tick into preallocated vectors
//! (single writer, no locks, no allocation on the hot path). Aggregation into a
//! [`ProbeReport`] happens off the hot path, after a run or on demand.
//!
//! Three things are measured:
//!   - **inter-send interval** vs the target period → jitter and its distribution,
//!   - **produced→wire latency** (frame staged → bytes on the socket) → the §1.5
//!     budget contribution downstream of the handoff,
//!   - **resends** (a tick with no fresh frame) and **drops** (send failures).

use std::fmt;
use std::time::{Duration, Instant};

use crate::priority::SchedulingMode;
use crate::transport::OutputStatus;

/// Records timing samples during a run. Owned by the output thread.
pub struct TimingProbe {
    target_period: Duration,
    late_threshold: Duration,
    intervals_ns: Vec<u64>,
    latencies_ns: Vec<u64>,
    last_send: Option<Instant>,
    resends: u64,
    drops: u64,
    generation_mismatches: u64,
}

impl TimingProbe {
    /// Preallocate for `expected_samples` ticks so the hot path never allocates.
    pub fn with_capacity(
        target_period: Duration,
        late_threshold: Duration,
        expected_samples: usize,
    ) -> Self {
        Self {
            target_period,
            late_threshold,
            intervals_ns: Vec::with_capacity(expected_samples),
            latencies_ns: Vec::with_capacity(expected_samples),
            last_send: None,
            resends: 0,
            drops: 0,
            generation_mismatches: 0,
        }
    }

    /// Record one output tick: `now` is the send instant, `produced_at` the
    /// staging instant of the frame that went out, `fresh` whether this tick
    /// carried a new frame (false = a resend of the previous one).
    #[inline]
    pub fn record_tick(&mut self, now: Instant, produced_at: Instant, fresh: bool) {
        if let Some(last) = self.last_send {
            self.intervals_ns.push((now - last).as_nanos() as u64);
        }
        self.last_send = Some(now);
        self.latencies_ns
            .push(now.saturating_duration_since(produced_at).as_nanos() as u64);
        if !fresh {
            self.resends += 1;
        }
    }

    /// Record a send that failed (a dropped output frame).
    #[inline]
    pub fn record_drop(&mut self) {
        self.drops += 1;
    }

    /// Record a frame skipped because the output plan's layout generation did not
    /// match the master buffer's (see [`crate::router::GenerationMismatch`]). No
    /// bytes were emitted for that frame — the output holds rather than paint
    /// wrong pixels. A non-zero count is a loud "your plan and layout disagree".
    #[inline]
    pub fn record_generation_mismatch(&mut self) {
        self.generation_mismatches += 1;
    }

    /// Aggregate the samples collected so far into a report.
    pub fn report(
        &self,
        mode: SchedulingMode,
        pinned_core: Option<usize>,
        outputs: Vec<OutputStatus>,
    ) -> ProbeReport {
        let target_ns = self.target_period.as_nanos() as f64;
        let n = self.intervals_ns.len();

        let (mean, min, max, stddev, abs_dev_mean) = if n == 0 {
            (0.0, 0, 0, 0.0, 0.0)
        } else {
            let sum: u128 = self.intervals_ns.iter().map(|&x| x as u128).sum();
            let mean = sum as f64 / n as f64;
            let min = *self.intervals_ns.iter().min().unwrap();
            let max = *self.intervals_ns.iter().max().unwrap();
            let var = self
                .intervals_ns
                .iter()
                .map(|&x| {
                    let d = x as f64 - mean;
                    d * d
                })
                .sum::<f64>()
                / n as f64;
            let abs_dev_mean = self
                .intervals_ns
                .iter()
                .map(|&x| (x as f64 - target_ns).abs())
                .sum::<f64>()
                / n as f64;
            (mean, min, max, var.sqrt(), abs_dev_mean)
        };

        // Absolute deviation percentiles.
        let mut abs_dev: Vec<u64> = self
            .intervals_ns
            .iter()
            .map(|&x| (x as f64 - target_ns).abs() as u64)
            .collect();
        abs_dev.sort_unstable();
        let pct = |p: f64| -> u64 {
            if abs_dev.is_empty() {
                0
            } else {
                let idx = ((p / 100.0) * (abs_dev.len() - 1) as f64).round() as usize;
                abs_dev[idx]
            }
        };

        let late_ns = (self.target_period + self.late_threshold).as_nanos() as u64;
        let late = self.intervals_ns.iter().filter(|&&x| x > late_ns).count();

        let histogram = build_histogram(&self.intervals_ns, target_ns);

        let latency_mean = if self.latencies_ns.is_empty() {
            0.0
        } else {
            self.latencies_ns.iter().map(|&x| x as f64).sum::<f64>()
                / self.latencies_ns.len() as f64
        };
        let latency_max = self.latencies_ns.iter().copied().max().unwrap_or(0);

        ProbeReport {
            mode,
            pinned_core,
            target_period: self.target_period,
            sample_count: n,
            mean_interval_ns: mean as u64,
            min_interval_ns: min,
            max_interval_ns: max,
            jitter_stddev_ns: stddev as u64,
            jitter_abs_mean_ns: abs_dev_mean as u64,
            dev_p50_ns: pct(50.0),
            dev_p95_ns: pct(95.0),
            dev_p99_ns: pct(99.0),
            dev_max_ns: abs_dev.last().copied().unwrap_or(0),
            late_frames: late,
            resends: self.resends,
            drops: self.drops,
            generation_mismatches: self.generation_mismatches,
            latency_mean_ns: latency_mean as u64,
            latency_max_ns: latency_max,
            histogram,
            outputs,
        }
    }
}

/// Absolute-deviation histogram bucket boundaries, in microseconds. A sample's
/// signed deviation `interval - target` is bucketed by magnitude.
const BUCKET_BOUNDS_US: [u64; 6] = [50, 100, 250, 500, 1000, 2000];

fn build_histogram(intervals_ns: &[u64], target_ns: f64) -> Vec<HistogramBucket> {
    let mut counts = vec![0u64; BUCKET_BOUNDS_US.len() + 1];
    for &x in intervals_ns {
        let dev_us = ((x as f64 - target_ns).abs() / 1000.0) as u64;
        let mut placed = false;
        for (i, &bound) in BUCKET_BOUNDS_US.iter().enumerate() {
            if dev_us < bound {
                counts[i] += 1;
                placed = true;
                break;
            }
        }
        if !placed {
            *counts.last_mut().unwrap() += 1;
        }
    }
    let mut buckets = Vec::with_capacity(counts.len());
    let mut lo = 0u64;
    for (i, &count) in counts.iter().enumerate() {
        let hi = BUCKET_BOUNDS_US.get(i).copied();
        buckets.push(HistogramBucket {
            lo_us: lo,
            hi_us: hi,
            count,
        });
        if let Some(h) = hi {
            lo = h;
        }
    }
    buckets
}

/// One histogram bucket: samples whose |interval − target| falls in
/// `[lo_us, hi_us)` microseconds (`hi_us = None` is the overflow bucket).
#[derive(Debug, Clone)]
pub struct HistogramBucket {
    pub lo_us: u64,
    pub hi_us: Option<u64>,
    pub count: u64,
}

/// The aggregated timing report. Everything a probe consumer (a CLI dump today,
/// the UI later) needs to judge whether the timing is acceptable.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    /// The scheduling mode actually achieved on this machine — queryable state,
    /// not just a log line (surfaced in the UI when it exists).
    pub mode: SchedulingMode,
    /// The core the output thread was pinned to, if affinity took effect.
    pub pinned_core: Option<usize>,
    pub target_period: Duration,
    pub sample_count: usize,
    pub mean_interval_ns: u64,
    pub min_interval_ns: u64,
    pub max_interval_ns: u64,
    /// Standard deviation of the inter-send interval — the headline jitter number.
    pub jitter_stddev_ns: u64,
    /// Mean absolute deviation from the target period.
    pub jitter_abs_mean_ns: u64,
    pub dev_p50_ns: u64,
    pub dev_p95_ns: u64,
    pub dev_p99_ns: u64,
    pub dev_max_ns: u64,
    /// Ticks whose interval exceeded target + late-threshold.
    pub late_frames: usize,
    /// Ticks that resent the previous frame (no fresh frame was ready).
    pub resends: u64,
    /// Output frames dropped by a failed send.
    pub drops: u64,
    /// Frames skipped because the plan's layout generation did not match the
    /// master buffer's. Non-zero means a stale plan/buffer pairing — a bug the
    /// generation guard caught before it corrupted output (see
    /// [`crate::router::GenerationMismatch`]).
    pub generation_mismatches: u64,
    pub latency_mean_ns: u64,
    pub latency_max_ns: u64,
    pub histogram: Vec<HistogramBucket>,
    /// Per-output link state and counters at end of run.
    pub outputs: Vec<OutputStatus>,
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.3} ms", ns as f64 / 1_000_000.0)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1_000.0)
    } else {
        format!("{ns} ns")
    }
}

impl fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target_ns = self.target_period.as_nanos() as u64;
        writeln!(f, "── Timing report ──────────────────────────────")?;
        writeln!(f, "  scheduling mode : {}", self.mode.label())?;
        writeln!(
            f,
            "  core affinity   : {}",
            match self.pinned_core {
                Some(c) => format!("pinned to core {c}"),
                None => "none (floating)".to_string(),
            }
        )?;
        writeln!(
            f,
            "  target rate     : {:.2} Hz  (period {})",
            1.0 / self.target_period.as_secs_f64(),
            fmt_ns(target_ns)
        )?;
        writeln!(f, "  samples         : {}", self.sample_count)?;
        writeln!(
            f,
            "  interval        : mean {}  min {}  max {}",
            fmt_ns(self.mean_interval_ns),
            fmt_ns(self.min_interval_ns),
            fmt_ns(self.max_interval_ns)
        )?;
        writeln!(
            f,
            "  jitter          : stddev {}  abs-mean {}",
            fmt_ns(self.jitter_stddev_ns),
            fmt_ns(self.jitter_abs_mean_ns)
        )?;
        writeln!(
            f,
            "  |deviation|     : p50 {}  p95 {}  p99 {}  max {}",
            fmt_ns(self.dev_p50_ns),
            fmt_ns(self.dev_p95_ns),
            fmt_ns(self.dev_p99_ns),
            fmt_ns(self.dev_max_ns)
        )?;
        writeln!(
            f,
            "  late / resend / drop : {} / {} / {}",
            self.late_frames, self.resends, self.drops
        )?;
        if self.generation_mismatches > 0 {
            writeln!(
                f,
                "  ⚠ generation mismatches : {} (stale plan/layout — output held)",
                self.generation_mismatches
            )?;
        }
        writeln!(
            f,
            "  produced→wire   : mean {}  max {}",
            fmt_ns(self.latency_mean_ns),
            fmt_ns(self.latency_max_ns)
        )?;
        writeln!(f, "  interval deviation distribution:")?;
        let total = self.sample_count.max(1) as f64;
        for b in &self.histogram {
            let label = match b.hi_us {
                Some(hi) => format!("{:>4}–{:<4} µs", b.lo_us, hi),
                None => format!("  ≥{:<5} µs", b.lo_us),
            };
            let pct = 100.0 * b.count as f64 / total;
            let bar_len = (pct / 2.0).round() as usize;
            writeln!(
                f,
                "    {label} | {:>7} {:5.1}% {}",
                b.count,
                pct,
                "█".repeat(bar_len)
            )?;
        }
        writeln!(f, "  outputs:")?;
        for o in &self.outputs {
            writeln!(
                f,
                "    {} → {}  [{:?}]  sent {}  errors {}",
                o.bind, o.target, o.state, o.sent, o.send_errors
            )?;
        }
        write!(f, "───────────────────────────────────────────────")
    }
}

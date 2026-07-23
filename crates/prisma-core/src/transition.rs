//! Clock-driven transitions (fades) between two states (CLAUDE.md M4).
//!
//! A [`Transition`] is defined against **show time**, in one of two units:
//!
//! - [`TimeBase::Seconds`] — a wall-length fade (in show seconds). Unaffected by
//!   tempo.
//! - [`TimeBase::Beats`] — a musically-locked fade. Its length in **beats** is
//!   the invariant: change the BPM mid-fade and it still spans the same beats,
//!   so its wall duration re-scales with tempo (a 4-beat fade stays 4 beats).
//!
//! Both read the clock's show quantities, which **freeze when show time pauses**.
//! A paused fade therefore holds its progress and continues from there on resume
//! — it never jumps to the end. This falls straight out of measuring against show
//! time; there is no pause-handling code in `Transition` itself.
//!
//! `Transition` computes a normalized, eased `progress` in `0.0..=1.0`. It does
//! not know what it is fading — the caller lerps its own endpoints (a dimmer, a
//! colour, an opacity) with [`Transition::eased`].

use crate::clock::ClockSnapshot;

/// Which show quantity a transition is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBase {
    /// Show seconds. Tempo-independent.
    Seconds,
    /// Beats. Preserves its length in beats across BPM changes.
    Beats,
}

/// Easing applied to linear progress before it is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Map linear `t` in `0..=1` to eased `0..=1`.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => t * (2.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = 2.0 * t - 1.0;
                    0.5 + 0.5 * (u * (2.0 - u))
                }
            }
        }
    }
}

/// A fade running on clock time, independent of render rate.
#[derive(Debug, Clone, Copy)]
pub struct Transition {
    base: TimeBase,
    /// Start position, in the base's units (show seconds or beats).
    start: f64,
    /// Duration in the base's units. Zero means "instant" (always complete).
    duration: f64,
    easing: Easing,
}

impl Transition {
    /// A fade of `seconds` show-seconds, starting at `now`.
    pub fn seconds(now: &ClockSnapshot, seconds: f64, easing: Easing) -> Self {
        Self {
            base: TimeBase::Seconds,
            start: now.show_seconds(),
            duration: seconds.max(0.0),
            easing,
        }
    }

    /// A fade of `beats`, starting at `now`. Preserves its length in beats across
    /// BPM changes.
    pub fn beats(now: &ClockSnapshot, beats: f64, easing: Easing) -> Self {
        Self {
            base: TimeBase::Beats,
            start: now.beat,
            duration: beats.max(0.0),
            easing,
        }
    }

    /// Position of `now` in this transition's base units.
    fn position(&self, now: &ClockSnapshot) -> f64 {
        match self.base {
            TimeBase::Seconds => now.show_seconds(),
            TimeBase::Beats => now.beat,
        }
    }

    /// Raw (un-eased) progress in `0.0..=1.0`. Freezes automatically while show
    /// time is paused, because `position` stops advancing.
    pub fn raw_progress(&self, now: &ClockSnapshot) -> f32 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        let elapsed = self.position(now) - self.start;
        (elapsed / self.duration).clamp(0.0, 1.0) as f32
    }

    /// Eased progress in `0.0..=1.0` — feed this to a lerp.
    pub fn eased(&self, now: &ClockSnapshot) -> f32 {
        self.easing.apply(self.raw_progress(now))
    }

    /// Whether the fade has reached its end.
    pub fn is_complete(&self, now: &ClockSnapshot) -> bool {
        self.raw_progress(now) >= 1.0
    }

    /// Convenience: linearly interpolate `from`..`to` by the eased progress.
    pub fn lerp(&self, from: f32, to: f32, now: &ClockSnapshot) -> f32 {
        from + (to - from) * self.eased(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MasterClock;
    use crate::time::ManualClock;
    use std::sync::Arc;

    const SEC: u64 = 1_000_000_000;

    fn manual() -> (Arc<ManualClock>, MasterClock) {
        let w = Arc::new(ManualClock::new());
        (w.clone(), MasterClock::new(w))
    }

    #[test]
    fn seconds_fade_runs_on_clock_time() {
        let (w, mut c) = manual();
        c.play();
        let start = c.tick();
        let t = Transition::seconds(&start, 2.0, Easing::Linear);

        w.advance(SEC);
        let mid = c.tick();
        assert!((t.raw_progress(&mid) - 0.5).abs() < 1e-6);

        w.advance(SEC);
        let end = c.tick();
        assert!(t.is_complete(&end));
    }

    #[test]
    fn fade_freezes_while_paused_and_resumes() {
        let (w, mut c) = manual();
        c.play();
        let start = c.tick();
        let t = Transition::seconds(&start, 2.0, Easing::Linear);

        w.advance(SEC);
        let s = c.tick();
        let p_before = t.raw_progress(&s); // 0.5

        // Pause and let a lot of wall time pass.
        c.pause();
        w.advance(10 * SEC);
        let s = c.tick();
        assert!(
            (t.raw_progress(&s) - p_before).abs() < 1e-6,
            "must freeze, not jump"
        );

        // Resume: continues from 0.5, completes after one more show-second.
        c.play();
        w.advance(SEC);
        let s = c.tick();
        assert!(t.is_complete(&s));
    }

    #[test]
    fn beat_fade_preserves_beats_across_bpm_change() {
        let (w, mut c) = manual();
        c.set_bpm(120.0); // 2 beats/sec
        c.play();
        let start = c.tick(); // beat 0
        let t = Transition::beats(&start, 4.0, Easing::Linear); // 4-beat fade

        // 1 second → 2 beats → halfway through a 4-beat fade.
        w.advance(SEC);
        let s = c.tick();
        assert!((t.raw_progress(&s) - 0.5).abs() < 1e-6);

        // Double the tempo mid-fade. The fade is still 4 beats; the remaining
        // 2 beats now take only 0.5 s of wall time instead of 1 s.
        c.set_bpm(240.0); // 4 beats/sec
        w.advance(SEC / 2);
        let s = c.tick();
        assert!(t.is_complete(&s), "4 beats elapsed regardless of tempo");
    }

    #[test]
    fn seconds_fade_is_unaffected_by_bpm() {
        let (w, mut c) = manual();
        c.set_bpm(120.0);
        c.play();
        let start = c.tick();
        let t = Transition::seconds(&start, 2.0, Easing::Linear);

        c.set_bpm(240.0); // tempo change must not touch a seconds fade
        w.advance(SEC);
        let s = c.tick();
        assert!((t.raw_progress(&s) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn easing_endpoints_are_exact() {
        for e in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            assert!((e.apply(0.0)).abs() < 1e-6);
            assert!((e.apply(1.0) - 1.0).abs() < 1e-6);
        }
    }
}

//! Rate plan: base tick rate + per-output integer divisors (CLAUDE.md §1.4).
//!
//! There is no global frame rate. The base tick runs at the rate of the fastest
//! active output; every slower output takes an **integer divisor** of that tick
//! (every Nth frame), never a fractional cadence — fractional cadence is what
//! produces visible judder on continuous motion.

use std::time::Duration;

/// Art-Net's practical per-universe ceiling. The spec discourages exceeding
/// ~44 Hz per universe (CLAUDE.md §1.4). Used as the default base when no
/// faster output is present.
pub const ARTNET_MAX_HZ: u32 = 44;

/// A computed rate plan: one base rate, plus an integer divisor per output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatePlan {
    base_hz: u32,
    divisors: Vec<u32>,
}

impl RatePlan {
    /// Compute the plan from each output's desired rate. The base is the fastest
    /// desired rate; each output takes the nearest integer divisor of the base
    /// (`>= 1`), so its effective rate is `base / divisor`.
    pub fn compute(desired_hz: &[u32]) -> Self {
        let base_hz = desired_hz
            .iter()
            .copied()
            .max()
            .unwrap_or(ARTNET_MAX_HZ)
            .max(1);
        let divisors = desired_hz
            .iter()
            .map(|&hz| {
                let hz = hz.max(1);
                ((base_hz as f64 / hz as f64).round() as u32).max(1)
            })
            .collect();
        Self { base_hz, divisors }
    }

    /// The base tick rate (Hz) the output thread runs at.
    #[inline]
    pub fn base_hz(&self) -> u32 {
        self.base_hz
    }

    /// The base tick period.
    #[inline]
    pub fn base_period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.base_hz as f64)
    }

    /// Number of outputs in the plan.
    #[inline]
    pub fn output_count(&self) -> usize {
        self.divisors.len()
    }

    /// Integer divisor for output `i`.
    #[inline]
    pub fn divisor(&self, i: usize) -> u32 {
        self.divisors[i]
    }

    /// Effective transmission rate (Hz) of output `i`.
    #[inline]
    pub fn effective_hz(&self, i: usize) -> u32 {
        self.base_hz / self.divisors[i]
    }

    /// Whether output `i` transmits on tick `tick` (every Nth tick where
    /// N is its divisor).
    #[inline]
    pub fn should_send(&self, i: usize, tick: u64) -> bool {
        tick.is_multiple_of(self.divisors[i] as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_artnet_output_is_base_with_divisor_one() {
        let plan = RatePlan::compute(&[44]);
        assert_eq!(plan.base_hz(), 44);
        assert_eq!(plan.divisor(0), 1);
        assert_eq!(plan.effective_hz(0), 44);
        assert!(plan.should_send(0, 0));
        assert!(plan.should_send(0, 1));
    }

    #[test]
    fn slower_output_takes_integer_divisor() {
        // 44 Hz and 22 Hz -> base 44, the 22 Hz output sends every other tick.
        let plan = RatePlan::compute(&[44, 22]);
        assert_eq!(plan.base_hz(), 44);
        assert_eq!(plan.divisor(1), 2);
        assert_eq!(plan.effective_hz(1), 22);
        assert!(plan.should_send(1, 0));
        assert!(!plan.should_send(1, 1));
        assert!(plan.should_send(1, 2));
    }

    #[test]
    fn base_is_the_fastest_output() {
        let plan = RatePlan::compute(&[30, 60]);
        assert_eq!(plan.base_hz(), 60);
        assert_eq!(plan.divisor(0), 2); // 30 Hz -> every other tick
        assert_eq!(plan.divisor(1), 1); // 60 Hz -> the base
    }

    #[test]
    fn non_integer_ratio_rounds_to_nearest_divisor() {
        // 60 base, 44 desired: 60/44 = 1.36 -> divisor 1 (effective 60, capped
        // by the base). This is why an Art-Net output must not be paired with a
        // faster base above ~2x its ceiling; rate selection lives above M1.
        let plan = RatePlan::compute(&[60, 44]);
        assert_eq!(plan.divisor(1), 1);
    }
}

//! Per-zone always-live state and the canonical zone-state predicates
//! (CLAUDE.md §4.3).
//!
//! M5 introduces the minimal live state its done-criterion needs — dimmer, colour
//! filter, speed, solo, blackout. M8 wraps this in the full `Show` (scenes, cells,
//! persistence, uuid-keyed zones); here zones are addressed by **slot**, their
//! index in `Patch::zones`, which is the same coordinate the patch index and the
//! overlap counter use.
//!
//! # Two predicates, deliberately separate
//!
//! [`ZoneStates::contributes_pixels`] and [`ZoneStates::is_active`] answer related
//! but different questions and feed different subsystems. They live here and
//! nothing recomputes them ad hoc:
//!
//! - **`contributes_pixels`** — is this zone putting light into the composite
//!   *right now*? Consumed by the overlap counter (§5.2.1) and the compositor. A
//!   zone silenced by *another* zone's solo contributes nothing, so it must neither
//!   count toward overlap nor be composited — hence the solo term.
//! - **`is_active`** — does this zone have live state, regardless of what solo is
//!   doing elsewhere? Consumed by MIDI feedback (pad LEDs) and the preview. A zone
//!   muted by someone else's solo still *has* state; its pad must not vanish from
//!   the console because solo was pressed on a different zone. Hence **no** solo
//!   term.
//!
//! Both read the hysteretic [`dimmer_up`](ZoneLiveState::dimmer_up) latch, never
//! the raw dimmer.

/// The dimmer must rise above this for the latch to set.
pub const DIMMER_UP_RISE: f32 = 0.001;
/// The dimmer must fall below this for the latch to clear.
pub const DIMMER_UP_FALL: f32 = 0.0005;

/// One zone's always-live performable state (§4.3 `ZoneLiveState`).
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneLiveState {
    dimmer: f32,
    /// Hysteretic latch over `dimmer`; see [`ZoneLiveState::dimmer_up`].
    dimmer_up: bool,
    /// Multiplied into this zone's output (linear light).
    pub color_filter: [f32; 3],
    /// Scales animation timing for this zone's effects; applied in render.
    pub speed_multiplier: f32,
    pub solo: bool,
    pub blackout: bool,
}

impl Default for ZoneLiveState {
    fn default() -> Self {
        Self {
            dimmer: 0.0,
            dimmer_up: false,
            color_filter: [1.0, 1.0, 1.0],
            speed_multiplier: 1.0,
            solo: false,
            blackout: false,
        }
    }
}

impl ZoneLiveState {
    /// The raw dimmer, `0.0..=1.0`.
    #[inline]
    pub fn dimmer(&self) -> f32 {
        self.dimmer
    }

    /// Set the dimmer and update the latch.
    #[inline]
    pub fn set_dimmer(&mut self, value: f32) {
        self.dimmer = value.clamp(0.0, 1.0);
        // Hysteresis: rise above RISE to set, fall below FALL to clear, hold in
        // the dead band between. Without this a noisy analog fader parked at zero
        // would toggle the predicate every frame — and because the overlap counter
        // keys off it, that would change a pixel's *pipeline topology* frame to
        // frame (§4.3).
        if self.dimmer > DIMMER_UP_RISE {
            self.dimmer_up = true;
        } else if self.dimmer < DIMMER_UP_FALL {
            self.dimmer_up = false;
        }
    }

    /// The latched "dimmer is up" state. **Never compare the raw dimmer to a
    /// threshold anywhere else** — this latch is the single source of truth.
    #[inline]
    pub fn dimmer_up(&self) -> bool {
        self.dimmer_up
    }

    /// Does this zone have live state? No solo term — see the module docs.
    #[inline]
    pub fn is_active(&self) -> bool {
        !self.blackout && self.dimmer_up
    }
}

/// All zones' live state, plus the solo bookkeeping the predicates need.
#[derive(Debug, Clone, Default)]
pub struct ZoneStates {
    zones: Vec<ZoneLiveState>,
    /// How many zones are soloed. Kept incrementally so the "is any zone soloed"
    /// term of `contributes_pixels` is O(1), and so the counter can tell when the
    /// count **crosses zero** — the only transition that changes every zone's
    /// predicate at once (§5.2.1).
    solo_count: usize,
}

impl ZoneStates {
    /// State for `zone_count` zones, all at their defaults (dimmer down, so none
    /// contributes until something drives them).
    pub fn new(zone_count: usize) -> Self {
        Self {
            zones: vec![ZoneLiveState::default(); zone_count],
            solo_count: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.zones.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    #[inline]
    pub fn get(&self, slot: usize) -> &ZoneLiveState {
        &self.zones[slot]
    }

    /// Whether any zone is soloed.
    #[inline]
    pub fn any_solo(&self) -> bool {
        self.solo_count > 0
    }

    /// Is this zone putting light into the composite right now? Consumed by the
    /// overlap counter and the compositor (§5.2.1). Includes the solo term.
    #[inline]
    pub fn contributes_pixels(&self, slot: usize) -> bool {
        let z = &self.zones[slot];
        !z.blackout && z.dimmer_up && (self.solo_count == 0 || z.solo)
    }

    /// Does this zone have live state, independent of solo elsewhere? Consumed by
    /// MIDI feedback and the preview. No solo term.
    #[inline]
    pub fn is_active(&self, slot: usize) -> bool {
        self.zones[slot].is_active()
    }

    pub fn set_dimmer(&mut self, slot: usize, value: f32) {
        self.zones[slot].set_dimmer(value);
    }

    pub fn set_blackout(&mut self, slot: usize, on: bool) {
        self.zones[slot].blackout = on;
    }

    pub fn set_color_filter(&mut self, slot: usize, rgb: [f32; 3]) {
        self.zones[slot].color_filter = rgb;
    }

    pub fn set_speed_multiplier(&mut self, slot: usize, value: f32) {
        self.zones[slot].speed_multiplier = value;
    }

    /// Set a zone's solo, maintaining `solo_count`. Returns whether the count
    /// **crossed zero** — the caller (the overlap counter) uses that to know
    /// whether every zone's predicate must be re-evaluated or only this one.
    pub fn set_solo(&mut self, slot: usize, on: bool) -> bool {
        let z = &mut self.zones[slot];
        if z.solo == on {
            return false;
        }
        z.solo = on;
        let before = self.solo_count;
        if on {
            self.solo_count += 1;
        } else {
            self.solo_count -= 1;
        }
        (before == 0) != (self.solo_count == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimmer_latch_has_a_dead_band() {
        let mut z = ZoneLiveState::default();
        assert!(!z.dimmer_up(), "starts down");

        z.set_dimmer(0.5);
        assert!(z.dimmer_up());

        // Inside the dead band: holds its previous value, in BOTH directions.
        z.set_dimmer(0.0008);
        assert!(z.dimmer_up(), "dead band holds the previous true");

        z.set_dimmer(0.0001); // below FALL
        assert!(!z.dimmer_up());

        z.set_dimmer(0.0008); // back into the band from below
        assert!(!z.dimmer_up(), "dead band holds the previous false");

        z.set_dimmer(0.002); // above RISE
        assert!(z.dimmer_up());
    }

    #[test]
    fn a_noisy_fader_in_the_dead_band_never_flaps() {
        // The failure this latch exists to prevent: an analog fader jittering
        // around zero must not toggle the predicate (and thus a pixel's pipeline
        // topology) frame to frame.
        let mut z = ZoneLiveState::default();
        z.set_dimmer(0.0);
        assert!(!z.dimmer_up());
        for jitter in [0.0006, 0.0009, 0.0007, 0.001, 0.0006, 0.0008] {
            z.set_dimmer(jitter);
            assert!(
                !z.dimmer_up(),
                "jitter inside the band must not raise the latch"
            );
        }
    }

    #[test]
    fn predicates_differ_only_by_the_solo_term() {
        let mut zs = ZoneStates::new(3);
        for slot in 0..3 {
            zs.set_dimmer(slot, 1.0);
        }
        // No solo: everything contributes and is active.
        for slot in 0..3 {
            assert!(zs.contributes_pixels(slot));
            assert!(zs.is_active(slot));
        }

        // Solo zone 1: only it contributes, but ALL three stay `is_active` — a
        // zone muted by someone else's solo must not vanish from the console.
        zs.set_solo(1, true);
        assert!(!zs.contributes_pixels(0));
        assert!(zs.contributes_pixels(1));
        assert!(!zs.contributes_pixels(2));
        for slot in 0..3 {
            assert!(
                zs.is_active(slot),
                "solo elsewhere must not clear is_active"
            );
        }
    }

    #[test]
    fn blackout_and_dimmer_clear_both_predicates() {
        let mut zs = ZoneStates::new(1);
        zs.set_dimmer(0, 1.0);
        assert!(zs.contributes_pixels(0) && zs.is_active(0));

        zs.set_blackout(0, true);
        assert!(!zs.contributes_pixels(0) && !zs.is_active(0));

        zs.set_blackout(0, false);
        zs.set_dimmer(0, 0.0);
        assert!(!zs.contributes_pixels(0) && !zs.is_active(0));
    }

    #[test]
    fn solo_count_reports_only_the_zero_crossing() {
        let mut zs = ZoneStates::new(3);
        // 0 -> 1 crosses.
        assert!(zs.set_solo(0, true));
        // 1 -> 2 does not: only the pressed zone's predicate changes.
        assert!(!zs.set_solo(1, true));
        // 2 -> 1 does not.
        assert!(!zs.set_solo(1, false));
        // 1 -> 0 crosses back.
        assert!(zs.set_solo(0, false));
        // Redundant writes are no-ops.
        assert!(!zs.set_solo(0, false));
    }
}

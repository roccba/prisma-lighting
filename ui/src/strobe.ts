// Strobe speed mapping, shared by the per-zone strobe (ZoneCard) and the global strobe
// (GlobalBar). The on-screen control is a 0..1 "speed"; the engine takes Hz. The range
// stays away from 0 so that engaging the strobe always visibly flashes — "off" is
// expressed by NOT engaging the ⚡, never by a 0 rate. Fine control is meant for MIDI
// later (the operator's intent), so the on-screen rate is deliberately coarse (§3.6).

export const STROBE_MIN_HZ = 1;
export const STROBE_MAX_HZ = 25;

/** A 0..1 strobe "speed" → Hz for the engine. */
export const strobeHz = (rate: number) =>
  STROBE_MIN_HZ + Math.min(1, Math.max(0, rate)) * (STROBE_MAX_HZ - STROBE_MIN_HZ);

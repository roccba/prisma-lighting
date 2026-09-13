// Strobe speed mapping, shared by the per-zone strobe (ZoneCard) and the global strobe
// (GlobalBar). The on-screen control is a 0..1 "speed"; the engine takes Hz over the same
// 1–25 Hz range its `strobe/rate` parameter declares (keep in lockstep with
// `STROBE_MIN_HZ`/`STROBE_MAX_HZ` in live_params.rs). "Off" is the separate `strobe/on`
// toggle, never a 0 rate.

export const STROBE_MIN_HZ = 1;
export const STROBE_MAX_HZ = 25;

/** A 0..1 strobe "speed" → Hz for the engine. */
export const strobeHz = (rate: number) =>
  STROBE_MIN_HZ + Math.min(1, Math.max(0, rate)) * (STROBE_MAX_HZ - STROBE_MIN_HZ);

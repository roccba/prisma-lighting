import { useEffect, useState } from "react";
import { masterState, setMasterBlackout, setMasterDimmer, setMasterStrobe } from "./api";
import { Fader } from "./Fader";
import { strobeHz } from "./strobe";

// The global operation bar (M8-SPEC §3.6): master dimmer + blackout and the GLOBAL
// strobe, applied ABOVE all zones at the output stage. BPM/chaser are out of this
// increment (BPM dropped by decision; chaser waits on a groups model). Self-contained
// (§7): it owns its own state, seeded once from the engine, and writes each change live.
//
// The global strobe mirrors the per-zone one: a ⚡ that engages on HOLD (momentary) or
// DOUBLE-CLICK (latched), plus a 0..1 speed. Same idiom everywhere, MIDI-mappable later.

export function GlobalBar() {
  const [dimmer, setDimmer] = useState(1);
  const [blackout, setBlackout] = useState(false);
  const [strobeRate, setStrobeRate] = useState(0.4);
  const [strobeHeld, setStrobeHeld] = useState(false);
  const [strobeLatched, setStrobeLatched] = useState(false);
  const strobeOn = strobeHeld || strobeLatched;

  // Seed from the engine once, so the bar reflects a loaded show / another transport.
  useEffect(() => {
    masterState()
      .then((m) => {
        setDimmer(m.dimmer);
        setBlackout(m.blackout);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    setMasterStrobe(strobeOn ? strobeHz(strobeRate) : 0, 0.5).catch(() => {});
  }, [strobeOn, strobeRate]);

  const changeDimmer = (v: number) => {
    setDimmer(v);
    setMasterDimmer(v).catch(() => {});
  };
  const toggleBlackout = () => {
    const on = !blackout;
    setBlackout(on);
    setMasterBlackout(on).catch(() => {});
  };

  return (
    <div className="pr-globalbar">
      <div className="pr-globalbar-group">
        <span className="pr-globalbar-label">Master</span>
        <div className="pr-globalbar-fader">
          <Fader value={dimmer} onChange={changeDimmer} aria-label="Master dimmer" />
        </div>
        <span className="pr-value">{Math.round(dimmer * 100)}</span>
        <button
          className={`pr-btn${blackout ? " is-active" : ""}`}
          onClick={toggleBlackout}
          title="Blackout — apaga todo sin mover el fader"
        >
          BO
        </button>
      </div>

      <div className="pr-globalbar-sep" />

      <div className="pr-globalbar-group">
        <span className="pr-globalbar-label">Strobo</span>
        <button
          className={`pr-strobe-trigger pr-strobe-trigger-lg${strobeOn ? " is-on" : ""}${strobeLatched ? " is-latched" : ""}`}
          onPointerDown={() => setStrobeHeld(true)}
          onPointerUp={() => setStrobeHeld(false)}
          onPointerLeave={() => setStrobeHeld(false)}
          onDoubleClick={() => setStrobeLatched((l) => !l)}
          title="Strobo global — mantené = momentáneo, doble clic = fijo"
          aria-pressed={strobeOn}
        >
          ⚡
        </button>
        <label className="pr-zonecard-strobe pr-globalbar-strobe" title="Velocidad del strobo">
          <input
            type="range"
            className="pr-strobe-rate"
            min={0}
            max={1}
            step={0.01}
            value={strobeRate}
            onChange={(e) => setStrobeRate(Number(e.target.value))}
            aria-label="Global strobe rate"
          />
        </label>
      </div>
    </div>
  );
}

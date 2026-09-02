import { useEffect, useState } from "react";
import { masterState, setMasterBlackout, setMasterDimmer, setMasterStrobe } from "./api";
import { VFader } from "./VFader";
import { Bolt } from "./icons";
import { strobeHz } from "./strobe";

// The master column (M8-SPEC §3.6): master dimmer + blackout and the GLOBAL strobe,
// applied ABOVE all zones at the output stage. It lives on the RIGHT of the bottom work
// row, beside the effect parameters — a fixed-width vertical strip (MADRIX-style), not a
// bar. Self-contained (§7): owns its state, seeded once from the engine, writes live.
//
// The global strobe mirrors the per-zone one: a ⚡ that engages on HOLD (momentary) or
// DOUBLE-CLICK (latched), plus a 0..1 speed.

export function GlobalBar() {
  const [dimmer, setDimmer] = useState(1);
  const [blackout, setBlackout] = useState(false);
  const [strobeRate, setStrobeRate] = useState(0.4);
  const [strobeHeld, setStrobeHeld] = useState(false);
  const [strobeLatched, setStrobeLatched] = useState(false);
  const strobeOn = strobeHeld || strobeLatched;

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
    <div className="pr-master">
      <span className="pr-master-title">Master</span>
      <div className="pr-master-fader">
        <VFader value={dimmer} onChange={changeDimmer} aria-label="Master dimmer" />
      </div>
      <span className="pr-value pr-master-value">{Math.round(dimmer * 100)}%</span>
      <div className="pr-master-actions">
        <button
          className={`pr-btn${blackout ? " is-active" : ""}`}
          onClick={toggleBlackout}
          title="Blackout — apaga todo sin mover el fader"
        >
          BO
        </button>
        <button
          className={`pr-strobe-trigger${strobeOn ? " is-on" : ""}${strobeLatched ? " is-latched" : ""}`}
          onPointerDown={() => setStrobeHeld(true)}
          onPointerUp={() => setStrobeHeld(false)}
          onPointerLeave={() => setStrobeHeld(false)}
          onDoubleClick={() => setStrobeLatched((l) => !l)}
          title="Strobo global — mantené = momentáneo, doble clic = fijo"
          aria-pressed={strobeOn}
        >
          <Bolt size={14} />
        </button>
      </div>
      <input
        type="range"
        className="pr-strobe-rate pr-master-strobe-rate"
        min={0}
        max={1}
        step={0.01}
        value={strobeRate}
        onChange={(e) => setStrobeRate(Number(e.target.value))}
        aria-label="Global strobe rate"
        title="Velocidad del strobo"
      />
    </div>
  );
}

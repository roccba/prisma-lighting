import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  masterState,
  setMasterBlackout,
  setMasterDimmer,
  setMasterStrobeOn,
  setMasterStrobeRate,
  type MasterInfo,
} from "./api";
import { VFader } from "./VFader";
import { Bolt } from "./icons";
import { strobeHz } from "./strobe";
import { LearnTarget } from "./midiLearn";
import { scrubHandler } from "./scrub";

// The master column (M8-SPEC §3.6): master dimmer + blackout and the GLOBAL strobe,
// applied ABOVE all zones at the output stage. It lives on the RIGHT of the bottom work
// row, beside the effect parameters — a fixed-width vertical strip (console-style), not a
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
  const [strobeRemote, setStrobeRemote] = useState(false);
  const strobeOn = strobeHeld || strobeLatched || strobeRemote;

  // Seeded from the engine, and re-read whenever a mapped MIDI control moved (M9), so the
  // fader, blackout and strobe follow the hardware.
  useEffect(() => {
    const apply = (m: MasterInfo) => {
      setDimmer(m.dimmer);
      setBlackout(m.blackout);
      setStrobeRemote(m.strobe_on);
      setStrobeRate(m.strobe_rate);
    };
    masterState().then(apply).catch(() => {});
    const un = listen("midi-applied", () => {
      masterState().then(apply).catch(() => {});
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  // Strobe writes happen only on the operator's gestures, never on mount.
  const engage = (held: boolean, latched: boolean) => {
    setStrobeHeld(held);
    setStrobeLatched(latched);
    setStrobeRemote(held || latched);
    setMasterStrobeOn(held || latched).catch(() => {});
  };
  const changeStrobeRate = (v: number) => {
    setStrobeRate(v);
    setMasterStrobeRate(strobeHz(v)).catch(() => {});
  };

  // In compact density the value shows only while the fader moves (theme.css).
  const [moving, setMoving] = useState(false);
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
    <div className={`pr-master${moving ? " is-moving" : ""}`}>
      <div className="pr-master-fader">
        <LearnTarget target={{ path: "/master/dimmer" }} name="Master dimmer">
          <VFader value={dimmer} onChange={changeDimmer} onGrabbedChange={setMoving} aria-label="Master dimmer" />
        </LearnTarget>
      </div>
      <span
        className="pr-value pr-master-value"
        onPointerDown={scrubHandler(() => ({ get: () => dimmer, set: changeDimmer, min: 0, max: 1, perPixel: 1 / 255, step: 1 / 255 }))}
      >
        {Math.round(dimmer * 255)}
      </span>
      <span className="pr-master-label">MASTER</span>
      <div className="pr-master-actions">
        <LearnTarget target={{ path: "/master/blackout" }} name="Blackout master" compact>
          <button
            className={`pr-btn${blackout ? " is-active" : ""}`}
            onClick={toggleBlackout}
            title="Blackout — apaga todo sin mover el fader"
          >
            BLACKOUT
          </button>
        </LearnTarget>
        <LearnTarget target={{ path: "/master/strobe/on" }} name="Strobo global" compact>
          <button
            className={`pr-strobe-trigger${strobeOn ? " is-on" : ""}${strobeLatched ? " is-latched" : ""}`}
            onPointerDown={() => engage(true, strobeLatched)}
            onPointerUp={() => strobeHeld && engage(false, strobeLatched)}
            onPointerLeave={() => strobeHeld && engage(false, strobeLatched)}
            onDoubleClick={() => engage(false, !strobeLatched)}
            title="Strobo global — mantené = momentáneo, doble clic = fijo"
            aria-pressed={strobeOn}
          >
            <Bolt size={14} />
          </button>
        </LearnTarget>
      </div>
      <div className="pr-master-strobe">
      <LearnTarget
        target={{ path: "/master/strobe/rate" }}
        name="Velocidad strobo global"
        compact
      >
        <input
          type="range"
          className="pr-strobe-rate pr-master-strobe-rate pr-master-strobe-wrap"
          min={0}
          max={1}
          step={0.01}
          value={strobeRate}
          onChange={(e) => changeStrobeRate(Number(e.target.value))}
          aria-label="Global strobe rate"
          title="Velocidad del strobo"
        />
      </LearnTarget>
      <span className="pr-strobe-hz" title="Velocidad del strobo (Hz)">
        {Math.round(strobeHz(strobeRate))} Hz
      </span>
      </div>
    </div>
  );
}

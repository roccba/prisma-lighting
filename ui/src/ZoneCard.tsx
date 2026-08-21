import { useCallback, useEffect, useState } from "react";
import { VFader } from "./VFader";
import { Bolt } from "./icons";
import {
  setZoneBlackout,
  setZoneDimmer,
  setZoneSolo,
  setZoneSpeed,
  setZoneStrobe,
  type ZoneInfo,
} from "./api";
import { strobeHz } from "./strobe";

// One zone's card in the zone row (M8-SPEC §3.1): name, a LIVE mini-preview of the zone's
// own fixtures, flanked by two vertical faders — the dimmer on the left and the bipolar
// VELOCITY on the right (0 = static, up = forward, down = reverse) — with
// solo / mute / strobe below. Selecting the card marks it with the accent border and
// changes the UI's *selected* zone; it does NOT change what the lights do (selected ≠
// active, §3.2).
//
// The mini-preview itself is NOT drawn here: the preview slot is an empty framed screen,
// and the zone row paints every card's thumbnail into ONE shared overlay canvas (see
// ZoneRow — the single-accelerated-canvas flicker fix). The card registers its preview
// slot so the row knows where to draw. The per-zone strobe trigger lives in the footer (it
// used to overlay the preview; kept off the canvas so the overlay stays a clean picture).

interface ZoneCardProps {
  zone: ZoneInfo;
  selected: boolean;
  onSelect: () => void;
  /** Register (or, with `null`, unregister) this card's preview slot with the zone row so
   *  the row's single overlay canvas can draw this zone's thumbnail into it. */
  registerPreview: (id: string, el: HTMLElement | null) => void;
}

// Zone velocity is a real -10..10 value; the fader works in 0..1 with 0.5 = 0 (static).
const SPEED_MIN = -10;
const SPEED_MAX = 10;
const speedToNorm = (real: number) => (real - SPEED_MIN) / (SPEED_MAX - SPEED_MIN);
const normToSpeed = (norm: number) => SPEED_MIN + norm * (SPEED_MAX - SPEED_MIN);
const fmtSpeed = (real: number) => {
  const r = Math.abs(real) < 0.05 ? 0 : real;
  return r === 0 ? "0" : r.toFixed(1);
};

export function ZoneCard({ zone, selected, onSelect, registerPreview }: ZoneCardProps) {
  // The card owns its control state after mount (seeded from the show's loaded values),
  // writing each change straight to the engine. Local state keeps the controls
  // responsive; the wire reflects it live.
  const [dimmer, setDimmer] = useState(zone.dimmer);
  const [speed, setSpeed] = useState(zone.speed);
  const [solo, setSolo] = useState(zone.solo);
  const [mute, setMute] = useState(zone.blackout);

  // Per-zone strobe (§3.6). The bolt engages it: HOLD = momentary, DOUBLE-CLICK = latched.
  // `rate` (0..1) is a separate speed. Engaged ⇒ strobe at `strobeHz(rate)`; not engaged ⇒
  // rate 0 (off). Written live to the engine.
  const [strobeRate, setStrobeRate] = useState(0.4);
  const [strobeHeld, setStrobeHeld] = useState(false);
  const [strobeLatched, setStrobeLatched] = useState(false);
  const strobeOn = strobeHeld || strobeLatched;

  useEffect(() => {
    setZoneStrobe(zone.id, strobeOn ? strobeHz(strobeRate) : 0, 0.5).catch(() => {});
  }, [zone.id, strobeOn, strobeRate]);

  // Stable ref callback so React does not deregister/reregister the preview slot on every
  // re-render (a fresh inline arrow would). Keyed on the zone id + the stable registrar.
  const previewRef = useCallback(
    (el: HTMLElement | null) => registerPreview(zone.id, el),
    [zone.id, registerPreview],
  );

  const swatch = `rgb(${zone.color[0]},${zone.color[1]},${zone.color[2]})`;

  const changeDimmer = (v: number) => {
    setDimmer(v);
    setZoneDimmer(zone.id, v).catch(() => {});
  };
  const changeSpeed = (norm: number) => {
    // Detent at 0 (static): a small dead zone around centre snaps to exactly 0, so
    // "leaving it in the middle" truly freezes the zone instead of a tiny residual speed.
    let real = normToSpeed(norm);
    if (Math.abs(real) < 0.4) real = 0;
    setSpeed(real);
    setZoneSpeed(zone.id, real).catch(() => {});
  };
  const toggleSolo = () => {
    const on = !solo;
    setSolo(on);
    setZoneSolo(zone.id, on).catch(() => {});
  };
  const toggleMute = () => {
    const on = !mute;
    setMute(on);
    setZoneBlackout(zone.id, on).catch(() => {});
  };

  return (
    <div
      className={`pr-zonecard${selected ? " is-selected" : ""}`}
      onPointerDown={onSelect}
      role="button"
      tabIndex={0}
      aria-pressed={selected}
    >
      <div className="pr-zonecard-head">
        <span className="pr-zonecard-swatch" style={{ background: swatch }} />
        <span className="pr-zonecard-name">{zone.name}</span>
      </div>

      {/* Body: dimmer fader · mini-preview slot · velocity fader. Faders stop the pointer
          from bubbling to card-select. The preview slot is an empty framed screen; the zone
          row's overlay canvas paints this zone's thumbnail into it. */}
      <div className="pr-zonecard-body">
        <div className="pr-zonecard-fader" onPointerDown={(e) => e.stopPropagation()}>
          <VFader value={dimmer} onChange={changeDimmer} aria-label={`${zone.name} dimmer`} />
          <span className="pr-zonecard-faderval">{Math.round(dimmer * 100)}</span>
          <span className="pr-zonecard-faderlbl">DIM</span>
        </div>

        <div className="pr-zonecard-preview" ref={previewRef} />

        <div className="pr-zonecard-fader" onPointerDown={(e) => e.stopPropagation()}>
          <VFader
            value={speedToNorm(speed)}
            onChange={changeSpeed}
            bipolar
            aria-label={`${zone.name} velocidad`}
          />
          <span className="pr-zonecard-faderval">{fmtSpeed(speed)}</span>
          <span className="pr-zonecard-faderlbl">VEL</span>
        </div>
      </div>

      {/* Footer: solo / mute · strobe trigger · strobe rate (coarse on-screen; meant for
          MIDI, §3.6). The strobe trigger sits here rather than over the preview so the row's
          overlay canvas stays a clean picture. */}
      <div className="pr-zonecard-footer" onPointerDown={(e) => e.stopPropagation()}>
        <button
          className={`pr-btn pr-zonecard-sm${solo ? " is-active" : ""}`}
          onClick={toggleSolo}
          title="Solo"
        >
          S
        </button>
        <button
          className={`pr-btn pr-zonecard-sm${mute ? " is-active" : ""}`}
          onClick={toggleMute}
          title="Mute"
        >
          M
        </button>
        <button
          className={`pr-strobe-trigger pr-strobe-trigger-sm${strobeOn ? " is-on" : ""}${strobeLatched ? " is-latched" : ""}`}
          onPointerDown={(e) => {
            e.stopPropagation();
            setStrobeHeld(true);
          }}
          onPointerUp={() => setStrobeHeld(false)}
          onPointerLeave={() => setStrobeHeld(false)}
          onDoubleClick={() => setStrobeLatched((l) => !l)}
          title="Strobo — mantené = momentáneo, doble clic = fijo"
          aria-label={`${zone.name} strobe`}
          aria-pressed={strobeOn}
        >
          <Bolt size={12} />
        </button>
        <label className="pr-zonecard-strobe" title="Velocidad del strobo">
          <input
            type="range"
            className="pr-strobe-rate"
            min={0}
            max={1}
            step={0.01}
            value={strobeRate}
            onChange={(e) => setStrobeRate(Number(e.target.value))}
            aria-label={`${zone.name} strobe rate`}
          />
        </label>
      </div>
    </div>
  );
}

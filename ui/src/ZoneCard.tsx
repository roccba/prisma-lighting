import { useCallback, useEffect, useRef, useState } from "react";
import { VFader } from "./VFader";
import { Bolt } from "./icons";
import {
  setZoneBlackout,
  setZoneDimmer,
  setZoneSolo,
  setZoneSpeed,
  setZoneStrobeOn,
  setZoneStrobeRate,
  type ZoneInfo,
} from "./api";
import { strobeHz } from "./strobe";
import { LearnTarget } from "./midiLearn";
import { scrubHandler } from "./scrub";

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
  /** Position in the show's zone order — the MIDI mapping address (zone slot, M9). */
  slot: number;
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

export function ZoneCard({ zone, slot, selected, onSelect, registerPreview }: ZoneCardProps) {
  // The card owns its control state after mount (seeded from the show's loaded values),
  // writing each change straight to the engine. Local state keeps the controls
  // responsive; the wire reflects it live.
  const [dimmer, setDimmer] = useState(zone.dimmer);
  const [speed, setSpeed] = useState(zone.speed);
  const [solo, setSolo] = useState(zone.solo);
  const [mute, setMute] = useState(zone.blackout);

  // Follow values set by another transport (a MIDI fader, M9): the zone list is re-fetched
  // on `midi-applied`, so a changed prop is the engine's current value. The UI's own writes
  // never re-fetch the list, so this cannot fight a drag.
  useEffect(() => setDimmer(zone.dimmer), [zone.dimmer]);
  useEffect(() => setSpeed(zone.speed), [zone.speed]);
  useEffect(() => setSolo(zone.solo), [zone.solo]);
  useEffect(() => setMute(zone.blackout), [zone.blackout]);

  // Per-zone strobe (§3.6). The bolt engages it: HOLD = momentary, DOUBLE-CLICK = latched;
  // the slider is its speed (0..1 → 1–25 Hz). Both are tree parameters (M9), so a pad or a
  // knob can drive them too: the card shows the engine's value, refreshed on `midi-applied`.
  const [strobeRate, setStrobeRate] = useState(zone.strobe_rate);
  const [strobeHeld, setStrobeHeld] = useState(false);
  const [strobeLatched, setStrobeLatched] = useState(false);
  const [strobeRemote, setStrobeRemote] = useState(zone.strobe_on);
  useEffect(() => setStrobeRate(zone.strobe_rate), [zone.strobe_rate]);
  useEffect(() => setStrobeRemote(zone.strobe_on), [zone.strobe_on]);
  const strobeOn = strobeHeld || strobeLatched || strobeRemote;

  // Write only on the operator's own gestures (never on mount), so a strobe a pad engaged
  // is not switched off by the card appearing.
  const engage = (held: boolean, latched: boolean) => {
    setStrobeHeld(held);
    setStrobeLatched(latched);
    setStrobeRemote(held || latched);
    setZoneStrobeOn(zone.id, held || latched).catch(() => {});
  };
  const changeStrobeRate = (v: number) => {
    setStrobeRate(v);
    setZoneStrobeRate(zone.id, strobeHz(v)).catch(() => {});
  };

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

  // A selection made from elsewhere (MIDI focus follow) may land on a card scrolled out of
  // the row; bring it into view. `nearest` is a no-op for a card already visible (a click).
  const cardRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (selected) cardRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [selected]);

  return (
    <div
      ref={cardRef}
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
        <LearnTarget
          target={{ zone_slot: { slot, sub: "dimmer" } }}
          name={`Dimmer — ${zone.name}`}
        >
          <div className="pr-zonecard-fader" onPointerDown={(e) => e.stopPropagation()}>
            <VFader value={dimmer} onChange={changeDimmer} aria-label={`${zone.name} dimmer`} />
            <span
              className="pr-zonecard-faderval"
              onPointerDown={scrubHandler(() => ({ get: () => dimmer, set: changeDimmer, min: 0, max: 1, perPixel: 1 / 255, step: 1 / 255 }))}
            >
              {Math.round(dimmer * 255)}
            </span>
            <span className="pr-zonecard-faderlbl">DIM</span>
          </div>
        </LearnTarget>

        <div className="pr-zonecard-preview" ref={previewRef} />

        <LearnTarget
          target={{ zone_slot: { slot, sub: "speed" } }}
          name={`Velocidad — ${zone.name}`}
        >
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
        </LearnTarget>
      </div>

      {/* Footer: solo / mute · strobe trigger · strobe rate (coarse on-screen; meant for
          MIDI, §3.6). The strobe trigger sits here rather than over the preview so the row's
          overlay canvas stays a clean picture. */}
      <div className="pr-zonecard-footer" onPointerDown={(e) => e.stopPropagation()}>
        <LearnTarget
          target={{ zone_slot: { slot, sub: "solo" } }}
          name={`Solo — ${zone.name}`}
          className="pr-learn-compact"
        >
          <button
            className={`pr-btn pr-zonecard-sm${solo ? " is-active" : ""}`}
            onClick={toggleSolo}
            title="Solo"
          >
            S
          </button>
        </LearnTarget>
        <LearnTarget
          target={{ zone_slot: { slot, sub: "blackout" } }}
          name={`Mute — ${zone.name}`}
          className="pr-learn-compact"
        >
          <button
            className={`pr-btn pr-zonecard-sm${mute ? " is-active" : ""}`}
            onClick={toggleMute}
            title="Mute"
          >
            M
          </button>
        </LearnTarget>
        <LearnTarget
          target={{ zone_slot: { slot, sub: "strobe_on" } }}
          name={`Strobo — ${zone.name}`}
          className="pr-learn-compact"
        >
          <button
            className={`pr-strobe-trigger pr-strobe-trigger-sm${strobeOn ? " is-on" : ""}${strobeLatched ? " is-latched" : ""}`}
            onPointerDown={(e) => {
              e.stopPropagation();
              engage(true, strobeLatched);
            }}
            onPointerUp={() => strobeHeld && engage(false, strobeLatched)}
            onPointerLeave={() => strobeHeld && engage(false, strobeLatched)}
            onDoubleClick={() => engage(false, !strobeLatched)}
            title="Strobo — mantené = momentáneo, doble clic = fijo"
            aria-label={`${zone.name} strobe`}
            aria-pressed={strobeOn}
          >
            <Bolt size={12} />
          </button>
        </LearnTarget>
        <LearnTarget
          target={{ zone_slot: { slot, sub: "strobe_rate" } }}
          name={`Velocidad strobo — ${zone.name}`}
          className="pr-learn-compact pr-zonecard-strobe"
        >
          <input
            type="range"
            className="pr-strobe-rate"
            min={0}
            max={1}
            step={0.01}
            value={strobeRate}
            onChange={(e) => changeStrobeRate(Number(e.target.value))}
            aria-label={`${zone.name} strobe rate`}
            title="Velocidad del strobo"
          />
        </LearnTarget>
        <span className="pr-strobe-hz" title="Velocidad del strobo (Hz)">
          {Math.round(strobeHz(strobeRate))}
        </span>
      </div>
    </div>
  );
}

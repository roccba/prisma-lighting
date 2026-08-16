import { useEffect, useRef, useState } from "react";
import { Fader } from "./Fader";
import { usePreviewStream } from "./previewStream";
import { setZoneBlackout, setZoneDimmer, setZoneSolo, type ZoneInfo } from "./api";

// One zone's card in the zone row (M8-SPEC §3.1): name, a LIVE mini-preview of the
// zone's own fixtures (a filter over the shared M6 preview stream — only this zone's
// member pixels), a dimmer fader, and solo / mute. Selecting the card marks it with
// the accent border and changes the UI's *selected* zone — it does NOT change what the
// lights do (selected ≠ active, §3.2); only the fader / solo / mute do that.

interface ZoneCardProps {
  zone: ZoneInfo;
  selected: boolean;
  onSelect: () => void;
}

/** The live mini-preview: this zone's member pixels, at their real positions, filled
 *  with the colour the engine rendered. Redraws each frame from the shared stream. */
function ZoneMiniPreview({ zoneId }: { zoneId: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const stream = usePreviewStream();

  useEffect(() => {
    const draw = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      const dpr = window.devicePixelRatio || 1;
      const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, w, h);

      const d = stream.descriptorRef.current;
      const colors = stream.colorsRef.current;
      if (!d || !colors) return;
      const ze = d.zones.find((z) => z.id === zoneId);
      if (!ze || ze.members.length === 0) return;

      let minX = Infinity,
        maxX = -Infinity,
        minY = Infinity,
        maxY = -Infinity;
      for (const p of ze.members) {
        const px = d.pixels[p];
        if (!px) continue;
        minX = Math.min(minX, px.x);
        maxX = Math.max(maxX, px.x);
        minY = Math.min(minY, px.y);
        maxY = Math.max(maxY, px.y);
      }
      const spanX = Math.max(1e-3, maxX - minX);
      const spanY = Math.max(1e-3, maxY - minY);
      const pad = 6 * dpr;
      const s = Math.min((w - 2 * pad) / spanX, (h - 2 * pad) / spanY);
      const r = Math.max(1, Math.min(s * 0.42, 6 * dpr));
      const ox = (w - s * spanX) / 2 - minX * s;
      const oy = (h - s * spanY) / 2 - minY * s;

      for (const p of ze.members) {
        const px = d.pixels[p];
        if (!px) continue;
        ctx.beginPath();
        ctx.arc(ox + px.x * s, oy + px.y * s, r, 0, Math.PI * 2);
        ctx.fillStyle = `rgb(${colors[p * 3]},${colors[p * 3 + 1]},${colors[p * 3 + 2]})`;
        ctx.fill();
      }
    };

    const unsub = stream.subscribe(draw);
    draw();
    return unsub;
  }, [zoneId, stream]);

  return <canvas ref={canvasRef} className="pr-zonecard-canvas" />;
}

export function ZoneCard({ zone, selected, onSelect }: ZoneCardProps) {
  // The card owns its control state after mount (seeded from the show's loaded values),
  // writing each change straight to the engine. Local state keeps the fader/buttons
  // responsive; the wire reflects it live.
  const [dimmer, setDimmer] = useState(zone.dimmer);
  const [solo, setSolo] = useState(zone.solo);
  const [mute, setMute] = useState(zone.blackout);

  const swatch = `rgb(${zone.color[0]},${zone.color[1]},${zone.color[2]})`;

  const changeDimmer = (v: number) => {
    setDimmer(v);
    setZoneDimmer(zone.id, v).catch(() => {});
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
      <ZoneMiniPreview zoneId={zone.id} />
      {/* Controls stop the click from bubbling to card-select, so adjusting a fader
          does not re-select (though selecting is harmless — it never changes output). */}
      <div className="pr-zonecard-controls" onPointerDown={(e) => e.stopPropagation()}>
        <Fader value={dimmer} onChange={changeDimmer} aria-label={`${zone.name} dimmer`} />
        <div className="pr-zonecard-btns">
          <button
            className={`pr-btn${solo ? " is-active" : ""}`}
            onClick={toggleSolo}
            title="Solo"
          >
            S
          </button>
          <button
            className={`pr-btn${mute ? " is-active" : ""}`}
            onClick={toggleMute}
            title="Mute"
          >
            M
          </button>
        </div>
      </div>
    </div>
  );
}

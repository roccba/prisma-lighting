import { useEffect, useRef, useState } from "react";
import { setParam, type ParamInfo } from "./api";
import { linearToHex, linearToSrgb255, srgb255ToLinear } from "./srgb";

// The effect colour control, MADRIX-style: a swatch square that opens a VERTICAL preset
// palette, plus three 0–255 R/G/B boxes for manual entry. The colour's three channels
// are three scalar params sharing a manifest colour group (§3.5); this drives all three
// at once. Presentation is 255-space sRGB; every write converts to linear-light 0..1
// (§2.3) so the engine's value model is untouched.

interface ColorControlProps {
  zoneId: string;
  r: ParamInfo;
  g: ParamInfo;
  b: ParamInfo;
}

// A compact preset palette (sRGB hex), shown vertically when the swatch is clicked.
const PRESETS = [
  "#ffffff", "#ffbf80", "#ff0000", "#ff7f00", "#ffff00", "#7fff00",
  "#00ff00", "#00ff7f", "#00ffff", "#007fff", "#0000ff", "#7f00ff",
  "#ff00ff", "#ff007f", "#808080", "#000000",
];

function ChannelBox({
  label,
  v255,
  tint,
  onChange,
}: {
  label: string;
  v255: number;
  tint: string;
  onChange: (v: number) => void;
}) {
  return (
    <label className="pr-color-box" style={{ borderTopColor: tint }} title={`${label} 0–255`}>
      <input
        type="number"
        min={0}
        max={255}
        value={v255}
        onChange={(e) => onChange(Math.max(0, Math.min(255, Math.round(Number(e.target.value) || 0))))}
        aria-label={label}
      />
    </label>
  );
}

export function ColorControl({ zoneId, r, g, b }: ColorControlProps) {
  const [lin, setLin] = useState({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  useEffect(() => {
    setLin({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  }, [r.value_norm, g.value_norm, b.value_norm]);

  // The preset palette popover; closes on an outside click.
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [open]);

  const writeChannel = (ch: "r" | "g" | "b", param: ParamInfo, v255: number) => {
    const linv = srgb255ToLinear(v255);
    setLin((p) => ({ ...p, [ch]: linv }));
    setParam(zoneId, param.name, linv).catch(() => {});
  };

  const setHex = (hex: string) => {
    const h = hex.replace("#", "");
    const R = parseInt(h.slice(0, 2), 16);
    const G = parseInt(h.slice(2, 4), 16);
    const B = parseInt(h.slice(4, 6), 16);
    const next = { r: srgb255ToLinear(R), g: srgb255ToLinear(G), b: srgb255ToLinear(B) };
    setLin(next);
    setParam(zoneId, r.name, next.r).catch(() => {});
    setParam(zoneId, g.name, next.g).catch(() => {});
    setParam(zoneId, b.name, next.b).catch(() => {});
  };

  const hex = linearToHex(lin.r, lin.g, lin.b);
  const R = linearToSrgb255(lin.r);
  const G = linearToSrgb255(lin.g);
  const B = linearToSrgb255(lin.b);

  return (
    <div className="pr-color" ref={ref}>
      <div className="pr-color-fields">
        <button
          className="pr-color-swatch-btn"
          style={{ background: hex }}
          onClick={() => setOpen((o) => !o)}
          title="Abrir paleta"
          aria-label="Abrir paleta"
        />
        <ChannelBox label="R" v255={R} tint="#e05555" onChange={(v) => writeChannel("r", r, v)} />
        <ChannelBox label="G" v255={G} tint="#5bbf5b" onChange={(v) => writeChannel("g", g, v)} />
        <ChannelBox label="B" v255={B} tint="#5b8fe0" onChange={(v) => writeChannel("b", b, v)} />
      </div>
      {open && (
        <div className="pr-color-palette" role="listbox">
          {PRESETS.map((p) => (
            <button
              key={p}
              className="pr-color-preset"
              style={{ background: p }}
              title={p}
              onClick={() => {
                setHex(p);
                setOpen(false);
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

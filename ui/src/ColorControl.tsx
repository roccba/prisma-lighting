import { useEffect, useState } from "react";
import { setParam, type ParamInfo } from "./api";
import { hexToLinear, linearToHex, linearToSrgb255 } from "./srgb";

// One compound colour control (M8 parameter types): a swatch that opens the native
// picker, plus the 0–255 sRGB readout the operator reads. The colour's three channels
// are three scalar params sharing a manifest colour group (§3.5, option (a)); this
// control drives all three at once. The picker works in sRGB; each write converts to
// linear-light `0..1` (`hexToLinear`) so the engine's value model is untouched — only
// the presentation is 255-space (§2.3).

interface ColorControlProps {
  zoneId: string;
  label: string;
  r: ParamInfo;
  g: ParamInfo;
  b: ParamInfo;
}

export function ColorControl({ zoneId, label, r, g, b }: ColorControlProps) {
  // Linear channel values (the model). Seeded from the params; re-seeded when a cell
  // fires (the params reload with new values).
  const [lin, setLin] = useState({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  useEffect(() => {
    setLin({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  }, [r.value_norm, g.value_norm, b.value_norm]);

  const change = (hex: string) => {
    const next = hexToLinear(hex);
    setLin(next);
    // Write each channel to its own param path (the three scalars stay independent in
    // the tree/uniform); the running effect picks them up live.
    setParam(zoneId, r.name, next.r).catch(() => {});
    setParam(zoneId, g.name, next.g).catch(() => {});
    setParam(zoneId, b.name, next.b).catch(() => {});
  };

  const hex = linearToHex(lin.r, lin.g, lin.b);
  const rgb255 = `${linearToSrgb255(lin.r)}, ${linearToSrgb255(lin.g)}, ${linearToSrgb255(lin.b)}`;

  return (
    <div className="pr-param-row pr-color-row">
      <span className="pr-param-label">{label}</span>
      <span className="pr-value pr-param-value pr-color-255">{rgb255}</span>
      <label className="pr-color-swatch" style={{ background: hex }} title={`sRGB ${rgb255}`}>
        <input
          type="color"
          value={hex}
          onChange={(e) => change(e.target.value)}
          aria-label={label}
        />
      </label>
    </div>
  );
}

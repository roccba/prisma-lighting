import { useEffect, useState } from "react";
import { usePanelWrites } from "./panelCell";
import { type ParamInfo } from "./api";
import { ByteBox } from "./ByteBox";
import { ColorSwatch, type Rgb } from "./ColorPicker";
import { FaderBox } from "./FaderBox";
import { Sliders } from "./icons";
import { linearToSrgb255, srgb255ToLinear } from "./srgb";

// The effect colour control, console-style: [swatch] [R] [G] [B] [W] [A] [fader box].
//  · the swatch opens the "Selector de color" (press, hold and drag; release to keep);
//  · R/G/B are 0–255 boxes (type, or drag up/down);
//  · W and A are shown like professional consoles but DISABLED: the effects' colours are RGB today
//    (no white channel, no alpha) — the boxes are ready for when they exist;
//  · the fader button opens the "Caja de Fader" with one fader per channel.
// The colour's three channels are three scalar params sharing a manifest colour group
// (§3.5). Presentation is 255-space sRGB; every write converts to linear light 0..1 (§2.3)
// so the engine's value model is untouched.

interface ColorControlProps {
  trackId: string;
  r: ParamInfo;
  g: ParamInfo;
  b: ParamInfo;
}

const TINT = { r: "#e05555", g: "#5bbf5b", b: "#5b8fe0", w: "#9a9a9a", a: "#e8e8e8" };

export function ColorControl({ trackId, r, g, b }: ColorControlProps) {
  const write = usePanelWrites(trackId);
  const [lin, setLin] = useState({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  useEffect(() => {
    setLin({ r: r.value_norm, g: g.value_norm, b: b.value_norm });
  }, [r.value_norm, g.value_norm, b.value_norm]);

    const [faderBoxAt, setFaderBoxAt] = useState<{ x: number; y: number } | null>(null);

  const writeChannel = (ch: "r" | "g" | "b", v255: number) => {
    const param = { r, g, b }[ch];
    const linv = srgb255ToLinear(v255);
    setLin((p) => ({ ...p, [ch]: linv }));
    write.param(param.name, linv);
  };

  const writeRgb = ([R, G, B]: Rgb) => {
    const next = { r: srgb255ToLinear(R), g: srgb255ToLinear(G), b: srgb255ToLinear(B) };
    setLin(next);
    write.param(r.name, next.r);
    write.param(g.name, next.g);
    write.param(b.name, next.b);
  };

  const R = linearToSrgb255(lin.r);
  const G = linearToSrgb255(lin.g);
  const B = linearToSrgb255(lin.b);

  return (
    <div className="pr-color">
      <div className="pr-color-fields">
        <ColorSwatch className="pr-color-swatch-btn" value={[R, G, B]} onChange={writeRgb} />
        <ByteBox label="R" value={R} tint={TINT.r} onChange={(v) => writeChannel("r", v)} />
        <ByteBox label="G" value={G} tint={TINT.g} onChange={(v) => writeChannel("g", v)} />
        <ByteBox label="B" value={B} tint={TINT.b} onChange={(v) => writeChannel("b", v)} />
        <ByteBox label="W" value={0} tint={TINT.w} disabled />
        <ByteBox label="A" value={0} tint={TINT.a} disabled />
        <button
          type="button"
          className={`pr-btn pr-color-faderbtn${faderBoxAt ? " is-active" : ""}`}
          onClick={(e) => {
            const at = { x: e.clientX, y: e.clientY };
            setFaderBoxAt((cur) => (cur ? null : at));
          }}
          title="Caja de Fader"
          aria-label="Caja de Fader"
        >
          <Sliders size={16} />
        </button>
      </div>
      {faderBoxAt && (
        <FaderBox
          channels={[
            { label: "R", chip: "#ff0000", value: R, onChange: (v) => writeChannel("r", v) },
            { label: "G", chip: "#00c000", value: G, onChange: (v) => writeChannel("g", v) },
            { label: "B", chip: "#0040ff", value: B, onChange: (v) => writeChannel("b", v) },
            { label: "W", chip: "#000000", value: 0, disabled: true },
            { label: "A", chip: "#ffffff", value: 0, disabled: true },
          ]}
          preview={<ColorSwatch className="pr-faderbox-swatch" value={[R, G, B]} onChange={writeRgb} />}
          at={faderBoxAt}
          onClose={() => setFaderBoxAt(null)}
        />
      )}
    </div>
  );
}

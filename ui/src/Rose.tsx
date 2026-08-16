import { useEffect, useState } from "react";
import { setParam, type ParamInfo } from "./api";

// The 8-direction rose (M8-SPEC §3.5, screenshot 15), reusing the editor's eight signal
// directions. It is WIRED but dormant: no built-in effect declares a `direction` param
// yet, so the panel never renders one today — it lights up the day an effect's manifest
// asks for it (that is the whole point of manifest-driven types). The direction is
// carried as a scalar `0..1` like any other param; the index→value encoding here is
// provisional (there is no consumer to fix it) and formalises when the first
// direction-taking effect defines it.

interface RoseProps {
  zoneId: string;
  param: ParamInfo;
}

// The editor's eight signal directions, in enum order, each with a compass cell and
// glyph. Index → normalized value is `i / (N - 1)`.
const DIRS = [
  { name: "Right", glyph: "→", col: 3, row: 2 },
  { name: "Left", glyph: "←", col: 1, row: 2 },
  { name: "Down", glyph: "↓", col: 2, row: 3 },
  { name: "Up", glyph: "↑", col: 2, row: 1 },
  { name: "DownRight", glyph: "↘", col: 3, row: 3 },
  { name: "DownLeft", glyph: "↙", col: 1, row: 3 },
  { name: "UpRight", glyph: "↗", col: 3, row: 1 },
  { name: "UpLeft", glyph: "↖", col: 1, row: 1 },
];

export function Rose({ zoneId, param }: RoseProps) {
  const toIndex = (norm: number) => Math.round(norm * (DIRS.length - 1));
  const [index, setIndex] = useState(toIndex(param.value_norm));
  useEffect(() => setIndex(toIndex(param.value_norm)), [param.value_norm]);

  const pick = (i: number) => {
    setIndex(i);
    setParam(zoneId, param.name, i / (DIRS.length - 1)).catch(() => {});
  };

  return (
    <div className="pr-param-row pr-rose-row">
      <span className="pr-param-label">{param.label}</span>
      <div className="pr-rose">
        {DIRS.map((d, i) => (
          <button
            key={d.name}
            className={`pr-rose-btn${i === index ? " is-active" : ""}`}
            style={{ gridColumn: d.col, gridRow: d.row }}
            onClick={() => pick(i)}
            title={d.name}
          >
            {d.glyph}
          </button>
        ))}
      </div>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import type { ParamInfo } from "./api";
import { scrubHandler } from "./scrub";

// The single, reusable frame every effect parameter renders inside, so labels and value
// boxes are IDENTICAL and aligned for every control type (§3.5). You instantiate it with
// the parameter's label (and, for scalars, a `ValueBox`); it lays out label · value ·
// control the same way everywhere — inline for one-line controls (a fader), block (label
// above) for the multi-cell controls (rose, colour, palette).

/** Format a normalized value as its real value. All current effect scalars use linear
 *  curves, so `min + t*(max-min)` is exact; decimals scale with the range so a 1–20 scale
 *  reads "12" and a 0–1 amount reads "0.85". */
export function formatReal(p: ParamInfo, norm: number): string {
  const real = p.min + norm * (p.max - p.min);
  const span = p.max - p.min;
  // Wide ranges read as whole numbers, except small values (the classic 0.01 step width).
  if (span >= 10) return Math.abs(real) < 10 ? String(parseFloat(real.toFixed(2))) : real.toFixed(0);
  return real.toFixed(span >= 3 ? 1 : 2);
}

export function ParamField({
  label,
  value,
  block,
  badge,
  children,
}: {
  label: string;
  /** A small number beside the label: the device knob (1–8) that drives this parameter. */
  badge?: number | null;
  /** The value box element (a `ValueBox`), placed in the value column. Omit for controls
   *  with no scalar readout (rose, colour, palette). */
  value?: React.ReactNode;
  /** Stack the label above the control instead of an inline row. */
  block?: boolean;
  children: React.ReactNode;
}) {
  if (block) {
    return (
      <div className="pr-field pr-field-block">
        <span className="pr-field-label" title={label}>
          {label}
        </span>
        <div className="pr-field-control">{children}</div>
      </div>
    );
  }
  return (
    <div className="pr-field">
      <span className="pr-field-label" title={label}>
        {badge != null && (
          <span className="pr-knob-badge" title={`Perilla ${badge} del controlador`}>
            {badge}
          </span>
        )}
        {label}
      </span>
      {value}
      <div className="pr-field-control">{children}</div>
    </div>
  );
}

/** The consistent, editable value box shared by every scalar parameter. Shows the real
 *  value; type a new one (committed on Enter/blur) and it maps back through the range and
 *  writes via `onCommit`. Controlled by the parent's normalized value so a fader drag and
 *  a typed value stay in sync. */
export function ValueBox({
  param,
  valueNorm,
  onCommit,
}: {
  param: ParamInfo;
  valueNorm: number;
  onCommit: (norm: number) => void;
}) {
  const [text, setText] = useState(() => formatReal(param, valueNorm));
  const editing = useRef(false);
  useEffect(() => {
    if (!editing.current) setText(formatReal(param, valueNorm));
  }, [valueNorm, param]);

  const commit = () => {
    editing.current = false;
    const real = Number(text);
    if (Number.isFinite(real) && param.max > param.min) {
      const norm = Math.max(0, Math.min(1, (real - param.min) / (param.max - param.min)));
      onCommit(norm);
      setText(formatReal(param, norm));
    } else {
      setText(formatReal(param, valueNorm));
    }
  };

  return (
    <input
      className="pr-field-value"
      value={text}
      inputMode="decimal"
      onFocus={() => {
        editing.current = true;
      }}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
      // Drag up/down = move the fader: the whole range over ~200 px.
      onPointerDown={scrubHandler(() => ({ get: () => valueNorm, set: onCommit, min: 0, max: 1, perPixel: 1 / 200 }))}
      aria-label={param.label}
    />
  );
}

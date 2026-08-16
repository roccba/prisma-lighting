import { useEffect, useState } from "react";
import { effectPanel, setParam, type EffectPanelInfo, type ParamInfo } from "./api";
import { Fader } from "./Fader";

// The effect parameter panel for the SELECTED zone (M8-SPEC §3.5): the active cell's
// effect, and one row per manifest parameter — label · value · horizontal fader. The
// rows are built entirely from the effect's manifest (`main_effect_panel`), so the
// panel never hardcodes an effect's controls (§5.3); a new effect gets its panel for
// free the day it declares its params. Moving a fader writes the value live through the
// engine (and persists it into the show), so the change shows at once in the large
// preview beside it and on the wire — the two halves of the "effect work panel".
//
// The 8-direction rose (screenshot 15) is intentionally NOT rendered: no effect in the
// M5 catalogue exposes a direction parameter, so a rose would drive nothing. It returns
// with the first effect whose manifest has a direction to bind — kept manifest-driven,
// not a fake control.

interface EffectParamsProps {
  zoneId: string;
  /** Bumped by the scene grid when the active cell changes, to re-fetch the panel. */
  rev: number;
}

/** Format a normalized value as its real value, for the readout. All current effect
 *  params use linear curves, so `min + t*(max-min)` is exact; decimals scale with the
 *  range so a 1–20 scale reads "12" and a 0–1 amount reads "0.85". */
function formatReal(p: ParamInfo, norm: number): string {
  const real = p.min + norm * (p.max - p.min);
  const span = p.max - p.min;
  const decimals = span >= 10 ? 0 : span >= 3 ? 1 : 2;
  return real.toFixed(decimals);
}

/** One parameter row, owning its fader value locally for a smooth drag while writing
 *  every change straight to the engine. Re-seeds when the panel reloads (`seed`). */
function ParamRow({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const [value, setValue] = useState(param.value_norm);

  // Re-seed if the backing value changed (a cell fired, or the effect swapped).
  useEffect(() => setValue(param.value_norm), [param.value_norm]);

  const change = (v: number) => {
    setValue(v);
    setParam(zoneId, param.name, v).catch(() => {});
  };

  return (
    <div className="pr-param-row">
      <span className="pr-param-label">{param.label}</span>
      <span className="pr-value pr-param-value">{formatReal(param, value)}</span>
      <div className="pr-param-fader">
        <Fader value={value} onChange={change} aria-label={param.label} />
      </div>
    </div>
  );
}

export function EffectParams({ zoneId, rev }: EffectParamsProps) {
  const [panel, setPanel] = useState<EffectPanelInfo | null>(null);

  useEffect(() => {
    let live = true;
    effectPanel(zoneId)
      .then((p) => live && setPanel(p))
      .catch(() => live && setPanel(null));
    return () => {
      live = false;
    };
  }, [zoneId, rev]);

  if (!panel || !panel.effect_id) {
    return (
      <div className="pr-placeholder-body">
        Celda vacía — asigná un efecto en la grilla
      </div>
    );
  }

  return (
    <div className="pr-params">
      <div className="pr-params-head">
        <span className="pr-params-effect">{panel.effect_name}</span>
      </div>
      {panel.params.length === 0 ? (
        <div className="pr-params-none">Este efecto no tiene parámetros</div>
      ) : (
        <div className="pr-params-rows">
          {/* Key includes the effect id so a swap to another effect rebuilds the rows
              (and their local state) rather than reusing a stale one by position. */}
          {panel.params.map((p) => (
            <ParamRow key={`${panel.effect_id}:${p.name}`} zoneId={zoneId} param={p} />
          ))}
        </div>
      )}
    </div>
  );
}

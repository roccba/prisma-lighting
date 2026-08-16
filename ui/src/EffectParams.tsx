import { useEffect, useState } from "react";
import { effectPanel, setParam, type EffectPanelInfo, type ParamInfo } from "./api";
import { ColorControl } from "./ColorControl";
import { Fader } from "./Fader";
import { Rose } from "./Rose";

// The effect parameter panel for the SELECTED zone (M8-SPEC §3.5). It builds itself from
// the effect's manifest (`main_effect_panel`) and now honours each parameter's TYPE: a
// plain scalar → a slider; a colour group → one 255-space picker (the three channels are
// scalar params sharing a manifest colour group); a direction → the rose. Nothing is
// hardcoded per effect (§5.3) — a new effect's controls follow from what its manifest
// declares. Every change writes live through the engine (and persists into the show), so
// it shows at once in the large preview beside it and on the wire.

interface EffectParamsProps {
  zoneId: string;
  /** Bumped by the scene grid when the active cell changes, to re-fetch the panel. */
  rev: number;
}

// A panel item: the grouping of raw manifest params into rendered controls. Colour
// channels sharing a group collapse into one colour item (placed where the group's first
// channel appears); everything else stays one-to-one.
type PanelItem =
  | { kind: "scalar"; param: ParamInfo }
  | { kind: "direction"; param: ParamInfo }
  | { kind: "color"; group: string; r?: ParamInfo; g?: ParamInfo; b?: ParamInfo };

function buildItems(params: ParamInfo[]): PanelItem[] {
  const items: PanelItem[] = [];
  const colorItems = new Map<string, Extract<PanelItem, { kind: "color" }>>();
  for (const p of params) {
    if (p.role === "color" && p.group) {
      let item = colorItems.get(p.group);
      if (!item) {
        item = { kind: "color", group: p.group };
        colorItems.set(p.group, item);
        items.push(item);
      }
      if (p.channel === "r") item.r = p;
      else if (p.channel === "g") item.g = p;
      else if (p.channel === "b") item.b = p;
    } else if (p.role === "direction") {
      items.push({ kind: "direction", param: p });
    } else {
      items.push({ kind: "scalar", param: p });
    }
  }
  return items;
}

/** Format a normalized value as its real value, for a scalar readout. All current
 *  effect scalars use linear curves, so `min + t*(max-min)` is exact; decimals scale
 *  with the range so a 1–20 scale reads "12" and a 0–1 amount reads "0.85". */
function formatReal(p: ParamInfo, norm: number): string {
  const real = p.min + norm * (p.max - p.min);
  const span = p.max - p.min;
  const decimals = span >= 10 ? 0 : span >= 3 ? 1 : 2;
  return real.toFixed(decimals);
}

/** One scalar parameter row: label · value · fader, owning its value locally for a
 *  smooth drag while writing every change straight to the engine. */
function ScalarRow({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const [value, setValue] = useState(param.value_norm);
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
      <div className="pr-placeholder-body">Celda vacía — asigná un efecto en la grilla</div>
    );
  }

  const items = buildItems(panel.params);

  return (
    <div className="pr-params">
      <div className="pr-params-head">
        <span className="pr-params-effect">{panel.effect_name}</span>
      </div>
      {items.length === 0 ? (
        <div className="pr-params-none">Este efecto no tiene parámetros</div>
      ) : (
        <div className="pr-params-rows">
          {items.map((item) => {
            // Keys carry the effect id so an effect swap rebuilds controls (and their
            // local state) rather than reusing a stale one by position.
            if (item.kind === "color") {
              // A complete colour → the picker; an incomplete group degrades to sliders.
              if (item.r && item.g && item.b) {
                return (
                  <ColorControl
                    key={`${panel.effect_id}:color:${item.group}`}
                    zoneId={zoneId}
                    label="Color"
                    r={item.r}
                    g={item.g}
                    b={item.b}
                  />
                );
              }
              return [item.r, item.g, item.b]
                .filter((p): p is ParamInfo => !!p)
                .map((p) => (
                  <ScalarRow key={`${panel.effect_id}:${p.name}`} zoneId={zoneId} param={p} />
                ));
            }
            if (item.kind === "direction") {
              return (
                <Rose
                  key={`${panel.effect_id}:${item.param.name}`}
                  zoneId={zoneId}
                  param={item.param}
                />
              );
            }
            return (
              <ScalarRow
                key={`${panel.effect_id}:${item.param.name}`}
                zoneId={zoneId}
                param={item.param}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

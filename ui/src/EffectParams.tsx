import { useEffect, useState } from "react";
import { effectPanel, setParam, type EffectPanelInfo, type ParamInfo } from "./api";
import { ColorControl } from "./ColorControl";
import { Dropdown } from "./Dropdown";
import { Fader } from "./Fader";
import { PaletteControl } from "./PaletteControl";
import { ParamField, ValueBox } from "./ParamField";
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
type PaletteSlot = { index: number; r?: ParamInfo; g?: ParamInfo; b?: ParamInfo };
type PanelItem =
  | { kind: "scalar"; param: ParamInfo }
  | { kind: "direction"; param: ParamInfo }
  | { kind: "enum"; param: ParamInfo }
  | { kind: "color"; group: string; r?: ParamInfo; g?: ParamInfo; b?: ParamInfo }
  | { kind: "palette"; slots: Map<number, PaletteSlot>; count?: ParamInfo };

function buildItems(params: ParamInfo[]): PanelItem[] {
  const items: PanelItem[] = [];
  const colorItems = new Map<string, Extract<PanelItem, { kind: "color" }>>();
  // A single palette per effect (the only shape today); created where its count or its
  // first channel appears, and filled as the remaining palette params stream past.
  let palette: Extract<PanelItem, { kind: "palette" }> | undefined;
  const ensurePalette = () => {
    if (!palette) {
      palette = { kind: "palette", slots: new Map() };
      items.push(palette);
    }
    return palette;
  };
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
    } else if (p.role === "palette_count") {
      ensurePalette().count = p;
    } else if (p.role === "palette" && p.group !== null) {
      const item = ensurePalette();
      const idx = parseInt(p.group, 10);
      let slot = item.slots.get(idx);
      if (!slot) {
        slot = { index: idx };
        item.slots.set(idx, slot);
      }
      if (p.channel === "r") slot.r = p;
      else if (p.channel === "g") slot.g = p;
      else if (p.channel === "b") slot.b = p;
    } else if (p.role === "direction") {
      items.push({ kind: "direction", param: p });
    } else if (p.role === "enum") {
      items.push({ kind: "enum", param: p });
    } else {
      items.push({ kind: "scalar", param: p });
    }
  }
  return items;
}

/** One scalar parameter: label · value box · fader, all through the shared `ParamField`
 *  so it is identical to every other control. Owns its value locally for a smooth drag
 *  while writing every change straight to the engine; the value box edits the same value. */
function ScalarRow({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const [value, setValue] = useState(param.value_norm);
  useEffect(() => setValue(param.value_norm), [param.value_norm]);

  const change = (v: number) => {
    setValue(v);
    setParam(zoneId, param.name, v).catch(() => {});
  };

  return (
    <ParamField label={param.label} value={<ValueBox param={param} valueNorm={value} onCommit={change} />}>
      <Fader value={value} onChange={change} aria-label={param.label} />
    </ParamField>
  );
}

/** One enum parameter: a labelled dropdown (a block control on the right). The value is a
 *  scalar INDEX (0..steps): the normalized value maps linearly to the choice index, and
 *  picking a choice writes the index back as `i / steps`. Used by the shape/mode selectors. */
function EnumRow({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const choices = param.choices ?? [];
  const steps = Math.max(1, choices.length - 1);
  const idxOf = (norm: number) => Math.min(choices.length - 1, Math.max(0, Math.round(norm * steps)));
  const [idx, setIdx] = useState(idxOf(param.value_norm));
  useEffect(() => setIdx(idxOf(param.value_norm)), [param.value_norm]);

  const change = (v: string) => {
    const i = Number(v);
    setIdx(i);
    setParam(zoneId, param.name, i / steps).catch(() => {});
  };

  return (
    <ParamField label={param.label} block>
      <Dropdown
        value={String(idx)}
        options={choices.map((label, i) => ({ value: String(i), label }))}
        onChange={change}
        minWidth={140}
        aria-label={param.label}
      />
    </ParamField>
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
  const scalarItems = items.filter((i): i is Extract<PanelItem, { kind: "scalar" }> => i.kind === "scalar");
  const controlItems = items.filter((i) => i.kind !== "scalar");

  return (
    <div className="pr-params">
      <div className="pr-params-head">
        <span className="pr-params-effect">{panel.effect_name}</span>
      </div>
      {items.length === 0 ? (
        <div className="pr-params-none">Este efecto no tiene parámetros</div>
      ) : (
        // Two columns: the scalar sliders as a vertical
        // list on the left, the effect-specific controls (direction rose, palette,
        // colour picker) on the right.
        <div className="pr-params-rows">
          <div className="pr-params-col pr-params-scalars">
            {scalarItems.map((item) => (
              <ScalarRow
                key={`${panel.effect_id}:${item.param.name}`}
                zoneId={zoneId}
                param={item.param}
              />
            ))}
          </div>
          {controlItems.length > 0 && (
            <div className="pr-params-col pr-params-controls">
              {controlItems.map((item) => renderControl(item, panel.effect_id!))}
            </div>
          )}
        </div>
      )}
    </div>
  );

  // One non-scalar control (colour picker, rose, or palette editor). Keys carry the
  // effect id so an effect swap rebuilds the control (and its local state) rather than
  // reusing a stale one by position.
  function renderControl(item: PanelItem, effectId: string) {
    if (item.kind === "color") {
      if (item.r && item.g && item.b) {
        return (
          <ParamField key={`${effectId}:color:${item.group}`} label="Color" block>
            <ColorControl zoneId={zoneId} r={item.r} g={item.g} b={item.b} />
          </ParamField>
        );
      }
      return [item.r, item.g, item.b]
        .filter((p): p is ParamInfo => !!p)
        .map((p) => <ScalarRow key={`${effectId}:${p.name}`} zoneId={zoneId} param={p} />);
    }
    if (item.kind === "direction") {
      return (
        <ParamField key={`${effectId}:${item.param.name}`} label={item.param.label} block>
          <Rose zoneId={zoneId} param={item.param} />
        </ParamField>
      );
    }
    if (item.kind === "enum") {
      return (
        <EnumRow key={`${effectId}:${item.param.name}`} zoneId={zoneId} param={item.param} />
      );
    }
    if (item.kind === "palette") {
      // Only complete slots (r+g+b present) with a count param form the editor; anything
      // malformed degrades to sliders rather than a broken control.
      const slots = [...item.slots.values()]
        .filter(
          (s): s is { index: number; r: ParamInfo; g: ParamInfo; b: ParamInfo } =>
            !!(s.r && s.g && s.b),
        )
        .sort((a, b) => a.index - b.index);
      if (item.count && slots.length > 0) {
        return (
          <ParamField key={`${effectId}:palette`} label="Colores" block>
            <PaletteControl zoneId={zoneId} slots={slots} count={item.count} />
          </ParamField>
        );
      }
      const stray = [item.count, ...slots.flatMap((s) => [s.r, s.g, s.b])].filter(
        (p): p is ParamInfo => !!p,
      );
      return stray.map((p) => (
        <ScalarRow key={`${effectId}:${p.name}`} zoneId={zoneId} param={p} />
      ));
    }
    return null;
  }
}

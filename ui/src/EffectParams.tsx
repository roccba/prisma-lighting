import { useEffect, useRef, useState, type CSSProperties } from "react";
import { effectPanel, PARAM_WRITTEN, setParam, type EffectPanelInfo, type ParamInfo, type ParamWritten } from "./api";
import { ColorControl } from "./ColorControl";
import { Dropdown } from "./Dropdown";
import { Fader } from "./Fader";
import { ColorTable } from "./ColorTable";
import { DirectionMenu } from "./DirectionMenu";
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
  | { kind: "toggle"; param: ParamInfo }
  | { kind: "dirmenu"; param: ParamInfo }
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
    } else if (p.role === "toggle") {
      items.push({ kind: "toggle", param: p });
    } else if (p.role === "direction_menu") {
      items.push({ kind: "dirmenu", param: p });
    } else {
      items.push({ kind: "scalar", param: p });
    }
  }
  return items;
}

/** Whether `p` is enabled given the other params' current values (its `requires`). */
function enabledOf(p: ParamInfo, all: ParamInfo[]): boolean {
  const req = p.requires;
  if (!req) return true;
  const other = all.find((q) => q.name === req.param);
  if (!other) return true;
  if (req.choice == null) return other.min + other.value_norm * (other.max - other.min) > 0;
  const choices = other.choices ?? [];
  const i = Math.round(other.value_norm * Math.max(1, choices.length - 1));
  return choices[i] === req.choice;
}

/** the classic "Pulsar" (tap tempo): tap at least four times on the beat and the BPM is the
 *  average interval of the recent taps (a pause of 2 s starts a new count). */
function TapButton({ onBpm, disabled }: { onBpm: (bpm: number) => void; disabled?: boolean }) {
  const taps = useRef<number[]>([]);
  return (
    <button
      type="button"
      className="pr-btn pr-tap"
      disabled={disabled}
      onPointerDown={() => {
        const now = performance.now();
        const list = taps.current;
        if (list.length && now - list[list.length - 1] > 2000) list.length = 0;
        list.push(now);
        if (list.length > 8) list.shift();
        if (list.length >= 4) {
          const avg = (list[list.length - 1] - list[0]) / (list.length - 1);
          if (avg > 0) onBpm(60000 / avg);
        }
      }}
    >
      Pulsar
    </button>
  );
}

/** One scalar parameter: label · value box · fader, all through the shared `ParamField`
 *  so it is identical to every other control. Owns its value locally for a smooth drag
 *  while writing every change straight to the engine; the value box edits the same value. */
function ScalarRow({ zoneId, param, disabled }: { zoneId: string; param: ParamInfo; disabled?: boolean }) {
  const [value, setValue] = useState(param.value_norm);
  useEffect(() => setValue(param.value_norm), [param.value_norm]);

  const change = (v: number) => {
    setValue(v);
    setParam(zoneId, param.name, v).catch(() => {});
  };

  const fader = <Fader value={value} onChange={change} aria-label={param.label} />;
  return (
    <div className={disabled ? "pr-param-off" : undefined}>
      <ParamField label={param.label} value={<ValueBox param={param} valueNorm={value} onCommit={change} />}>
        {param.role === "bpm" ? (
          <div className="pr-field-bpm">
            {fader}
            <TapButton
              disabled={disabled}
              onBpm={(bpm) => change(Math.max(0, Math.min(1, (bpm - param.min) / (param.max - param.min))))}
            />
          </div>
        ) : (
          fader
        )}
      </ParamField>
    </div>
  );
}

/** One enum parameter: a labelled dropdown (a block control on the right). The value is a
 *  scalar INDEX (0..steps): the normalized value maps linearly to the choice index, and
 *  picking a choice writes the index back as `i / steps`. Used by the shape/mode selectors. */
function EnumRow({ zoneId, param, disabled }: { zoneId: string; param: ParamInfo; disabled?: boolean }) {
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
    <div className={disabled ? "pr-param-off" : undefined}>
      <ParamField label={param.label} block>
        <Dropdown
          value={String(idx)}
          options={choices.map((label, i) => ({ value: String(i), label }))}
          onChange={change}
          minWidth={140}
          disabled={disabled}
          aria-label={param.label}
        />
      </ParamField>
    </div>
  );
}

/** An on/off parameter as a latching button (the classic "Atenuar"): lit while on. */
function ToggleButton({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const [on, setOn] = useState(param.value_norm > 0.5);
  useEffect(() => setOn(param.value_norm > 0.5), [param.value_norm]);
  return (
    <button
      type="button"
      className={`pr-btn pr-params-btn${on ? " is-active" : ""}`}
      aria-pressed={on}
      onClick={() => {
        setOn(!on);
        setParam(zoneId, param.name, on ? 0 : 1).catch(() => {});
      }}
    >
      {param.label}
    </button>
  );
}

/** "Colores": opens the "Tabla de color" where the click happened. */
function ColorsButton({
  zoneId,
  slots,
  count,
}: {
  zoneId: string;
  slots: { index: number; r: ParamInfo; g: ParamInfo; b: ParamInfo }[];
  count: ParamInfo;
}) {
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  return (
    <>
      <button
        type="button"
        className={`pr-btn pr-params-btn${at ? " is-active" : ""}`}
        onClick={(e) => {
          const p = { x: e.clientX, y: e.clientY };
          setAt((cur) => (cur ? null : p));
        }}
      >
        Colores
      </button>
      {at && <ColorTable zoneId={zoneId} slots={slots} count={count} at={at} onClose={() => setAt(null)} />}
    </>
  );
}

/** Height of one scalar row (the slider) and the vertical gap between rows, in px —
 *  must match `.pr-field-control .pr-hslider` and `.pr-params-scalars` in theme.css. */
const ROW_H = 40;
const ROW_GAP = 12;

export function EffectParams({ zoneId, rev }: EffectParamsProps) {
  const [panel, setPanel] = useState<EffectPanelInfo | null>(null);
  // How many slider rows fit the panel's height; the scalars fill that many rows, then
  // continue in the next column (and the column really takes its width).
  const rowsRef = useRef<HTMLDivElement>(null);
  const [rows, setRows] = useState(3);
  useEffect(() => {
    const el = rowsRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setRows(Math.max(1, Math.floor((el.clientHeight + ROW_GAP) / (ROW_H + ROW_GAP))));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [panel?.effect_id]);

  useEffect(() => {
    let live = true;
    effectPanel(zoneId)
      .then((p) => live && setPanel(p))
      .catch(() => live && setPanel(null));
    return () => {
      live = false;
    };
  }, [zoneId, rev]);

  // Keep the loaded values current with every write, so a control mounted later (the
  // Tabla de color, the Caja de Fader, a re-rendered row) starts from the real value.
  useEffect(() => {
    const onWrite = (e: Event) => {
      const { zone, name, value } = (e as CustomEvent<ParamWritten>).detail;
      if (zone !== zoneId) return;
      setPanel((p) =>
        p ? { ...p, params: p.params.map((q) => (q.name === name ? { ...q, value_norm: value } : q)) } : p,
      );
    };
    window.addEventListener(PARAM_WRITTEN, onWrite);
    return () => window.removeEventListener(PARAM_WRITTEN, onWrite);
  }, [zoneId]);

  if (!panel || !panel.effect_id) {
    return null; // an empty cell has no parameters: the panel simply stays blank
  }

  const items = buildItems(panel.params);
  const scalarItems = items.filter((i): i is Extract<PanelItem, { kind: "scalar" }> => i.kind === "scalar");
  // Buttons row on top (professional consoles: [Colores] [Atenuar] above the sliders): the palette's
  // "Colores" and every on/off toggle. The rest: sliders left, other controls right.
  const buttonItems = items.filter((i) => i.kind === "palette" || i.kind === "toggle");
  const controlItems = items.filter((i) => i.kind !== "scalar" && i.kind !== "palette" && i.kind !== "toggle");

  return (
    <div className="pr-params">
      {buttonItems.length > 0 && (
        <div className="pr-params-buttons">{buttonItems.map((item) => renderButton(item, panel.effect_id!))}</div>
      )}
      {items.length === 0 ? (
        null
      ) : (
        // Two columns: the scalar sliders as a vertical list on the left, the
        // effect-specific controls (direction rose, colour picker, …) on the right.
        <div className="pr-params-rows" ref={rowsRef}>
          <div className="pr-params-col pr-params-scalars" style={{ "--param-rows": rows } as CSSProperties}>
            {scalarItems.map((item) => (
              <ScalarRow
                key={`${panel.effect_id}:${item.param.name}`}
                zoneId={zoneId}
                param={item.param}
                disabled={!enabledOf(item.param, panel.params)}
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

  // A button of the top row: the palette's "Colores" (opens the Tabla de color) or an
  // on/off toggle. A malformed palette (missing channels / count) is simply not offered.
  function renderButton(item: PanelItem, effectId: string) {
    if (item.kind === "toggle") {
      return <ToggleButton key={`${effectId}:${item.param.name}`} zoneId={zoneId} param={item.param} />;
    }
    if (item.kind === "palette") {
      const slots = [...item.slots.values()]
        .filter(
          (s): s is { index: number; r: ParamInfo; g: ParamInfo; b: ParamInfo } =>
            !!(s.r && s.g && s.b),
        )
        .sort((a, b) => a.index - b.index);
      if (!item.count || slots.length === 0) return null;
      return <ColorsButton key={`${effectId}:palette`} zoneId={zoneId} slots={slots} count={item.count} />;
    }
    return null;
  }

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
        <EnumRow
          key={`${effectId}:${item.param.name}`}
          zoneId={zoneId}
          param={item.param}
          disabled={!enabledOf(item.param, panel!.params)}
        />
      );
    }
    if (item.kind === "dirmenu") {
      return (
        <ParamField key={`${effectId}:${item.param.name}`} label={item.param.label} block>
          <DirectionMenu zoneId={zoneId} param={item.param} />
        </ParamField>
      );
    }
    return null;
  }
}

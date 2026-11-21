import { useContext, useEffect, useRef, useState, type CSSProperties } from "react";
import {
  assignCell,
  assignCellSilent,
  effectCatalogue,
  effectPanel,
  PARAM_WRITTEN,
  renameCell,
  setCellColor,
  setParam,
  triggerCell,
  zoneGrid,
  type EffectListing,
  type EffectPanelInfo,
  type ParamInfo,
  type ParamWritten,
} from "./api";
import { ColorControl } from "./ColorControl";
import { Dropdown } from "./Dropdown";
import { Fader } from "./Fader";
import { ColorTable } from "./ColorTable";
import { DirectionMenu } from "./DirectionMenu";
import { ParamField, ValueBox } from "./ParamField";
import { Rose } from "./Rose";
import { PingPong, PlayLoop } from "./icons";
import { PanelCellContext } from "./panelCell";

// The effect parameter panel for the SELECTED zone's SELECTED cell (M8-SPEC §3.5). On top:
// the cell's effect (a dropdown over the engine's catalogue — changing it reassigns the
// cell) and GO, which plays the cell (the classic double-click / Enter). Selecting a cell
// never plays it (selected ≠ active): a cell that is not running is edited "blind" — its
// values are stored and take effect when it is played; the running cell's edits are live.
//
// Below, the panel builds itself from
// the effect's manifest (`main_effect_panel`) and now honours each parameter's TYPE: a
// plain scalar → a slider; a colour group → one 255-space picker (the three channels are
// scalar params sharing a manifest colour group); a direction → the rose. Nothing is
// hardcoded per effect (§5.3) — a new effect's controls follow from what its manifest
// declares. Every change persists into the show and, for the running cell, writes live
// through the engine, so it shows at once in the large preview beside it and on the wire.

interface EffectParamsProps {
  zoneId: string;
  /** The grid's selected cell; null = the zone's running cell. */
  cell: number | null;
  /** Bumped when the grid changes (fire / assign / clear), to re-fetch the panel. */
  rev: number;
  /** Called after the panel changed the grid (effect reassigned, cell played). */
  onGridChange?: () => void;
}

// A panel item: the grouping of raw manifest params into rendered controls. Colour
// channels sharing a group collapse into one colour item (placed where the group's first
// channel appears); everything else stays one-to-one.
type PanelItem =
  | { kind: "scalar"; param: ParamInfo }
  | { kind: "direction"; param: ParamInfo }
  | { kind: "enum"; param: ParamInfo }
  | { kind: "toggle"; param: ParamInfo }
  | { kind: "palmode"; param: ParamInfo }
  | { kind: "dirmenu"; param: ParamInfo }
  | { kind: "color"; group: string; label: string; r?: ParamInfo; g?: ParamInfo; b?: ParamInfo };

function buildItems(params: ParamInfo[]): PanelItem[] {
  const items: PanelItem[] = [];
  const colorItems = new Map<string, Extract<PanelItem, { kind: "color" }>>();
  for (const p of params) {
    if (p.role === "color" && p.group) {
      let item = colorItems.get(p.group);
      if (!item) {
        item = { kind: "color", group: p.group, label: "Color" };
        colorItems.set(p.group, item);
        items.push(item);
      }
      if (p.channel === "r") item.r = p;
      else if (p.channel === "g") item.g = p;
      else if (p.channel === "b") item.b = p;
    } else if (p.role === "direction") {
      items.push({ kind: "direction", param: p });
    } else if (p.role === "enum") {
      items.push({ kind: "enum", param: p });
    } else if (p.role === "toggle") {
      items.push({ kind: "toggle", param: p });
    } else if (p.role === "palette_mode") {
      items.push({ kind: "palmode", param: p });
    } else if (p.role === "direction_menu") {
      items.push({ kind: "dirmenu", param: p });
    } else {
      items.push({ kind: "scalar", param: p });
    }
  }
  // Several colours in one effect (e.g. Color Change's four): number them in manifest order.
  if (colorItems.size > 1) {
    let n = 0;
    for (const item of colorItems.values()) item.label = `Color ${++n}`;
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
  const cell = useContext(PanelCellContext);
  const [value, setValue] = useState(param.value_norm);
  useEffect(() => setValue(param.value_norm), [param.value_norm]);

  const change = (v: number) => {
    setValue(v);
    setParam(zoneId, param.name, v, cell).catch(() => {});
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
  const cell = useContext(PanelCellContext);
  const choices = param.choices ?? [];
  const steps = Math.max(1, choices.length - 1);
  const idxOf = (norm: number) => Math.min(choices.length - 1, Math.max(0, Math.round(norm * steps)));
  const [idx, setIdx] = useState(idxOf(param.value_norm));
  useEffect(() => setIdx(idxOf(param.value_norm)), [param.value_norm]);

  const change = (v: string) => {
    const i = Number(v);
    setIdx(i);
    setParam(zoneId, param.name, i / steps, cell).catch(() => {});
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
  const cell = useContext(PanelCellContext);
  const [on, setOn] = useState(param.value_norm > 0.5);
  useEffect(() => setOn(param.value_norm > 0.5), [param.value_norm]);
  return (
    <button
      type="button"
      className={`pr-btn pr-params-btn${on ? " is-active" : ""}`}
      aria-pressed={on}
      onClick={() => {
        setOn(!on);
        setParam(zoneId, param.name, on ? 0 : 1, cell).catch(() => {});
      }}
    >
      {param.label}
    </button>
  );
}

/** The glyph for a colour-table play mode, by choice INDEX (PALETTE_MODES order in
 *  effects.rs): loop, ping-pong, RND. An unknown index falls back to its label. */
function paletteModeGlyph(i: number, label: string) {
  if (i === 0) return <PlayLoop size={18} />;
  if (i === 1) return <PingPong size={18} />;
  if (i === 2) return <span className="pr-palmode-rnd">RND</span>;
  return <span className="pr-palmode-rnd">{label}</span>;
}

/** The colour table's play mode (the classic button beside "Colores"): an icon button that
 *  drops a vertical menu of the modes. The value is the choice INDEX, like an enum. */
function PaletteModeButton({ zoneId, param }: { zoneId: string; param: ParamInfo }) {
  const cell = useContext(PanelCellContext);
  const choices = param.choices ?? [];
  const steps = Math.max(1, choices.length - 1);
  const idxOf = (norm: number) => Math.min(choices.length - 1, Math.max(0, Math.round(norm * steps)));
  const [idx, setIdx] = useState(idxOf(param.value_norm));
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => setIdx(idxOf(param.value_norm)), [param.value_norm]);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const pick = (i: number) => {
    setIdx(i);
    setOpen(false);
    setParam(zoneId, param.name, i / steps, cell).catch(() => {});
  };

  return (
    <div ref={rootRef} className="pr-palmode">
      <button
        type="button"
        className={`pr-btn pr-palmode-btn${open ? " is-active" : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`${param.label}: ${choices[idx] ?? ""}`}
        title={choices[idx]}
        onClick={() => setOpen((o) => !o)}
      >
        {paletteModeGlyph(idx, choices[idx] ?? "")}
      </button>
      {open && (
        <div className="pr-palmode-list" role="listbox">
          {choices.map((label, i) => (
            <div
              key={label}
              role="option"
              aria-selected={i === idx}
              title={label}
              className={`pr-palmode-opt${i === idx ? " is-sel" : ""}`}
              onClick={() => pick(i)}
            >
              {paletteModeGlyph(i, label)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** "Colores": opens the "Tabla de color" where the click happened. */
function ColorsButton({
  zoneId,
  colors,
  max,
  onChange,
}: {
  zoneId: string;
  colors: [number, number, number][];
  max: number;
  onChange: (colors: [number, number, number][]) => void;
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
      {at && <ColorTable zoneId={zoneId} colors={colors} max={max} onChange={onChange} at={at} onClose={() => setAt(null)} />}
    </>
  );
}

/** The panel's head: the cell's effect (a dropdown over the catalogue, grouped by
 *  category) and GO. Changing the effect reassigns the cell with the new effect's
 *  defaults, keeping the cell's label and grid colour; a running cell keeps running
 *  (with the new effect), any other cell stays un-played. GO plays the cell. */
function EffectHead({
  zoneId,
  panel,
  onGridChange,
}: {
  zoneId: string;
  panel: EffectPanelInfo;
  onGridChange?: () => void;
}) {
  const [catalogue, setCatalogue] = useState<EffectListing[]>([]);
  useEffect(() => {
    effectCatalogue()
      .then(setCatalogue)
      .catch(() => setCatalogue([]));
  }, []);
  const index = panel.cell_index!;

  const reassign = async (effectId: string) => {
    if (effectId === panel.effect_id) return;
    try {
      // The cell's label and colour survive an effect change (the assign rewrites the cell).
      const before = (await zoneGrid(zoneId))[index];
      const assign = panel.active ? assignCell : assignCellSilent;
      await assign(zoneId, index, effectId, null, null);
      if (before?.name) await renameCell(zoneId, index, before.name);
      if (before?.color) await setCellColor(zoneId, index, before.color);
    } catch {
      /* the grid and panel re-read below either way */
    }
    onGridChange?.();
  };

  const go = () => {
    triggerCell(zoneId, index)
      .then(() => onGridChange?.())
      .catch(() => {});
  };

  return (
    <div className="pr-params-head">
      <span className="pr-params-head-label">Celda {index + 1}</span>
      <Dropdown
        value={panel.effect_id ?? ""}
        options={catalogue.map((e) => ({ value: e.id, label: e.name, group: e.category }))}
        onChange={(v) => void reassign(v)}
        minWidth={220}
        aria-label="Efecto de la celda"
      />
      <span className={`pr-params-state${panel.active ? " is-live" : ""}`}>
        {panel.active ? "En vivo" : "Sin activar"}
      </span>
      <button
        type="button"
        className="pr-btn pr-go-btn"
        disabled={!panel.effect_id}
        onClick={go}
        title="Activar la celda (doble clic o Enter en la grilla)"
      >
        GO
      </button>
    </div>
  );
}

/** Height of one scalar row (the slider) and the vertical gap between rows, in px —
 *  must match `.pr-field-control .pr-hslider` and `.pr-params-scalars` in theme.css. */
const ROW_H = 40;
const ROW_GAP = 12;

export function EffectParams({ zoneId, cell, rev, onGridChange }: EffectParamsProps) {
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
    effectPanel(zoneId, cell)
      .then((p) => live && setPanel(p))
      .catch(() => live && setPanel(null));
    return () => {
      live = false;
    };
  }, [zoneId, cell, rev]);

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

  if (!panel || panel.cell_index == null) {
    return null; // no cell selected and none running: the panel simply stays blank
  }
  const head = <EffectHead zoneId={zoneId} panel={panel} onGridChange={onGridChange} />;
  if (!panel.effect_id) {
    // An empty cell (cleared): pick an effect for it from the head; no parameters yet.
    return <div className="pr-params">{head}</div>;
  }

  const items = buildItems(panel.params);
  const scalarItems = items.filter((i): i is Extract<PanelItem, { kind: "scalar" }> => i.kind === "scalar");
  // Buttons row on top (professional consoles: [Colores] [Atenuar] above the sliders): the palette's
  // "Colores" and every on/off toggle. The rest: sliders left, other controls right.
  // The colour table's play mode sits right after "Colores" (professional consoles: [Colores] [⇄▾]).
  const palModeItems = items.filter((i) => i.kind === "palmode");
  const buttonItems = items.filter((i) => i.kind === "toggle");
  const controlItems = items.filter((i) => i.kind !== "scalar" && i.kind !== "toggle" && i.kind !== "palmode");
  const palette = panel.palette;

  return (
    // Keyed by cell: switching the selected cell rebuilds every control (and closes an
    // open Tabla de color), so nothing carries one cell's local state into another.
    <PanelCellContext.Provider value={panel.cell_index}>
      <div className="pr-params" key={panel.cell_index}>
        {head}
        {(palette || buttonItems.length > 0 || palModeItems.length > 0) && (
          <div className="pr-params-buttons">
            {palette && (
              <ColorsButton
                key={`${panel.effect_id}:palette`}
                zoneId={zoneId}
                colors={palette}
                max={panel.palette_max}
                onChange={(next) => setPanel((p) => (p ? { ...p, palette: next } : p))}
              />
            )}
            {palModeItems.map(
              (item) =>
                item.kind === "palmode" && (
                  <PaletteModeButton key={`${panel.effect_id}:${item.param.name}`} zoneId={zoneId} param={item.param} />
                ),
            )}
            {buttonItems.map((item) => renderButton(item, panel.effect_id!))}
          </div>
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
    </PanelCellContext.Provider>
  );

  // A button of the top row: an on/off toggle ("Colores" is drawn separately).
  function renderButton(item: PanelItem, effectId: string) {
    if (item.kind === "toggle") {
      return <ToggleButton key={`${effectId}:${item.param.name}`} zoneId={zoneId} param={item.param} />;
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
          <ParamField key={`${effectId}:color:${item.group}`} label={item.label} block>
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

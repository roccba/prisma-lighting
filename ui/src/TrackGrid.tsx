import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog } from "@tauri-apps/plugin-dialog";
import {
  addTrack,
  armCell,
  assignCellSilent,
  clearCell,
  grid as fetchGrid,
  launchScene,
  moveTrack,
  removeTrack,
  renameCell,
  renameTrack,
  setCellColor,
  setTrackBehaviour,
  setTrackBlend,
  setTrackMute,
  setTrackOpacity,
  setTrackSolo,
  stopAll,
  stopTrack,
  triggerCell,
  type BlendMode,
  type CellInfo,
  type Easing,
  type GridInfo,
  type LaunchMode,
  type TrackInfo,
  type TrackTransition,
  type ZoneInfo,
} from "./api";
import { CellColorPopover, CellMenu, isLightCell, NO_COLOR, RenameInput, type MenuItem } from "./cellParts";
import { type Rgb } from "./ColorPicker";
import { Check } from "./Check";
import { Dropdown } from "./Dropdown";
import { LearnTarget } from "./midiLearn";
import { NumberField } from "./NumberField";
import { scrubHandler } from "./scrub";
import { linearToSrgb255 } from "./srgb";
import { VFader } from "./VFader";

// The track grid (docs/LIVE-MODEL-SPEC.md §7): tracks are columns, clips are rows, scene
// launch on the right — every zone's content reachable at once, no zone to select first.
// The cells are the classic grid's cells, with the same select-then-play model:
//  · a click SELECTS a clip (the panel below edits it) and never changes output;
//  · a double click or Enter plays it (arms it in "Modo armar" or on an Arm track);
//  · Shift+click (or Shift+Enter) arms it for GO.
// Under each track: stop, opacity fader, M / S / sel. Over each: its header (zone colour
// line, name, destination, blend · transition) — click to select, ⋯ for its settings,
// right-click to rename or delete. "+" adds a track to a zone.

interface TrackGridProps {
  zones: ZoneInfo[];
  /** The selected track (what the panel edits). */
  selectedTrack: string | null;
  /** The selected clip per track (track id → clip index). */
  selectedCells: Record<string, number>;
  /** Select a track (and optionally one of its clips); `zone` is the track's first
   *  destination zone, which the large preview follows. Selection only — never plays. */
  onSelect: (track: string, cell: number | null, zone: string | null) => void;
  /** The global "Modo armar": a play gesture arms instead of playing. */
  armMode: boolean;
  /** Bumped when something else changed the grid (the panel), to re-read it. */
  rev: number;
  /** Called after the grid changed what plays or what a clip holds. */
  onChanged: () => void;
  /** Whether any clip is armed (the global bar shows "Desarmar" then). */
  onArmedChange?: (anyArmed: boolean) => void;
}

const EMPTY: GridInfo = { tracks: [], scenes: [] };

/** Clip row height and the gaps, in px — must match `--tgrid-row`, the `.pr-tgrid` gap
 *  and the `.pr-tgrid-wrap` gap in theme.css (the body is sized to whole rows from them). */
const ROW_H = 52;
const ROW_GAP = 5;
const SECTION_GAP = 5;

const EASINGS: { value: Easing; label: string }[] = [
  { value: "linear", label: "Lineal" },
  { value: "ease_in", label: "Entrada suave" },
  { value: "ease_out", label: "Salida suave" },
  { value: "ease_in_out", label: "Suave (S)" },
];

/** The blend modes the popover offers, in console order. */
const BLENDS: { value: BlendMode; label: string }[] = [
  { value: "Normal", label: "Normal" },
  { value: "Add", label: "Add" },
  { value: "Subtract", label: "Subtract" },
  { value: "Multiply", label: "Multiply" },
  { value: "Screen", label: "Screen" },
  { value: "Max", label: "Max" },
  { value: "Min", label: "Min" },
  { value: "Mask", label: "Mask" },
  { value: "LumaKey", label: "Luma key" },
];

/** A blend mode's label (the header and the popover use the same words). */
function blendLabel(mode: BlendMode): string {
  return BLENDS.find((b) => b.value === mode)?.label ?? mode;
}

/** A cell's state, in the words the whole UI uses (LIVE-MODEL-SPEC §7). */
function cellState(cell: CellInfo): string {
  if (!cell.effect_id) return "Vacía";
  if (cell.active) return "En vivo";
  if (cell.armed) return "Armada";
  return "Cargada";
}

/** What a cell shows when it has no label colour: its effect's main colour, in the
 *  grid's 0–255 sRGB (the effect keeps it in linear light). */
function effectSwatch(cell: CellInfo): Rgb | null {
  const c = cell.effect_color;
  return c ? [linearToSrgb255(c[0]), linearToSrgb255(c[1]), linearToSrgb255(c[2])] : null;
}

function transitionLabel(t: TrackTransition): string {
  if (t.time <= 0) return "corte";
  const n = Number.isInteger(t.time) ? t.time.toFixed(0) : t.time.toFixed(1);
  return t.base === "beats" ? `${n} beats` : `${n} s`;
}

/** The zone slot a track's cells answer to on MIDI — phase 1 bridge: only a zone's own
 *  track (the first whose destination is exactly that zone) has grid pads until the
 *  track MIDI targets land (phase 3). */
function homeSlot(t: TrackInfo, tracks: TrackInfo[], zones: ZoneInfo[]): number | null {
  if (t.zones.length !== 1) return null;
  const first = tracks.find((o) => o.zones.length === 1 && o.zones[0] === t.zones[0]);
  if (first?.id !== t.id) return null;
  const slot = zones.findIndex((z) => z.id === t.zones[0]);
  return slot >= 0 ? slot : null;
}

interface CellMenuState {
  track: string;
  index: number;
  x: number;
  y: number;
}

interface ColorState {
  track: string;
  index: number;
  cell: DOMRect;
  original: Rgb | null;
  value: Rgb;
}

export function TrackGrid({
  zones,
  selectedTrack,
  selectedCells,
  onSelect,
  armMode,
  rev,
  onChanged,
  onArmedChange,
}: TrackGridProps) {
  const [g, setG] = useState<GridInfo>(EMPTY);
  const [menu, setMenu] = useState<CellMenuState | null>(null);
  const [headMenu, setHeadMenu] = useState<{ track: string; x: number; y: number } | null>(null);
  const [addMenu, setAddMenu] = useState<{ x: number; y: number } | null>(null);
  const [settings, setSettings] = useState<{ track: string; anchor: DOMRect } | null>(null);
  const [editing, setEditing] = useState<{ track: string; index: number } | null>(null);
  const [renamingTrack, setRenamingTrack] = useState<string | null>(null);
  const [color, setColor] = useState<ColorState | null>(null);
  // The cell copy buffer: a whole clip (effect + params + colour table + label + colour).
  const [clipboard, setClipboard] = useState<CellInfo | null>(null);
  const cellEls = useRef(new Map<string, HTMLDivElement>());
  const key = (track: string, index: number) => `${track}:${index}`;

  const refresh = useCallback(() => {
    fetchGrid()
      .then(setG)
      .catch(() => {});
  }, []);

  useEffect(() => refresh(), [refresh, rev]);

  // What plays changes asynchronously: a click, a pad, a flash releasing, GO all go through
  // the engine's clip controller, which reports back per track. A recompile (add/remove a
  // track, a patch edit) rebuilds the grid; a MIDI move can change a track's controls.
  useEffect(() => {
    const subs = [
      listen("cells-changed", () => {
        refresh();
        onChanged();
      }),
      listen("layout-changed", refresh),
      listen("midi-applied", refresh),
    ];
    return () => {
      subs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [refresh, onChanged]);

  const track = (id: string) => g.tracks.find((t) => t.id === id);
  const anyArmed = g.tracks.some((t) => t.armed !== null);
  useEffect(() => onArmedChange?.(anyArmed), [anyArmed, onArmedChange]);
  const select = (id: string, cell: number | null) => onSelect(id, cell, track(id)?.zones[0] ?? null);

  // Always have a selected track while there are tracks: the first one by default, and
  // again if the selected one was removed.
  useEffect(() => {
    const first = g.tracks[0];
    if (first && !g.tracks.some((t) => t.id === selectedTrack)) {
      onSelect(first.id, null, first.zones[0] ?? null);
    }
  }, [g.tracks, selectedTrack, onSelect]);
  const cellOf = (id: string, index: number) => track(id)?.clips[index];

  // --- Playing -------------------------------------------------------------------------

  /** Play a clip — or arm it (Shift, the global arm mode). An empty clip has nothing to
   *  play: it is played from the panel's GO, which assigns its draft first. */
  const play = (trackId: string, index: number, arm: boolean) => {
    if (!cellOf(trackId, index)?.effect_id) return;
    select(trackId, index);
    const done = () => {
      refresh();
      onChanged();
    };
    (arm ? armCell(trackId, index) : triggerCell(trackId, index)).then(done).catch(() => {});
  };

  const doClear = (trackId: string, index: number) => {
    clearCell(trackId, index)
      .then(() => {
        refresh();
        onChanged();
      })
      .catch(() => {});
  };

  const doPaste = async (trackId: string, index: number) => {
    const src = clipboard;
    if (!src?.effect_id) return;
    if (cellOf(trackId, index)?.effect_id) {
      const ok = await confirmDialog("¿Reemplazar el contenido de esta celda?", {
        title: "Pegar celda",
        kind: "warning",
        okLabel: "Reemplazar",
        cancelLabel: "Cancelar",
      }).catch(() => false);
      if (!ok) return;
    }
    try {
      await assignCellSilent(trackId, index, src.effect_id, src.params, src.palette);
      await renameCell(trackId, index, src.name ?? "");
      if (src.color) await setCellColor(trackId, index, src.color);
    } catch {
      /* re-read below either way */
    }
    refresh();
    onChanged();
  };

  const commitRename = (trackId: string, index: number, name: string) => {
    setEditing(null);
    renameCell(trackId, index, name)
      .then(refresh)
      .catch(() => {});
    cellEls.current.get(key(trackId, index))?.focus();
  };

  const openColor = (trackId: string, index: number) => {
    const el = cellEls.current.get(key(trackId, index));
    const cell = cellOf(trackId, index);
    if (!el || !cell) return;
    setColor({ track: trackId, index, cell: el.getBoundingClientRect(), original: cell.color, value: cell.color ?? NO_COLOR });
  };
  const onColorChange = (rgb: Rgb) => {
    setColor((c) => (c ? { ...c, value: rgb } : c));
    setG((cur) => ({
      ...cur,
      tracks: cur.tracks.map((t) =>
        t.id !== color?.track ? t : { ...t, clips: t.clips.map((c) => (c.index === color.index ? { ...c, color: rgb } : c)) },
      ),
    }));
  };
  const onColorClose = () => {
    const c = color;
    setColor(null);
    if (!c) return;
    const changed = !c.original || c.original.some((v, i) => v !== c.value[i]);
    if (changed && !(c.original === null && c.value === NO_COLOR)) {
      setCellColor(c.track, c.index, c.value)
        .then(refresh)
        .catch(() => {});
    }
  };

  // --- Track controls ------------------------------------------------------------------

  const patchTrack = (id: string, change: Partial<TrackInfo>) =>
    setG((cur) => ({ ...cur, tracks: cur.tracks.map((t) => (t.id === id ? { ...t, ...change } : t)) }));

  // --- Reordering: drag a header among the tracks of its zone ------------------------
  // Pointer-driven (not HTML drag-and-drop, which the desktop webviews handle unevenly).
  // A press starts a candidate; past a few pixels it becomes a drag, the drop target is
  // the header under the pointer (same zone only), and release moves the track. Later =
  // on top inside the zone.
  const headEls = useRef(new Map<string, HTMLDivElement>());
  const [drag, setDrag] = useState<{
    track: string;
    moved: boolean;
    target: { before: string | null; after: string | null } | null;
  } | null>(null);
  const startDrag = (t: TrackInfo, e: React.PointerEvent) => {
    const x0 = e.clientX;
    const sameZone = g.tracks.filter((o) => o.zones.join() === t.zones.join());
    let state: { moved: boolean; target: { before: string | null; after: string | null } | null } = {
      moved: false,
      target: null,
    };
    const onMove = (ev: PointerEvent) => {
      if (!state.moved && Math.abs(ev.clientX - x0) < 6) return;
      // The drop slot: before the first same-zone header whose centre is right of the
      // pointer, else after the last one.
      const others = sameZone.filter((o) => o.id !== t.id);
      const before = others.find((o) => {
        const r = headEls.current.get(o.id)?.getBoundingClientRect();
        return r ? ev.clientX < r.left + r.width / 2 : false;
      });
      state = {
        moved: true,
        target: before ? { before: before.id, after: null } : { before: null, after: others[others.length - 1]?.id ?? null },
      };
      setDrag({ track: t.id, ...state });
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      setDrag(null);
      if (!state.moved || !state.target) return;
      const { before } = state.target;
      // Dropping where it already is changes nothing (and skips a recompile).
      const order = sameZone.map((o) => o.id);
      const at = order.indexOf(t.id);
      const nextId = order[at + 1] ?? null;
      if (before === nextId || (before === null && at === order.length - 1)) return;
      moveTrack(t.id, before).catch(() => {});
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  const changeOpacity = (t: TrackInfo, v: number) => {
    patchTrack(t.id, { opacity: v });
    setTrackOpacity(t.id, v).catch(() => {});
  };

  const doRemoveTrack = async (t: TrackInfo) => {
    const ok = await confirmDialog(`¿Eliminar el track "${t.name}" y sus celdas?`, {
      title: "Eliminar track",
      kind: "warning",
      okLabel: "Eliminar",
      cancelLabel: "Cancelar",
    }).catch(() => false);
    if (ok) removeTrack(t.id).catch(() => {});
  };

  const rows = g.scenes.length;

  // The body shows only WHOLE rows: its height is the most rows (and their gaps) that fit
  // between the header and the footer, re-measured whenever the region resizes.
  const wrapRef = useRef<HTMLDivElement>(null);
  const headRef = useRef<HTMLDivElement>(null);
  const footRef = useRef<HTMLDivElement>(null);
  const [bodyHeight, setBodyHeight] = useState<number | undefined>(undefined);
  useLayoutEffect(() => {
    const wrap = wrapRef.current;
    if (!wrap) return;
    const measure = () => {
      const head = headRef.current?.offsetHeight ?? 0;
      const foot = footRef.current?.offsetHeight ?? 0;
      const style = getComputedStyle(wrap);
      const pad = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
      // The wrap's own gap between head / body / foot, and the scrollbar if any.
      const free = wrap.clientHeight - pad - head - foot - 2 * SECTION_GAP;
      const fit = Math.max(1, Math.floor((free + ROW_GAP) / (ROW_H + ROW_GAP)));
      const shown = Math.min(fit, Math.max(1, rows));
      setBodyHeight(shown * ROW_H + (shown - 1) * ROW_GAP);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [rows, g.tracks.length]);
  // Track columns share the width left by the "+" and scene columns, never narrower than
  // --tgrid-col (then the grid scrolls sideways).
  const columns = `repeat(${g.tracks.length}, minmax(var(--tgrid-col), 1fr)) var(--tgrid-add) var(--tgrid-scene)`;
  const menuCell = menu ? cellOf(menu.track, menu.index) : undefined;
  const headTrack = headMenu ? track(headMenu.track) : undefined;
  const settingsTrack = settings ? track(settings.track) : undefined;

  const cols = { gridTemplateColumns: columns } as React.CSSProperties;
  return (
    <div className="pr-tgrid-wrap" ref={wrapRef}>
      <div className="pr-tgrid pr-tgrid-head" ref={headRef} style={cols}>
        {/* Header row */}
        {g.tracks.map((t) => (
          <div
            key={t.id}
            ref={(el) => {
              if (el) headEls.current.set(t.id, el);
              else headEls.current.delete(t.id);
            }}
            className={`pr-thead${selectedTrack === t.id ? " is-selected" : ""}${
              drag?.track === t.id && drag.moved ? " is-dragging" : ""
            }${drag?.moved && drag.target?.before === t.id ? " is-drop-before" : ""}${
              drag?.moved && drag.target && drag.target.before === null && drag.target.after === t.id ? " is-drop-after" : ""
            }`}
            style={{ borderBottomColor: `rgb(${t.color.join(",")})` }}
            onPointerDown={(e) => {
              if (e.button !== 0 || renamingTrack === t.id) return;
              select(t.id, selectedCells[t.id] ?? null);
              startDrag(t, e);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setHeadMenu({ track: t.id, x: e.clientX, y: e.clientY });
            }}
            title={`${t.name} → ${t.destination}`}
          >
            {renamingTrack === t.id ? (
              <RenameInput
                initial={t.name}
                onCommit={(name) => {
                  setRenamingTrack(null);
                  renameTrack(t.id, name)
                    .then(refresh)
                    .catch(() => {});
                }}
                onCancel={() => setRenamingTrack(null)}
              />
            ) : (
              <span className="pr-thead-name">{t.name}</span>
            )}
            <span className="pr-thead-sub">→ {t.destination}</span>
            <span className="pr-thead-sub">
              {blendLabel(t.blend_mode)} · {transitionLabel(t.transition)}
              {t.launch === "arm" && <span className="pr-thead-arm"> · ARM</span>}
            </span>
            <button
              type="button"
              className="pr-thead-more"
              title="Transición y lanzamiento"
              aria-label={`Ajustes de ${t.name}`}
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                const r = (e.currentTarget.parentElement as HTMLElement).getBoundingClientRect();
                setSettings((s) => (s?.track === t.id ? null : { track: t.id, anchor: r }));
              }}
            >
              ⋯
            </button>
          </div>
        ))}
        <button
          type="button"
          className="pr-btn pr-tgrid-addbtn"
          title="Agregar un track a una zona"
          aria-label="Agregar track"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setAddMenu({ x: r.left, y: r.bottom + 2 });
          }}
        >
          +
        </button>
        <div className="pr-thead pr-thead-scenes">
          <span className="pr-thead-name">Escenas</span>
        </div>
      </div>

      {/* Clip rows: a scroll box exactly as tall as the whole rows that fit, snapping to
          rows, so neither the last visible row nor a scene button is ever cut. */}
      <div className="pr-tgrid-body" style={{ height: bodyHeight }}>
        <div className="pr-tgrid" style={cols}>
        {Array.from({ length: rows }, (_, row) => (
          <Row key={row}>
            {g.tracks.map((t) => {
              const cell = t.clips[row];
              if (!cell) return <div key={t.id} className="pr-tgrid-gap" />;
              const assigned = !!cell.effect_id;
              const title = cell.name ?? cell.effect_name ?? "";
              const selected = selectedTrack === t.id && (selectedCells[t.id] ?? t.active) === row;
              const slot = homeSlot(t, g.tracks, zones);
              const isEditing = editing?.track === t.id && editing.index === row;
              // Label colour first, else the effect's own colour, else the zone's (washed).
              const shown = cell.color ?? effectSwatch(cell);
              const body = (
                <div
                  ref={(el) => {
                    if (el) cellEls.current.set(key(t.id, row), el);
                    else cellEls.current.delete(key(t.id, row));
                  }}
                  className={`pr-cell ${assigned ? "is-assigned" : "is-empty"}${cell.active ? " is-active" : ""}${
                    cell.armed ? " is-armed" : ""
                  }${assigned && shown ? ` has-color${isLightCell(shown, cell.active) ? " is-light" : ""}` : ""}${
                    assigned && !shown ? " has-zone-color" : ""
                  }${
                    menu?.track === t.id && menu.index === row ? " is-menu" : ""
                  }${selected ? " is-selected" : ""}`}
                  style={
                    assigned
                      ? ({ "--cell-rgb": (shown ?? t.color).join(", ") } as React.CSSProperties)
                      : undefined
                  }
                  onPointerDown={(e) => {
                    if (e.button !== 0 || isEditing) return;
                    // An armed cell: a click (with or without Shift) disarms it. Otherwise
                    // Shift+click arms and a plain click only selects.
                    if (cell.armed || e.shiftKey) play(t.id, row, true);
                    else select(t.id, row);
                  }}
                  onDoubleClick={(e) => {
                    if (!isEditing && !e.shiftKey) play(t.id, row, armMode);
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setMenu({ track: t.id, index: row, x: e.clientX, y: e.clientY });
                  }}
                  onKeyDown={(e) => {
                    if (isEditing) return;
                    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                      e.preventDefault();
                      const r = e.currentTarget.getBoundingClientRect();
                      setMenu({ track: t.id, index: row, x: r.left + 8, y: r.top + 8 });
                    } else if (e.key === "Enter") {
                      e.preventDefault();
                      play(t.id, row, armMode || e.shiftKey);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                  aria-pressed={selected}
                  title={
                    assigned
                      ? `${title}${cell.name ? ` — ${cell.effect_name ?? ""}` : ""} · ${cellState(cell)} (doble clic o Enter: lanzar · Shift+clic: armar · clic en una armada: desarmar)`
                      : "Vacía — clic para seleccionarla y elegir un efecto"
                  }
                >
                  <span className="pr-cell-num">{row + 1}</span>
                  {assigned &&
                    (isEditing ? (
                      <RenameInput
                        initial={cell.name ?? cell.effect_name ?? ""}
                        onCommit={(name) => commitRename(t.id, row, name)}
                        onCancel={() => setEditing(null)}
                      />
                    ) : (
                      <>
                        <span className="pr-cell-title">{title}</span>
                        {cell.name && <span className="pr-cell-effect">{cell.effect_name}</span>}
                      </>
                    ))}
                </div>
              );
              return slot === null ? (
                <div key={t.id} className="pr-learn-host pr-learn-cell">
                  {body}
                </div>
              ) : (
                <LearnTarget
                  key={t.id}
                  target={{ zone_slot: { slot, sub: { cell: row } } }}
                  name={`${t.name} · celda ${row + 1}`}
                  className="pr-learn-cell"
                >
                  {body}
                </LearnTarget>
              );
            })}
            <div className="pr-tgrid-gap" />
            <button
              type="button"
              className="pr-btn pr-scene-btn"
              onClick={(e) => launchScene(row, armMode || e.shiftKey).catch(() => {})}
              title={armMode ? "Armar la escena en todos los tracks" : "Lanzar la escena en todos los tracks (Shift = armar)"}
            >
              ▶ {g.scenes[row]?.name ?? row + 1}
            </button>
          </Row>
        ))}
        </div>
      </div>

      <div className="pr-tgrid pr-tgrid-foot" ref={footRef} style={cols}>
        {/* Footer: stop · opacity · M / S / sel */}
        {g.tracks.map((t) => (
          <div key={t.id} className="pr-tfoot" onPointerDown={(e) => e.stopPropagation()}>
            <button
              type="button"
              className="pr-btn pr-tfoot-stop"
              onClick={() => stopTrack(t.id).catch(() => {})}
              title="Stop (con la transición del track; otro stop corta en seco)"
            >
              ■ Stop
            </button>
            <div className="pr-tfoot-fader">
              <VFader value={t.opacity} onChange={(v) => changeOpacity(t, v)} aria-label={`Opacidad de ${t.name}`} />
              <span
                className="pr-zonecard-faderval"
                onPointerDown={scrubHandler(() => ({
                  get: () => t.opacity,
                  set: (v: number) => changeOpacity(t, v),
                  min: 0,
                  max: 1,
                  perPixel: 1 / 255,
                  step: 1 / 255,
                }))}
              >
                {Math.round(t.opacity * 255)}
              </span>
            </div>
            <div className="pr-tfoot-btns">
              <button
                type="button"
                className={`pr-btn pr-zonecard-sm${t.mute ? " is-active" : ""}`}
                onClick={() => {
                  patchTrack(t.id, { mute: !t.mute });
                  setTrackMute(t.id, !t.mute).catch(() => {});
                }}
                title="Mute"
              >
                M
              </button>
              <button
                type="button"
                className={`pr-btn pr-zonecard-sm${t.solo ? " is-active" : ""}`}
                onClick={() => {
                  patchTrack(t.id, { solo: !t.solo });
                  setTrackSolo(t.id, !t.solo).catch(() => {});
                }}
                title="Solo"
              >
                S
              </button>
              <button
                type="button"
                className={`pr-btn pr-zonecard-sm${selectedTrack === t.id ? " is-active" : ""}`}
                onClick={() => select(t.id, selectedCells[t.id] ?? null)}
                title="Seleccionar el track"
              >
                sel
              </button>
            </div>
          </div>
        ))}
        <div className="pr-tgrid-gap" />
        <div className="pr-tfoot pr-tfoot-all">
          <button
            type="button"
            className="pr-btn pr-tfoot-stopall"
            onClick={() => stopAll().catch(() => {})}
            title="Stop en todos los tracks, cada uno con su transición (otro toque durante el fade corta en seco)"
          >
            ■ Stop todo
          </button>
        </div>
      </div>

      {menu && menuCell && (
        <CellMenu
          key={`${menu.track}:${menu.index}:${menu.x}:${menu.y}`}
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            { label: "Armar", disabled: !menuCell.effect_id, onSelect: () => play(menu.track, menu.index, true) },
            "sep",
            { label: "Renombrar", disabled: !menuCell.effect_id, onSelect: () => setEditing({ track: menu.track, index: menu.index }) },
            { label: "Color…", disabled: !menuCell.effect_id, onSelect: () => openColor(menu.track, menu.index) },
            { label: "Copiar", disabled: !menuCell.effect_id, onSelect: () => setClipboard(menuCell) },
            { label: "Pegar", disabled: !clipboard?.effect_id, onSelect: () => void doPaste(menu.track, menu.index) },
            "sep",
            { label: "Limpiar celda", disabled: !menuCell.effect_id, onSelect: () => doClear(menu.track, menu.index) },
          ]}
        />
      )}
      {headMenu && headTrack && (
        <CellMenu
          x={headMenu.x}
          y={headMenu.y}
          onClose={() => setHeadMenu(null)}
          items={[
            { label: "Renombrar", onSelect: () => setRenamingTrack(headTrack.id) },
            "sep",
            { label: "Eliminar track", onSelect: () => void doRemoveTrack(headTrack) },
          ]}
        />
      )}
      {addMenu && (
        <CellMenu
          x={addMenu.x}
          y={addMenu.y}
          onClose={() => setAddMenu(null)}
          items={
            zones.length === 0
              ? [{ label: "No hay zonas", disabled: true, onSelect: () => {} }]
              : zones.map<MenuItem>((z) => ({
                  label: `Track en ${z.name}`,
                  // The new track opens its name for editing once the grid shows it.
                  onSelect: () =>
                    void addTrack(z.id)
                      .then((id) => setRenamingTrack(id))
                      .catch(() => {}),
                }))
          }
        />
      )}
      {settings && settingsTrack && (
        <TrackSettings
          key={settingsTrack.id}
          track={settingsTrack}
          anchor={settings.anchor}
          onClose={() => setSettings(null)}
          onChange={(transition, launch) => {
            patchTrack(settingsTrack.id, { transition, launch });
            setTrackBehaviour(settingsTrack.id, transition, launch).catch(() => {});
          }}
          onBlend={(mode, threshold, softness) => {
            patchTrack(settingsTrack.id, { blend_mode: mode, key_threshold: threshold, key_softness: softness });
            setTrackBlend(settingsTrack.id, mode, threshold, softness).catch(() => {});
          }}
        />
      )}
      {color && <CellColorPopover cell={color.cell} value={color.value} onChange={onColorChange} onClose={onColorClose} />}
    </div>
  );
}

/** One grid row: its children sit directly in the parent CSS grid. */
function Row({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}

// --- The track settings popover (phase 2: transition + launch mode) -------------------

function TrackSettings({
  track,
  anchor,
  onChange,
  onBlend,
  onClose,
}: {
  track: TrackInfo;
  anchor: DOMRect;
  onChange: (t: TrackTransition, launch: LaunchMode) => void;
  onBlend: (mode: BlendMode, threshold: number, softness: number) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: anchor.left, top: anchor.bottom + 4 });
  const t = track.transition;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(anchor.left, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(anchor.bottom + 4, window.innerHeight - r.height - 4)),
    });
  }, [anchor]);

  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      const target = e.target as HTMLElement;
      if (!ref.current?.contains(target)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [onClose]);

  return (
    <div ref={ref} className="pr-popover pr-tsettings" style={pos} role="dialog" aria-label={`Ajustes de ${track.name}`}>
      <div className="pr-tsettings-title">{track.name}</div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Mezcla</span>
        <Dropdown
          value={track.blend_mode}
          options={BLENDS}
          onChange={(v) => onBlend(v as BlendMode, track.key_threshold, track.key_softness)}
          minWidth={140}
          aria-label="Modo de mezcla"
        />
      </div>
      {track.blend_mode === "LumaKey" && (
        <>
          <div className="pr-tsettings-row">
            <span className="pr-tsettings-label">Umbral</span>
            <NumberField
              value={Math.round(track.key_threshold * 100)}
              min={0}
              max={100}
              step={1}
              width={60}
              onChange={(v) => onBlend("LumaKey", v / 100, track.key_softness)}
              aria-label="Umbral del key"
            />
            <span className="pr-tsettings-unit">%</span>
          </div>
          <div className="pr-tsettings-row">
            <span className="pr-tsettings-label">Suavidad</span>
            <NumberField
              value={Math.round(track.key_softness * 100)}
              min={0}
              max={100}
              step={1}
              width={60}
              onChange={(v) => onBlend("LumaKey", track.key_threshold, v / 100)}
              aria-label="Suavidad del key"
            />
            <span className="pr-tsettings-unit">%</span>
          </div>
        </>
      )}
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Transición</span>
        <NumberField
          value={t.time}
          min={0}
          max={600}
          step={t.base === "beats" ? 1 : 0.1}
          width={60}
          onChange={(time) => onChange({ ...t, time }, track.launch)}
          aria-label="Tiempo de transición"
        />
        <span className="pr-tsettings-unit">{t.base === "beats" ? "beats" : "s"}</span>
      </div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Curva</span>
        <Dropdown
          value={t.easing}
          options={EASINGS}
          onChange={(v) => onChange({ ...t, easing: v as Easing }, track.launch)}
          minWidth={140}
          aria-label="Curva de la transición"
        />
      </div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label" />
        <Check checked={t.base === "beats"} onChange={(on) => onChange({ ...t, base: on ? "beats" : "seconds" }, track.launch)}>
          En beats
        </Check>
      </div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Al tocar</span>
        <Dropdown
          value={track.launch}
          options={[
            { value: "direct", label: "Lanzar directo" },
            { value: "arm", label: "Armar (espera GO)" },
          ]}
          onChange={(v) => onChange(t, v as LaunchMode)}
          minWidth={140}
          aria-label="Modo de lanzamiento"
        />
      </div>
      <div className="pr-tsettings-hint">0 = corte. Una celda con fade propio lo usa en lugar de este.</div>
    </div>
  );
}

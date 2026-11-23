import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog } from "@tauri-apps/plugin-dialog";
import {
  addTrack,
  armCell,
  assignCellSilent,
  clearCell,
  grid as fetchGrid,
  launchScene,
  renameScene,
  moveTrack,
  PARAM_STORED,
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
  type GridInfo,
  type LaunchMode,
  type MapTarget,
  type Selection,
  type TrackInfo,
  type TrackSub,
  type TrackTransition,
  type ZoneInfo,
} from "./api";
import { CellColorPopover, CellMenu, cellState, isLightCell, NO_COLOR, RenameInput, type MenuItem } from "./cellParts";
import { type Rgb } from "./ColorPicker";
import { LearnTarget, useMidiLearn } from "./midiLearn";
import { BlendFields, blendLabel, TrackDialog, TransitionFields } from "./TrackDialog";
import { scrubHandler } from "./scrub";
import { linearToSrgb255 } from "./srgb";
import { VFader } from "./VFader";
import { DENSITIES, GRID_GAP, type Density } from "./density";

// The track grid (docs/LIVE-MODEL-SPEC.md §7): tracks are columns, clips are rows, scene
// launch on the right — every zone's content reachable at once, no zone to select first.
// The cells are the classic grid's cells, with the same select-then-play model:
//  · a click SELECTS a clip (the panel below edits it) and never changes output;
//  · a double click plays it (arms it in "Modo armar" or on an Arm track), and so does
//    Enter — the window's shortcut for "launch the selected clip" (Shift+Enter arms);
//  · Shift+click arms it for GO.
// Every control is learnable for MIDI: cells and scenes relative to the controller's bank
// window (the pad under the operator's finger), the footer per track slot.
// Under each track: stop, opacity fader, M / S / sel. Over each: its header (a colour
// strip with one segment per destination zone, name, destination, blend · transition) —
// click to select, drag to reorder (anywhere: the order is global), ⋯ for quick blend and
// transition, double-click or "Ajustes…" for the whole track dialog, right-click to
// rename or delete. "+" adds a track to a zone; more zones are added in the dialog.

interface TrackGridProps {
  zones: ZoneInfo[];
  /** The selected track (what the panel edits). */
  selectedTrack: string | null;
  /** The selected clip per track (track id → clip index). */
  selectedCells: Record<string, number>;
  /** Select a track (and optionally one of its clips); `zone` is the track's first
   *  destination zone, which the large preview follows. Selection only — never plays.
   *  `fromEngine`: the selection came from the engine (keyboard, MIDI) — show it only. */
  onSelect: (track: string, cell: number | null, zone: string | null, fromEngine?: boolean) => void;
  /** The global "Modo armar": a play gesture arms instead of playing. */
  armMode: boolean;
  /** Bumped when something else changed the grid (the panel), to re-read it. */
  rev: number;
  /** Called after the grid changed what plays or what a clip holds. */
  onChanged: () => void;
  /** Whether any clip is armed (the global bar shows "Desarmar" then). */
  onArmedChange?: (anyArmed: boolean) => void;
  /** Sizes of cells, header and footer (Compacta / Normal / Grande). */
  density: Density;
  /** Vista → Bloquear layout: no dragging, adding or deleting tracks, no destination
   *  changes (LIVE-MODEL-SPEC §10.2). */
  layoutLocked?: boolean;
}

const EMPTY: GridInfo = { tracks: [], scenes: [], groups: [] };

/** How long a fired scene's button flashes (a confirmation, under half a second). */
const SCENE_FLASH_MS = 350;


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
  density,
  layoutLocked = false,
}: TrackGridProps) {
  const [g, setG] = useState<GridInfo>(EMPTY);
  const [menu, setMenu] = useState<CellMenuState | null>(null);
  const [headMenu, setHeadMenu] = useState<{ track: string; x: number; y: number } | null>(null);
  const [addMenu, setAddMenu] = useState<{ x: number; y: number } | null>(null);
  const [settings, setSettings] = useState<{ track: string; anchor: DOMRect } | null>(null);
  const [dialog, setDialog] = useState<string | null>(null);
  // The track whose opacity fader is being moved: compact density shows its value only then.
  const [movingFader, setMovingFader] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ track: string; index: number } | null>(null);
  const [renamingTrack, setRenamingTrack] = useState<string | null>(null);
  const [color, setColor] = useState<ColorState | null>(null);
  // The scene column: right-click → Renombrar (in place) or Armar escena; a plain click
  // launches the scene. A scene button is a trigger like GO: it holds no state. When a
  // scene fires — from the screen, the keyboard or a controller — its button flashes once,
  // a confirmation of the press (`n` remounts it so a second fire restarts the flash).
  const [sceneMenu, setSceneMenu] = useState<{ row: number; x: number; y: number } | null>(null);
  const [renamingScene, setRenamingScene] = useState<number | null>(null);
  const [fired, setFired] = useState<{ row: number; n: number } | null>(null);
  useEffect(() => {
    let timer: number | undefined;
    const un = listen<number>("scene-fired", (e) => {
      setFired((f) => ({ row: e.payload, n: (f?.n ?? 0) + 1 }));
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setFired(null), SCENE_FLASH_MS);
    });
    return () => {
      window.clearTimeout(timer);
      un.then((f) => f()).catch(() => {});
    };
  }, []);
  // The cell copy buffer: a whole clip (effect + params + colour table + label + colour).
  const [clipboard, setClipboard] = useState<CellInfo | null>(null);
  const cellEls = useRef(new Map<string, HTMLDivElement>());
  const key = (track: string, index: number) => `${track}:${index}`;
  // MIDI targets are positional in the controller's bank window: the window's first track
  // is slot 0 and its first row is clip 0. A control left of / above it has no pad.
  const { bank, gridWindow } = useMidiLearn();
  const trackTarget = (index: number, sub: TrackSub): MapTarget | null =>
    index >= bank.track ? { track_slot: { slot: index - bank.track, sub } } : null;
  const clipTarget = (index: number, row: number): MapTarget | null =>
    row >= bank.row ? trackTarget(index, { clip: row - bank.row }) : null;
  const sceneTarget = (row: number): MapTarget | null => (row >= bank.row ? { scene: row - bank.row } : null);

  const refresh = useCallback(() => {
    fetchGrid()
      .then(setG)
      .catch(() => {});
  }, []);

  useEffect(() => refresh(), [refresh, rev]);

  // A cell's colour on the grid: its hand-picked label colour, else its effect's main
  // colour, else the zone's (washed). The effect's colour is never stored as a label: the
  // app computes it from the current values, so every stored parameter edit re-reads the
  // grid — at most every 100 ms, so a dragged colour is followed live.
  useEffect(() => {
    let timer: number | undefined;
    const onStored = () => {
      if (timer !== undefined) return;
      timer = window.setTimeout(() => {
        timer = undefined;
        refresh();
      }, 100);
    };
    window.addEventListener(PARAM_STORED, onStored);
    return () => {
      window.removeEventListener(PARAM_STORED, onStored);
      window.clearTimeout(timer);
    };
  }, [refresh]);

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

  // The selection moved elsewhere (arrows, a controller's track select): show it. The
  // grid knows each track's zone, so the large preview follows too.
  const tracksRef = useRef(g.tracks);
  tracksRef.current = g.tracks;
  useEffect(() => {
    const un = listen<Selection>("selection-changed", (e) => {
      const { track: id, clip } = e.payload;
      const t = id ? tracksRef.current.find((o) => o.id === id) : undefined;
      if (!t) return;
      onSelect(t.id, clip, t.zones[0] ?? null, true);
      if (clip !== null) cellEls.current.get(key(t.id, clip))?.scrollIntoView({ block: "nearest", inline: "nearest" });
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, [onSelect]);
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
  // The colour being picked, current even inside a callback created before the last
  // pick: the selector can report a colour and close in the same gesture, and the close
  // must save that colour, not the one its render saw.
  const colorRef = useRef<ColorState | null>(null);
  colorRef.current = color;
  const onColorChange = (rgb: Rgb) => {
    if (colorRef.current) colorRef.current = { ...colorRef.current, value: rgb };
    setColor((c) => (c ? { ...c, value: rgb } : c));
    setG((cur) => ({
      ...cur,
      tracks: cur.tracks.map((t) =>
        t.id !== color?.track ? t : { ...t, clips: t.clips.map((c) => (c.index === color.index ? { ...c, color: rgb } : c)) },
      ),
    }));
  };
  const onColorClose = () => {
    const c = colorRef.current;
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

  // --- Reordering: drag a header anywhere among the tracks ----------------------------
  // Pointer-driven (not HTML drag-and-drop, which the desktop webviews handle unevenly).
  // A press on a header starts a candidate; past a few pixels it becomes a drag: a
  // semi-transparent copy of the header follows the pointer, the track's column dims and
  // a line marks where it will land (the gap before the header under the pointer, or
  // after the last one). Release moves the track (live, no recompile). The order is
  // global: it is the compositing order in every zone two tracks share (later = on top).
  const headEls = useRef(new Map<string, HTMLDivElement>());
  const [drag, setDrag] = useState<{
    track: string;
    moved: boolean;
    target: { before: string | null; after: string | null } | null;
    /** Where the header copy is drawn (viewport px) and its size. */
    ghost: { x: number; y: number; w: number; h: number };
  } | null>(null);
  const startDrag = (t: TrackInfo, e: React.PointerEvent) => {
    if (layoutLocked) return;
    const x0 = e.clientX;
    const head = headEls.current.get(t.id)?.getBoundingClientRect();
    // The copy keeps the grip point under the pointer.
    const grip = head ? { x: x0 - head.left, y: e.clientY - head.top, w: head.width, h: head.height } : null;
    const sameZone = g.tracks;
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
      setDrag({
        track: t.id,
        ...state,
        ghost: grip
          ? { x: ev.clientX - grip.x, y: ev.clientY - grip.y, w: grip.w, h: grip.h }
          : { x: ev.clientX, y: ev.clientY, w: 0, h: 0 },
      });
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

  // ONE grid in ONE scroll box: header, clip rows and footer share the same columns and
  // the same horizontal scroll. The header and footer stick to the top and bottom, the
  // scene column (with "Escenas" and "Stop todo") to the right. Sizes come only from the
  // density; the box is as tall as the whole clip rows that fit (at least 2), so the window
  // size changes how many rows are seen, never how big they are.
  const wrapRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const scenesRef = useRef<HTMLDivElement>(null);
  const [boxHeight, setBoxHeight] = useState<number | undefined>(undefined);
  const [hScrollbar, setHScrollbar] = useState(0);
  const syncScenes = () => {
    const box = scrollRef.current;
    if (box && scenesRef.current) scenesRef.current.scrollTop = box.scrollTop;
  };
  const sizes = DENSITIES[density];
  useLayoutEffect(() => {
    const wrap = wrapRef.current;
    const box = scrollRef.current;
    if (!wrap || !box) return;
    const measure = () => {
      const style = getComputedStyle(wrap);
      const avail = wrap.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
      const scrollbar = box.offsetHeight - box.clientHeight; // the horizontal one, if shown
      setHScrollbar(scrollbar);
      const fixed = sizes.head + sizes.foot + 2 * GRID_GAP + scrollbar;
      const fit = Math.floor((avail - fixed + GRID_GAP) / (sizes.row + GRID_GAP));
      // As many whole rows as fit; at least one even when not even one fits, but never a
      // forced second row that would push the footer (stop, opacity, M / S / sel) out of
      // the region (LIVE-MODEL-SPEC §10.3).
      const shown = Math.max(Math.min(1, rows), Math.min(fit, rows));
      const body = shown > 0 ? shown * sizes.row + (shown - 1) * GRID_GAP : 0;
      setBoxHeight(fixed + body);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(wrap);
    ro.observe(box);
    return () => ro.disconnect();
  }, [rows, g.tracks.length, sizes]);
  // Track columns share the width left by the "+" and scene columns, never narrower than
  // the density's minimum (then the grid scrolls sideways).
  const columns = `repeat(${g.tracks.length}, minmax(var(--tgrid-col), 1fr)) var(--tgrid-add)`;
  const gridRows = `var(--tgrid-head) repeat(${rows}, var(--tgrid-row)) var(--tfoot-h)`;
  // The controller's bank window (an APC40: 8 tracks × 5 rows), drawn over the cells it
  // controls. Only while a device with a grid is connected.
  const ring =
    gridWindow && g.tracks.length > 0 && rows > 0
      ? {
          gridColumn: `${bank.track + 1} / span ${Math.max(1, Math.min(gridWindow[0], g.tracks.length - bank.track))}`,
          gridRow: `${bank.row + 2} / span ${Math.max(1, Math.min(gridWindow[1], rows - bank.row))}`,
        }
      : null;
  // The dragged track (its column dims) and the grid column whose edge the drop line sits
  // on: the left edge of the header it lands before, else the right edge of the last one.
  const dragSource = drag?.moved ? drag.track : null;
  const dropLine = (() => {
    if (!drag?.moved || !drag.target) return null;
    const { before, after } = drag.target;
    const at = g.tracks.findIndex((o) => o.id === (before ?? after));
    if (at < 0) return null;
    return { column: at + 1, side: before ? ("before" as const) : ("after" as const) };
  })();
  const menuCell = menu ? cellOf(menu.track, menu.index) : undefined;

  const headTrack = headMenu ? track(headMenu.track) : undefined;
  const settingsTrack = settings ? track(settings.track) : undefined;
  const dialogSlot = dialog ? g.tracks.findIndex((t) => t.id === dialog) : -1;
  const dialogTrack = dialogSlot >= 0 ? g.tracks[dialogSlot] : undefined;

  return (
    <div className="pr-tgrid-wrap" ref={wrapRef}>
      <div className="pr-tgrid-frame" style={{ height: boxHeight }}>
      <div className="pr-tgrid-scroll" ref={scrollRef} onScroll={syncScenes}>
      <div className="pr-tgrid" style={{ gridTemplateColumns: columns, gridTemplateRows: gridRows }}>
      {ring && <div className="pr-tgrid-ring" style={ring} title="Ventana del controlador (banco)" />}
      {dropLine && (
        <div
          className={`pr-tgrid-dropline is-${dropLine.side}`}
          style={{ gridColumn: `${dropLine.column}`, gridRow: "1 / -1" }}
          aria-hidden
        />
      )}
      <div className="pr-tgrid-head">
        {/* Header row */}
        {g.tracks.map((t) => (
          <div
            key={t.id}
            ref={(el) => {
              if (el) headEls.current.set(t.id, el);
              else headEls.current.delete(t.id);
            }}
            data-track={t.id}
            className={`pr-thead${selectedTrack === t.id ? " is-selected" : ""}${
              dragSource === t.id ? " is-drag-source" : ""
            }`}
            onPointerDown={(e) => {
              if (e.button !== 0 || renamingTrack === t.id) return;
              select(t.id, selectedCells[t.id] ?? null);
              startDrag(t, e);
            }}
            onDoubleClick={() => {
              if (renamingTrack !== t.id) setDialog(t.id);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setHeadMenu({ track: t.id, x: e.clientX, y: e.clientY });
            }}
            title={`${t.name} → ${t.destination} (doble clic: ajustes del track)`}
          >
            <span className="pr-thead-strip" aria-hidden>
              {(t.colors.length > 0 ? t.colors : [t.color]).map((c, i) => (
                <span key={i} style={{ background: `rgb(${c.join(",")})` }} />
              ))}
            </span>
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
              title="Mezcla y transición"
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
          disabled={layoutLocked}
          title={layoutLocked ? "Layout bloqueado (Vista → Bloquear layout)" : "Agregar un track a una zona"}
          aria-label="Agregar track"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setAddMenu({ x: r.left, y: r.bottom + 2 });
          }}
        >
          +
        </button>
      </div>

      {/* Clip rows, snapping to whole rows under the sticky header. */}
      <div className="pr-tgrid-body">
        {Array.from({ length: rows }, (_, row) => (
          <Row key={row}>
            {g.tracks.map((t, ti) => {
              const cell = t.clips[row];
              if (!cell) return <div key={t.id} className="pr-tgrid-gap" />;
              const assigned = !!cell.effect_id;
              const title = cell.name ?? cell.effect_name ?? "";
              const selected = selectedTrack === t.id && (selectedCells[t.id] ?? t.active) === row;
              const pad = clipTarget(ti, row);
              const isEditing = editing?.track === t.id && editing.index === row;
              // Label colour first, else the effect's own colour, else the zone's (washed).
              const shown = cell.color ?? effectSwatch(cell);
              const body = (
                <div
                  ref={(el) => {
                    if (el) cellEls.current.set(key(t.id, row), el);
                    else cellEls.current.delete(key(t.id, row));
                  }}
                  data-track={t.id}
                  className={`pr-cell${dragSource === t.id ? " is-drag-source" : ""} ${assigned ? "is-assigned" : "is-empty"}${cell.active ? " is-active" : ""}${
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
                    // Enter / Shift+Enter are the window's shortcuts (launch / arm the
                    // selected clip); a focused cell is the selected one.
                    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                      e.preventDefault();
                      const r = e.currentTarget.getBoundingClientRect();
                      setMenu({ track: t.id, index: row, x: r.left + 8, y: r.top + 8 });
                    }
                  }}
                  role="button"
                  tabIndex={0}
                  aria-pressed={selected}
                  title={
                    assigned
                      ? `${title}${cell.name ? ` — ${cell.effect_name ?? ""}` : ""} · ${cellState(cell)} (doble clic o Enter: lanzar · Shift+clic o Shift+Enter: armar · clic en una armada: desarmar)`
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
              return pad === null ? (
                <Fragment key={t.id}>{body}</Fragment>
              ) : (
                <LearnTarget key={t.id} target={pad} name={`${t.name} · celda ${row + 1}`}>
                  {body}
                </LearnTarget>
              );
            })}
            <div className="pr-tgrid-gap" />
          </Row>
        ))}
      </div>

      <div className="pr-tgrid-foot">
        {/* Footer: stop · opacity · M / S / sel */}
        {g.tracks.map((t, ti) => (
          <div
            key={t.id}
            data-track={t.id}
            className={`pr-tfoot${dragSource === t.id ? " is-drag-source" : ""}`}
            onPointerDown={(e) => e.stopPropagation()}
          >
            <Learnable target={trackTarget(ti, "stop")} name={`Stop — ${t.name}`} compact>
              <button
                type="button"
                className="pr-btn pr-tfoot-stop"
                onClick={() => stopTrack(t.id).catch(() => {})}
                title="Stop (con la transición del track; otro stop corta en seco)"
              >
                ■ Stop
              </button>
            </Learnable>
            <Learnable target={trackTarget(ti, "opacity")} name={`Opacidad — ${t.name}`} compact>
            <div className={`pr-tfoot-fader${movingFader === t.id ? " is-moving" : ""}`}>
              <VFader
                value={t.opacity}
                onChange={(v) => changeOpacity(t, v)}
                onGrabbedChange={(on) => setMovingFader(on ? t.id : null)}
                aria-label={`Opacidad de ${t.name}`}
              />
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
            </Learnable>
            <div className="pr-tfoot-btns">
              <Learnable target={trackTarget(ti, "mute")} name={`Mute — ${t.name}`} compact>
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
              </Learnable>
              <Learnable target={trackTarget(ti, "solo")} name={`Solo — ${t.name}`} compact>
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
              </Learnable>
              <Learnable target={trackTarget(ti, "select")} name={`Seleccionar — ${t.name}`} compact>
                <button
                  type="button"
                  className={`pr-btn pr-zonecard-sm${selectedTrack === t.id ? " is-active" : ""}`}
                  onClick={() => select(t.id, selectedCells[t.id] ?? null)}
                  title="Seleccionar el track"
                >
                  sel
                </button>
              </Learnable>
            </div>
          </div>
        ))}
        <div className="pr-tgrid-gap" />
      </div>
      </div>
      </div>

      {/* The scene column: outside the horizontal scroll so it never covers a track, its
          vertical scroll mirrored from the tracks' (the wheel over it scrolls them). */}
      <div className="pr-tgrid-scenes-col" style={{ paddingBottom: hScrollbar }}>
        <div
          className="pr-tgrid-scenes"
          ref={scenesRef}
          onWheel={(e) => scrollRef.current?.scrollBy({ top: e.deltaY })}
        >
          <div className="pr-tgrid pr-tgrid-scenegrid" style={{ gridTemplateRows: gridRows }}>
            <div className="pr-tgrid-head">
              <div className="pr-thead pr-thead-scenes">
                <span className="pr-thead-name">Escenas</span>
              </div>
            </div>
            <div className="pr-tgrid-body">
              {Array.from({ length: rows }, (_, row) => (
            <div key={row} className="pr-tgrid-scene">
            {renamingScene === row ? (
              <RenameInput
                initial={g.scenes[row]?.name ?? ""}
                onCommit={(name) => {
                  setRenamingScene(null);
                  renameScene(row, name)
                    .then(refresh)
                    .catch(() => {});
                }}
                onCancel={() => setRenamingScene(null)}
              />
            ) : (
            <Learnable target={sceneTarget(row)} name={`Escena ${row + 1}`}>
              <button
                key={fired?.row === row ? `fired-${fired.n}` : "idle"}
                type="button"
                className={`pr-btn pr-scene-btn${fired?.row === row ? " is-fired" : ""}`}
                onClick={(e) => launchScene(row, armMode || e.shiftKey).catch(() => {})}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setSceneMenu({ row, x: e.clientX, y: e.clientY });
                }}
                title={
                  armMode
                    ? "Armar la escena en todos los tracks (clic derecho: renombrar)"
                    : `Lanzar la escena en todos los tracks (Shift = armar${row < 9 ? ` · tecla ${row + 1}` : ""} · clic derecho: renombrar)`
                }
              >
                ▶ {g.scenes[row]?.name ?? row + 1}
              </button>
            </Learnable>
            )}
            </div>
              ))}
            </div>
            <div className="pr-tgrid-foot">
        <div className="pr-tfoot pr-tfoot-all">
          <LearnTarget target={{ path: "/tracks/stop" }} name="Stop todo" compact>
            <button
              type="button"
              className="pr-btn pr-tfoot-stopall"
              onClick={() => stopAll().catch(() => {})}
              title="Stop en todos los tracks, cada uno con su transición (otro toque durante el fade corta en seco · Ctrl+Retroceso)"
            >
              ■ Stop todo
            </button>
          </LearnTarget>
        </div>
            </div>
          </div>
        </div>
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
            { label: "Ajustes…", onSelect: () => setDialog(headTrack.id) },
            { label: "Renombrar", onSelect: () => setRenamingTrack(headTrack.id) },
            "sep",
            { label: "Eliminar track", disabled: layoutLocked, onSelect: () => void doRemoveTrack(headTrack) },
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
          onOpenDialog={() => {
            setSettings(null);
            setDialog(settingsTrack.id);
          }}
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
      {dialogTrack && (
        <TrackDialog
          key={dialogTrack.id}
          track={dialogTrack}
          slot={dialogSlot}
          zones={zones}
          groups={g.groups ?? []}
          layoutLocked={layoutLocked}
          onPatch={(change) => patchTrack(dialogTrack.id, change)}
          onClose={() => setDialog(null)}
        />
      )}
      {sceneMenu && (
        <CellMenu
          key={`scene:${sceneMenu.row}:${sceneMenu.x}:${sceneMenu.y}`}
          x={sceneMenu.x}
          y={sceneMenu.y}
          onClose={() => setSceneMenu(null)}
          items={[
            { label: "Renombrar", onSelect: () => setRenamingScene(sceneMenu.row) },
            "sep",
            { label: "Armar escena", onSelect: () => void launchScene(sceneMenu.row, true).catch(() => {}) },
          ]}
        />
      )}
      {color && <CellColorPopover cell={color.cell} value={color.value} onChange={onColorChange} onClose={onColorClose} />}
      {drag?.moved &&
        (() => {
          const t = track(drag.track);
          if (!t) return null;
          return (
            <div
              className="pr-thead pr-thead-ghost"
              style={{ left: drag.ghost.x, top: drag.ghost.y, width: drag.ghost.w, height: drag.ghost.h }}
              aria-hidden
            >
              <span className="pr-thead-strip">
                {(t.colors.length > 0 ? t.colors : [t.color]).map((c, i) => (
                  <span key={i} style={{ background: `rgb(${c.join(",")})` }} />
                ))}
              </span>
              <span className="pr-thead-name">{t.name}</span>
              <span className="pr-thead-sub">→ {t.destination}</span>
            </div>
          );
        })()}
    </div>
  );
}

/** A learnable control, or just the control when it has no MIDI target (left of / above
 *  the controller's bank window). */
function Learnable({
  target,
  name,
  compact,
  children,
}: {
  target: MapTarget | null;
  name: string;
  compact?: boolean;
  children: React.ReactElement;
}) {
  if (!target) return children;
  return (
    <LearnTarget target={target} name={name} compact={compact}>
      {children}
    </LearnTarget>
  );
}

/** One grid row: its children sit directly in the parent CSS grid. */
function Row({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}

// --- The ⋯ popover: quick access to blend and transition ------------------------------

function TrackSettings({
  track,
  anchor,
  onChange,
  onBlend,
  onOpenDialog,
  onClose,
}: {
  track: TrackInfo;
  anchor: DOMRect;
  onChange: (t: TrackTransition, launch: LaunchMode) => void;
  onBlend: (mode: BlendMode, threshold: number, softness: number) => void;
  onOpenDialog: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: anchor.left, top: anchor.bottom + 4 });

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
    <div ref={ref} className="pr-popover pr-tsettings" style={pos} role="dialog" aria-label={`Mezcla y transición de ${track.name}`}>
      <div className="pr-tsettings-title">{track.name}</div>
      <BlendFields track={track} onBlend={onBlend} />
      <TransitionFields transition={track.transition} onChange={(t) => onChange(t, track.launch)} />
      <div className="pr-tsettings-hint">0 = corte. Una celda con fade propio lo usa en lugar de este.</div>
      <button type="button" className="pr-btn pr-tsettings-more" onClick={onOpenDialog}>
        Ajustes del track…
      </button>
    </div>
  );
}

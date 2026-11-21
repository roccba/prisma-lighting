import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirm as confirmDialog } from "@tauri-apps/plugin-dialog";
import {
  assignCell,
  assignCellSilent,
  clearCell,
  renameCell,
  setCellColor,
  triggerCell,
  zoneGrid,
  type CellInfo,
} from "./api";
import { ColorPicker, type Rgb } from "./ColorPicker";
import { EffectPicker } from "./EffectPicker";
import { LearnTarget } from "./midiLearn";

// The scene grid of the SELECTED zone (M8-SPEC §3.3): a grid of assignable cells (the
// zone's "scenes"). Empty cells open the effect picker; assigned cells trigger — which
// makes that cell the zone's active content, enqueued through the frame-boundary queue
// (step 2), so the change shows in the zone's mini-preview and on the wire. The active
// cell is marked with the accent. Per zone, not cross-zone (the product's thesis): this
// is the selected zone's own grid; switching the selected zone swaps to its cells.
//
// Each cell carries an operator label and a grid colour (UI only — never light on the
// rig). Right-click opens the cell menu: rename in place, colour, copy / paste, clear.
// The copy buffer is local to this grid and lives only for the session.

interface SceneGridProps {
  zoneId: string;
  /** The zone's position in the show — what a MIDI pad maps to (`zone_slot`). */
  slot: number;
  /** Called after the zone's active cell changes (fire / assign / clear), so the
   *  effect parameter panel can re-fetch the now-active effect. */
  onActiveChange?: () => void;
}

/** Quick colours offered above the colour selector — fixed by design. */
const QUICK_CELL_COLORS: { label: string; rgb: Rgb }[] = [
  { label: "Rojo", rgb: [220, 40, 40] },
  { label: "Naranja", rgb: [235, 120, 20] },
  { label: "Amarillo", rgb: [225, 200, 30] },
  { label: "Verde", rgb: [40, 170, 60] },
  { label: "Cian", rgb: [20, 180, 200] },
  { label: "Azul", rgb: [40, 90, 220] },
  { label: "Violeta", rgb: [140, 60, 210] },
  { label: "Rosa", rgb: [225, 70, 160] },
  { label: "Blanco", rgb: [235, 235, 235] },
];

/** What the colour selector shows as "was" for a cell with no colour of its own. */
const NO_COLOR: Rgb = [42, 42, 42];
/** Height of the quick-colour strip (keep in sync with `.pr-cellcolor-quick`). */
const QUICK_STRIP_H = 28;
/** The colour selector's box (keep in sync with `PICKER_W`/`PICKER_H` in ColorPicker). */
const PICKER_W = 640;
const PICKER_H = 330;

/** Whether a coloured cell reads as LIGHT as shown — idle darkened by `--cell-shade`
 *  (35% black), running lit by `--cell-glow` (20% white) — so its text goes dark
 *  (keep the factors in sync with those tokens). Same luma rule as the colour selector. */
function isLightCell([r, g, b]: Rgb, active: boolean): boolean {
  const shown = (c: number) => (active ? c + (255 - c) * 0.2 : c * 0.65);
  return shown(r) * 0.299 + shown(g) * 0.587 + shown(b) * 0.114 > 150;
}

interface MenuState {
  index: number;
  x: number;
  y: number;
}

interface ColorState {
  index: number;
  /** The cell's box: the quick strip opens just below it, the selector below the strip. */
  cell: DOMRect;
  original: Rgb | null;
  value: Rgb;
}

export function SceneGrid({ zoneId, slot, onActiveChange }: SceneGridProps) {
  const [cells, setCells] = useState<CellInfo[]>([]);
  const [pickerFor, setPickerFor] = useState<number | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [editing, setEditing] = useState<number | null>(null);
  const [color, setColor] = useState<ColorState | null>(null);
  // The cell copy buffer: a whole cell (effect + params + colour table + label + colour).
  const [clipboard, setClipboard] = useState<CellInfo | null>(null);
  const cellEls = useRef(new Map<number, HTMLDivElement>());

  useEffect(() => {
    zoneGrid(zoneId)
      .then(setCells)
      .catch(() => setCells([]));
    setPickerFor(null);
    setMenu(null);
    setEditing(null);
    setColor(null);
  }, [zoneId]);

  // The running cell changes asynchronously (M9): a click, a pad, or a flash releasing all
  // go through the engine's cell controller, which reports back per zone. Re-read then.
  useEffect(() => {
    const un = listen<string>("cells-changed", (e) => {
      if (e.payload !== zoneId) return;
      zoneGrid(zoneId)
        .then(setCells)
        .catch(() => {});
      onActiveChange?.();
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, [zoneId, onActiveChange]);

  const onCellPointerDown = (e: React.PointerEvent, cell: CellInfo) => {
    if (e.button !== 0) return; // right button opens the cell menu, never fires
    if (editing === cell.index) return;
    if (cell.effect_id) {
      triggerCell(zoneId, cell.index)
        .then((c) => {
          setCells(c);
          onActiveChange?.();
        })
        .catch(() => {});
    } else {
      setPickerFor(cell.index);
    }
  };

  const onPick = (effectId: string) => {
    const idx = pickerFor;
    setPickerFor(null);
    if (idx === null) return;
    assignCell(zoneId, idx, effectId)
      .then((c) => {
        setCells(c);
        onActiveChange?.();
      })
      .catch(() => {});
  };

  const doClear = (index: number) => {
    clearCell(zoneId, index)
      .then((c) => {
        setCells(c);
        onActiveChange?.();
      })
      .catch(() => {});
  };

  const onClearButton = (e: React.PointerEvent, cell: CellInfo) => {
    e.stopPropagation();
    if (e.button !== 0) return;
    doClear(cell.index);
  };

  const openMenu = (index: number, x: number, y: number) => {
    setPickerFor(null);
    setMenu({ index, x, y });
  };

  const onCellKeyDown = (e: React.KeyboardEvent, cell: CellInfo) => {
    if (editing === cell.index) return;
    // The keyboard way into the cell menu: the Menu key or Shift+F10.
    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
      e.preventDefault();
      const r = e.currentTarget.getBoundingClientRect();
      openMenu(cell.index, r.left + 8, r.top + 8);
    }
  };

  const focusCell = (index: number) => cellEls.current.get(index)?.focus();

  const closeMenu = (refocus: boolean) => {
    const idx = menu?.index;
    setMenu(null);
    if (refocus && idx !== undefined) focusCell(idx);
  };

  // Paste = a full copy of the source cell: its effect with its parameters and colour
  // table, then its label and colour. It only READIES the cell — never fires it, so the
  // zone keeps running what it was running.
  const doPaste = async (index: number) => {
    const src = clipboard;
    if (!src?.effect_id) return;
    // An empty cell takes it straight away; one with content (running or not) asks first.
    // (The native dialog: `window.confirm` is not reliable inside the Tauri webviews.)
    if (cells[index]?.effect_id) {
      const ok = await confirmDialog("¿Reemplazar el contenido de esta celda?", {
        title: "Pegar celda",
        kind: "warning",
        okLabel: "Reemplazar",
        cancelLabel: "Cancelar",
      }).catch(() => false);
      if (!ok) return;
    }
    try {
      let grid = await assignCellSilent(zoneId, index, src.effect_id, src.params, src.palette);
      grid = await renameCell(zoneId, index, src.name ?? "");
      if (src.color) grid = await setCellColor(zoneId, index, src.color);
      setCells(grid);
      // Pasting over the running cell changes what the parameter panel reads.
      if (grid[index]?.active) onActiveChange?.();
    } catch {
      /* the grid re-reads on the next cells-changed */
    }
  };

  const commitRename = (index: number, name: string) => {
    setEditing(null);
    renameCell(zoneId, index, name)
      .then(setCells)
      .catch(() => {});
    focusCell(index);
  };

  const openColor = (index: number) => {
    const el = cellEls.current.get(index);
    const cell = cells[index];
    if (!el || !cell) return;
    setColor({ index, cell: el.getBoundingClientRect(), original: cell.color, value: cell.color ?? NO_COLOR });
  };

  // The selector previews live on the cell (local only) and commits once on close.
  const onColorChange = (rgb: Rgb) => {
    setColor((c) => (c ? { ...c, value: rgb } : c));
    setCells((cs) => cs.map((c) => (c.index === color?.index ? { ...c, color: rgb } : c)));
  };
  const onColorClose = () => {
    const c = color;
    setColor(null);
    if (!c) return;
    const changed = !c.original || c.original.some((v, i) => v !== c.value[i]);
    if (changed && !(c.original === null && c.value === NO_COLOR)) {
      setCellColor(zoneId, c.index, c.value)
        .then(setCells)
        .catch(() => {});
    }
  };

  const menuCell = menu ? cells[menu.index] : undefined;

  return (
    <div className="pr-grid-wrap">
      {/* `--rows` lets the CSS size cells so every row fits the panel height too. */}
      <div className="pr-grid" style={{ "--rows": Math.max(1, Math.ceil(cells.length / 8)) } as React.CSSProperties}>
        {cells.map((cell) => {
          const assigned = !!cell.effect_id;
          const title = cell.name ?? cell.effect_name ?? "";
          const style = cell.color
            ? ({ "--cell-rgb": cell.color.join(", ") } as React.CSSProperties)
            : undefined;
          return (
            <LearnTarget
              key={cell.index}
              target={{ zone_slot: { slot, sub: { cell: cell.index } } }}
              name={`Celda ${cell.index + 1}`}
              className="pr-learn-cell"
            >
              <div
                ref={(el) => {
                  if (el) cellEls.current.set(cell.index, el);
                  else cellEls.current.delete(cell.index);
                }}
                className={`pr-cell ${assigned ? "is-assigned" : "is-empty"}${cell.active ? " is-active" : ""}${
                  assigned && cell.color ? ` has-color${isLightCell(cell.color, cell.active) ? " is-light" : ""}` : ""
                }${menu?.index === cell.index ? " is-menu" : ""}`}
                style={assigned ? style : undefined}
                onPointerDown={(e) => onCellPointerDown(e, cell)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openMenu(cell.index, e.clientX, e.clientY);
                }}
                onKeyDown={(e) => onCellKeyDown(e, cell)}
                role="button"
                tabIndex={0}
                title={assigned ? `${title}${cell.name ? ` — ${cell.effect_name ?? ""}` : ""}` : "Vacío — asignar un efecto"}
              >
                <span className="pr-cell-num">{cell.index + 1}</span>
                {assigned &&
                  (editing === cell.index ? (
                    <RenameInput
                      initial={cell.name ?? cell.effect_name ?? ""}
                      onCommit={(name) => commitRename(cell.index, name)}
                      onCancel={() => {
                        setEditing(null);
                        focusCell(cell.index);
                      }}
                    />
                  ) : (
                    <>
                      <span className="pr-cell-title">{title}</span>
                      {cell.name && <span className="pr-cell-effect">{cell.effect_name}</span>}
                    </>
                  ))}
                {assigned && editing !== cell.index && (
                  <button
                    className="pr-cell-clear"
                    onPointerDown={(e) => onClearButton(e, cell)}
                    title="Vaciar celda"
                    tabIndex={-1}
                  >
                    ×
                  </button>
                )}
              </div>
            </LearnTarget>
          );
        })}
      </div>
      {pickerFor !== null && <EffectPicker onPick={onPick} onClose={() => setPickerFor(null)} />}
      {menu && menuCell && (
        <CellMenu
          key={`${menu.index}:${menu.x}:${menu.y}`}
          x={menu.x}
          y={menu.y}
          onClose={closeMenu}
          items={[
            {
              label: "Renombrar",
              disabled: !menuCell.effect_id,
              onSelect: () => setEditing(menu.index),
            },
            { label: "Color…", disabled: !menuCell.effect_id, onSelect: () => openColor(menu.index) },
            { label: "Copiar", disabled: !menuCell.effect_id, onSelect: () => setClipboard(menuCell) },
            { label: "Pegar", disabled: !clipboard?.effect_id, onSelect: () => doPaste(menu.index) },
            "sep",
            { label: "Limpiar celda", disabled: !menuCell.effect_id, onSelect: () => doClear(menu.index) },
          ]}
        />
      )}
      {color && (
        <CellColorPopover
          cell={color.cell}
          value={color.value}
          onChange={onColorChange}
          onClose={onColorClose}
        />
      )}
    </div>
  );
}

// --- In-place rename ------------------------------------------------------------------

/** The cell's label as an editable field: Enter or blur confirms, Escape cancels. */
function RenameInput({
  initial,
  onCommit,
  onCancel,
}: {
  initial: string;
  onCommit: (name: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  const done = useRef(false);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const finish = (commit: boolean) => {
    if (done.current) return; // Enter/Escape unmounts the field, which also blurs it
    done.current = true;
    if (commit) onCommit(text);
    else onCancel();
  };
  return (
    <input
      ref={ref}
      className="pr-cell-rename"
      value={text}
      maxLength={64}
      spellCheck={false}
      aria-label="Nombre de la celda"
      onChange={(e) => setText(e.target.value)}
      onPointerDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") {
          e.preventDefault();
          finish(true);
        } else if (e.key === "Escape") {
          e.preventDefault();
          finish(false);
        }
      }}
      onBlur={() => finish(true)}
    />
  );
}

// --- The cell menu --------------------------------------------------------------------

type MenuItem = { label: string; disabled?: boolean; onSelect: () => void } | "sep";

/** A small right-click menu. Closes on a press outside it, on Escape, and after an item
 *  runs. Keyboard: ↑/↓ (and Home/End) move between enabled items, Enter/Space runs one. */
function CellMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: (refocus: boolean) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // Keep it on screen: flip back from the right / bottom edge once its size is known.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
  }, [x, y]);

  const buttons = () => Array.from(ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);

  useEffect(() => {
    buttons()[0]?.focus();
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose(true);
      }
    };
    const onBlur = () => onClose(false);
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onKeyDown = (e: React.KeyboardEvent) => {
    const list = buttons();
    if (list.length === 0) return;
    const at = list.indexOf(document.activeElement as HTMLButtonElement);
    let next = -1;
    if (e.key === "ArrowDown") next = (at + 1) % list.length;
    else if (e.key === "ArrowUp") next = (at - 1 + list.length) % list.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = list.length - 1;
    else if (e.key === "Tab") {
      e.preventDefault(); // focus stays in the menu while it is open
      next = e.shiftKey ? (at - 1 + list.length) % list.length : (at + 1) % list.length;
    }
    if (next >= 0) {
      e.preventDefault();
      list[next].focus();
    }
  };

  return (
    <div
      ref={ref}
      className="pr-ctx-menu"
      role="menu"
      style={pos}
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === "sep" ? (
          <div key={i} className="pr-ctx-sep" role="separator" />
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            className="pr-ctx-item"
            disabled={it.disabled}
            onClick={() => {
              onClose(false);
              it.onSelect();
            }}
          >
            {it.label}
          </button>
        ),
      )}
    </div>
  );
}

// --- The cell colour popover ----------------------------------------------------------

/** The quick colours strip over the project's colour selector. Both follow the selector's
 *  momentary model: hovering previews on the cell, a press-and-release over a colour keeps
 *  it, Escape closes. The strip's swatches are `data-cpick` targets of the selector itself,
 *  so one gesture spans both. */
function CellColorPopover({
  cell,
  value,
  onChange,
  onClose,
}: {
  cell: DOMRect;
  value: Rgb;
  onChange: (rgb: Rgb) => void;
  onClose: () => void;
}) {
  const left = Math.max(4, Math.min(cell.left, window.innerWidth - PICKER_W - 4));
  // Strip + selector open below the cell as one stack; near the window's bottom the whole
  // stack rises so the selector (which keeps itself on screen) never covers the strip.
  const top = Math.max(4, Math.min(cell.bottom + 2, window.innerHeight - PICKER_H - 4 - QUICK_STRIP_H - 2));
  // The selector opens below its anchor: anchor it to the strip.
  const anchor = new DOMRect(left, top, PICKER_W, QUICK_STRIP_H);
  return (
    <>
      <div className="pr-cellcolor-quick" style={{ left, top }} role="group" aria-label="Colores rápidos">
        {QUICK_CELL_COLORS.map((c) => (
          <div
            key={c.label}
            className="pr-cellcolor-swatch"
            data-cpick="rgb"
            data-rgb={c.rgb.join(",")}
            title={c.label}
            style={{ background: `rgb(${c.rgb.join(",")})` }}
          />
        ))}
      </div>
      <ColorPicker anchor={anchor} value={value} onChange={onChange} onClose={onClose} />
    </>
  );
}

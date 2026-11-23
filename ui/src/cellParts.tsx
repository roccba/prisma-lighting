import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ColorPicker, type Rgb } from "./ColorPicker";
import type { CellInfo } from "./api";

// The pieces of a grid cell shared by the track grid: the in-place rename field, the
// right-click menu, the cell colour popover and the light/dark ink rule.

/** Quick colours offered above the colour selector — fixed by design. */
export const QUICK_CELL_COLORS: { label: string; rgb: Rgb }[] = [
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
export const NO_COLOR: Rgb = [42, 42, 42];
/** Height of the quick-colour strip (keep in sync with `.pr-cellcolor-quick`). */
const QUICK_STRIP_H = 28;
/** The colour selector's box (keep in sync with `PICKER_W`/`PICKER_H` in ColorPicker). */
const PICKER_W = 640;
const PICKER_H = 330;

/** Whether a coloured cell reads as LIGHT as shown — idle darkened by `--cell-shade`
 *  (35% black), running lit by `--cell-glow` (20% white) — so its text goes dark
 *  (keep the factors in sync with those tokens). Same luma rule as the colour selector. */
export function isLightCell([r, g, b]: Rgb, active: boolean): boolean {
  const shown = (c: number) => (active ? c + (255 - c) * 0.2 : c * 0.65);
  return shown(r) * 0.299 + shown(g) * 0.587 + shown(b) * 0.114 > 150;
}

// --- In-place rename ------------------------------------------------------------------

/** The cell's label as an editable field: Enter or blur confirms, Escape cancels. */
export function RenameInput({
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

export type MenuItem = { label: string; disabled?: boolean; onSelect: () => void } | "sep";

/** A small right-click menu. Closes on a press outside it, on Escape, and after an item
 *  runs. Keyboard: ↑/↓ (and Home/End) move between enabled items, Enter/Space runs one. */
export function CellMenu({
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
export function CellColorPopover({
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

/** A cell's state, in the words the whole UI uses (LIVE-MODEL-SPEC §7): Vacía (no effect),
 *  Cargada (an effect, not playing), En vivo (the track's running clip), Armada (waiting
 *  for GO). A running clip reads En vivo even while a new arm is pending elsewhere. */
export function cellState(cell: Pick<CellInfo, "effect_id" | "active" | "armed">): string {
  if (!cell.effect_id) return "Vacía";
  if (cell.active) return "En vivo";
  if (cell.armed) return "Armada";
  return "Cargada";
}

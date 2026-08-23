import { useRef } from "react";

// A thin horizontal grab bar that resizes the pane above it (drag vertically). Controlled:
// the parent owns the height value and clamps it. `invert` flips the direction for a bar
// that sits ABOVE the pane it resizes (dragging up grows that pane).

interface Props {
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  invert?: boolean;
}

export function Splitter({ value, onChange, min = 60, max = 2000, invert = false }: Props) {
  const drag = useRef<{ startY: number; startVal: number } | null>(null);

  const onDown = (e: React.PointerEvent) => {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag.current = { startY: e.clientY, startVal: value };
    e.preventDefault();
  };
  const onMove = (e: React.PointerEvent) => {
    if (!drag.current) return;
    const dy = e.clientY - drag.current.startY;
    const next = drag.current.startVal + (invert ? -dy : dy);
    onChange(Math.max(min, Math.min(max, next)));
  };
  const onUp = (e: React.PointerEvent) => {
    drag.current = null;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  };

  return (
    <div
      className="pr-splitter-h"
      role="separator"
      aria-orientation="horizontal"
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
    >
      <span className="pr-splitter-grip" />
    </div>
  );
}

/** Read a persisted pane height from localStorage (falls back on any failure). */
export function loadPaneHeight(key: string, fallback: number): number {
  try {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : fallback;
  } catch {
    return fallback;
  }
}

/** Persist a pane height (best-effort; ignores storage failures). */
export function savePaneHeight(key: string, value: number): void {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    /* private mode / disabled storage — the height just won't persist */
  }
}

import { useEffect, useMemo, useState } from "react";
import { setParam, type ParamInfo } from "./api";
import { hexToLinear, linearToHex } from "./srgb";

// The palette editor (M8 parameter types, the colours behind Scroll de color).
// A palette is a fixed block of colour slots plus a length ("count") param; every value
// is a plain linear-light scalar in the tree (§2.3), so the palette persists and
// MIDI-maps like any other parameter — this control is presentation only. It shows the
// `count` live slots as swatches: click one to edit its colour (native sRGB picker,
// converted to linear on the way in), drag to reorder, × to remove, + to add.

interface Slot {
  index: number;
  r: ParamInfo;
  g: ParamInfo;
  b: ParamInfo;
}

interface PaletteControlProps {
  zoneId: string;
  /** All palette-role params (one per channel), and the palette_count param. */
  slots: Slot[];
  count: ParamInfo;
}

type Rgb = { r: number; g: number; b: number };

export function PaletteControl({ zoneId, slots, count }: PaletteControlProps) {
  const max = slots.length;
  // count is a 1..max real param carried normalized; recover the integer length.
  const countOf = (norm: number) => Math.round(count.min + norm * (count.max - count.min));

  // Linear colours for every slot (the model), and the active length. Seeded from the
  // params; re-seeded when a cell fires (params reload).
  const seed = useMemo<Rgb[]>(
    () => slots.map((s) => ({ r: s.r.value_norm, g: s.g.value_norm, b: s.b.value_norm })),
    [slots],
  );
  const [cols, setCols] = useState<Rgb[]>(seed);
  const [len, setLen] = useState(countOf(count.value_norm));
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  useEffect(() => setCols(seed), [seed]);
  useEffect(() => setLen(countOf(count.value_norm)), [count.value_norm]);

  const writeSlot = (i: number, c: Rgb) => {
    setParam(zoneId, slots[i].r.name, c.r).catch(() => {});
    setParam(zoneId, slots[i].g.name, c.g).catch(() => {});
    setParam(zoneId, slots[i].b.name, c.b).catch(() => {});
  };
  const writeLen = (n: number) => {
    const norm = (n - count.min) / (count.max - count.min);
    setParam(zoneId, count.name, norm).catch(() => {});
  };

  // Edit one swatch's colour in place.
  const edit = (i: number, hex: string) => {
    const next = cols.slice();
    next[i] = hexToLinear(hex);
    setCols(next);
    writeSlot(i, next[i]);
  };

  // Rewrite the whole slot block from a new colour array + length. Overwriting
  // unchanged slots is harmless and keeps add/remove/reorder a single clear path.
  const commit = (next: Rgb[], n: number) => {
    setCols(next);
    setLen(n);
    for (let i = 0; i < max; i++) writeSlot(i, next[i]);
    writeLen(n);
  };

  const add = () => {
    if (len >= max) return;
    // The new slot keeps its existing colour; only the length grows.
    setLen(len + 1);
    writeLen(len + 1);
  };

  const remove = (i: number) => {
    if (len <= 1) return;
    const active = cols.slice(0, len);
    active.splice(i, 1);
    // Keep the array length == max: append the removed colour at the end (so a later
    // "add" brings back a real colour, not black).
    const next = active.concat(cols.slice(len), [cols[i]]).slice(0, max);
    commit(next, len - 1);
  };

  const reorder = (from: number, to: number) => {
    if (from === to) return;
    const next = cols.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    commit(next, len);
  };

  // Just the palette swatches; the "Colores" label is provided by the enclosing ParamField.
  return (
    <div className="pr-palette">
        {Array.from({ length: len }, (_, i) => {
          const hex = linearToHex(cols[i].r, cols[i].g, cols[i].b);
          return (
            <div
              key={i}
              className={`pr-palette-slot${dragFrom === i ? " is-dragging" : ""}`}
              draggable
              onDragStart={() => setDragFrom(i)}
              onDragOver={(e) => e.preventDefault()}
              onDrop={() => {
                if (dragFrom !== null) reorder(dragFrom, i);
                setDragFrom(null);
              }}
              onDragEnd={() => setDragFrom(null)}
            >
              <label className="pr-palette-swatch" style={{ background: hex }} title={hex}>
                <input
                  type="color"
                  value={hex}
                  onChange={(e) => edit(i, e.target.value)}
                  aria-label={`Color ${i + 1}`}
                />
              </label>
              {len > 1 && (
                <button
                  className="pr-palette-remove"
                  onClick={() => remove(i)}
                  title="Quitar color"
                  aria-label={`Quitar color ${i + 1}`}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
        {len < max && (
          <button className="pr-palette-add" onClick={add} title="Agregar color" aria-label="Agregar color">
            +
          </button>
        )}
    </div>
  );
}

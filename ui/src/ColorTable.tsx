import { useEffect, useMemo, useState } from "react";
import { setParam, type ParamInfo } from "./api";
import { ByteBox } from "./ByteBox";
import { ColorSwatch, type Rgb } from "./ColorPicker";
import { FaderBox } from "./FaderBox";
import { FloatWindow } from "./FloatWindow";
import { Sliders } from "./icons";
import { linearToSrgb255, srgb255ToLinear } from "./srgb";

// "Tabla de color" — the classic Color Table for a palette effect (Scroll de color), opened
// from the effect's "Colores" button. One row per colour: # · thumbnail · R G B W A.
//  · the thumbnail opens the "Selector de color" (press, hold, drag, release);
//  · R/G/B are 0–255 boxes (type or drag up/down); W/A are shown but disabled (RGB only);
//  · drag a row by its # to move it, or select it and use ▲ / ▼;
//  · + adds a colour (white, or a copy of the selected one), − removes the selected one,
//    ⧉ duplicates it, the faders button opens the Caja de Fader for it.
// The palette is a fixed block of slots plus a "count" param (the number of rows); every
// value is a plain linear-light scalar in the tree (§2.3), so it persists and MIDI-maps
// like any other parameter. Presentation is sRGB 0–255.

export interface PaletteSlot {
  index: number;
  r: ParamInfo;
  g: ParamInfo;
  b: ParamInfo;
}

interface ColorTableProps {
  zoneId: string;
  slots: PaletteSlot[];
  count: ParamInfo;
  at?: { x: number; y: number };
  onClose: () => void;
}

type Lin = { r: number; g: number; b: number };
const toRgb = (c: Lin): Rgb => [linearToSrgb255(c.r), linearToSrgb255(c.g), linearToSrgb255(c.b)];
const toLin = ([r, g, b]: Rgb): Lin => ({ r: srgb255ToLinear(r), g: srgb255ToLinear(g), b: srgb255ToLinear(b) });
const WHITE: Lin = { r: 1, g: 1, b: 1 };

export function ColorTable({ zoneId, slots, count, at, onClose }: ColorTableProps) {
  const max = slots.length;
  const countOf = (norm: number) => Math.round(count.min + norm * (count.max - count.min));

  const seed = useMemo<Lin[]>(() => slots.map((s) => ({ r: s.r.value_norm, g: s.g.value_norm, b: s.b.value_norm })), [slots]);
  const [cols, setCols] = useState<Lin[]>(seed);
  const [len, setLen] = useState(countOf(count.value_norm));
  const [sel, setSel] = useState<number | null>(null);
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const [faderAt, setFaderAt] = useState<{ x: number; y: number } | null>(null);
  useEffect(() => setCols(seed), [seed]);
  useEffect(() => setLen(countOf(count.value_norm)), [count.value_norm]);

  const writeSlot = (i: number, c: Lin) => {
    setParam(zoneId, slots[i].r.name, c.r).catch(() => {});
    setParam(zoneId, slots[i].g.name, c.g).catch(() => {});
    setParam(zoneId, slots[i].b.name, c.b).catch(() => {});
  };
  const writeLen = (n: number) => {
    setParam(zoneId, count.name, (n - count.min) / (count.max - count.min)).catch(() => {});
  };

  /** Rewrite the whole slot block + length (add/remove/reorder share this path). */
  const commit = (next: Lin[], n: number) => {
    setCols(next);
    setLen(n);
    for (let i = 0; i < max; i++) writeSlot(i, next[i]);
    writeLen(n);
  };

  const setColor = (i: number, c: Lin) => {
    const next = cols.slice();
    next[i] = c;
    setCols(next);
    writeSlot(i, c);
  };
  const setChannel = (i: number, ch: 0 | 1 | 2, v: number) => {
    const rgb = toRgb(cols[i]);
    rgb[ch] = v;
    setColor(i, toLin(rgb));
  };

  /** Insert `c` after row `after` (or at the end) and select it. */
  const insert = (c: Lin, after: number | null) => {
    if (len >= max) return;
    const at2 = after == null ? len : after + 1;
    const active = cols.slice(0, len);
    active.splice(at2, 0, c);
    const next = active.concat(cols.slice(len)).slice(0, max);
    commit(next, len + 1);
    setSel(at2);
  };
  const add = () => insert(sel != null ? cols[sel] : WHITE, sel);
  const duplicate = () => sel != null && insert(cols[sel], sel);
  const remove = () => {
    if (sel == null || len <= 1) return;
    const active = cols.slice(0, len);
    const [gone] = active.splice(sel, 1);
    // Keep the block full-length: the removed colour goes past the live end.
    const next = active.concat(cols.slice(len), [gone]).slice(0, max);
    commit(next, len - 1);
    setSel(Math.min(sel, len - 2));
  };
  const move = (from: number, to: number) => {
    if (from === to || to < 0 || to >= len) return;
    const next = cols.slice();
    const [m] = next.splice(from, 1);
    next.splice(to, 0, m);
    commit(next, len);
    setSel(to);
  };

  const selRgb = sel != null ? toRgb(cols[sel]) : null;

  return (
    <FloatWindow title="Tabla de color" at={at} className="pr-ctable" onClose={onClose}>
      <div className="pr-ctable-body">
        <div className="pr-ctable-wrap">
          <table className="pr-table pr-ctable-table">
            <thead>
              <tr>
                <th className="c-idx">#</th>
                <th className="c-sw" />
                <th>R</th>
                <th>G</th>
                <th>B</th>
                <th className="is-off">W</th>
                <th className="is-off">A</th>
              </tr>
            </thead>
            <tbody>
              {Array.from({ length: len }, (_, i) => {
                const rgb = toRgb(cols[i]);
                return (
                  <tr
                    key={i}
                    className={`${sel === i ? "is-sel" : ""}${dropAt === i && dragFrom !== null && dragFrom !== i ? " is-drop" : ""}`}
                    onPointerDown={() => setSel(i)}
                    onDragOver={(e) => {
                      if (dragFrom === null) return;
                      e.preventDefault();
                      setDropAt(i);
                    }}
                    onDrop={() => {
                      if (dragFrom !== null) move(dragFrom, i);
                      setDragFrom(null);
                      setDropAt(null);
                    }}
                  >
                    <td
                      className="c-idx"
                      draggable
                      title="Arrastrá para cambiar de posición"
                      onDragStart={(e) => {
                        e.dataTransfer.effectAllowed = "move";
                        setDragFrom(i);
                        setSel(i);
                      }}
                      onDragEnd={() => {
                        setDragFrom(null);
                        setDropAt(null);
                      }}
                    >
                      {i + 1}
                    </td>
                    <td className="c-sw">
                      <ColorSwatch className="pr-ctable-swatch" value={rgb} onChange={(c) => setColor(i, toLin(c))} />
                    </td>
                    {([0, 1, 2] as const).map((ch) => (
                      <td key={ch}>
                        <ByteBox label={"RGB"[ch]} value={rgb[ch]} onChange={(v) => setChannel(i, ch, v)} className="pr-ctable-val" />
                      </td>
                    ))}
                    <td>
                      <ByteBox label="W" value={0} disabled className="pr-ctable-val" />
                    </td>
                    <td>
                      <ByteBox label="A" value={0} disabled className="pr-ctable-val" />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        <div className="pr-ctable-bar">
          <button type="button" className="pr-btn" onClick={add} disabled={len >= max} title="Agregar color (copia del seleccionado, o blanco)">
            +
          </button>
          <button type="button" className="pr-btn" onClick={remove} disabled={sel == null || len <= 1} title="Quitar el color seleccionado">
            −
          </button>
          <button type="button" className="pr-btn" onClick={duplicate} disabled={sel == null || len >= max} title="Duplicar el color seleccionado">
            ⧉
          </button>
          <span className="pr-ctable-count"># {len}</span>
          <button
            type="button"
            className={`pr-btn${faderAt ? " is-active" : ""}`}
            disabled={sel == null}
            onClick={(e) => {
              const p = { x: e.clientX, y: e.clientY };
              setFaderAt((cur) => (cur ? null : p));
            }}
            title="Caja de Fader del color seleccionado"
            aria-label="Caja de Fader"
          >
            <Sliders size={15} />
          </button>
          <span className="pr-ctable-spacer" />
          <button type="button" className="pr-btn" onClick={() => sel != null && move(sel, sel - 1)} disabled={sel == null || sel === 0} title="Subir">
            ▲
          </button>
          <button type="button" className="pr-btn" onClick={() => sel != null && move(sel, sel + 1)} disabled={sel == null || sel >= len - 1} title="Bajar">
            ▼
          </button>
        </div>
      </div>

      {faderAt && sel != null && selRgb && (
        <FaderBox
          channels={[
            { label: "R", chip: "#ff0000", value: selRgb[0], onChange: (v) => setChannel(sel, 0, v) },
            { label: "G", chip: "#00c000", value: selRgb[1], onChange: (v) => setChannel(sel, 1, v) },
            { label: "B", chip: "#0040ff", value: selRgb[2], onChange: (v) => setChannel(sel, 2, v) },
            { label: "W", chip: "#000000", value: 0, disabled: true },
            { label: "A", chip: "#ffffff", value: 0, disabled: true },
          ]}
          preview={<ColorSwatch className="pr-faderbox-swatch" value={selRgb} onChange={(c) => setColor(sel, toLin(c))} />}
          at={faderAt}
          onClose={() => setFaderAt(null)}
        />
      )}
    </FloatWindow>
  );
}

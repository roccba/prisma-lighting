import { useEffect, useRef, useState } from "react";
import { setPalette } from "./api";
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
// The table is a LIST stored with the cell (up to 255 colours), not one parameter per
// colour; every edit sends the whole list (at most once per frame) and it lands on the
// running effect at the next frame boundary. Presentation is sRGB 0–255.

interface ColorTableProps {
  zoneId: string;
  /** The current colour table (linear-light RGB). */
  colors: [number, number, number][];
  /** Most colours allowed (255). */
  max: number;
  /** Called with every new table (already sent to the engine), so the panel's copy
   *  stays current. */
  onChange: (colors: [number, number, number][]) => void;
  at?: { x: number; y: number };
  onClose: () => void;
}

type Lin = { r: number; g: number; b: number };
const toRgb = (c: Lin): Rgb => [linearToSrgb255(c.r), linearToSrgb255(c.g), linearToSrgb255(c.b)];
const toLin = ([r, g, b]: Rgb): Lin => ({ r: srgb255ToLinear(r), g: srgb255ToLinear(g), b: srgb255ToLinear(b) });
const WHITE: Lin = { r: 1, g: 1, b: 1 };

export function ColorTable({ zoneId, colors, max, onChange, at, onClose }: ColorTableProps) {
  const [cols, setCols] = useState<Lin[]>(() => colors.map(([r, g, b]) => ({ r, g, b })));
  const [sel, setSel] = useState<number | null>(null);
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const [faderAt, setFaderAt] = useState<{ x: number; y: number } | null>(null);
  const len = cols.length;

  // Send the whole table, at most once per animation frame (a dragged value box or
  // fader produces many edits; only the latest table needs to reach the engine).
  const pending = useRef<Lin[] | null>(null);
  const frame = useRef<number | null>(null);
  const send = (next: Lin[]) => {
    pending.current = next;
    if (frame.current != null) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = null;
      const list = (pending.current ?? []).map((c) => [c.r, c.g, c.b] as [number, number, number]);
      pending.current = null;
      setPalette(zoneId, list).catch(() => {});
      onChange(list);
    });
  };
  useEffect(() => () => {
    if (frame.current != null) cancelAnimationFrame(frame.current);
  }, []);

  const commit = (next: Lin[]) => {
    setCols(next);
    send(next);
  };
  const setColor = (i: number, c: Lin) => {
    const next = cols.slice();
    next[i] = c;
    commit(next);
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
    const next = cols.slice();
    next.splice(at2, 0, { ...c });
    commit(next);
    setSel(at2);
  };
  const add = () => insert(sel != null ? cols[sel] : WHITE, sel);
  const duplicate = () => sel != null && insert(cols[sel], sel);
  const remove = () => {
    if (sel == null || len <= 1) return;
    const next = cols.slice();
    next.splice(sel, 1);
    commit(next);
    setSel(Math.min(sel, next.length - 1));
  };
  const move = (from: number, to: number) => {
    if (from === to || to < 0 || to >= len) return;
    const next = cols.slice();
    const [m] = next.splice(from, 1);
    next.splice(to, 0, m);
    commit(next);
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

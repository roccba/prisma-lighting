import { useCallback, useEffect, useMemo, useRef } from "react";
import type { Patch } from "./api";
import type { ViewMode } from "./Toolbar";

// S4 layers on top of the raw S3 canvas, each gated by zoom (LOD, §5.2) so the
// operator judges them at their natural scale — no debug toggles:
//   • a 1px+ GAP between pixel cells so a tube's pixels read individually;
//   • a SUBTLE per-fixture shade so adjacent same-zone fixtures separate;
//   • DIRECTION ARROWS (signal flow) once a fixture is big enough to matter;
//   • per-pixel DMX detail (U/C) only when zoomed right in;
//   • a strong selection SILHOUETTE (dark halo + white) — not a weak wash;
//   • view modes (all / highlight / isolate) around the active zone (§5.3).

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
  activeZone: string;
  viewMode: ViewMode;
}

interface View {
  scale: number; // world units (matrix cells) → CSS pixels
  ox: number;
  oy: number;
}

const BG = "#0e0f13";
const MATRIX_BORDER = "#2a2d38";
const RUBBER_FILL = "rgba(120, 170, 255, 0.15)";
const RUBBER_STROKE = "rgba(150, 190, 255, 0.9)";

// LOD thresholds (CSS px per matrix cell).
const GAP_MIN_SCALE = 4;
const ARROW_MIN_SCALE = 6;
const DMX_MIN_SCALE = 28;

type RGB = [number, number, number];

export function Canvas({ patch, selection, onSelectionChange, activeZone, viewMode }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const view = useRef<View>({ scale: 8, ox: 20, oy: 20 });
  const drag = useRef<null | {
    mode: "select" | "pan";
    startX: number; startY: number; lastX: number; lastY: number;
    additive: boolean; subtractive: boolean; moved: boolean;
  }>(null);
  const rubber = useRef<null | { x0: number; y0: number; x1: number; y1: number }>(null);

  // Per-fixture render colour: the zone colour, modulated by a SUBTLE alternating
  // shade within the zone so adjacent same-zone fixtures don't merge. Recomputed
  // only when the patch changes.
  const fixtureColor = useMemo(() => {
    const zoneRgb = new Map<string, RGB>();
    for (const z of patch.zones) zoneRgb.set(z.id, z.display_color);
    const perZoneIndex = new Map<string, number>();
    const out = new Map<string, RGB>();
    for (const f of patch.fixtures) {
      const i = perZoneIndex.get(f.zone) ?? 0;
      perZoneIndex.set(f.zone, i + 1);
      const base = zoneRgb.get(f.zone) ?? [136, 136, 136];
      const k = i % 2 === 0 ? 1.0 : 0.84; // subtle: gap does the heavy lifting
      out.set(f.id, [base[0] * k, base[1] * k, base[2] * k] as RGB);
    }
    return out;
  }, [patch]);

  const canvasSize = () => {
    const c = canvasRef.current!;
    return { w: c.clientWidth, h: c.clientHeight };
  };

  // How a fixture renders under the current view mode / active zone (§5.3).
  const emphasis = useCallback(
    (zone: string): "full" | "dim" | "hidden" => {
      if (viewMode === "isolate") return zone === activeZone ? "full" : "hidden";
      if (viewMode === "highlight") return zone === activeZone ? "full" : "dim";
      return "full";
    },
    [viewMode, activeZone],
  );

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const { w, h } = canvasSize();
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, w, h);

    const { scale, ox, oy } = view.current;
    const sx = (wx: number) => wx * scale + ox;
    const sy = (wy: number) => wy * scale + oy;

    // Matrix extent outline — orientation only.
    ctx.strokeStyle = MATRIX_BORDER;
    ctx.lineWidth = 1;
    ctx.strokeRect(sx(-0.5), sy(-0.5), patch.matrix_size[0] * scale, patch.matrix_size[1] * scale);

    const gap = scale >= GAP_MIN_SCALE ? Math.min(3, Math.max(1, scale * 0.14)) : 0;
    const cell = Math.max(1, scale - gap);
    const half = cell / 2;
    const onScreen = (x: number, y: number) => x >= -scale && x <= w + scale && y >= -scale && y <= h + scale;

    // --- Pass 1: pixel cells ---
    for (const f of patch.fixtures) {
      const e = emphasis(f.zone);
      if (e === "hidden") continue;
      const [r, g, b] = fixtureColor.get(f.id)!;
      ctx.fillStyle = e === "dim" ? `rgba(${r},${g},${b},0.20)` : `rgb(${r},${g},${b})`;
      for (const p of f.pixels) {
        const px = sx(p.position[0]);
        const py = sy(p.position[1]);
        if (!onScreen(px, py)) continue;
        ctx.fillRect(px - half, py - half, cell, cell);
      }
    }

    // --- Pass 2: direction arrows (signal flow), medium zoom+ ---
    // A SHORT arrow contained at the fixture's END, not a spine along its whole
    // length — a full line competes with the pixels on long (e.g. vertical) tubes.
    if (scale >= ARROW_MIN_SCALE) {
      for (const f of patch.fixtures) {
        if (emphasis(f.zone) !== "full") continue;
        if (f.pixels.length < 2) continue;
        const a = f.pixels[0].position;
        const b = f.pixels[f.pixels.length - 1].position;
        const ex = sx(b[0]), ey = sy(b[1]);
        const dx = b[0] - a[0], dy = b[1] - a[1];
        const wlen = Math.hypot(dx, dy) || 1;
        const ux = dx / wlen, uy = dy / wlen; // unit direction (screen == world dir)
        const stub = Math.min(wlen * scale * 0.5, 15); // capped length in px
        drawArrow(ctx, ex - ux * stub, ey - uy * stub, ex, ey);
      }
    }

    // --- Pass 3: per-pixel DMX detail, zoomed right in ---
    if (scale >= DMX_MIN_SCALE) {
      const fs = Math.max(7, scale * 0.19);
      ctx.font = `${fs}px system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.lineWidth = 3;
      for (const f of patch.fixtures) {
        if (emphasis(f.zone) !== "full") continue;
        for (const p of f.pixels) {
          const px = sx(p.position[0]);
          const py = sy(p.position[1]);
          if (!onScreen(px, py)) continue;
          label(ctx, `U${p.universe}`, px, py - fs * 0.55);
          label(ctx, `${p.start_channel}`, px, py + fs * 0.55);
        }
      }
    }

    // --- Pass 4: selection silhouette (strong: dark halo + white) ---
    if (selection.size > 0) {
      const sel = new Set<string>();
      for (const f of patch.fixtures) {
        if (!selection.has(f.id)) continue;
        for (const p of f.pixels) sel.add(`${Math.round(p.position[0])},${Math.round(p.position[1])}`);
      }
      const outline = () => {
        ctx.beginPath();
        for (const key of sel) {
          const [gx, gy] = key.split(",").map(Number);
          const cx = sx(gx), cy = sy(gy), h2 = scale / 2;
          if (!sel.has(`${gx - 1},${gy}`)) { ctx.moveTo(cx - h2, cy - h2); ctx.lineTo(cx - h2, cy + h2); }
          if (!sel.has(`${gx + 1},${gy}`)) { ctx.moveTo(cx + h2, cy - h2); ctx.lineTo(cx + h2, cy + h2); }
          if (!sel.has(`${gx},${gy - 1}`)) { ctx.moveTo(cx - h2, cy - h2); ctx.lineTo(cx + h2, cy - h2); }
          if (!sel.has(`${gx},${gy + 1}`)) { ctx.moveTo(cx - h2, cy + h2); ctx.lineTo(cx + h2, cy + h2); }
        }
        ctx.stroke();
      };
      ctx.lineCap = "round";
      ctx.strokeStyle = "rgba(0,0,0,0.85)";
      ctx.lineWidth = 4;
      outline();
      ctx.strokeStyle = "#ffffff";
      ctx.lineWidth = 1.75;
      outline();
    }

    // --- Rubber-band ---
    if (rubber.current) {
      const { x0, y0, x1, y1 } = rubber.current;
      const rx = Math.min(x0, x1), ry = Math.min(y0, y1);
      const rw = Math.abs(x1 - x0), rh = Math.abs(y1 - y0);
      ctx.fillStyle = RUBBER_FILL;
      ctx.fillRect(rx, ry, rw, rh);
      ctx.strokeStyle = RUBBER_STROKE;
      ctx.lineWidth = 1;
      ctx.strokeRect(rx + 0.5, ry + 0.5, rw, rh);
    }
  }, [patch, selection, fixtureColor, emphasis]);

  // Fit the whole rig into view whenever the patch changes.
  const fit = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const { w, h } = canvasSize();
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const f of patch.fixtures)
      for (const p of f.pixels) {
        minX = Math.min(minX, p.position[0]); minY = Math.min(minY, p.position[1]);
        maxX = Math.max(maxX, p.position[0]); maxY = Math.max(maxY, p.position[1]);
      }
    if (!isFinite(minX)) { minX = 0; minY = 0; maxX = patch.matrix_size[0]; maxY = patch.matrix_size[1]; }
    const spanX = Math.max(1, maxX - minX + 1);
    const spanY = Math.max(1, maxY - minY + 1);
    const margin = 40;
    const scale = Math.max(1, Math.min((w - margin) / spanX, (h - margin) / spanY));
    view.current = {
      scale,
      ox: (w - spanX * scale) / 2 - minX * scale + scale / 2,
      oy: (h - spanY * scale) / 2 - minY * scale + scale / 2,
    };
    draw();
  }, [patch, draw]);

  useEffect(() => { fit(); }, [fit]);
  useEffect(() => { draw(); }, [draw]);
  useEffect(() => {
    const onResize = () => draw();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [draw]);

  // --- Interaction ---
  const localXY = (e: React.PointerEvent | React.WheelEvent) => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };
  const worldFromScreen = (px: number, py: number) => {
    const { scale, ox, oy } = view.current;
    return { wx: (px - ox) / scale, wy: (py - oy) / scale };
  };

  const onPointerDown = (e: React.PointerEvent) => {
    canvasRef.current!.setPointerCapture(e.pointerId);
    const { x, y } = localXY(e);
    const pan = e.button === 1 || e.button === 2;
    drag.current = {
      mode: pan ? "pan" : "select",
      startX: x, startY: y, lastX: x, lastY: y,
      additive: e.shiftKey, subtractive: e.altKey, moved: false,
    };
    if (!pan) rubber.current = { x0: x, y0: y, x1: x, y1: y };
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const { x, y } = localXY(e);
    if (Math.abs(x - d.startX) + Math.abs(y - d.startY) > 3) d.moved = true;
    if (d.mode === "pan") {
      view.current.ox += x - d.lastX;
      view.current.oy += y - d.lastY;
    } else if (rubber.current) {
      rubber.current.x1 = x; rubber.current.y1 = y;
    }
    d.lastX = x; d.lastY = y;
    draw();
  };

  const fixturesInRect = (rx0: number, ry0: number, rx1: number, ry1: number) => {
    const a = worldFromScreen(Math.min(rx0, rx1), Math.min(ry0, ry1));
    const b = worldFromScreen(Math.max(rx0, rx1), Math.max(ry0, ry1));
    const hit = new Set<string>();
    for (const f of patch.fixtures) {
      if (emphasis(f.zone) === "hidden") continue; // can't select what isn't shown
      if (f.pixels.some((p) => p.position[0] >= a.wx && p.position[0] <= b.wx && p.position[1] >= a.wy && p.position[1] <= b.wy))
        hit.add(f.id);
    }
    return hit;
  };

  const fixtureAt = (px: number, py: number): string | null => {
    const { wx, wy } = worldFromScreen(px, py);
    for (const f of patch.fixtures) {
      if (emphasis(f.zone) === "hidden") continue;
      if (f.pixels.some((p) => Math.abs(p.position[0] - wx) <= 0.5 && Math.abs(p.position[1] - wy) <= 0.5))
        return f.id;
    }
    return null;
  };

  const onPointerUp = (e: React.PointerEvent) => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    canvasRef.current!.releasePointerCapture(e.pointerId);
    if (d.mode === "pan") { rubber.current = null; draw(); return; }
    const { x, y } = localXY(e);
    let hits: Set<string>;
    if (d.moved) hits = fixturesInRect(d.startX, d.startY, x, y);
    else { const f = fixtureAt(x, y); hits = f ? new Set([f]) : new Set(); }
    rubber.current = null;
    let next: Set<string>;
    if (d.additive) { next = new Set(selection); for (const id of hits) next.add(id); }
    else if (d.subtractive) { next = new Set(selection); for (const id of hits) next.delete(id); }
    else next = hits;
    onSelectionChange(next);
  };

  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const { x, y } = localXY(e);
    const before = worldFromScreen(x, y);
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    view.current.scale = Math.min(80, Math.max(0.5, view.current.scale * factor));
    view.current.ox = x - before.wx * view.current.scale;
    view.current.oy = y - before.wy * view.current.scale;
    draw();
  };

  return (
    <div className="canvas-wrap">
      <canvas
        ref={canvasRef}
        className="canvas"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onWheel={onWheel}
        onContextMenu={(e) => e.preventDefault()}
      />
      <div className="canvas-hint">
        drag: select · shift add · alt remove · middle/right drag: pan · wheel: zoom
      </div>
    </div>
  );
}

/** A signal-flow arrow from (x0,y0) to (x1,y1): dark halo then light core so it
 *  reads over any zone colour. */
function drawArrow(ctx: CanvasRenderingContext2D, x0: number, y0: number, x1: number, y1: number) {
  const dx = x1 - x0, dy = y1 - y0;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len, uy = dy / len;
  const head = Math.min(9, len * 0.4);
  const hx = x1 - ux * head, hy = y1 - uy * head;
  const nx = -uy, ny = ux;
  const stroke = (color: string, width: number) => {
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.moveTo(x1, y1);
    ctx.lineTo(hx + nx * head * 0.5, hy + ny * head * 0.5);
    ctx.moveTo(x1, y1);
    ctx.lineTo(hx - nx * head * 0.5, hy - ny * head * 0.5);
    ctx.stroke();
  };
  stroke("rgba(0,0,0,0.55)", 3);
  stroke("rgba(255,255,255,0.92)", 1.1);
}

/** Small DMX label with a dark outline for legibility over any cell colour. */
function label(ctx: CanvasRenderingContext2D, text: string, x: number, y: number) {
  ctx.strokeStyle = "rgba(0,0,0,0.8)";
  ctx.strokeText(text, x, y);
  ctx.fillStyle = "#eef1f6";
  ctx.fillText(text, x, y);
}

import { useCallback, useEffect, useMemo, useRef } from "react";
import type { Patch } from "./api";

// S3 is the RAW canvas: pixels are drawn as cells filled with their zone colour,
// edge-to-edge with no gaps or borders — so adjacent fixtures are deliberately
// indistinguishable here. Fixture-boundary contrast, direction arrows and
// zoom-dependent detail (LOD) are S4, layered on top of this on purpose.

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
}

interface View {
  scale: number; // world units (matrix cells) → CSS pixels
  ox: number;
  oy: number;
}

const BG = "#0e0f13";
const MATRIX_BORDER = "#2a2d38";
const SELECT_WASH = "rgba(255, 255, 255, 0.55)";
const RUBBER_FILL = "rgba(120, 170, 255, 0.15)";
const RUBBER_STROKE = "rgba(150, 190, 255, 0.9)";

export function Canvas({ patch, selection, onSelectionChange }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const view = useRef<View>({ scale: 8, ox: 20, oy: 20 });
  const drag = useRef<null | {
    mode: "select" | "pan";
    startX: number;
    startY: number;
    lastX: number;
    lastY: number;
    additive: boolean;
    subtractive: boolean;
    moved: boolean;
  }>(null);
  const rubber = useRef<null | { x0: number; y0: number; x1: number; y1: number }>(null);

  // zone colour lookup, recomputed only when the patch changes.
  const zoneColor = useMemo(() => {
    const m = new Map<string, string>();
    for (const z of patch.zones) {
      m.set(z.id, `rgb(${z.display_color[0]},${z.display_color[1]},${z.display_color[2]})`);
    }
    return m;
  }, [patch]);

  const canvasSize = () => {
    const c = canvasRef.current!;
    return { w: c.clientWidth, h: c.clientHeight };
  };

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

    // Matrix extent outline — orientation only, not fixture contrast.
    ctx.strokeStyle = MATRIX_BORDER;
    ctx.lineWidth = 1;
    ctx.strokeRect(ox - 0.5 * scale, oy - 0.5 * scale, patch.matrix_size[0] * scale, patch.matrix_size[1] * scale);

    const cell = scale; // edge-to-edge, no gap
    const half = cell / 2;

    // Pass 1: zone-coloured cells (culled to the viewport).
    for (const f of patch.fixtures) {
      ctx.fillStyle = zoneColor.get(f.zone) ?? "#888";
      for (const p of f.pixels) {
        const sx = p.position[0] * scale + ox;
        const sy = p.position[1] * scale + oy;
        if (sx < -cell || sx > w + cell || sy < -cell || sy > h + cell) continue;
        ctx.fillRect(sx - half, sy - half, cell, cell);
      }
    }

    // Pass 2: a white wash over selected fixtures — selection feedback without
    // per-pixel borders (which would read as the S4 contrast scheme).
    if (selection.size > 0) {
      ctx.fillStyle = SELECT_WASH;
      for (const f of patch.fixtures) {
        if (!selection.has(f.id)) continue;
        for (const p of f.pixels) {
          const sx = p.position[0] * scale + ox;
          const sy = p.position[1] * scale + oy;
          if (sx < -cell || sx > w + cell || sy < -cell || sy > h + cell) continue;
          ctx.fillRect(sx - half, sy - half, cell, cell);
        }
      }
    }

    // Rubber-band selection rectangle.
    if (rubber.current) {
      const { x0, y0, x1, y1 } = rubber.current;
      const rx = Math.min(x0, x1);
      const ry = Math.min(y0, y1);
      const rw = Math.abs(x1 - x0);
      const rh = Math.abs(y1 - y0);
      ctx.fillStyle = RUBBER_FILL;
      ctx.fillRect(rx, ry, rw, rh);
      ctx.strokeStyle = RUBBER_STROKE;
      ctx.lineWidth = 1;
      ctx.strokeRect(rx + 0.5, ry + 0.5, rw, rh);
    }
  }, [patch, selection, zoneColor]);

  // Fit the whole rig into view whenever the patch changes.
  const fit = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const { w, h } = canvasSize();
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const f of patch.fixtures)
      for (const p of f.pixels) {
        minX = Math.min(minX, p.position[0]);
        minY = Math.min(minY, p.position[1]);
        maxX = Math.max(maxX, p.position[0]);
        maxY = Math.max(maxY, p.position[1]);
      }
    if (!isFinite(minX)) {
      minX = 0; minY = 0; maxX = patch.matrix_size[0]; maxY = patch.matrix_size[1];
    }
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

  const onPointerDown = (e: React.PointerEvent) => {
    canvasRef.current!.setPointerCapture(e.pointerId);
    const { x, y } = localXY(e);
    const pan = e.button === 1 || e.button === 2; // middle / right → pan
    drag.current = {
      mode: pan ? "pan" : "select",
      startX: x, startY: y, lastX: x, lastY: y,
      additive: e.shiftKey,
      subtractive: e.altKey,
      moved: false,
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
      rubber.current.x1 = x;
      rubber.current.y1 = y;
    }
    d.lastX = x; d.lastY = y;
    draw();
  };

  const worldFromScreen = (sx: number, sy: number) => {
    const { scale, ox, oy } = view.current;
    return { wx: (sx - ox) / scale, wy: (sy - oy) / scale };
  };

  const fixturesInRect = (rx0: number, ry0: number, rx1: number, ry1: number) => {
    const a = worldFromScreen(Math.min(rx0, rx1), Math.min(ry0, ry1));
    const b = worldFromScreen(Math.max(rx0, rx1), Math.max(ry0, ry1));
    const hit = new Set<string>();
    for (const f of patch.fixtures) {
      if (f.pixels.some((p) => p.position[0] >= a.wx && p.position[0] <= b.wx && p.position[1] >= a.wy && p.position[1] <= b.wy)) {
        hit.add(f.id);
      }
    }
    return hit;
  };

  const fixtureAt = (sx: number, sy: number): string | null => {
    const { wx, wy } = worldFromScreen(sx, sy);
    for (const f of patch.fixtures) {
      if (f.pixels.some((p) => Math.abs(p.position[0] - wx) <= 0.5 && Math.abs(p.position[1] - wy) <= 0.5)) {
        return f.id;
      }
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
    if (d.moved) {
      hits = fixturesInRect(d.startX, d.startY, x, y);
    } else {
      const f = fixtureAt(x, y);
      hits = f ? new Set([f]) : new Set();
    }
    rubber.current = null;

    let next: Set<string>;
    if (d.additive) {
      next = new Set(selection);
      for (const id of hits) next.add(id);
    } else if (d.subtractive) {
      next = new Set(selection);
      for (const id of hits) next.delete(id);
    } else {
      next = hits;
    }
    onSelectionChange(next);
  };

  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const { x, y } = localXY(e);
    const before = worldFromScreen(x, y);
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    view.current.scale = Math.min(60, Math.max(0.5, view.current.scale * factor));
    // Keep the point under the cursor fixed.
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

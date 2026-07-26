import { useCallback, useEffect, useMemo, useRef } from "react";
import { diagnosticMarks, type Diagnostic, type Patch } from "./api";
import type { ViewMode } from "./Toolbar";

// S4 layers (zoom-gated: gap, contrast, arrows, per-pixel DMX, selection
// silhouette, view modes) plus S5b additions: a background GRID and the active
// zone's matrix bounds (§5.6) for spatial reference, and DRAG-to-move (§5.5) —
// distinct from rubber-band selection — committed as one undo on release.
//
// S6 (replanned) adds a DISCREET mark on every pixel a DMX wire fault (address
// collision / channel out of range) points at — a thin ring, not an alarm — drawn
// on top and not gated by view mode so the fault is locatable at a glance (§4.3).

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
  onMove: (ids: string[], delta: [number, number, number]) => void;
  activeZone: string;
  viewMode: ViewMode;
  diagnostics: Diagnostic[];
}

interface View {
  scale: number;
  ox: number;
  oy: number;
}

const BG = "#0e0f13"; // the "space beyond the matrix" — the faintest of the three tones
const MATRIX_FILL = "rgba(150,160,185,0.07)"; // the zone's defined matrix — a light tone
const GRID = "rgba(255,255,255,0.045)";
const RUBBER_FILL = "rgba(120, 170, 255, 0.15)";
const RUBBER_STROKE = "rgba(150, 190, 255, 0.9)";

const GAP_MIN_SCALE = 4;
const ARROW_MIN_SCALE = 6;
const GRID_MIN_SCALE = 9;
const DMX_MIN_SCALE = 28;
const FAULT_MIN_PX = 8; // fault rings never shrink below this, so they stay locatable

const FAULT = "#ffb454"; // amber — a discreet "look here", not a red alarm

type RGB = [number, number, number];

export function Canvas({ patch, selection, onSelectionChange, onMove, activeZone, viewMode, diagnostics }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const view = useRef<View>({ scale: 8, ox: 20, oy: 20 });
  const drag = useRef<null | {
    mode: "select" | "pan" | "move";
    startX: number; startY: number; lastX: number; lastY: number;
    additive: boolean; subtractive: boolean; moved: boolean;
    moveIds: string[];
  }>(null);
  const rubber = useRef<null | { x0: number; y0: number; x1: number; y1: number }>(null);
  const moveDelta = useRef<[number, number]>([0, 0]); // cell delta during a move

  const fixtureColor = useMemo(() => {
    const zoneRgb = new Map<string, RGB>();
    for (const z of patch.zones) zoneRgb.set(z.id, z.display_color);
    const perZoneIndex = new Map<string, number>();
    const out = new Map<string, RGB>();
    for (const f of patch.fixtures) {
      const i = perZoneIndex.get(f.zone) ?? 0;
      perZoneIndex.set(f.zone, i + 1);
      const base = zoneRgb.get(f.zone) ?? [136, 136, 136];
      const k = i % 2 === 0 ? 1.0 : 0.84;
      out.set(f.id, [base[0] * k, base[1] * k, base[2] * k] as RGB);
    }
    return out;
  }, [patch]);

  // The matrix the canvas draws (§5.6): the SELECTED zone's, at its declared
  // `mapping_size`, anchored to the min corner of its fixtures' footprint (world
  // position is M8 debt). Only when a zone is in focus — All mode shows no matrix.
  const activeMatrix = useMemo(() => {
    if (viewMode === "all") return null;
    const zone = patch.zones.find((z) => z.id === activeZone);
    if (!zone) return null;
    let minX = Infinity, minY = Infinity;
    for (const f of patch.fixtures) {
      if (f.zone !== activeZone) continue;
      for (const p of f.pixels) {
        minX = Math.min(minX, Math.round(p.position[0]));
        minY = Math.min(minY, Math.round(p.position[1]));
      }
    }
    if (!isFinite(minX)) { minX = 0; minY = 0; } // empty zone → anchor at origin
    return { ox: minX, oy: minY, mx: zone.mapping_size[0], my: zone.mapping_size[1], color: zone.display_color };
  }, [patch, activeZone, viewMode]);

  // fixtureId → set of pixel indices that a DMX wire fault points at, for the discreet
  // canvas marks. One treatment for both fault kinds (collision / out-of-range).
  const faultPixels = useMemo(() => {
    const m = new Map<string, Set<number>>();
    for (const d of diagnostics) {
      for (const mk of diagnosticMarks(d)) {
        let inner = m.get(mk.fixture);
        if (!inner) { inner = new Set(); m.set(mk.fixture, inner); }
        inner.add(mk.pixel);
      }
    }
    return m;
  }, [diagnostics]);

  const canvasSize = () => {
    const c = canvasRef.current!;
    return { w: c.clientWidth, h: c.clientHeight };
  };

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
    const [dmx, dmy] = drag.current?.mode === "move" ? moveDelta.current : [0, 0];

    // Selected zone's matrix — three concentric tones (§5.6): the LIGHT matrix fill
    // over the fainter background (already filled), a zoom-gated grid inside it, and a
    // subtle border in the zone colour. Pixels (zone colour) paint on top in pass 1.
    // Only one zone's matrix at a time; All mode shows none.
    if (activeMatrix) {
      const { ox: mox, oy: moy, mx, my, color } = activeMatrix;
      const x0 = sx(mox - 0.5), y0 = sy(moy - 0.5);
      const wpx = mx * scale, hpx = my * scale;
      ctx.fillStyle = MATRIX_FILL;
      ctx.fillRect(x0, y0, wpx, hpx);
      if (scale >= GRID_MIN_SCALE) {
        ctx.strokeStyle = GRID;
        ctx.lineWidth = 1;
        ctx.beginPath();
        for (let i = 0; i <= mx; i++) {
          const x = sx(mox + i - 0.5);
          if (x < -1 || x > w + 1) continue;
          ctx.moveTo(x, Math.max(y0, -1)); ctx.lineTo(x, Math.min(y0 + hpx, h + 1));
        }
        for (let j = 0; j <= my; j++) {
          const y = sy(moy + j - 0.5);
          if (y < -1 || y > h + 1) continue;
          ctx.moveTo(Math.max(x0, -1), y); ctx.lineTo(Math.min(x0 + wpx, w + 1), y);
        }
        ctx.stroke();
      }
      ctx.strokeStyle = `rgba(${color.join(",")},0.55)`;
      ctx.lineWidth = 1.5;
      ctx.strokeRect(x0, y0, wpx, hpx);
    }

    const gap = scale >= GAP_MIN_SCALE ? Math.min(3, Math.max(1, scale * 0.14)) : 0;
    const cell = Math.max(1, scale - gap);
    const half = cell / 2;
    const onScreen = (x: number, y: number) => x >= -scale && x <= w + scale && y >= -scale && y <= h + scale;
    const sel = (id: string) => selection.has(id);

    // Pass 1: pixel cells (selected fixtures offset by the live move delta).
    for (const f of patch.fixtures) {
      const e = emphasis(f.zone);
      if (e === "hidden") continue;
      const [r, g, b] = fixtureColor.get(f.id)!;
      ctx.fillStyle = e === "dim" ? `rgba(${r},${g},${b},0.20)` : `rgb(${r},${g},${b})`;
      const off = sel(f.id);
      for (const p of f.pixels) {
        const px = sx(p.position[0] + (off ? dmx : 0));
        const py = sy(p.position[1] + (off ? dmy : 0));
        if (!onScreen(px, py)) continue;
        ctx.fillRect(px - half, py - half, cell, cell);
      }
    }

    // Pass 2: contained direction arrows.
    if (scale >= ARROW_MIN_SCALE) {
      for (const f of patch.fixtures) {
        if (emphasis(f.zone) !== "full") continue;
        if (f.pixels.length < 2) continue;
        const off = sel(f.id);
        const a = f.pixels[0].position, b = f.pixels[f.pixels.length - 1].position;
        const ex = sx(b[0] + (off ? dmx : 0)), ey = sy(b[1] + (off ? dmy : 0));
        const dx = b[0] - a[0], dy = b[1] - a[1];
        const wlen = Math.hypot(dx, dy) || 1;
        const ux = dx / wlen, uy = dy / wlen;
        const stub = Math.min(wlen * scale * 0.5, 15);
        drawArrow(ctx, ex - ux * stub, ey - uy * stub, ex, ey);
      }
    }

    // Pass 3: per-pixel DMX detail.
    if (scale >= DMX_MIN_SCALE) {
      const fs = Math.max(7, scale * 0.19);
      ctx.font = `${fs}px system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.lineWidth = 3;
      for (const f of patch.fixtures) {
        if (emphasis(f.zone) !== "full") continue;
        const off = sel(f.id);
        for (const p of f.pixels) {
          const px = sx(p.position[0] + (off ? dmx : 0)), py = sy(p.position[1] + (off ? dmy : 0));
          if (!onScreen(px, py)) continue;
          label(ctx, `U${p.universe}`, px, py - fs * 0.55);
          label(ctx, `${p.start_channel}`, px, py + fs * 0.55);
        }
      }
    }

    // Pass 4: selection silhouette.
    if (selection.size > 0) {
      const cells = new Set<string>();
      for (const f of patch.fixtures) {
        if (!sel(f.id)) continue;
        for (const p of f.pixels) cells.add(`${Math.round(p.position[0] + dmx)},${Math.round(p.position[1] + dmy)}`);
      }
      const outline = () => {
        ctx.beginPath();
        for (const key of cells) {
          const [gx, gy] = key.split(",").map(Number);
          const cx = sx(gx), cy = sy(gy), h2 = scale / 2;
          if (!cells.has(`${gx - 1},${gy}`)) { ctx.moveTo(cx - h2, cy - h2); ctx.lineTo(cx - h2, cy + h2); }
          if (!cells.has(`${gx + 1},${gy}`)) { ctx.moveTo(cx + h2, cy - h2); ctx.lineTo(cx + h2, cy + h2); }
          if (!cells.has(`${gx},${gy - 1}`)) { ctx.moveTo(cx - h2, cy - h2); ctx.lineTo(cx + h2, cy - h2); }
          if (!cells.has(`${gx},${gy + 1}`)) { ctx.moveTo(cx - h2, cy + h2); ctx.lineTo(cx + h2, cy + h2); }
        }
        ctx.stroke();
      };
      ctx.lineCap = "round";
      ctx.strokeStyle = "rgba(0,0,0,0.85)"; ctx.lineWidth = 4; outline();
      ctx.strokeStyle = "#ffffff"; ctx.lineWidth = 1.75; outline();
    }

    // DMX wire-fault marks — topmost, no view-mode gate so the fault is locatable even
    // in a dimmed/isolated zone (§4.3). Discreet: a thin amber ring, not an alarm.
    if (faultPixels.size > 0) {
      const size = Math.max(cell, FAULT_MIN_PX);
      for (const f of patch.fixtures) {
        const flags = faultPixels.get(f.id);
        if (!flags) continue;
        const off = sel(f.id);
        for (const p of f.pixels) {
          if (!flags.has(p.index)) continue;
          const px = sx(p.position[0] + (off ? dmx : 0));
          const py = sy(p.position[1] + (off ? dmy : 0));
          if (!onScreen(px, py)) continue;
          drawFaultMark(ctx, px, py, size);
        }
      }
    }

    // Rubber-band.
    if (rubber.current) {
      const { x0, y0, x1, y1 } = rubber.current;
      const rx = Math.min(x0, x1), ry = Math.min(y0, y1);
      const rw = Math.abs(x1 - x0), rh = Math.abs(y1 - y0);
      ctx.fillStyle = RUBBER_FILL; ctx.fillRect(rx, ry, rw, rh);
      ctx.strokeStyle = RUBBER_STROKE; ctx.lineWidth = 1; ctx.strokeRect(rx + 0.5, ry + 0.5, rw, rh);
    }
  }, [patch, selection, fixtureColor, emphasis, activeMatrix, faultPixels]);

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
    const spanX = Math.max(1, maxX - minX + 1), spanY = Math.max(1, maxY - minY + 1);
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

  const localXY = (e: React.PointerEvent | React.WheelEvent) => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };
  const worldFromScreen = (px: number, py: number) => {
    const { scale, ox, oy } = view.current;
    return { wx: (px - ox) / scale, wy: (py - oy) / scale };
  };

  const fixtureAt = (px: number, py: number): string | null => {
    const { wx, wy } = worldFromScreen(px, py);
    for (const f of patch.fixtures) {
      if (emphasis(f.zone) === "hidden") continue;
      if (f.pixels.some((p) => Math.abs(p.position[0] - wx) <= 0.5 && Math.abs(p.position[1] - wy) <= 0.5)) return f.id;
    }
    return null;
  };

  const onPointerDown = (e: React.PointerEvent) => {
    canvasRef.current!.setPointerCapture(e.pointerId);
    const { x, y } = localXY(e);
    if (e.button === 1 || e.button === 2) {
      drag.current = { mode: "pan", startX: x, startY: y, lastX: x, lastY: y, additive: false, subtractive: false, moved: false, moveIds: [] };
      return;
    }
    const hit = e.shiftKey || e.altKey ? null : fixtureAt(x, y);
    if (hit) {
      // Move the selection (selecting the hit fixture if it wasn't already).
      let moveIds: string[];
      if (selection.has(hit)) moveIds = [...selection];
      else { moveIds = [hit]; onSelectionChange(new Set([hit])); }
      moveDelta.current = [0, 0];
      drag.current = { mode: "move", startX: x, startY: y, lastX: x, lastY: y, additive: false, subtractive: false, moved: false, moveIds };
    } else {
      drag.current = { mode: "select", startX: x, startY: y, lastX: x, lastY: y, additive: e.shiftKey, subtractive: e.altKey, moved: false, moveIds: [] };
      rubber.current = { x0: x, y0: y, x1: x, y1: y };
    }
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const { x, y } = localXY(e);
    if (Math.abs(x - d.startX) + Math.abs(y - d.startY) > 3) d.moved = true;
    if (d.mode === "pan") {
      view.current.ox += x - d.lastX; view.current.oy += y - d.lastY;
    } else if (d.mode === "move") {
      moveDelta.current = [
        Math.round((x - d.startX) / view.current.scale),
        Math.round((y - d.startY) / view.current.scale),
      ];
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
      if (emphasis(f.zone) === "hidden") continue;
      if (f.pixels.some((p) => p.position[0] >= a.wx && p.position[0] <= b.wx && p.position[1] >= a.wy && p.position[1] <= b.wy)) hit.add(f.id);
    }
    return hit;
  };

  const onPointerUp = (e: React.PointerEvent) => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    canvasRef.current!.releasePointerCapture(e.pointerId);

    if (d.mode === "move") {
      const [mx, my] = moveDelta.current;
      moveDelta.current = [0, 0];
      if (mx !== 0 || my !== 0) onMove(d.moveIds, [mx, my, 0]);
      else draw();
      return;
    }
    if (d.mode === "pan") { rubber.current = null; draw(); return; }

    // Select mode. A drag is a rectangle; a click hit-tests one fixture (this path
    // is only reached for empty clicks or shift/alt on a fixture — a plain click on
    // a fixture went to move mode).
    const { x, y } = localXY(e);
    let hits: Set<string>;
    if (d.moved) hits = fixturesInRect(d.startX, d.startY, x, y);
    else { const f = fixtureAt(x, y); hits = f ? new Set([f]) : new Set(); }
    rubber.current = null;

    const next = new Set(selection);
    if (d.subtractive) {
      for (const id of hits) next.delete(id);
    } else if (d.additive) {
      // shift+click toggles; shift+drag adds.
      for (const id of hits) (!d.moved && next.has(id)) ? next.delete(id) : next.add(id);
    } else {
      // no modifier → replace (only empty click/drag reaches here).
      onSelectionChange(hits);
      return;
    }
    onSelectionChange(next);
  };

  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const { x, y } = localXY(e);
    const before = worldFromScreen(x, y);
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    // Grid zoom caps at both ends (§5.7): min 0.5 px/cell still shows a full 512-cell
    // matrix (~256 px); max 80 px/cell is a sensible per-cell zoom-in. Browser zoom
    // (Ctrl+wheel / pinch) is blocked app-wide in App, so only the grid zooms here.
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
        drag fixture: move · drag empty: select · shift/alt add/remove · middle/right: pan · wheel: zoom
      </div>
    </div>
  );
}

function drawArrow(ctx: CanvasRenderingContext2D, x0: number, y0: number, x1: number, y1: number) {
  const dx = x1 - x0, dy = y1 - y0;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len, uy = dy / len;
  const head = Math.min(9, len * 0.4);
  const hx = x1 - ux * head, hy = y1 - uy * head;
  const nx = -uy, ny = ux;
  const stroke = (color: string, width: number) => {
    ctx.strokeStyle = color; ctx.lineWidth = width; ctx.lineCap = "round"; ctx.lineJoin = "round";
    ctx.beginPath();
    ctx.moveTo(x0, y0); ctx.lineTo(x1, y1);
    ctx.moveTo(x1, y1); ctx.lineTo(hx + nx * head * 0.5, hy + ny * head * 0.5);
    ctx.moveTo(x1, y1); ctx.lineTo(hx - nx * head * 0.5, hy - ny * head * 0.5);
    ctx.stroke();
  };
  stroke("rgba(0,0,0,0.55)", 3);
  stroke("rgba(255,255,255,0.92)", 1.1);
}

function label(ctx: CanvasRenderingContext2D, text: string, x: number, y: number) {
  ctx.strokeStyle = "rgba(0,0,0,0.8)"; ctx.strokeText(text, x, y);
  ctx.fillStyle = "#eef1f6"; ctx.fillText(text, x, y);
}

// A discreet fault mark centred on a cell: a thin amber ring over a dark halo, so it
// reads against any fixture colour and at any zoom without shouting like a red overlay.
function drawFaultMark(ctx: CanvasRenderingContext2D, cx: number, cy: number, size: number) {
  const h = size / 2 + 1;
  ctx.save();
  ctx.strokeStyle = "rgba(0,0,0,0.7)"; ctx.lineWidth = 2.5;
  ctx.strokeRect(cx - h, cy - h, h * 2, h * 2);
  ctx.strokeStyle = FAULT; ctx.lineWidth = 1.25;
  ctx.strokeRect(cx - h, cy - h, h * 2, h * 2);
  ctx.restore();
}

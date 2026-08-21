import type { PreviewDescriptor } from "./previewStream";

// Shared draw for a single zone's live pixels (M8 §3.1 mini-preview and §3.4 large
// preview). Both consume the SAME M6 stream and paint the SAME thing at different sizes —
// the zone's member pixels at their real matrix positions, filled with the colour the
// engine rendered — so the draw lives here once. Mini and large are literally the SAME
// source image at different scales (see below), so they cannot disagree about content.
//
// # Why one blit, not per-pixel fillRect
//
// The naive draw issues one `fillRect` per pixel/cell every frame. WebKitGTK's accelerated
// 2D-canvas backend DEFERS those operations and replays them (Skia `SkBigPicture`) when the
// page presents. Hundreds/thousands of rects × several mini-previews × 30 Hz builds a
// deferred-op pile so large that flushing it blocks the WebProcess main thread — visible as
// a mini-preview flicker, and, when it stalls past WebKit's 10 s watchdog, a SIGABRT crash
// (`flushDeferredOperations` → `crashAfter10Seconds` in the core dump). The fix is to make
// the per-frame work a FIXED, tiny number of canvas ops regardless of pixel count: scatter
// the colours into a small `ImageData` (plain array writes — not canvas ops), `putImageData`
// it into a shared offscreen once, then `drawImage` that offscreen scaled to fit. Two canvas
// ops per frame instead of thousands.

export interface ZoneDrawOptions {
  /** Fraction of the inter-pixel spacing a dot fills (0..1). Retained for API stability;
   *  the blit path does not draw dots. */
  dotScale: number;
  /** Hard cap on a dot's radius in CSS px, before dpr. Retained for API stability. */
  maxDotCss: number;
  /** Inner padding in CSS px, before dpr. */
  padCss: number;
}

/** Cap on an offscreen grid axis. A matrix larger than this on an axis is bucketed down to
 *  it (rare — keeps the `ImageData` and the scatter loop bounded); the `drawImage` then
 *  scales the grid to the display size either way. */
const MAX_GRID = 160;

// One shared offscreen canvas, reused across every zone/card each frame (drawZone runs
// synchronously per call, so a single scratch is safe). Grown as needed, never shrunk.
let offscreen: HTMLCanvasElement | null = null;
function getOffscreen(gw: number, gh: number): HTMLCanvasElement | null {
  if (!offscreen) {
    if (typeof document === "undefined") return null;
    offscreen = document.createElement("canvas");
  }
  if (offscreen.width < gw) offscreen.width = gw;
  if (offscreen.height < gh) offscreen.height = gh;
  return offscreen;
}

/** Draw `zoneId`'s member pixels into `canvas` from the latest stream buffers. Resizes the
 *  backing store to the element's box (× dpr) and clears each call. A no-op (cleared canvas)
 *  when the descriptor, colours, or zone are missing. */
export function drawZone(
  canvas: HTMLCanvasElement,
  descriptor: PreviewDescriptor | null,
  colors: Uint8Array | null,
  zoneId: string,
  opts: ZoneDrawOptions,
): void {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
  const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.clearRect(0, 0, w, h);

  if (!descriptor || !colors) return;
  const zone = descriptor.zones.find((z) => z.id === zoneId);
  if (!zone || zone.members.length === 0) return;

  let minX = Infinity,
    maxX = -Infinity,
    minY = Infinity,
    maxY = -Infinity;
  for (const p of zone.members) {
    const px = descriptor.pixels[p];
    if (!px) continue;
    minX = Math.min(minX, px.x);
    maxX = Math.max(maxX, px.x);
    minY = Math.min(minY, px.y);
    maxY = Math.max(maxY, px.y);
  }
  if (!isFinite(minX)) return;
  const spanX = Math.max(1e-3, maxX - minX);
  const spanY = Math.max(1e-3, maxY - minY);

  // Offscreen grid: one cell per matrix unit, capped to MAX_GRID per axis (bucketing down
  // for an unusually large matrix so the ImageData stays small).
  const gwRaw = Math.round(spanX) + 1;
  const ghRaw = Math.round(spanY) + 1;
  const bucket = Math.max(1, Math.ceil(Math.max(gwRaw, ghRaw) / MAX_GRID));
  const gw = Math.max(1, Math.ceil(gwRaw / bucket));
  const gh = Math.max(1, Math.ceil(ghRaw / bucket));

  const off = getOffscreen(gw, gh);
  if (!off) return;
  const offctx = off.getContext("2d");
  if (!offctx) return;

  // Scatter the pixel colours into a small ImageData — plain typed-array writes, NOT canvas
  // ops. A cell with no pixel stays transparent (alpha 0), so empty space reads as the
  // canvas background. Several pixels in one cell: last wins (a faithful-enough thumbnail;
  // the point here is cost, and it matches what a sub-sampled preview shows).
  const img = offctx.createImageData(gw, gh);
  const data = img.data;
  for (const p of zone.members) {
    const px = descriptor.pixels[p];
    if (!px) continue;
    const cx = Math.min(gw - 1, Math.floor((px.x - minX) / bucket));
    const cy = Math.min(gh - 1, Math.floor((px.y - minY) / bucket));
    const di = (cy * gw + cx) * 4;
    data[di] = colors[p * 3];
    data[di + 1] = colors[p * 3 + 1];
    data[di + 2] = colors[p * 3 + 2];
    data[di + 3] = 255;
  }
  offctx.putImageData(img, 0, 0);

  // Fit the grid into the canvas and blit it in one drawImage. Smoothing ON when shrinking
  // (a clean, faithful low-res that tracks the large preview), OFF when enlarging (crisp
  // pixels). Mini and large differ only by this scale — the same source image.
  const pad = opts.padCss * dpr;
  const s = Math.min((w - 2 * pad) / spanX, (h - 2 * pad) / spanY);
  const dispW = gw * bucket * s;
  const dispH = gh * bucket * s;
  const ox = (w - dispW) / 2;
  const oy = (h - dispH) / 2;
  ctx.imageSmoothingEnabled = s < 1;
  ctx.drawImage(off, 0, 0, gw, gh, ox, oy, dispW, dispH);
}

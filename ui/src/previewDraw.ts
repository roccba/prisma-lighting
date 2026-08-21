import type { PreviewDescriptor } from "./previewStream";

// Shared draw for a single zone's live pixels (M8 §3.1 mini-preview and §3.4 large
// preview). Both consume the SAME M6 stream and paint the SAME thing at different sizes —
// the zone's member pixels at their real matrix positions, filled with the colour the
// engine rendered. Mini and large are literally the SAME source image at different scales,
// so they cannot disagree about content.
//
// # Why one blit, not per-pixel fillRect
//
// The naive draw issues one `fillRect` per pixel/cell every frame. WebKitGTK's accelerated
// 2D-canvas backend DEFERS those operations and replays them (Skia `SkBigPicture`) when the
// page presents. Hundreds/thousands of rects × several previews × 30 Hz builds a deferred-op
// pile so large that flushing it blocks the WebProcess main thread — visible as a flicker,
// and, when it stalls past WebKit's 10 s watchdog, a SIGABRT crash (`flushDeferredOperations`
// → `crashAfter10Seconds`). The fix is to make the per-frame work a FIXED, tiny number of
// canvas ops regardless of pixel count: scatter the colours into a small `ImageData` (plain
// array writes — not canvas ops), `putImageData` it into a shared offscreen once, then
// `drawImage` that offscreen scaled to fit. Two canvas ops per zone per frame, not thousands.
//
// # Why the zone row draws through `drawZoneInto`
//
// A separate, subtler flicker came from the NUMBER of hot accelerated canvases: one big
// canvas (the large preview) is fine, but ~9 small per-card canvases thrash WebKitGTK's
// accelerated-canvas GL-context pool (recycling contexts ~1×/s → a ~1 s flash). The row
// therefore draws every zone's thumbnail into ONE canvas via `drawZoneInto` (each into its
// card's rectangle), matching the proven-fine single-canvas large preview. `drawZone` stays
// the self-contained path the large preview uses.

export interface ZoneDrawOptions {
  /** Fraction of the inter-pixel spacing a dot fills (0..1). Retained for API stability;
   *  the blit path does not draw dots. */
  dotScale: number;
  /** Hard cap on a dot's radius in CSS px, before dpr. Retained for API stability. */
  maxDotCss: number;
  /** Inner padding in CSS px, before dpr. */
  padCss: number;
}

/** A destination rectangle in DEVICE pixels on an already-sized, already-cleared canvas. */
export interface DestRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Cap on an offscreen grid axis. A matrix larger than this on an axis is bucketed down to
 *  it (rare — keeps the `ImageData` and the scatter loop bounded); the `drawImage` then
 *  scales the grid to the display size either way. */
const MAX_GRID = 160;

// One shared offscreen canvas, reused across every zone/card each frame. `drawZone` and
// `drawZoneInto` both run synchronously and never interleave mid-call, so a single scratch
// is safe: each build fills it and the following blit consumes it before the next build
// overwrites it. Grown as needed, never shrunk.
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

// Reused ImageData per grid size — NOT allocated per draw. Allocating a fresh ImageData
// every frame (≈9 minis + the large, 30 Hz, up to ~100 KB each) is tens of MB/s of
// garbage. Zones have stable grid sizes, so a small size-keyed cache reuses the buffers
// indefinitely; `fill(0)` clears a reused one (cheap, no allocation).
const imageDataCache = new Map<string, ImageData>();
function getImageData(gw: number, gh: number): ImageData {
  const key = `${gw}x${gh}`;
  let img = imageDataCache.get(key);
  if (img) {
    img.data.fill(0);
  } else {
    img = new ImageData(gw, gh); // zero-initialised
    imageDataCache.set(key, img);
  }
  return img;
}

interface ZoneImage {
  off: HTMLCanvasElement;
  gw: number;
  gh: number;
  bucket: number;
  spanX: number;
  spanY: number;
}

/** Scatter a zone's member colours into the shared offscreen and return the geometry the
 *  blit needs. Touches NO destination canvas. Returns null when the descriptor, colours or
 *  zone are missing/empty. */
function buildZoneImage(
  descriptor: PreviewDescriptor,
  colors: Uint8Array,
  zoneId: string,
): ZoneImage | null {
  const zone = descriptor.zones.find((z) => z.id === zoneId);
  if (!zone || zone.members.length === 0) return null;

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
  if (!isFinite(minX)) return null;
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
  if (!off) return null;
  const offctx = off.getContext("2d");
  if (!offctx) return null;

  // Scatter the pixel colours into a small ImageData — plain typed-array writes, NOT canvas
  // ops. A cell with no pixel stays transparent (alpha 0), so empty space reads as the
  // canvas background. Several pixels in one cell: last wins (a faithful-enough thumbnail).
  const img = getImageData(gw, gh);
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

  return { off, gw, gh, bucket, spanX, spanY };
}

/** Blit a built zone image into a device-px rectangle, centred, with `padDev` inner
 *  padding. Smoothing ON when shrinking (a clean low-res that tracks the large), OFF when
 *  enlarging (crisp pixels). The caller has already cleared the region. */
function blit(
  ctx: CanvasRenderingContext2D,
  zi: ZoneImage,
  dx: number,
  dy: number,
  dw: number,
  dh: number,
  padDev: number,
): void {
  const s = Math.min((dw - 2 * padDev) / zi.spanX, (dh - 2 * padDev) / zi.spanY);
  const dispW = zi.gw * zi.bucket * s;
  const dispH = zi.gh * zi.bucket * s;
  const ox = dx + (dw - dispW) / 2;
  const oy = dy + (dh - dispH) / 2;
  ctx.imageSmoothingEnabled = s < 1;
  ctx.drawImage(zi.off, 0, 0, zi.gw, zi.gh, ox, oy, dispW, dispH);
}

/** Self-contained draw of `zoneId` into its OWN `canvas` (the large preview): resizes the
 *  backing store to the element box (× dpr), clears it, and blits. A no-op (cleared canvas)
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
  const zi = buildZoneImage(descriptor, colors, zoneId);
  if (!zi) return;
  blit(ctx, zi, 0, 0, w, h, opts.padCss * dpr);
}

/** Draw `zoneId` into a sub-rectangle `dest` (device px) of a SHARED canvas the caller owns
 *  (the zone row's single overlay). Does NOT resize or clear the whole canvas — the caller
 *  clears once per frame and loops the zones — so many zones share one accelerated canvas. */
export function drawZoneInto(
  ctx: CanvasRenderingContext2D,
  descriptor: PreviewDescriptor | null,
  colors: Uint8Array | null,
  zoneId: string,
  dest: DestRect,
  opts: ZoneDrawOptions,
): void {
  if (!descriptor || !colors) return;
  const zi = buildZoneImage(descriptor, colors, zoneId);
  if (!zi) return;
  const dpr = window.devicePixelRatio || 1;
  blit(ctx, zi, dest.x, dest.y, dest.w, dest.h, opts.padCss * dpr);
}

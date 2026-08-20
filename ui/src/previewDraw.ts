import type { PreviewDescriptor } from "./previewStream";

// Shared draw for a single zone's live pixels (M8 §3.1 mini-preview and §3.4 large
// preview). Both consume the SAME M6 stream and paint the SAME thing at different
// sizes — the zone's member pixels at their real matrix positions, filled with the
// colour the engine rendered — so the draw lives here once instead of in each
// component. Keeping it in one place is why the large preview and a card's mini
// preview can never disagree about a zone's content (a flicker seen in one but not
// the other is then a size/artefact clue, not two code paths drifting).

export interface ZoneDrawOptions {
  /** Fraction of the inter-pixel spacing a dot fills (0..1). */
  dotScale: number;
  /** Hard cap on a dot's radius in CSS px, before dpr. */
  maxDotCss: number;
  /** Inner padding in CSS px, before dpr. */
  padCss: number;
}

/** Draw `zoneId`'s member pixels into `canvas` from the latest stream buffers.
 *  Resizes the backing store to the element's box (× dpr) and clears each call.
 *  A no-op (cleared canvas) when the descriptor, colours, or zone are missing. */
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
  const spanX = Math.max(1e-3, maxX - minX);
  const spanY = Math.max(1e-3, maxY - minY);
  const pad = opts.padCss * dpr;
  const s = Math.min((w - 2 * pad) / spanX, (h - 2 * pad) / spanY);
  const r = Math.max(1, Math.min(s * opts.dotScale, opts.maxDotCss * dpr));
  const ox = (w - s * spanX) / 2 - minX * s;
  const oy = (h - s * spanY) / 2 - minY * s;

  // SQUARE dots, always (a pixel is square) via `fillRect` — no per-pixel path
  // tessellation like `arc`. Uniform across small and large rigs, just scaled.
  const cap = Math.max(128, Math.floor((w * h) / 40));

  if (zone.members.length <= cap) {
    // Few enough pixels to draw each at its real position.
    const d = 2 * r;
    for (const p of zone.members) {
      const px = descriptor.pixels[p];
      if (!px) continue;
      ctx.fillStyle = `rgb(${colors[p * 3]},${colors[p * 3 + 1]},${colors[p * 3 + 2]})`;
      ctx.fillRect(ox + px.x * s - r, oy + px.y * s - r, d, d);
    }
    return;
  }

  // Too many pixels for this canvas: DOWN-SCALE spatially into a coarse grid and fill one
  // TILED cell per occupied bucket. This looks like a lower-resolution version of the same
  // picture (clean blocks) — NOT the diagonal striping a linear "draw every Nth pixel"
  // subsample produced (that skips through row-major order and beats against the row width).
  // The large preview's canvas is big enough that `cap` exceeds the pixel count, so it never
  // takes this path and stays full-resolution.
  const bs = Math.max(1, Math.round(Math.sqrt((spanX * spanY) / cap)) || 1); // bucket, matrix units
  const bw = Math.floor(spanX / bs) + 1;
  const bh = Math.floor(spanY / bs) + 1;
  const drawn = new Uint8Array(bw * bh);
  const cell = Math.ceil(bs * s) + 1; // +1 so tiles overlap by a hair — no seams/gaps
  for (const p of zone.members) {
    const px = descriptor.pixels[p];
    if (!px) continue;
    const bx = Math.floor((px.x - minX) / bs);
    const by = Math.floor((px.y - minY) / bs);
    const k = by * bw + bx;
    if (drawn[k]) continue; // first pixel in the bucket is its representative colour
    drawn[k] = 1;
    ctx.fillStyle = `rgb(${colors[p * 3]},${colors[p * 3 + 1]},${colors[p * 3 + 2]})`;
    ctx.fillRect(ox + (minX + bx * bs) * s, oy + (minY + by * bs) * s, cell, cell);
  }
}

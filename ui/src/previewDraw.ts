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

  for (const p of zone.members) {
    const px = descriptor.pixels[p];
    if (!px) continue;
    ctx.beginPath();
    ctx.arc(ox + px.x * s, oy + px.y * s, r, 0, Math.PI * 2);
    ctx.fillStyle = `rgb(${colors[p * 3]},${colors[p * 3 + 1]},${colors[p * 3 + 2]})`;
    ctx.fill();
  }
}

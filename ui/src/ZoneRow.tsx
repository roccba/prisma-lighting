import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import { ZoneCard } from "./ZoneCard";
import { drawZoneInto } from "./previewDraw";
import { usePreviewStream } from "./previewStream";
import type { ZoneInfo } from "./api";

// The zone row (M8-SPEC §3.1): every zone at once, horizontally, so the operator sees the
// whole venue at a glance. Selecting a card is a UI state (which zone the grid / preview /
// parameters show) — it never changes what a zone outputs (selected ≠ active, §3.2).
//
// # One canvas for every mini-preview (flicker fix)
//
// The mini-previews are NOT a `<canvas>` per card. WebKitGTK renders one big accelerated
// canvas (the large preview) perfectly, but ~9 small hot canvases thrash its accelerated-
// canvas GL-context pool and flash ~1×/s. So the row owns ONE overlay canvas covering all
// the cards and draws each zone's thumbnail into that card's preview rectangle — the same
// single-accelerated-canvas shape that is proven flicker-free. The canvas is
// `pointer-events:none` and sits above the cards; each card's controls stay live DOM below
// it, and its preview slot is just an empty framed screen the canvas paints into. Because
// the canvas is an absolutely-positioned child of the scrolling row, it scrolls WITH the
// cards, and each card's rect (measured relative to the canvas) is scroll-invariant.

interface ZoneRowProps {
  zones: ZoneInfo[];
  selected: string | null;
  onSelect: (id: string) => void;
}

// Mini thumbnail draw options (were inline in the old per-card ZoneMiniPreview).
const MINI_OPTS = { dotScale: 0.42, maxDotCss: 6, padCss: 6 };

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export function ZoneRow({ zones, selected, onSelect }: ZoneRowProps) {
  const rowRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Each card's preview-slot element, keyed by zone id, registered by the card via a ref.
  const previewEls = useRef(new Map<string, HTMLElement>());
  // Cached per-zone destination rects in CSS px, relative to the canvas (scroll-invariant).
  const rects = useRef(new Map<string, Rect>());
  const stream = usePreviewStream();

  const registerPreview = useCallback((id: string, el: HTMLElement | null) => {
    if (el) previewEls.current.set(id, el);
    else previewEls.current.delete(id);
  }, []);

  // Size the overlay canvas to the row's scroll content and recompute each card's rect. Run
  // on mount, when the zone set changes, and on any resize — NOT per frame (rects are stable
  // while the layout is, and getBoundingClientRect forces layout, so we avoid it in the draw).
  const layout = useCallback(() => {
    const row = rowRef.current;
    const canvas = canvasRef.current;
    if (!row || !canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const cssW = row.scrollWidth;
    const cssH = row.clientHeight;
    canvas.style.width = `${cssW}px`;
    canvas.style.height = `${cssH}px`;
    const bw = Math.max(1, Math.round(cssW * dpr));
    const bh = Math.max(1, Math.round(cssH * dpr));
    if (canvas.width !== bw) canvas.width = bw;
    if (canvas.height !== bh) canvas.height = bh;
    const cr = canvas.getBoundingClientRect();
    const next = new Map<string, Rect>();
    for (const [id, el] of previewEls.current) {
      const r = el.getBoundingClientRect();
      next.set(id, { x: r.left - cr.left, y: r.top - cr.top, w: r.width, h: r.height });
    }
    rects.current = next;
  }, []);

  // Recompute layout after cards mount/unmount (refs registered during commit, before this).
  useLayoutEffect(() => {
    layout();
  }, [zones, layout]);

  // Keep rects in sync with size changes (window resize, panel resize, dpr moves).
  useEffect(() => {
    const row = rowRef.current;
    if (!row) return;
    const ro = new ResizeObserver(() => layout());
    ro.observe(row);
    const onResize = () => layout();
    window.addEventListener("resize", onResize);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", onResize);
    };
  }, [layout]);

  // The single per-frame draw: clear the whole overlay once, then blit every zone's
  // thumbnail into its cached rect. Driven by the shared 30 Hz stream, never by React.
  useEffect(() => {
    const draw = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      const dpr = window.devicePixelRatio || 1;
      const desc = stream.descriptorRef.current;
      const cols = stream.colorsRef.current;
      for (const [id, r] of rects.current) {
        drawZoneInto(
          ctx,
          desc,
          cols,
          id,
          { x: r.x * dpr, y: r.y * dpr, w: r.w * dpr, h: r.h * dpr },
          MINI_OPTS,
        );
      }
    };
    const unsub = stream.subscribe(draw);
    draw();
    return unsub;
  }, [stream]);

  return (
    <div className="pr-zonerow" ref={rowRef}>
      <canvas className="pr-zonerow-canvas" ref={canvasRef} />
      {zones.length === 0 ? (
        <div className="pr-zonerow-empty">No zones in the show</div>
      ) : (
        zones.map((z) => (
          <ZoneCard
            key={z.id}
            zone={z}
            selected={z.id === selected}
            onSelect={() => onSelect(z.id)}
            registerPreview={registerPreview}
          />
        ))
      )}
    </div>
  );
}

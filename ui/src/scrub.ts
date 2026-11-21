import type { PointerEvent as ReactPointerEvent } from "react";

// console-style "scrub": press on a value box and drag UP to raise it / DOWN to lower it,
// as if you had grabbed its fader. A press that does not move is an ordinary click, so the
// box can still be focused and typed into. Shared by every numeric box (colour channels,
// effect parameter values, the −/+ number fields, the master readout) so they all behave
// the same.

/** Pixels of vertical travel before a press becomes a drag (below it, it is a click). */
const THRESHOLD = 3;

export interface ScrubOptions {
  /** The value when the press starts. */
  get: () => number;
  /** Called with each new (already clamped) value while dragging. */
  set: (v: number) => void;
  min: number;
  max: number;
  /** Value change per pixel of vertical travel (up = increase). */
  perPixel: number;
  /** Round to this step (e.g. 1 for integers). Omit for continuous. */
  step?: number;
}

/** `onPointerDown` handler that turns a vertical drag on the element into value changes. */
export function scrubHandler(opts: () => ScrubOptions | null) {
  return (e: ReactPointerEvent<HTMLElement>) => {
    if (e.button !== 0) return;
    const o = opts();
    if (!o) return;
    const el = e.currentTarget;
    const startY = e.clientY;
    const start = o.get();
    let dragging = false;

    const move = (ev: PointerEvent) => {
      const dy = startY - ev.clientY;
      if (!dragging) {
        if (Math.abs(dy) < THRESHOLD) return;
        dragging = true;
        try {
          el.setPointerCapture(ev.pointerId);
        } catch {
          /* pointer already gone */
        }
        // A drag is not an edit: drop focus and any text selection it started.
        if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
        window.getSelection()?.removeAllRanges();
        document.body.classList.add("is-scrubbing");
      }
      ev.preventDefault();
      let v = start + dy * o.perPixel;
      if (o.step) v = Math.round(v / o.step) * o.step;
      v = Math.max(o.min, Math.min(o.max, v));
      o.set(v);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      document.body.classList.remove("is-scrubbing");
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };
}

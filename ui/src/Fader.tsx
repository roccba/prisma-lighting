import { useRef, useState } from "react";

// The signature horizontal fader (DESIGN.md §4 "one knob to rule them all"), built on
// the design-system `.pr-hslider` / `.pr-knob` primitives. Pointer-drag along the rail
// sets a 0..1 value; the knob and blue fill track it, and the grabbed state draws the
// accent outline (`.is-grabbed`). Ticks every fifth of the range (major at the ends).

interface FaderProps {
  value: number; // 0..1
  onChange: (v: number) => void;
  /** Optional: called on grab / release, e.g. to pause polling while dragging. */
  onGrabbedChange?: (grabbed: boolean) => void;
  "aria-label"?: string;
}

const TICKS = 11; // 10 segments; major at both ends
/** Knob width in px — keep in sync with `.pr-hslider` `--pr-knob-w` in theme.css. */
const KNOB_W = 30;

export function Fader({ value, onChange, onGrabbedChange, ...rest }: FaderProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const [grabbed, setGrabbed] = useState(false);
  const v = Math.max(0, Math.min(1, value));
  // Constrain the knob's travel to WITHIN the rail (like VFader): at v=0 its left edge is
  // at 0, at v=1 its right edge is at the rail's end. Without this it overhung each end by
  // half its width and covered the value box beside the fader. Slot, fill and ticks are
  // inset by half a knob so the knob's centre always sits on the fill's end.
  const travel = `${v.toFixed(4)} * (100% - var(--pr-knob-w))`;

  const valueFromEvent = (clientX: number): number => {
    const rail = railRef.current;
    if (!rail) return v;
    const rect = rail.getBoundingClientRect();
    // The knob's CENTRE travels between half a knob in from each end (see the knob style
    // below), so the pointer maps onto that same span — the knob lands under the cursor.
    const half = KNOB_W / 2;
    const x = clientX - rect.left - half;
    const span = Math.max(1, rect.width - KNOB_W);
    return Math.max(0, Math.min(1, x / span));
  };

  const onPointerDown = (e: React.PointerEvent) => {
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    setGrabbed(true);
    onGrabbedChange?.(true);
    onChange(valueFromEvent(e.clientX));
    e.preventDefault();
  };

  const onPointerMove = (e: React.PointerEvent) => {
    if (!grabbed) return;
    onChange(valueFromEvent(e.clientX));
  };

  const endDrag = (e: React.PointerEvent) => {
    if (!grabbed) return;
    setGrabbed(false);
    onGrabbedChange?.(false);
    try {
      (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
    } catch {
      /* not captured */
    }
  };

  // Mouse wheel nudges the value (up = raise). Fine step with Shift. preventDefault so
  // the wheel adjusts the fader instead of scrolling the container it sits in (the zone
  // row / params panel both scroll horizontally). React wheel handlers are cancelable.
  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const step = e.shiftKey ? 0.01 : 0.03;
    const dir = e.deltaY < 0 ? 1 : -1;
    onChange(Math.max(0, Math.min(1, v + dir * step)));
  };

  return (
    <div
      className="pr-hslider"
      ref={railRef}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onWheel={onWheel}
      role="slider"
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={v}
      aria-label={rest["aria-label"]}
    >
      <div className="slot" />
      <div className="fill" style={{ width: `calc(${travel})` }} />
      <div className="ticks">
        {Array.from({ length: TICKS }, (_, i) => (
          <i key={i} className={i === 0 || i === TICKS - 1 ? "major" : ""} />
        ))}
      </div>
      <div className={`pr-knob${grabbed ? " is-grabbed" : ""}`} style={{ left: `calc(${travel})` }}>
        <span className="ridge" />
        <span className="ridge" />
        <span className="ridge" />
      </div>
    </div>
  );
}

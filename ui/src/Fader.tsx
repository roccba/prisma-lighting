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

export function Fader({ value, onChange, onGrabbedChange, ...rest }: FaderProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const [grabbed, setGrabbed] = useState(false);
  const v = Math.max(0, Math.min(1, value));
  const pct = `${v * 100}%`;

  const valueFromEvent = (clientX: number): number => {
    const rail = railRef.current;
    if (!rail) return v;
    const rect = rail.getBoundingClientRect();
    const pad = 4; // matches .pr-hslider padding
    const x = clientX - rect.left - pad;
    const span = Math.max(1, rect.width - 2 * pad);
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

  return (
    <div
      className="pr-hslider"
      ref={railRef}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      role="slider"
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={v}
      aria-label={rest["aria-label"]}
    >
      <div className="slot" />
      <div className="fill" style={{ width: pct }} />
      <div className="ticks">
        {Array.from({ length: TICKS }, (_, i) => (
          <i key={i} className={i === 0 || i === TICKS - 1 ? "major" : ""} />
        ))}
      </div>
      <div className={`pr-knob${grabbed ? " is-grabbed" : ""}`} style={{ left: `calc(${pct} - 15px)` }}>
        <span className="ridge" />
        <span className="ridge" />
        <span className="ridge" />
      </div>
    </div>
  );
}

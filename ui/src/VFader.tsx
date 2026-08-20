import { useRef, useState } from "react";

// The vertical console fader (DESIGN.md §4 "one knob"), on the design-system `.pr-fader`
// primitive. Pointer-drag along the rail sets a 0..1 value (top = 1, bottom = 0); the
// knob and blue fill track it. In `bipolar` mode the fill grows from the CENTRE (0.5),
// so a professional consoles-style velocity fader reads its zero at the middle and its sign by which way
// the fill runs. The value is always 0..1 here; the caller maps it to a real range.

interface VFaderProps {
  value: number; // 0..1
  onChange: (v: number) => void;
  /** Fill from the centre instead of the bottom (for bipolar controls like velocity). */
  bipolar?: boolean;
  onGrabbedChange?: (grabbed: boolean) => void;
  "aria-label"?: string;
}

const TICKS = 11;

export function VFader({ value, onChange, bipolar, onGrabbedChange, ...rest }: VFaderProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const [grabbed, setGrabbed] = useState(false);
  const v = Math.max(0, Math.min(1, value));

  const valueFromEvent = (clientY: number): number => {
    const rail = railRef.current;
    if (!rail) return v;
    const rect = rail.getBoundingClientRect();
    const pad = 4; // matches .pr-fader tick inset
    const y = clientY - rect.top - pad;
    const span = Math.max(1, rect.height - 2 * pad);
    return Math.max(0, Math.min(1, 1 - y / span)); // top = 1
  };

  const onPointerDown = (e: React.PointerEvent) => {
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    setGrabbed(true);
    onGrabbedChange?.(true);
    onChange(valueFromEvent(e.clientY));
    e.preventDefault();
  };
  const onPointerMove = (e: React.PointerEvent) => {
    if (!grabbed) return;
    onChange(valueFromEvent(e.clientY));
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
  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const step = e.shiftKey ? 0.01 : 0.03;
    const dir = e.deltaY < 0 ? 1 : -1;
    onChange(Math.max(0, Math.min(1, v + dir * step)));
  };

  // Fill geometry: from the bottom normally; from the centre in bipolar mode.
  const fillStyle = bipolar
    ? v >= 0.5
      ? { bottom: "50%", height: `${(v - 0.5) * 100}%` }
      : { bottom: `${v * 100}%`, height: `${(0.5 - v) * 100}%` }
    : { bottom: 0, height: `${v * 100}%` };
  // Constrain the knob's travel to WITHIN the rail: at v=1 its top sits at 0, at v=0 at
  // (rail height − knob height). Without this the knob overhangs the top by half its
  // height and covers whatever sits above the fader (the zone name). `--pr-knob-h` is the
  // knob's height, set alongside the knob's size in CSS.
  const knobTop = `calc(${(1 - v).toFixed(4)} * (100% - var(--pr-knob-h, 30px)))`;

  return (
    <div
      className={`pr-fader${bipolar ? " is-bipolar" : ""}`}
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
      {bipolar && <div className="pr-fader-center" />}
      <div className="fill" style={fillStyle} />
      <div className="ticks">
        {Array.from({ length: TICKS }, (_, i) => (
          <i key={i} className={i === 0 || i === TICKS - 1 || (bipolar && i === (TICKS - 1) / 2) ? "major" : ""} />
        ))}
      </div>
      <div className={`pr-knob${grabbed ? " is-grabbed" : ""}`} style={{ top: knobTop }}>
        <span className="ridge" />
        <span className="ridge" />
        <span className="ridge" />
      </div>
    </div>
  );
}

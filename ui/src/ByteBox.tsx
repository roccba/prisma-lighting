import { useEffect, useState } from "react";
import { scrubHandler } from "./scrub";

// A 0–255 value box (the classic channel field), shared by the colour control and the Fader
// Box. Only digits are accepted and the value is clamped to 255 as you type (no leading
// zeros, no letters); it can be cleared to type over, and an empty box reverts on blur.
// Drag up/down on it to scrub the value like a fader. `tint` draws the coloured top
// border (R/G/B…); `disabled` greys it out (a channel the target does not have).

interface ByteBoxProps {
  label: string;
  value: number;
  onChange?: (v: number) => void;
  tint?: string;
  disabled?: boolean;
  className?: string;
}

export function ByteBox({ label, value, onChange, tint, disabled, className }: ByteBoxProps) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);

  return (
    <label
      className={`pr-color-box${disabled ? " is-off" : ""}${className ? ` ${className}` : ""}`}
      style={tint ? { borderTopColor: tint } : undefined}
      title={`${label} 0–255`}
    >
      <input
        value={draft}
        inputMode="numeric"
        maxLength={3}
        disabled={disabled}
        onChange={(e) => {
          const digits = e.target.value.replace(/\D/g, "");
          if (digits === "") {
            setDraft("");
            return;
          }
          const v = Math.min(255, parseInt(digits, 10));
          setDraft(String(v));
          onChange?.(v);
        }}
        onBlur={() => setDraft(String(value))}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        onPointerDown={scrubHandler(() =>
          disabled || !onChange ? null : { get: () => value, set: onChange, min: 0, max: 255, perPixel: 1, step: 1 },
        )}
        aria-label={label}
      />
    </label>
  );
}

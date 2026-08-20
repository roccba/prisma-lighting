import { useEffect, useState } from "react";

// Numeric field on the design-system `.pr-num` primitive with the stacked `.pr-spin`
// up/down buttons. Replaces native <input type="number"> across the app (matrix size,
// placement counts/offsets, addressing). Pure presentation: it emits the same numeric
// value the native input would, clamped to [min, max] on commit.
//
// A local text draft lets the user clear the field and type freely; the committed value
// (onChange) only fires on a parseable number, on blur, and on the spin buttons.

interface NumberFieldProps {
  value: number;
  onChange: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  width?: number; // input width in px (default matches the theme's 54px)
  disabled?: boolean;
  "aria-label"?: string;
}

export function NumberField({
  value,
  onChange,
  min,
  max,
  step = 1,
  width,
  disabled,
  ...rest
}: NumberFieldProps) {
  const [draft, setDraft] = useState(String(value));

  // Keep the draft in sync when the value changes from outside (not while editing an
  // equivalent value — avoids clobbering "1." mid-type).
  useEffect(() => {
    if (parseFloat(draft) !== value) setDraft(String(value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);

  const clamp = (n: number) => {
    if (min != null && n < min) n = min;
    if (max != null && n > max) n = max;
    return n;
  };

  const commit = (raw: string) => {
    const n = parseFloat(raw);
    if (Number.isNaN(n)) {
      setDraft(String(value)); // revert to last good value
      return;
    }
    const c = clamp(n);
    setDraft(String(c));
    if (c !== value) onChange(c);
  };

  const bump = (dir: 1 | -1) => {
    const base = Number.isNaN(parseFloat(draft)) ? value : parseFloat(draft);
    const c = clamp(base + dir * step);
    setDraft(String(c));
    if (c !== value) onChange(c);
  };

  return (
    <div className="pr-num" aria-label={rest["aria-label"]}>
      <input
        value={draft}
        disabled={disabled}
        inputMode="decimal"
        style={width != null ? { width } : undefined}
        onChange={(e) => {
          setDraft(e.target.value);
          const n = parseFloat(e.target.value);
          if (!Number.isNaN(n) && String(n) === e.target.value.trim()) onChange(clamp(n));
        }}
        onBlur={(e) => commit(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit((e.target as HTMLInputElement).value);
          else if (e.key === "ArrowUp") {
            e.preventDefault();
            bump(1);
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            bump(-1);
          }
        }}
      />
      <span className="pr-spin">
        <button type="button" tabIndex={-1} disabled={disabled} onClick={() => bump(1)}>
          ▲
        </button>
        <button type="button" tabIndex={-1} disabled={disabled} onClick={() => bump(-1)}>
          ▼
        </button>
      </span>
    </div>
  );
}

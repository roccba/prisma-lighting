import { useEffect, useRef, useState } from "react";

// Custom dropdown on the design-system `.pr-dd` primitive (head + caret + list +
// options). Replaces every native <select> so the control matches the theme in both
// the editor and the main window. Pure presentation: it emits the same string value a
// <select> would. An optional per-option `swatch` (a colour) shows as a small chip in
// the head and the option row — used by the editor zone selector.

export interface DropdownOption {
  value: string;
  label: string;
  swatch?: string; // optional colour chip (data value → inline background is fine)
}

interface DropdownProps {
  value: string;
  options: DropdownOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  minWidth?: number;
  "aria-label"?: string;
}

function Swatch({ color }: { color: string }) {
  return (
    <span
      style={{
        width: 10,
        height: 10,
        flex: "0 0 auto",
        borderRadius: "var(--r-sm)",
        border: "1px solid var(--btn-border-dark)",
        background: color,
      }}
    />
  );
}

export function Dropdown({ value, options, onChange, disabled, minWidth, ...rest }: DropdownProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const selected = options.find((o) => o.value === value);

  // Close on outside click (mirrors the design-system reference behaviour).
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
  };

  return (
    <div
      ref={rootRef}
      className={`pr-dd${open ? " is-open" : ""}`}
      style={minWidth != null ? { minWidth } : undefined}
      aria-label={rest["aria-label"]}
    >
      <div
        className="pr-dd-head"
        onClick={() => !disabled && setOpen((o) => !o)}
        style={disabled ? { opacity: 0.5, cursor: "default" } : undefined}
        role="button"
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span style={{ display: "flex", alignItems: "center", gap: 6, overflow: "hidden" }}>
          {selected?.swatch && <Swatch color={selected.swatch} />}
          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {selected?.label ?? ""}
          </span>
        </span>
        <span className="pr-dd-caret">▼</span>
      </div>
      {open && (
        <div className="pr-dd-list" role="listbox">
          {options.map((o) => (
            <div
              key={o.value}
              className={`pr-dd-opt${o.value === value ? " is-sel" : ""}`}
              onClick={() => pick(o.value)}
              role="option"
              aria-selected={o.value === value}
              style={o.swatch ? { display: "flex", alignItems: "center", gap: 6 } : undefined}
            >
              {o.swatch && <Swatch color={o.swatch} />}
              {o.label}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

import type { ReactNode } from "react";

// Checkbox on the design-system `.pr-chk` primitive (box + tick + label). Replaces
// native <input type="checkbox">. Pure presentation: emits the new boolean on click.

interface CheckProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  children?: ReactNode; // label
  disabled?: boolean;
}

export function Check({ checked, onChange, children, disabled }: CheckProps) {
  return (
    <div
      className={`pr-chk${checked ? " is-on" : ""}`}
      onClick={() => !disabled && onChange(!checked)}
      role="checkbox"
      aria-checked={checked}
      aria-disabled={disabled || undefined}
      style={disabled ? { opacity: 0.5, cursor: "default" } : undefined}
    >
      <span className="box">
        <span className="tick">✔</span>
      </span>
      {children}
    </div>
  );
}

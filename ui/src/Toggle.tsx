// Toggle switch on the design-system `.pr-tgl` primitive (track + sliding knob).
// A compact on/off for booleans where a switch reads better than a checkbox (device
// toggles, live-preview on/off). Pure presentation: emits the new boolean on click.

interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  "aria-label"?: string;
}

export function Toggle({ checked, onChange, disabled, ...rest }: ToggleProps) {
  return (
    <div
      className={`pr-tgl${checked ? " is-on" : ""}`}
      onClick={() => !disabled && onChange(!checked)}
      role="switch"
      aria-checked={checked}
      aria-disabled={disabled || undefined}
      aria-label={rest["aria-label"]}
      style={disabled ? { opacity: 0.5, cursor: "default" } : undefined}
    >
      <span className="knob" />
    </div>
  );
}

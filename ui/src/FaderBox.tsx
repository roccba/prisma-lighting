import type { ReactNode } from "react";
import { ByteBox } from "./ByteBox";
import { FloatWindow } from "./FloatWindow";
import { VFader } from "./VFader";

// "Caja de Fader" — the classic Fader Box: a floating window with one vertical fader per
// channel (label on top, colour chip + 0–255 value box below) and an optional preview bar.
// GENERIC on purpose: it is used wherever a set of 0–255 channels needs faders (the colour
// effect first; more places later). A disabled channel shows greyed out (a channel the
// target does not have, e.g. W on an RGB colour).
//
// It is a floating, non-blocking, FIXED-SIZE window (no backdrop, no resize): it opens
// where it was clicked from, and the operator keeps using the rest of the console while
// it is open. Drag it by its title bar.

export interface FaderChannel {
  label: string;
  /** Colour of the chip under the fader. */
  chip: string;
  /** 0–255. */
  value: number;
  onChange?: (v: number) => void;
  disabled?: boolean;
}

interface FaderBoxProps {
  title?: string;
  channels: FaderChannel[];
  /** Optional full-width preview bar (e.g. the resulting colour). */
  preview?: ReactNode;
  /** Where it was opened from (the click point): it appears there, kept on screen. */
  at?: { x: number; y: number };
  onClose: () => void;
}

export function FaderBox({ title = "Caja de Fader", channels, preview, at, onClose }: FaderBoxProps) {
  return (
    <FloatWindow title={title} at={at} className="pr-faderbox" onClose={onClose}>
      <div className="pr-faderbox-body">
        <div className="pr-faderbox-chans">
          {channels.map((c) => (
            <div key={c.label} className={`pr-faderbox-chan${c.disabled ? " is-off" : ""}`}>
              <span className="pr-faderbox-lbl">{c.label}</span>
              <div className="pr-faderbox-fader">
                <VFader
                  value={c.value / 255}
                  onChange={(v) => !c.disabled && c.onChange?.(Math.round(v * 255))}
                  aria-label={c.label}
                />
              </div>
              <span className="pr-faderbox-chip" style={{ background: c.chip }} />
              <ByteBox label={c.label} value={c.value} onChange={c.onChange} disabled={c.disabled} className="pr-faderbox-val" />
            </div>
          ))}
        </div>
        {preview && <div className="pr-faderbox-preview">{preview}</div>}
        <button type="button" className="pr-btn pr-faderbox-close" onClick={onClose}>
          Cerrar
        </button>
      </div>
    </FloatWindow>
  );
}

// Density (Compacta / Normal / Grande): the ONLY thing that sizes the track grid's cells,
// its header and footer, the parameter panel and the master (LIVE-MODEL-SPEC §10.1). Window
// size never shrinks or stretches them; it only changes how many whole rows of the grid
// are shown. Remembered per machine (localStorage), never in the show.

export type Density = "compact" | "normal" | "large";

export interface DensitySizes {
  /** Clip row height. */
  row: number;
  /** Minimum track column width. */
  col: number;
  /** Track header height. */
  head: number;
  /** Opacity fader height in the track footer. */
  fader: number;
  /** Track footer height (stop + fader + value + M/S/sel). */
  foot: number;
  /** Height of the bottom row: the parameter panel and the master beside it. */
  panel: number;
  /** A parameter slider's height in the panel (its knob, slot and ticks scale with it). */
  slider: number;
  /** A parameter value field's height in the panel. */
  field: number;
  /** The master fader's height. */
  masterFader: number;
}

export const DENSITIES: Record<Density, DensitySizes> = {
  compact: { row: 34, col: 90, head: 46, fader: 44, foot: 132, panel: 150, slider: 28, field: 22, masterFader: 56 },
  normal: { row: 52, col: 104, head: 58, fader: 72, foot: 160, panel: 200, slider: 40, field: 26, masterFader: 72 },
  large: { row: 68, col: 128, head: 58, fader: 96, foot: 184, panel: 264, slider: 48, field: 30, masterFader: 136 },
};

export const DENSITY_LABEL: Record<Density, string> = {
  compact: "Compacta",
  normal: "Normal",
  large: "Grande",
};

/** Gap between grid rows and columns, px (must match `.pr-tgrid` gap in theme.css). */
export const GRID_GAP = 5;

const KEY = "prisma-grid-density";

export function loadDensity(): Density {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "compact" || v === "normal" || v === "large") return v;
  } catch {
    /* storage may be unavailable */
  }
  return "normal";
}

export function saveDensity(d: Density) {
  try {
    localStorage.setItem(KEY, d);
  } catch {
    /* storage may be unavailable */
  }
}

/** The CSS variables a density sets on the main window. */
export function densityVars(d: Density): Record<string, string> {
  const s = DENSITIES[d];
  return {
    "--tgrid-row": `${s.row}px`,
    "--tgrid-col": `${s.col}px`,
    "--tgrid-head": `${s.head}px`,
    "--tfoot-fader": `${s.fader}px`,
    "--tfoot-h": `${s.foot}px`,
    "--workrow-h": `${s.panel}px`,
    // Panel slider geometry, all inside its box: knob 5 px from the top and bottom, the
    // slot centred, the ticks under the slot.
    "--pslider-h": `${s.slider}px`,
    "--pslider-knob": `${s.slider - 10}px`,
    "--pslider-slot": `${s.slider / 2 - 3}px`,
    "--pslider-ticks": `${s.slider / 2 + 5}px`,
    "--pslider-major": `${Math.min(10, s.slider / 2 - 5)}px`,
    "--pfield-h": `${s.field}px`,
    "--master-fader": `${s.masterFader}px`,
  };
}

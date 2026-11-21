import { createContext, useContext } from "react";
import { setPalette, setParam } from "./api";

// The scene-grid cell the effect panel is editing (the grid's selection — which may not be
// the zone's running cell), and HOW to write to it. Every parameter / colour-table control
// inside the panel writes through `usePanelWrites`, never straight to the API:
//  · an assigned cell: the write goes to THAT cell — live when it is the running one,
//    stored-only otherwise (selected ≠ active);
//  · an EMPTY cell shown as a draft: the panel's own writers first assign the cell (with
//    the draft effect) and only then write — an empty cell stays empty until it is edited.
// Outside the panel (no provider) writes go to the zone's running cell.
export interface PanelTarget {
  /** The cell being edited; null = the zone's running cell. */
  cell: number | null;
  /** Panel-supplied parameter writer (normalized value). */
  writeParam?: (name: string, value: number) => void;
  /** Panel-supplied colour-table writer (linear-light RGB list). */
  writePalette?: (colors: [number, number, number][]) => void;
}

export const PanelCellContext = createContext<PanelTarget>({ cell: null });

/** The writers a panel control uses for `zoneId`'s edited cell. */
export function usePanelWrites(zoneId: string) {
  const t = useContext(PanelCellContext);
  return {
    param: (name: string, value: number) =>
      t.writeParam ? t.writeParam(name, value) : void setParam(zoneId, name, value, t.cell).catch(() => {}),
    palette: (colors: [number, number, number][]) =>
      t.writePalette ? t.writePalette(colors) : void setPalette(zoneId, colors, t.cell).catch(() => {}),
  };
}

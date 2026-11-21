import { createContext } from "react";

// The scene-grid cell the effect panel is editing (the grid's selection — which may not be
// the zone's running cell). Every parameter / colour-table control inside the panel writes
// to THIS cell: live when it is the running one, stored-only otherwise (selected ≠ active).
// `null` (outside the panel) = the zone's running cell.
export const PanelCellContext = createContext<number | null>(null);

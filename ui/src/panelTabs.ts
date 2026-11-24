// The bottom panel's tabs (LIVE-MODEL-SPEC §8.5 step 4), in order: Alt+1…Alt+5 pick them
// (configurable in Preferencias → Control, `ui:tab/{n}`). Each phase adds its own tab here
// (Colores, Cues, Timeline later); a tab exists only once it has something to show.
export type PanelTabId = "params" | "groups";

export const PANEL_TABS: { id: PanelTabId; label: string }[] = [
  { id: "params", label: "Parámetros" },
  { id: "groups", label: "Grupos" },
];

/** How many tab shortcuts the keyboard offers (Rust `keys::PANEL_TAB_KEYS`). */
export const PANEL_TAB_KEYS = 5;

const KEY = "prisma-panel-tab";

/** The active tab, remembered per machine. */
export function loadPanelTab(): PanelTabId {
  try {
    const v = localStorage.getItem(KEY);
    if (PANEL_TABS.some((t) => t.id === v)) return v as PanelTabId;
  } catch {
    /* storage may be unavailable */
  }
  return "params";
}

export function savePanelTab(id: PanelTabId) {
  try {
    localStorage.setItem(KEY, id);
  } catch {
    /* storage may be unavailable */
  }
}

/** The tab a `ui:tab/{n}` shortcut picks, if there is one. */
export function tabOfShortcut(path: string): PanelTabId | null {
  const m = /^ui:tab\/(\d+)$/.exec(path);
  if (!m) return null;
  return PANEL_TABS[Number(m[1]) - 1]?.id ?? null;
}

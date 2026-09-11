// Make the Tauri webview behave like a native desktop program, not a web page.
//
// Both windows (main operation window + patch editor) load the same entry, so installing
// this once from `main.tsx` covers the whole app. The goal: right-click, text selection,
// drag-ghosts, browser zoom and browser-only shortcuts (reload / find / print / view-source)
// must not betray that the UI is HTML. Any right-click the app *wants* is a feature a
// component adds explicitly — never the browser's menu.
//
// App-owned shortcuts (Ctrl+S/O/N, Ctrl+Alt+…, the F-keys the menus use) are handled inside
// the windows and are deliberately NOT intercepted here; the keys blocked below are only the
// browser's own, none of which the app binds.

/** Is the event target a real editable field (where selection/caret/editing belong)? */
function isEditable(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.isContentEditable)
  );
}

export function installDesktopShell(): void {
  // Right-click never opens the browser context menu. A component that wants its own menu
  // handles `contextmenu` on its element and calls `stopPropagation()` before it reaches
  // here, so this is the default, not an obstacle.
  document.addEventListener("contextmenu", (e) => e.preventDefault());

  // No drag-ghost from images/text/links. The app's real drags (fixtures, faders, splitter)
  // are pointer-based, so this never touches them.
  document.addEventListener("dragstart", (e) => {
    if (!isEditable(e.target)) e.preventDefault();
  });

  // Belt-and-suspenders with the CSS `user-select: none`: no stray selection outside inputs.
  document.addEventListener("selectstart", (e) => {
    if (!isEditable(e.target)) e.preventDefault();
  });

  // Browser zoom via Ctrl/⌘ + wheel (trackpad pinch is reported as ctrl+wheel too). Must be
  // a non-passive listener or `preventDefault` is ignored. The only zoom in the app is the
  // editor canvas grid, which handles a plain (non-ctrl) wheel itself.
  window.addEventListener(
    "wheel",
    (e) => {
      if (e.ctrlKey) e.preventDefault();
    },
    { passive: false },
  );

  // Browser-only keyboard shortcuts. None of these are app bindings (the app uses Ctrl+S/O/N,
  // Ctrl+Alt+…, and F3/F4/F7–F11), so swallowing them changes nothing the operator relies on.
  window.addEventListener("keydown", (e) => {
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    // Zoom (Ctrl +/-/0), reload (F5 / Ctrl+R), find (Ctrl+F / Ctrl+G), print (Ctrl+P),
    // view-source (Ctrl+U).
    if (
      (mod && ["=", "-", "+", "0"].includes(e.key)) ||
      e.key === "F5" ||
      (mod && (k === "r" || k === "f" || k === "g" || k === "p" || k === "u"))
    ) {
      e.preventDefault();
    }
  });
}

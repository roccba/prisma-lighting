import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// The design system (docs/DESIGN.md) — incorporated for M8, NOT yet applied to the
// editor. Imported BEFORE styles.css on purpose: styles.css loads last and wins the
// cascade, so the editor's existing tokens (its own `--accent`, `--text`) and component
// classes are unchanged. theme.css only *adds* the `.pr-*` classes + design tokens M8
// will build on. (Its global base — body font, webkit scrollbars — does reach the
// document; the app background and every component keep their current look.)
import "./theme.css";
import "./styles.css";
import { installDesktopShell } from "./desktopShell";

// Make the webview read as a native program (no browser menu/zoom/selection/shortcuts)
// before anything renders. Covers both windows — they share this entry.
installDesktopShell();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

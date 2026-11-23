// The mocked Tauri IPC for the Playwright harness (LIVE-MODEL-SPEC §10.1). Reads answer
// from the demo-show fixture that the app generates (`ui_fixture_is_current` in
// app/src/main.rs); writes are recorded in `window.__calls` and answered with nothing, so a
// test can assert what the UI sent. Events (`listen` / `emit`) are mocked too.
import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import fixture from "./fixtures/demo.json";

declare global {
  interface Window {
    __calls: { cmd: string; args: unknown }[];
  }
}

type Fixture = Record<string, unknown> & {
  main_effect_panel: Record<string, unknown>;
  empty_panel: Record<string, unknown>;
};
const fx = fixture as unknown as Fixture;

window.__calls = [];
mockWindows("main");
mockIPC(
  (cmd, args) => {
    window.__calls.push({ cmd, args });
    const a = (args ?? {}) as Record<string, unknown>;
    if (cmd === "plugin:window|is_maximized") return true;
    if (cmd === "main_effect_panel") {
      const cell = a.cell ?? null;
      return (
        fx.main_effect_panel[`${a.track}:${cell}`] ?? { ...fx.empty_panel, track_id: a.track, cell_index: cell }
      );
    }
    if (cmd in fx) return fx[cmd];
    return null;
  },
  { shouldMockEvents: true },
);

// The live preview's WebSocket: inert (never opens, never errors), so the previews stay
// black and still for screenshots and nothing reconnects in a loop.
class InertWebSocket extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = 0;
  binaryType = "arraybuffer";
  onopen: unknown = null;
  onmessage: unknown = null;
  onclose: unknown = null;
  onerror: unknown = null;
  constructor(public url: string) {
    super();
  }
  send() {}
  close() {
    this.readyState = 3;
  }
}
window.WebSocket = InertWebSocket as unknown as typeof WebSocket;

// The app emits `prisma://show-main` once the window is shown; the main window reveals
// itself on it (and on a stable size). Do the same here so a loaded or reloaded page is
// always revealed, however slowly frames come in a busy browser.
window.addEventListener("load", () => {
  setTimeout(() => emit("prisma://show-main").catch(() => {}), 50);
});

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
// The live fixture, for a test to set up a state (an armed clip…) before rendering.
(window as unknown as { __fixture: Fixture }).__fixture = fx;
// What the engine emits, for a test to play an event from elsewhere (a controller, a key).
(window as unknown as { __emit: typeof emit }).__emit = emit;

// --- Writes the UI tests read back. Kept to what the app does with them: a stored
// parameter changes the cell's panel values and, for a Color cell, its effect colour (the
// app computes it from the current r / g / b, linear); a label colour is stored as is.
interface GridCell {
  index: number;
  effect_id: string | null;
  color: number[] | null;
  effect_color: number[] | null;
}
interface GridTrack {
  id: string;
  active: number | null;
  clips: GridCell[];
}
interface PanelParam {
  name: string;
  value_norm: number;
}
function gridTrack(track: string): GridTrack | undefined {
  return (fx.main_grid as { tracks: GridTrack[] }).tracks.find((t) => t.id === track);
}
function setParamLike(track: string, cell: number | null, name: string, value: number) {
  const t = gridTrack(track);
  const index = cell ?? t?.active ?? null;
  if (!t || index === null) return null;
  for (const key of [`${track}:${index}`, ...(t.active === index ? [`${track}:null`] : [])]) {
    const panel = fx.main_effect_panel[key] as { params: PanelParam[] } | undefined;
    panel?.params.forEach((p) => {
      if (p.name === name) p.value_norm = value;
    });
  }
  const c = t.clips[index];
  const channel = ["r", "g", "b"].indexOf(name);
  if (c && c.effect_id === "solid" && channel >= 0) {
    c.effect_color = [...(c.effect_color ?? [0, 0, 0])];
    c.effect_color[channel] = value;
  }
  return null;
}
function setCellColorLike(track: string, index: number, color: number[]) {
  const c = gridTrack(track)?.clips[index];
  if (c) c.color = color;
  return gridTrack(track)?.clips ?? [];
}
// Group edits in the editor (LIVE-MODEL-SPEC §8.5 step 1), on the fixture's patch, with the
// same rules as the app: "Grupo n" with the smallest free n, members distinct.
interface MockGroup {
  id: string;
  name: string;
  members: string[];
  display_color: number[];
}
let groupSeq = 0;
function editorState() {
  return fx.editor_get_state as { patch: { groups?: MockGroup[] }; can_undo: boolean };
}
function groups(): MockGroup[] {
  const p = editorState().patch;
  p.groups ??= [];
  return p.groups;
}
function groupEdit(cmd: string, a: Record<string, unknown>): unknown {
  const gs = groups();
  const g = gs.find((x) => x.id === a.group);
  if (cmd === "editor_add_group") {
    let n = 1;
    while (gs.some((x) => x.name.toLowerCase() === `grupo ${n}`)) n++;
    const id = `00000000-0000-4000-8000-${String(++groupSeq).padStart(12, "0")}`;
    gs.push({ id, name: `Grupo ${n}`, members: [...new Set(a.members as string[])], display_color: [69, 123, 157] });
    editorState().can_undo = true;
    return [structuredClone(fx.editor_get_state), id];
  }
  if (!g) throw new Error(`group ${String(a.group)} not found`);
  if (cmd === "editor_rename_group") {
    const name = (a.name as string).trim();
    if (!name) throw new Error("El nombre no puede estar vacío.");
    if (gs.some((x) => x !== g && x.name.toLowerCase() === name.toLowerCase())) {
      throw new Error(`Ya existe un grupo llamado «${name}».`);
    }
    g.name = name;
  } else if (cmd === "editor_set_group_members") {
    g.members = a.members as string[];
  } else if (cmd === "editor_set_group_color") {
    g.display_color = a.color as number[];
  } else if (cmd === "editor_remove_group") {
    gs.splice(gs.indexOf(g), 1);
  }
  editorState().can_undo = true;
  return structuredClone(fx.editor_get_state);
}

// `?window=editor` opens the patch editor instead of the main window.
mockWindows(new URLSearchParams(location.search).get("window") ?? "main");
mockIPC(
  (cmd, args) => {
    window.__calls.push({ cmd, args });
    const a = (args ?? {}) as Record<string, unknown>;
    if (cmd === "plugin:window|is_maximized") return true;
    if (cmd === "main_set_param") return setParamLike(a.track as string, a.cell as number | null, a.name as string, a.value as number);
    if (cmd === "main_set_cell_color") return setCellColorLike(a.track as string, a.index as number, a.color as number[]);
    if (cmd === "main_rename_scene") {
      const scene = (fx.main_grid as { scenes: { name: string | null }[] }).scenes[a.row as number];
      if (scene) scene.name = (a.name as string).trim() || null;
      return null;
    }
    // The engine reports every fired scene (any transport): the UI flashes it.
    if (cmd === "main_launch_scene") {
      setTimeout(() => emit("scene-fired", a.row).catch(() => {}), 0);
      return null;
    }
    if (cmd.startsWith("editor_") && cmd.includes("_group")) return groupEdit(cmd, a);
    if (cmd === "main_effect_panel") {
      const cell = a.cell ?? null;
      return structuredClone(
        fx.main_effect_panel[`${a.track}:${cell}`] ?? { ...fx.empty_panel, track_id: a.track, cell_index: cell },
      );
    }
    // A fresh copy, like the app's JSON: the UI must see a new object after a write.
    if (cmd in fx) return structuredClone(fx[cmd]);
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

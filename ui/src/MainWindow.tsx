import { useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { mainZones, newShow, openEditor, openShow, saveShow, type ZoneInfo } from "./api";
import { DeviceManager } from "./DeviceManager";
import { MenuBar, type MenuDef } from "./MenuBar";
import { EffectParams } from "./EffectParams";
import { GlobalBar } from "./GlobalBar";
import { LargePreview } from "./LargePreview";
import { PreviewStreamProvider } from "./previewStream";
import { SceneGrid } from "./SceneGrid";
import { ZoneRow } from "./ZoneRow";

// The M8 main operation window (M8-SPEC §3). Step 3 builds the FIXED layout skeleton
// and the populated zone row; the scene grid, large preview, parameter panel and
// global operation bar are placeholders whose regions are laid out here and filled in
// steps 4–6. Each region is a self-contained panel that does not assume its neighbours
// (§7), so the later dashboard milestone can make them dockable without a rewrite.
//
// The window owns the ONE piece of cross-region state step 3 needs: the *selected*
// zone (a UI state — which zone the grid/preview/params will show). Selecting never
// changes output (selected ≠ active, §3.2); only the zone cards' faders / solo / mute
// do that.

const PRISMA_FILTERS = [{ name: "Prisma show", extensions: ["prisma"] }];

export default function MainWindow() {
  const [zones, setZones] = useState<ZoneInfo[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Whether the device-manager modal (network output) is open.
  const [deviceMgrOpen, setDeviceMgrOpen] = useState(false);
  // Colour theme (dark default), persisted per-viewer and applied as data-theme on <html>.
  const [theme, setTheme] = useState<"dark" | "light">(() => {
    try {
      return localStorage.getItem("prisma-theme") === "light" ? "light" : "dark";
    } catch {
      return "dark";
    }
  });
  // Transient toast for menu entries whose feature is not built yet.
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef<number | undefined>(undefined);
  // Bumped whenever the selected zone's active cell changes (fire / assign / clear), so
  // the effect parameter panel re-fetches the now-active effect and its values. The
  // large preview follows the stream on its own and needs no signal.
  const [gridRev, setGridRev] = useState(0);

  const refreshZones = () =>
    mainZones()
      .then((zs) => {
        setZones(zs);
        // Keep the selection if it still exists; otherwise fall back to the first zone.
        setSelected((cur) => (cur && zs.some((z) => z.id === cur) ? cur : (zs[0]?.id ?? null)));
      })
      .catch((e) => setError(String(e)));

  useEffect(() => {
    // Once the first zone load resolves the operation window is ready to be shown, so tell
    // the boot splash it can hand over (`prisma://ready` → the splash fades and calls
    // close_splash, which reveals this window). `finally` so a load error still hands over
    // rather than stranding the splash; the splash also has its own safety cap.
    refreshZones().finally(() => {
      emit("prisma://ready").catch(() => {});
    });
  }, []);

  // The patch editor (secondary window) applies its edits to the running show on close
  // and emits `layout-changed` (§7). The zone set / names / fixtures may have changed,
  // so re-fetch the row and refresh the grid/params.
  useEffect(() => {
    const un = listen("layout-changed", () => {
      refreshZones();
      setGridRev((r) => r + 1);
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  // Open a `.prisma` show: the backend recompiles the engine onto it and emits
  // `layout-changed`, which our listener above turns into a zone/grid refresh.
  const doOpenShow = async () => {
    try {
      const path = await openDialog({ multiple: false, filters: PRISMA_FILTERS });
      if (typeof path === "string") await openShow(path);
    } catch (e) {
      setError(String(e));
    }
  };

  // Save the live show (with its embedded patch) to a `.prisma` file.
  const doSaveShow = async () => {
    try {
      const path = await saveDialog({ filters: PRISMA_FILTERS, defaultPath: "untitled.prisma" });
      if (path) await saveShow(path);
    } catch (e) {
      setError(String(e));
    }
  };

  // Apply + persist the theme whenever it changes.
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("prisma-theme", theme);
    } catch {
      /* storage may be unavailable (private mode, etc.) */
    }
  }, [theme]);

  const showNotice = (msg: string) => {
    setNotice(msg);
    window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(null), 2500);
  };
  // Placeholder for menu entries whose feature is not implemented yet.
  const soon = () => showNotice("Función no disponible todavía");

  const toggleFullscreen = async () => {
    const w = getCurrentWindow();
    try {
      await w.setFullscreen(!(await w.isFullscreen()));
    } catch {
      /* ignore */
    }
  };
  const doExit = () => {
    getCurrentWindow()
      .close()
      .catch(() => {});
  };
  // New configuration: a clean show over the current rig (clears all effects).
  const doNewShow = async () => {
    try {
      await newShow();
    } catch (e) {
      setError(String(e));
    }
  };

  // Global keyboard shortcuts. A ref holds the latest action closures so the listener
  // subscribes once and never runs a stale one.
  const actions = useRef({ doNewShow, doOpenShow, doSaveShow, toggleFullscreen, soon });
  actions.current = { doNewShow, doOpenShow, doSaveShow, toggleFullscreen, soon };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const k = e.key;
      const ctrlOnly = e.ctrlKey && !e.altKey && !e.metaKey;
      if (ctrlOnly && (k === "o" || k === "O")) {
        e.preventDefault();
        actions.current.doOpenShow();
      } else if (ctrlOnly && (k === "s" || k === "S")) {
        e.preventDefault();
        actions.current.doSaveShow();
      } else if (ctrlOnly && (k === "n" || k === "N")) {
        e.preventDefault();
        actions.current.doNewShow();
      } else if (e.ctrlKey && e.altKey && (k === "c" || k === "C")) {
        e.preventDefault();
        actions.current.soon();
      } else if (k === "F3") {
        e.preventDefault();
        openEditor().catch(() => {});
      } else if (k === "F4") {
        e.preventDefault();
        setDeviceMgrOpen(true);
      } else if (k === "F11") {
        e.preventDefault();
        actions.current.toggleFullscreen();
      } else if (k === "F7" || k === "F8" || k === "F9" || k === "F10") {
        e.preventDefault();
        actions.current.soon();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const selectedZone = zones.find((z) => z.id === selected) ?? null;

  const menus: MenuDef[] = [
    {
      label: "Archivo",
      children: [
        { kind: "action", label: "Nueva configuración", shortcut: "Ctrl+N", onSelect: doNewShow },
        { kind: "action", label: "Abrir configuración…", shortcut: "Ctrl+O", onSelect: doOpenShow },
        { kind: "action", label: "Guardar configuración…", shortcut: "Ctrl+S", onSelect: doSaveShow },
        { kind: "divider" },
        {
          kind: "submenu",
          label: "Importar",
          children: [{ kind: "action", label: "Próximamente", disabled: true }],
        },
        {
          kind: "submenu",
          label: "Exportar",
          children: [{ kind: "action", label: "Próximamente", disabled: true }],
        },
        { kind: "divider" },
        { kind: "action", label: "Salir", shortcut: "Alt+F4", onSelect: doExit },
      ],
    },
    {
      label: "Vista",
      children: [
        { kind: "action", label: "Tocar", shortcut: "F8", onSelect: soon },
        {
          kind: "submenu",
          label: "Tema",
          children: [
            { kind: "action", label: "Claro", checked: theme === "light", onSelect: () => setTheme("light") },
            { kind: "action", label: "Oscuro", checked: theme === "dark", onSelect: () => setTheme("dark") },
          ],
        },
        { kind: "divider" },
        { kind: "action", label: "Pantalla completa", shortcut: "F11", onSelect: toggleFullscreen },
      ],
    },
    {
      label: "Preferencias",
      children: [
        { kind: "action", label: "Editor de patch", shortcut: "F3", onSelect: () => openEditor().catch(() => {}) },
        { kind: "action", label: "Gestor de dispositivos", shortcut: "F4", onSelect: () => setDeviceMgrOpen(true) },
        { kind: "divider" },
        {
          kind: "submenu",
          label: "Control remoto",
          children: [
            { kind: "action", label: "HTTP", onSelect: soon },
            { kind: "action", label: "OSC", onSelect: soon },
          ],
        },
        { kind: "action", label: "Opciones…", onSelect: soon },
      ],
    },
    {
      label: "Herramientas",
      children: [
        { kind: "action", label: "Colores", shortcut: "Ctrl+Alt+C", onSelect: soon },
        { kind: "action", label: "Grupos", shortcut: "F9", onSelect: soon },
        { kind: "action", label: "Listas de señales", shortcut: "F7", onSelect: soon },
        { kind: "action", label: "Líneas de tiempo", shortcut: "F10", onSelect: soon },
        { kind: "divider" },
        { kind: "action", label: "Inspector DMX", shortcut: "Ctrl+Alt+D", onSelect: soon },
        { kind: "action", label: "Inspector MIDI", shortcut: "Ctrl+Alt+M", onSelect: soon },
      ],
    },
    {
      label: "Ayuda",
      children: [{ kind: "action", label: "Acerca de Prisma", onSelect: () => showNotice("Prisma 0.1.0") }],
    },
  ];

  return (
    <PreviewStreamProvider>
      <div className="pr-mainwin">
        <MenuBar menus={menus} />

        <div className="pr-mainwin-body">
          <section className="pr-region pr-region-zonerow">
            <div className="pr-region-title">Zonas</div>
            <ZoneRow zones={zones} selected={selected} onSelect={setSelected} />
          </section>

          <div className="pr-mainwin-mid">
            <section className="pr-region pr-region-grid">
              <div className="pr-region-title">
                Grilla{selectedZone ? ` — ${selectedZone.name}` : ""}
              </div>
              {/* No `key={zone.id}` — keying would REMOUNT the grid on every zone switch
                  (unmount → blank → re-fetch), which is why the grid "disappeared" and felt
                  slow. SceneGrid re-fetches in place on a zoneId change (its `useEffect`),
                  so it stays mounted and just swaps its cells. */}
              {selectedZone ? (
                <SceneGrid
                  zoneId={selectedZone.id}
                  onActiveChange={() => setGridRev((r) => r + 1)}
                />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
            <section className="pr-region pr-region-preview">
              <div className="pr-region-title">
                Preview{selectedZone ? ` — ${selectedZone.name}` : ""}
              </div>
              {selectedZone ? (
                <LargePreview zoneId={selectedZone.id} />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
          </div>

          <section className="pr-region pr-region-params">
            <div className="pr-region-title">
              Parámetros{selectedZone ? ` — ${selectedZone.name}` : ""}
            </div>
            {selectedZone ? (
              <EffectParams zoneId={selectedZone.id} rev={gridRev} />
            ) : (
              <div className="pr-placeholder-body">Seleccioná una zona</div>
            )}
          </section>

          <section className="pr-region pr-region-global">
            <GlobalBar />
          </section>
        </div>

        {error && <div className="pr-mainwin-error">Error: {error}</div>}
        {notice && <div className="pr-toast">{notice}</div>}
        {deviceMgrOpen && <DeviceManager onClose={() => setDeviceMgrOpen(false)} />}
      </div>
    </PreviewStreamProvider>
  );
}

import { useCallback, useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  armClear,
  armGo,
  clockInfo,
  mainZones,
  newShow,
  openEditor,
  openShow,
  recoveryInfo,
  saveShow,
  setBpm,
  tap,
  type RecoveryInfo,
  type ZoneInfo,
} from "./api";
import { DeviceManager } from "./DeviceManager";
import { MenuBar, type MenuDef } from "./MenuBar";
import { EffectParams } from "./EffectParams";
import { GlobalBar } from "./GlobalBar";
import { LargePreview } from "./LargePreview";
import { PreviewStreamProvider } from "./previewStream";
import { TrackGrid } from "./TrackGrid";
import { loadPaneHeight, savePaneHeight, Splitter } from "./Splitter";
import { scrubHandler } from "./scrub";
import { ZoneRow } from "./ZoneRow";
import { MidiLearnProvider, useMidiLearn } from "./midiLearn";

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

/** The bottom work row's height: default (px), floor, and where it is remembered. */
const WORKROW_DEFAULT = 200;
const WORKROW_MIN = 120;
const WORKROW_KEY = "prisma-workrow-h";

export default function MainWindow() {
  return (
    <MidiLearnProvider>
      <MainWindowBody />
    </MidiLearnProvider>
  );
}

function MainWindowBody() {
  const learn = useMidiLearn();
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
  // Boot recovery (autosave): shown until dismissed, with a way back to the last save.
  const [recovery, setRecovery] = useState<RecoveryInfo | null>(null);
  useEffect(() => {
    recoveryInfo()
      .then((r) => setRecovery(r))
      .catch(() => {});
  }, []);
  // Transient toast for menu entries whose feature is not built yet.
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef<number | undefined>(undefined);
  // Bumped whenever the selected zone's active cell changes (fire / assign / clear), so
  // the effect parameter panel re-fetches the now-active effect and its values. The
  // large preview follows the stream on its own and needs no signal.
  const [gridRev, setGridRev] = useState(0);
  // Stable, so the grid's `cells-changed` subscription is not torn down every render.
  const bumpGrid = useCallback(() => setGridRev((r) => r + 1), []);
  // The SELECTED track and the selected clip of each track (track id → clip index) — UI
  // state only: they pick what the parameter panel edits and what its GO plays, and never
  // change output by themselves (selected ≠ active). A track with no entry shows its running
  // clip. Selecting a track also points the large preview at its zone.
  const [selectedTrack, setSelectedTrack] = useState<string | null>(null);
  const [selectedCells, setSelectedCells] = useState<Record<string, number>>({});
  const onSelectTrack = useCallback((track: string, cell: number | null, zone: string | null) => {
    setSelectedTrack(track);
    if (cell !== null) setSelectedCells((m) => ({ ...m, [track]: cell }));
    if (zone) setSelected(zone);
  }, []);
  // The bottom work row's height (effect parameters | master): the operator drags the
  // splitter above it; small by default so the grid gets the room. Persisted per viewer.
  const [workrowH, setWorkrowH] = useState(() => loadPaneHeight(WORKROW_KEY, WORKROW_DEFAULT));
  const changeWorkrowH = (h: number) => {
    setWorkrowH(h);
    savePaneHeight(WORKROW_KEY, h);
  };
  // The global "Modo armar" (LIVE-MODEL-SPEC §7): while on, every play gesture arms.
  const [armMode, setArmMode] = useState(false);
  // Whether any clip is armed: shows "Desarmar" beside GO, and makes Esc disarm all.
  const [anyArmed, setAnyArmed] = useState(false);
  const armedRef = useRef(false);
  armedRef.current = anyArmed;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !armedRef.current || e.defaultPrevented) return;
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)) return;
      armClear().catch(() => {});
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const refreshZones = () =>
    mainZones()
      .then((zs) => {
        setZones(zs);
        // Keep the selection if it still exists; otherwise fall back to the first zone.
        setSelected((cur) => (cur && zs.some((z) => z.id === cur) ? cur : (zs[0]?.id ?? null)));
      })
      .catch((e) => setError(String(e)));

  // Boot handover (see `show_main` in main.rs): the window is shown — behind the splash —
  // only when everything is loaded, and the window manager then brings it to its final
  // (maximized) size over a few frames. The console stays invisible until that size has
  // settled, so the operator never sees it laid out at an intermediate size; then it
  // appears and tells the splash to go (`prisma://shown`). A timeout reveals it anyway.
  const [revealed, setRevealed] = useState(false);
  useEffect(() => {
    let done = false;
    let raf = 0;
    let last = "";
    let stableFrames = 0;
    const reveal = () => {
      if (done) return;
      done = true;
      cancelAnimationFrame(raf);
      setRevealed(true);
      // Next frame, so the revealed console has been painted before the splash leaves.
      requestAnimationFrame(() => emit("prisma://shown").catch(() => {}));
    };
    // rAF does not run while the window is hidden, so this starts counting on show.
    const tick = () => {
      const size = `${window.innerWidth}x${window.innerHeight}`;
      stableFrames = size === last && window.innerWidth > 0 ? stableFrames + 1 : 0;
      last = size;
      if (stableFrames >= 3) {
        getCurrentWindow()
          .isMaximized()
          .then((max) => {
            if (max) reveal();
            else raf = requestAnimationFrame(tick);
          })
          .catch(reveal);
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    // Fallback: a window manager that never maximizes (or a tiling one) must not leave
    // the console invisible — once shown, reveal after a short grace period regardless.
    const un = listen("prisma://show-main", () => setTimeout(reveal, 1200));
    return () => {
      done = true;
      cancelAnimationFrame(raf);
      un.then((f) => f()).catch(() => {});
    };
  }, []);

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
      setSelectedCells({}); // a new show / edited rig: old cell indices mean nothing
      setGridRev((r) => r + 1);
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  // A mapped MIDI control moved a live value (M9): re-read the zone row so its faders follow
  // the hardware. The backend already coalesces these to ≤ 20/s with a trailing emit.
  useEffect(() => {
    const un = listen("midi-applied", () => {
      mainZones()
        .then(setZones)
        .catch(() => {});
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  // Follow MIDI focus (M9): when on, touching a hardware control that drives a zone selects
  // that zone, so the screen follows the operator's hands. Selection only — never output
  // (selected ≠ active). The backend emits `midi-focus` once per gesture, not per message.
  const [followFocus, setFollowFocus] = useState(() => {
    try {
      return localStorage.getItem("prisma-midi-focus") === "on";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem("prisma-midi-focus", followFocus ? "on" : "off");
    } catch {
      /* storage may be unavailable */
    }
  }, [followFocus]);
  const focusRef = useRef({ followFocus, zones });
  focusRef.current = { followFocus, zones };
  useEffect(() => {
    const un = listen<number>("midi-focus", (e) => {
      const { followFocus: on, zones: zs } = focusRef.current;
      const zone = on ? zs[e.payload] : undefined;
      if (zone) setSelected(zone.id);
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

  // If applying the editor's edits to the live show fails (e.g. on editor close), the
  // backend keeps the previous layout running (fail-safe) and emits this — surface it so
  // the operator knows their edits did NOT take, instead of the change silently vanishing.
  useEffect(() => {
    const un = listen<string>("editor-apply-failed", (e) => {
      setError(`No se pudieron aplicar los cambios del editor: ${e.payload}`);
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);
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
  const toggleLearn = () => learn.setActive(!learn.active);
  const actions = useRef({ doNewShow, doOpenShow, doSaveShow, toggleFullscreen, soon, toggleLearn });
  actions.current = { doNewShow, doOpenShow, doSaveShow, toggleFullscreen, soon, toggleLearn };
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
      } else if (ctrlOnly && (k === "m" || k === "M")) {
        e.preventDefault();
        actions.current.toggleLearn();
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
  const selectedCell = selectedTrack ? (selectedCells[selectedTrack] ?? null) : null;

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
        { kind: "action", label: "MIDI Learn", shortcut: "Ctrl+M", checked: learn.active, onSelect: toggleLearn },
        {
          kind: "action",
          label: "Seguir foco MIDI",
          checked: followFocus,
          onSelect: () => setFollowFocus((v) => !v),
        },
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
      <div className="pr-mainwin" style={revealed ? undefined : { visibility: "hidden" }}>
        <MenuBar
          menus={menus}
          right={
            <>
              {learn.active && (
                <button
                  className="pr-learn-ind"
                  type="button"
                  onClick={() => learn.setActive(false)}
                  title="Salir de MIDI Learn (Esc)"
                >
                  <span className="pr-learn-bar-dot" />
                  MIDI Learn
                </button>
              )}
              <TransportBar armMode={armMode} onArmMode={setArmMode} anyArmed={anyArmed} />
            </>
          }
        />

        <div className="pr-mainwin-body">
          <section className="pr-region pr-region-zonerow">
            <ZoneRow zones={zones} selected={selected} onSelect={setSelected} />
          </section>

          <div className="pr-mainwin-mid">
            <section className="pr-region pr-region-grid">
              <TrackGrid
                zones={zones}
                selectedTrack={selectedTrack}
                selectedCells={selectedCells}
                onSelect={onSelectTrack}
                armMode={armMode}
                rev={gridRev}
                onChanged={bumpGrid}
                onArmedChange={setAnyArmed}
              />
            </section>
            <section className="pr-region pr-region-preview">
              {selectedZone ? (
                <LargePreview zoneId={selectedZone.id} />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
          </div>

          {/* Bottom work row — FIXED size (no resize): the effect parameters on the left
              (a swappable slot: groups will share it later) and the master on the right. */}
          <Splitter
            value={workrowH}
            onChange={changeWorkrowH}
            min={WORKROW_MIN}
            max={Math.max(WORKROW_MIN, Math.round(window.innerHeight * 0.6))}
            invert
          />
          <div className="pr-workrow" style={{ height: workrowH }}>
            <section className="pr-region pr-workrow-panel">
              {selectedTrack ? (
                <EffectParams trackId={selectedTrack} cell={selectedCell} rev={gridRev} onGridChange={bumpGrid} />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una celda</div>
              )}
            </section>

            <section className="pr-region pr-workrow-master">
              <GlobalBar />
            </section>
          </div>
        </div>

        {error && <div className="pr-mainwin-error">Error: {error}</div>}
        {notice && <div className="pr-toast">{notice}</div>}
        {recovery && (
          <div className="pr-recovery" role="status">
            <span>Sesión recuperada</span>
            {recovery.last_saved && (
              <button
                type="button"
                className="pr-btn"
                onClick={() => {
                  const path = recovery.last_saved;
                  setRecovery(null);
                  if (path) openShow(path).catch((e) => setError(String(e)));
                }}
                title={recovery.last_saved}
              >
                Abrir el último guardado
              </button>
            )}
            <button type="button" className="pr-btn" onClick={() => setRecovery(null)} aria-label="Cerrar">
              ×
            </button>
          </div>
        )}
        {deviceMgrOpen && <DeviceManager onClose={() => setDeviceMgrOpen(false)} zones={zones} />}
      </div>
    </PreviewStreamProvider>
  );
}

// --- The global bar (LIVE-MODEL-SPEC §7): tempo, tap, arm mode, GO ----------------------

function TransportBar({
  armMode,
  onArmMode,
  anyArmed,
}: {
  armMode: boolean;
  onArmMode: (on: boolean) => void;
  anyArmed: boolean;
}) {
  const [bpm, setBpmState] = useState(120);
  const read = () =>
    clockInfo()
      .then((c) => setBpmState(c.bpm))
      .catch(() => {});
  useEffect(() => {
    read();
    // A recompile keeps the tempo; a MIDI tap may change it.
    const subs = [listen("layout-changed", read), listen("midi-applied", read)];
    return () => {
      subs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, []);
  const change = (v: number) => {
    const b = Math.round(Math.max(20, Math.min(300, v)) * 10) / 10;
    setBpmState(b);
    setBpm(b).catch(() => {});
  };
  return (
    <div className="pr-gbar">
      <span className="pr-gbar-label">BPM</span>
      <span
        className="pr-gbar-bpm"
        title="Arrastrá para cambiar el tempo"
        onPointerDown={scrubHandler(() => ({ get: () => bpm, set: change, min: 20, max: 300, perPixel: 0.5, step: 0.1 }))}
      >
        {bpm.toFixed(1)}
      </span>
      <button
        type="button"
        className="pr-btn"
        onClick={() => {
          tap().catch(() => {});
          // The clock applies the tap on its next frame; read the result back then.
          window.setTimeout(read, 80);
        }}
        title="Tap tempo"
      >
        Tap
      </button>
      <button
        type="button"
        className={`pr-btn pr-gbar-arm${armMode ? " is-active" : ""}`}
        onClick={() => onArmMode(!armMode)}
        title="Modo armar: tocar una celda o una escena la arma en lugar de lanzarla"
      >
        Modo armar
      </button>
      {anyArmed && (
        <button type="button" className="pr-btn" onClick={() => armClear().catch(() => {})} title="Desarmar todo (Esc)">
          Desarmar
        </button>
      )}
      <button type="button" className="pr-btn pr-go-btn" onClick={() => armGo().catch(() => {})} title="Lanzar todo lo armado">
        GO
      </button>
    </div>
  );
}

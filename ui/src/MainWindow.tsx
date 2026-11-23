import { useCallback, useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  armClear,
  armGo,
  clockInfo,
  controlKeys,
  mainKey,
  mainSelect,
  mainZones,
  newShow,
  openDemoShow,
  openEditor,
  openShow,
  mainLastSaved,
  recoveryInfo,
  saveShow,
  setBpm,
  tap,
  type KeyBinding,
  type ZoneInfo,
} from "./api";
import { ControlDialog } from "./ControlDialog";
import { comboOf, focusKind, isBound, isModifierOnly, isReserved } from "./keyboard";
import { DeviceManager } from "./DeviceManager";
import { MenuBar, type MenuDef } from "./MenuBar";
import { EffectParams } from "./EffectParams";
import { GlobalBar } from "./GlobalBar";
import { LargePreview } from "./LargePreview";
import { PreviewStreamProvider } from "./previewStream";
import { TrackGrid } from "./TrackGrid";
import { DENSITY_LABEL, densityVars, loadDensity, saveDensity, type Density } from "./density";
import { scrubHandler } from "./scrub";
import { ZoneRow } from "./ZoneRow";
import { LearnTarget, MidiLearnProvider, useMidiLearn } from "./midiLearn";

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

/** Where "parameter panel hidden" is remembered (per machine). */
const PANEL_HIDDEN_KEY = "prisma-panel-hidden";

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
  // Notices and errors go to the status line in the menu bar (never a floating box over
  // the controls) and clear themselves after a few seconds.
  const [status, setStatus] = useState<{ text: string; error: boolean } | null>(null);
  const statusTimer = useRef<number | undefined>(undefined);
  const showStatus = useCallback((text: string, error = false, ms?: number) => {
    setStatus({ text, error });
    window.clearTimeout(statusTimer.current);
    statusTimer.current = window.setTimeout(() => setStatus(null), ms ?? (error ? 8000 : 4000));
  }, []);
  const setError = useCallback((msg: string) => showStatus(`Error: ${msg}`, true), [showStatus]);
  // The last opened / saved show, for Archivo → "Abrir el último guardado".
  const [lastSaved, setLastSaved] = useState<string | null>(null);
  const refreshLastSaved = useCallback(() => {
    mainLastSaved()
      .then(setLastSaved)
      .catch(() => {});
  }, []);
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
  // Boot recovery (autosave): said once in the status line.
  useEffect(() => {
    refreshLastSaved();
    recoveryInfo()
      .then((r) => {
        if (r)
          showStatus(
            r.last_saved ? "Sesión recuperada · Archivo → Abrir el último guardado para volver a lo guardado" : "Sesión recuperada",
            false,
            8000,
          );
      })
      .catch(() => {});
  }, [refreshLastSaved, showStatus]);
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
  // The selection also lives in the engine (`/select/track`, `/select/clip`) so the keyboard
  // and a controller move the same one and the knobs follow it: a click here sends it; a
  // change from elsewhere arrives as `selection-changed` (handled in the grid, which knows
  // each track's zone) with `fromEngine` set, and is only shown, never sent back.
  const [selectedTrack, setSelectedTrack] = useState<string | null>(null);
  const [selectedCells, setSelectedCells] = useState<Record<string, number>>({});
  const onSelectTrack = useCallback(
    (track: string, cell: number | null, zone: string | null, fromEngine = false) => {
      setSelectedTrack(track);
      setSelectedCells((m) => {
        if (cell !== null) return { ...m, [track]: cell };
        if (!fromEngine || !(track in m)) return m;
        // The engine selected the track's running clip: forget the remembered one.
        const next = { ...m };
        delete next[track];
        return next;
      });
      if (zone) setSelected(zone);
      if (!fromEngine) mainSelect(track, cell).catch(() => {});
    },
    [],
  );
  const selectionRef = useRef({ selectedTrack, selectedCells });
  selectionRef.current = { selectedTrack, selectedCells };
  // Layout is fixed: the density (Compacta / Normal / Grande) sets the size of the grid's
  // cells and footer and the parameter panel's height; the grid takes the rest and shows
  // the whole rows that fit. The panel can be hidden (button, Tab) to give the grid its
  // rows. Both remembered per machine, never in the show.
  const [density, setDensityState] = useState<Density>(loadDensity);
  const setDensity = (d: Density) => {
    setDensityState(d);
    saveDensity(d);
  };
  const [panelHidden, setPanelHidden] = useState(() => {
    try {
      return localStorage.getItem(PANEL_HIDDEN_KEY) === "1";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(PANEL_HIDDEN_KEY, panelHidden ? "1" : "0");
    } catch {
      /* storage may be unavailable */
    }
  }, [panelHidden]);
  const togglePanel = useCallback(() => setPanelHidden((h) => !h), []);
  const togglePanelRef = useRef(togglePanel);
  togglePanelRef.current = togglePanel;
  // The global "Modo armar" (LIVE-MODEL-SPEC §7): while on, every play gesture arms.
  const [armMode, setArmMode] = useState(false);
  // Whether any clip is armed: shows "Desarmar" beside GO, and makes Esc disarm all.
  const [anyArmed, setAnyArmed] = useState(false);
  // The "Control" dialog (keyboard shortcuts).
  const [controlOpen, setControlOpen] = useState(false);

  // Keyboard shortcuts (LIVE-MODEL-SPEC §8.2): GO, Desarmar, Stop todo, blackout, scenes,
  // grid navigation. Whether a key does anything is decided in Rust (`main_key` →
  // `keys::dispatch`) from the combo and the focused element's kind, so a shortcut never
  // fires while typing in a text or numeric field. Here we only skip what Rust cannot see
  // (Learn mode, an open menu or dialog — Esc and arrows belong to them) and stop the
  // browser's own action for a bound key (Space scrolling, Enter clicking a button).
  const keysRef = useRef<KeyBinding[]>([]);
  const reloadKeys = useCallback(() => {
    controlKeys()
      .then((k) => (keysRef.current = k))
      .catch(() => {});
  }, []);
  useEffect(() => reloadKeys(), [reloadKeys]);
  const learnRef = useRef(false);
  learnRef.current = learn.active;
  useEffect(() => {
    const blocked = () =>
      learnRef.current ||
      document.querySelector('[role="dialog"], [role="menu"], [role="listbox"], .pr-midimenu, .pr-dirmenu-pop, .pr-menu-drop') !== null;
    const shortcut = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.repeat || isModifierOnly(e) || blocked()) return null;
      const combo = comboOf(e);
      if (isReserved(combo) || !isBound(keysRef.current, combo)) return null;
      const focus = focusKind(document.activeElement);
      return focus === "other" ? { combo, focus } : null;
    };
    const onDown = (e: KeyboardEvent) => {
      const hit = shortcut(e);
      if (!hit) return;
      e.preventDefault();
      // Tree paths run in the engine; a window action (`ui:…`) comes back to run here.
      mainKey(hit.combo, hit.focus)
        .then((path) => {
          if (path === "ui:panel") togglePanelRef.current();
        })
        .catch(() => {});
    };
    // A focused button activates on Space's keyup: stop that too for a bound Space.
    const onUp = (e: KeyboardEvent) => {
      if (shortcut(e)) e.preventDefault();
    };
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    return () => {
      window.removeEventListener("keydown", onDown);
      window.removeEventListener("keyup", onUp);
    };
  }, []);

  // A device connected for the first time to an empty profile got its default mappings.
  useEffect(() => {
    const un = listen<{ device: string; added: number }>("midi-seeded", (e) =>
      showNotice(`${e.payload.device}: se cargaron ${e.payload.added} mapeos del perfil`, 5000),
    );
    return () => {
      un.then((f) => f()).catch(() => {});
    };
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
      refreshLastSaved();
      setSelectedCells({}); // a new show / edited rig: old cell indices mean nothing
      setGridRev((r) => r + 1);
      // The new engine's tree starts with nothing selected: give it ours back.
      const track = selectionRef.current.selectedTrack;
      if (track) mainSelect(track, null).catch(() => {});
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

  const showNotice = (msg: string, ms?: number) => showStatus(msg, false, ms);

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
        {
          kind: "action",
          label: "Abrir show de demo",
          onSelect: () => {
            openDemoShow().catch((e) => setError(String(e)));
          },
        },
        {
          kind: "action",
          label: "Abrir el último guardado",
          disabled: !lastSaved,
          onSelect: () => {
            if (lastSaved) openShow(lastSaved).catch((e) => setError(String(e)));
          },
        },
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
        {
          kind: "submenu",
          label: "Densidad de la grilla",
          children: (["compact", "normal", "large"] as Density[]).map((d) => ({
            kind: "action" as const,
            label: DENSITY_LABEL[d],
            checked: density === d,
            onSelect: () => setDensity(d),
          })),
        },
        { kind: "action", label: "Panel de parámetros", shortcut: "Tab", checked: !panelHidden, onSelect: togglePanel },
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
        { kind: "action", label: "Control…", onSelect: () => setControlOpen(true) },
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
      <div
        className="pr-mainwin"
        style={{ ...densityVars(density), ...(revealed ? {} : { visibility: "hidden" }) } as React.CSSProperties}
      >
        <MenuBar
          menus={menus}
          right={
            <>
              <span
                className={`pr-statusline${status?.error ? " is-error" : ""}`}
                role="status"
                aria-live="polite"
                title={status?.text}
              >
                {status?.text ?? ""}
              </span>
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
                density={density}
              />
            </section>
            <section className="pr-region pr-region-preview">
              {selectedZone ? (
                <LargePreview zoneId={selectedZone.id} />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
            {/* With the panel hidden the master column moves up here, so the master and
                blackout never disappear with it. */}
            {panelHidden && (
              <section className="pr-region pr-workrow-master">
                <GlobalBar />
              </section>
            )}
          </div>

          {/* The parameter panel's show / hide handle. */}
          <div className="pr-panelbar">
            <button
              type="button"
              className="pr-btn pr-panelbar-btn"
              onClick={togglePanel}
              title={panelHidden ? "Mostrar el panel de parámetros (Tab)" : "Ocultar el panel de parámetros (Tab)"}
              aria-expanded={!panelHidden}
            >
              {panelHidden ? "▴ Panel" : "▾ Panel"}
            </button>
          </div>
          {/* Bottom work row — FIXED height set by the density: the effect parameters on the
              left (a swappable slot: groups will share it later) and the master on the right. */}
          {!panelHidden && (
          <div className="pr-workrow">
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
          )}
        </div>

        {deviceMgrOpen && <DeviceManager onClose={() => setDeviceMgrOpen(false)} zones={zones} />}
        {controlOpen && (
          <ControlDialog
            onClose={() => {
              setControlOpen(false);
              reloadKeys();
            }}
          />
        )}
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
  const learnActive = useMidiLearn().active;
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
      {(anyArmed || learnActive) && (
        <LearnTarget target={{ path: "/arm/clear" }} name="Desarmar" className="pr-learn-compact">
          <button type="button" className="pr-btn" onClick={() => armClear().catch(() => {})} title="Desarmar todo (Esc)">
            Desarmar
          </button>
        </LearnTarget>
      )}
      <LearnTarget target={{ path: "/arm/go" }} name="GO" className="pr-learn-compact">
        <button type="button" className="pr-btn pr-go-btn" onClick={() => armGo().catch(() => {})} title="Lanzar todo lo armado (Espacio)">
          GO
        </button>
      </LearnTarget>
    </div>
  );
}

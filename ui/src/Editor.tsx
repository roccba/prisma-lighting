import { useEffect, useMemo, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  addZone,
  applyToLive,
  getState,
  highlight as highlightCmd,
  openFrom,
  removeZone,
  renameZone,
  removeFixtures,
  saveTo,
  selectionAggregate,
  translateFixtures,
  undo as undoCmd,
  redo as redoCmd,
  type EditorState,
  type SelectionAggregate,
} from "./api";
import { Canvas } from "./Canvas";
import { ZonesPanel } from "./ZonesPanel";
import { GroupsPanel } from "./GroupsPanel";
import { LibraryManager } from "./LibraryManager";
import { MatrixDialog } from "./MatrixDialog";
import { NewZoneDialog } from "./NewZoneDialog";
import { PlacementDialog } from "./PlacementDialog";
import { Properties } from "./Properties";
import { Toolbar, type HighlightMode, type ViewMode } from "./Toolbar";
import { WireFaults } from "./WireFaults";

const PRISMAP_FILTERS = [{ name: "Prisma map", extensions: ["prismap"] }];

export default function Editor() {
  const [editor, setEditor] = useState<EditorState | null>(null);
  // `error` is FATAL — it replaces the whole editor (only the initial load uses it).
  // `notice` is a recoverable, dismissible message (a rejected edit, a failed apply):
  // it must NOT blank the editor, or a "zone not empty" rejection would look like a crash.
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selection, setSelection] = useState<Set<string>>(new Set());
  const [aggregate, setAggregate] = useState<SelectionAggregate | null>(null);
  const [activeZone, setActiveZone] = useState<string>("");
  const [viewMode, setViewMode] = useState<ViewMode>("all");
  const [highlightMode, setHighlightMode] = useState<HighlightMode>("off");
  const [modalOpen, setModalOpen] = useState(false);
  const [matrixOpen, setMatrixOpen] = useState(false);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [newZoneOpen, setNewZoneOpen] = useState(false);
  // Zone id being renamed (null = dialog closed).
  const [renameZoneId, setRenameZoneId] = useState<string | null>(null);
  const [currentPath, setCurrentPath] = useState<string | null>(null);

  useEffect(() => {
    getState()
      .then((s) => {
        setEditor(s);
        if (s.patch.zones.length > 0) setActiveZone(s.patch.zones[0].id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const patch = editor?.patch ?? null;

  const fixtureIds = useMemo(() => new Set((patch?.fixtures ?? []).map((f) => f.id)), [patch]);
  useEffect(() => {
    setSelection((sel) => {
      const pruned = new Set([...sel].filter((id) => fixtureIds.has(id)));
      return pruned.size === sel.size ? sel : pruned;
    });
  }, [fixtureIds]);

  useEffect(() => {
    if (selection.size === 0) { setAggregate(null); return; }
    selectionAggregate([...selection]).then(setAggregate).catch(() => setAggregate(null));
  }, [selection]);

  // Highlight the live rig (§7): when a mode is chosen, blink/solid the current
  // selection on the wire; Off clears it. Re-sent whenever the selection or mode changes.
  useEffect(() => {
    if (highlightMode === "off") { highlightCmd([], false).catch(() => {}); return; }
    highlightCmd([...selection], highlightMode === "blink").catch(() => {});
  }, [highlightMode, selection]);

  // --- Persistence (§9) ---
  const doOpen = async () => {
    try {
      const path = await openDialog({ multiple: false, filters: PRISMAP_FILTERS });
      if (typeof path === "string") {
        setEditor(await openFrom(path));
        setCurrentPath(path);
        setSelection(new Set());
      }
    } catch (e) { setNotice(String(e)); }
  };

  /** Save to the current file, or prompt for one. Returns whether it was saved. */
  const doSave = async (): Promise<boolean> => {
    try {
      let path = currentPath;
      if (!path) {
        const picked = await saveDialog({ filters: PRISMAP_FILTERS, defaultPath: "untitled.prismap" });
        if (!picked) return false;
        path = picked;
        setCurrentPath(picked);
      }
      setEditor(await saveTo(path));
      return true;
    } catch (e) { setNotice(String(e)); return false; }
  };

  const doSaveAs = async () => {
    try {
      const picked = await saveDialog({ filters: PRISMAP_FILTERS, defaultPath: currentPath ?? "untitled.prismap" });
      if (picked) { setEditor(await saveTo(picked)); setCurrentPath(picked); }
    } catch (e) { setNotice(String(e)); }
  };

  // --- Disable browser zoom app-wide (§5.7) ---
  // Ctrl+wheel and trackpad pinch (which the browser reports as a wheel event with
  // ctrlKey) zoom the entire web UI — fonts, panels, layout — which breaks the editor.
  // The only zoom is the canvas GRID (plain wheel, handled in Canvas). A non-passive
  // window listener is required: React's onWheel is passive, so preventDefault there is
  // ignored. Ctrl +/-/0 keyboard zoom is blocked too.
  useEffect(() => {
    const onWheel = (e: WheelEvent) => {
      if (e.ctrlKey) e.preventDefault();
    };
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && ["=", "-", "+", "0"].includes(e.key)) e.preventDefault();
    };
    window.addEventListener("wheel", onWheel, { passive: false });
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("wheel", onWheel);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  // Live-apply: edits flow to the running show automatically, debounced so a burst of
  // edits triggers ONE apply after they settle. This is affordable now that the engine
  // caches its GPU pipelines across recompiles (prisma-render `PipelineCache`) — the old
  // per-edit full pipeline rebuild was the lag. Live values (master/colour/dimmers) are
  // preserved across the apply (`apply_editor_draft`). Closing the window applies the
  // final state too (backend window-Destroyed handler), so a pending debounce is never
  // lost. Highlight (§7) is a separate live path, unaffected.
  useEffect(() => {
    if (!editor?.dirty) return;
    const t = setTimeout(() => {
      applyToLive().catch((e) => setNotice(String(e)));
    }, 350);
    return () => clearTimeout(t);
  }, [editor]);

  // --- Keyboard shortcuts ---
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (e.shiftKey) doSaveAs(); else doSave();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
        e.preventDefault();
        doOpen();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) redoCmd().then(setEditor); else undoCmd().then(setEditor);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentPath]);

  if (error) return <div className="fatal">Error: {error}</div>;
  if (!editor || !patch) return <div className="fatal">Loading patch…</div>;

  const del = async () => {
    if (selection.size === 0) return;
    setEditor(await removeFixtures([...selection]));
    setSelection(new Set());
  };

  const fileName = currentPath ? currentPath.split(/[\\/]/).pop() : "untitled";

  return (
    <div className="editor">
      <header className="topbar">
        <span className="title">{patch.name}</span>
        <span className="sub">
          {fileName}{editor.dirty ? " · edited" : ""} — {patch.fixtures.length} fixtures ·{" "}
          {patch.zones.length} zones · {selection.size} selected
        </span>
      </header>

      {notice && (
        <div className="editor-notice">
          <span>{notice}</span>
          <button className="btn" onClick={() => setNotice(null)} title="Descartar">
            ×
          </button>
        </div>
      )}

      <Toolbar
        patch={patch}
        activeZone={activeZone}
        viewMode={viewMode}
        onViewMode={setViewMode}
        onOpen={doOpen}
        onSave={doSave}
        onAddFixtures={() => setModalOpen(true)}
        onLibrary={() => setLibraryOpen(true)}
        onDelete={del}
        canDelete={selection.size > 0}
        onUndo={async () => setEditor(await undoCmd())}
        canUndo={editor.can_undo}
        onRedo={async () => setEditor(await redoCmd())}
        canRedo={editor.can_redo}
        highlightMode={highlightMode}
        onHighlightMode={setHighlightMode}
      />

      <WireFaults
        patch={patch}
        diagnostics={editor.diagnostics}
        onSelectFixtures={(ids) => setSelection(new Set(ids))}
      />

      <div className="body">
        <aside className="left">
          <ZonesPanel
            patch={patch}
            activeZone={activeZone}
            onActiveZone={setActiveZone}
            onAddZone={() => setNewZoneOpen(true)}
            onRenameZone={(id) => setRenameZoneId(id)}
            onRemoveZone={async (id) => {
              try {
                const s = await removeZone(id);
                setEditor(s);
                if (activeZone === id) setActiveZone(s.patch.zones[0]?.id ?? "");
              } catch (e) {
                setNotice(String(e));
              }
            }}
            onEditMatrix={() => setMatrixOpen(true)}
            selection={selection}
            onSelectionChange={setSelection}
          />
          <GroupsPanel />
        </aside>
        <main className="center">
          <Canvas
            patch={patch}
            selection={selection}
            onSelectionChange={setSelection}
            onMove={async (ids, delta) => setEditor(await translateFixtures(ids, delta))}
            activeZone={activeZone}
            viewMode={viewMode}
            diagnostics={editor.diagnostics}
          />
        </main>
        <aside className="right">
          <Properties patch={patch} selection={selection} aggregate={aggregate} />
        </aside>
      </div>

      {modalOpen && (
        <PlacementDialog
          patch={patch}
          activeZone={activeZone}
          onPlaced={setEditor}
          onClose={() => setModalOpen(false)}
        />
      )}

      {matrixOpen && patch.zones.find((z) => z.id === activeZone) && (
        <MatrixDialog
          zone={patch.zones.find((z) => z.id === activeZone)!}
          onApplied={setEditor}
          onClose={() => setMatrixOpen(false)}
        />
      )}

      {libraryOpen && <LibraryManager onClose={() => setLibraryOpen(false)} />}

      {newZoneOpen && (
        <NewZoneDialog
          existingNames={patch.zones.map((z) => z.name)}
          onClose={() => setNewZoneOpen(false)}
          onCreate={async (name) => {
            setNewZoneOpen(false);
            try {
              const s = await addZone(name);
              setEditor(s);
              const added = s.patch.zones[s.patch.zones.length - 1];
              if (added) setActiveZone(added.id);
            } catch (e) {
              setNotice(String(e));
            }
          }}
        />
      )}

      {renameZoneId &&
        (() => {
          const zone = patch.zones.find((z) => z.id === renameZoneId);
          if (!zone) return null;
          return (
            <NewZoneDialog
              title="Renombrar zona"
              initial={zone.name}
              submitLabel="Renombrar"
              // Exclude the zone itself so renaming to its own name is allowed.
              existingNames={patch.zones.filter((z) => z.id !== renameZoneId).map((z) => z.name)}
              onClose={() => setRenameZoneId(null)}
              onCreate={async (name) => {
                const id = renameZoneId;
                setRenameZoneId(null);
                try {
                  setEditor(await renameZone(id, name));
                } catch (e) {
                  setNotice(String(e));
                }
              }}
            />
          );
        })()}
    </div>
  );
}

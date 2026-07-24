import { useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  getState,
  openFrom,
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
import { FixtureList } from "./FixtureList";
import { PlacementDialog } from "./PlacementDialog";
import { Properties } from "./Properties";
import { Toolbar, type ViewMode } from "./Toolbar";

const PRISMAP_FILTERS = [{ name: "Prisma map", extensions: ["prismap"] }];

export default function App() {
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Set<string>>(new Set());
  const [aggregate, setAggregate] = useState<SelectionAggregate | null>(null);
  const [activeZone, setActiveZone] = useState<string>("");
  const [viewMode, setViewMode] = useState<ViewMode>("all");
  const [modalOpen, setModalOpen] = useState(false);
  const [currentPath, setCurrentPath] = useState<string | null>(null);
  const [unsavedOpen, setUnsavedOpen] = useState(false);

  const dirtyRef = useRef(false);
  const closingRef = useRef(false);

  useEffect(() => {
    getState()
      .then((s) => {
        setEditor(s);
        if (s.patch.zones.length > 0) setActiveZone(s.patch.zones[0].id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const patch = editor?.patch ?? null;
  useEffect(() => { dirtyRef.current = editor?.dirty ?? false; }, [editor]);

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

  // --- Persistence (§9) ---
  const doOpen = async () => {
    try {
      const path = await openDialog({ multiple: false, filters: PRISMAP_FILTERS });
      if (typeof path === "string") {
        setEditor(await openFrom(path));
        setCurrentPath(path);
        setSelection(new Set());
      }
    } catch (e) { setError(String(e)); }
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
    } catch (e) { setError(String(e)); return false; }
  };

  const doSaveAs = async () => {
    try {
      const picked = await saveDialog({ filters: PRISMAP_FILTERS, defaultPath: currentPath ?? "untitled.prismap" });
      if (picked) { setEditor(await saveTo(picked)); setCurrentPath(picked); }
    } catch (e) { setError(String(e)); }
  };

  // --- Unsaved-changes guard on window close ---
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onCloseRequested((e) => {
        if (dirtyRef.current && !closingRef.current) {
          e.preventDefault();
          setUnsavedOpen(true);
        }
      })
      .then((u) => { unlisten = u; })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  const forceClose = async () => {
    closingRef.current = true;
    await getCurrentWindow().destroy();
  };

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

      <Toolbar
        patch={patch}
        activeZone={activeZone}
        onActiveZone={setActiveZone}
        viewMode={viewMode}
        onViewMode={setViewMode}
        onOpen={doOpen}
        onSave={doSave}
        onAddFixtures={() => setModalOpen(true)}
        onDelete={del}
        canDelete={selection.size > 0}
        onUndo={async () => setEditor(await undoCmd())}
        canUndo={editor.can_undo}
        onRedo={async () => setEditor(await redoCmd())}
        canRedo={editor.can_redo}
      />

      {editor.validation && (
        <div className="validation-banner" title={editor.validation}>
          ⚠ Patch has a problem: {editor.validation}
        </div>
      )}

      <div className="body">
        <aside className="left">
          <FixtureList patch={patch} selection={selection} onSelectionChange={setSelection} />
        </aside>
        <main className="center">
          <Canvas
            patch={patch}
            selection={selection}
            onSelectionChange={setSelection}
            onMove={async (ids, delta) => setEditor(await translateFixtures(ids, delta))}
            activeZone={activeZone}
            viewMode={viewMode}
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

      {unsavedOpen && (
        <div className="modal-backdrop">
          <div className="modal">
            <div className="modal-head">Unsaved changes</div>
            <div className="modal-message">
              You have unsaved changes to <strong>{fileName}</strong>. Save before closing?
            </div>
            <div className="modal-foot">
              <button className="btn" onClick={() => setUnsavedOpen(false)}>Cancel</button>
              <div className="spacer" />
              <button
                className="btn"
                onClick={async () => { setUnsavedOpen(false); await forceClose(); }}
              >
                Discard
              </button>
              <button
                className="btn primary"
                onClick={async () => {
                  const saved = await doSave();
                  setUnsavedOpen(false);
                  if (saved) await forceClose();
                }}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

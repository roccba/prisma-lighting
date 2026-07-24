import { useEffect, useMemo, useState } from "react";
import {
  getState,
  removeFixtures,
  selectionAggregate,
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

export default function App() {
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Set<string>>(new Set());
  const [aggregate, setAggregate] = useState<SelectionAggregate | null>(null);
  const [activeZone, setActiveZone] = useState<string>("");
  const [viewMode, setViewMode] = useState<ViewMode>("all");
  const [modalOpen, setModalOpen] = useState(false);

  // Load the editor state once; default the active zone to the first zone.
  useEffect(() => {
    getState()
      .then((s) => {
        setEditor(s);
        if (s.patch.zones.length > 0) setActiveZone(s.patch.zones[0].id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const patch = editor?.patch ?? null;

  // Keep the selection consistent with the current fixtures (after delete/undo).
  const fixtureIds = useMemo(
    () => new Set((patch?.fixtures ?? []).map((f) => f.id)),
    [patch],
  );
  useEffect(() => {
    setSelection((sel) => {
      const pruned = new Set([...sel].filter((id) => fixtureIds.has(id)));
      return pruned.size === sel.size ? sel : pruned;
    });
  }, [fixtureIds]);

  // Recompute the selection aggregate (in Rust) whenever the selection changes.
  useEffect(() => {
    if (selection.size === 0) {
      setAggregate(null);
      return;
    }
    selectionAggregate([...selection]).then(setAggregate).catch(() => setAggregate(null));
  }, [selection]);

  if (error) return <div className="fatal">Failed to load patch: {error}</div>;
  if (!editor || !patch) return <div className="fatal">Loading patch…</div>;

  const del = async () => {
    if (selection.size === 0) return;
    const s = await removeFixtures([...selection]);
    setEditor(s);
    setSelection(new Set());
  };

  return (
    <div className="editor">
      <header className="topbar">
        <span className="title">{patch.name}</span>
        <span className="sub">
          {patch.fixtures.length} fixtures · {patch.zones.length} zones ·{" "}
          {selection.size} selected{editor.dirty ? " · edited" : ""}
        </span>
      </header>

      <Toolbar
        patch={patch}
        activeZone={activeZone}
        onActiveZone={setActiveZone}
        viewMode={viewMode}
        onViewMode={setViewMode}
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
    </div>
  );
}

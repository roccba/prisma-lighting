import { useEffect, useState } from "react";
import { getPatch, selectionAggregate, type Patch, type SelectionAggregate } from "./api";
import { Canvas } from "./Canvas";
import { FixtureList } from "./FixtureList";
import { Properties } from "./Properties";

export default function App() {
  const [patch, setPatch] = useState<Patch | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Set<string>>(new Set());
  const [aggregate, setAggregate] = useState<SelectionAggregate | null>(null);

  // Load the editor's working patch once.
  useEffect(() => {
    getPatch().then(setPatch).catch((e) => setError(String(e)));
  }, []);

  // Recompute the selection aggregate (in Rust) whenever the selection changes.
  useEffect(() => {
    if (selection.size === 0) {
      setAggregate(null);
      return;
    }
    selectionAggregate([...selection]).then(setAggregate).catch(() => setAggregate(null));
  }, [selection]);

  if (error) return <div className="fatal">Failed to load patch: {error}</div>;
  if (!patch) return <div className="fatal">Loading patch…</div>;

  return (
    <div className="editor">
      <header className="topbar">
        <span className="title">{patch.name}</span>
        <span className="sub">
          {patch.fixtures.length} fixtures · {patch.zones.length} zones ·{" "}
          {selection.size} selected
        </span>
      </header>
      <div className="body">
        <aside className="left">
          <FixtureList
            patch={patch}
            selection={selection}
            onSelectionChange={setSelection}
          />
        </aside>
        <main className="center">
          <Canvas patch={patch} selection={selection} onSelectionChange={setSelection} />
        </main>
        <aside className="right">
          <Properties patch={patch} selection={selection} aggregate={aggregate} />
        </aside>
      </div>
    </div>
  );
}

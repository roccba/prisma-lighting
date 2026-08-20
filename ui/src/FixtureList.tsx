import type { Patch } from "./api";

// Fixtures grouped by ZONE, one tab per zone (design-system `.pr-wtab`): pick a zone to
// see only its fixtures. This keeps the list ordered and legible with many zones, and —
// because only the active zone's rows are in the DOM, not every fixture in the patch —
// it stays cheap on a big rig. Fixtures are UNITS, one row each (not one row per pixel).
// The zone tabs share the editor's `activeZone`, so the toolbar selector and this list
// stay in sync. Two-way selection sync with the canvas.

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
  activeZone: string;
  onActiveZone: (id: string) => void;
}

export function FixtureList({ patch, selection, onSelectionChange, activeZone, onActiveZone }: Props) {
  // Per-zone fixture counts in a single pass (avoids O(zones × fixtures) per render).
  const counts = new Map<string, number>();
  for (const f of patch.fixtures) counts.set(f.zone, (counts.get(f.zone) ?? 0) + 1);

  const click = (id: string, e: React.MouseEvent) => {
    const next = new Set(selection);
    if (e.shiftKey || e.ctrlKey) {
      next.has(id) ? next.delete(id) : next.add(id);
    } else {
      next.clear();
      next.add(id);
    }
    onSelectionChange(next);
  };

  const shown = patch.fixtures.filter((f) => f.zone === activeZone);

  return (
    <div className="panel">
      <div className="panel-head">Fixtures</div>

      <div className="pr-wtabs" style={{ overflowX: "auto", flexWrap: "nowrap", flex: "0 0 auto" }}>
        {patch.zones.map((z) => (
          <div
            key={z.id}
            className={`pr-wtab${z.id === activeZone ? " is-active" : ""}`}
            role="button"
            title={z.name}
            onClick={() => onActiveZone(z.id)}
            style={{ display: "inline-flex", alignItems: "center", gap: 6, whiteSpace: "nowrap" }}
          >
            <span
              style={{
                width: 9,
                height: 9,
                borderRadius: 2,
                flex: "0 0 auto",
                background: `rgb(${z.display_color.join(",")})`,
              }}
            />
            {z.name}
            <span style={{ opacity: 0.6 }}>({counts.get(z.id) ?? 0})</span>
          </div>
        ))}
      </div>

      <ul className="fixture-list">
        {shown.map((f) => (
          <li
            key={f.id}
            className={selection.has(f.id) ? "row selected" : "row"}
            onClick={(e) => click(f.id, e)}
          >
            <span className="fx-name">{f.name}</span>
            <span className="fx-meta">{f.pixels.length}px</span>
          </li>
        ))}
        {shown.length === 0 && (
          <li style={{ color: "var(--text-dim)", padding: "8px 10px", cursor: "default" }}>
            No fixtures in this zone.
          </li>
        )}
      </ul>
    </div>
  );
}

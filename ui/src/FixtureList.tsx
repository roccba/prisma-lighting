import type { Patch } from "./api";

// Fixtures as UNITS, one row each — not one row per pixel (the density fix from
// screenshot 07). Two-way sync with the canvas: clicking a row selects, and the
// canvas selection highlights the rows.

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
}

export function FixtureList({ patch, selection, onSelectionChange }: Props) {
  const zoneName = new Map(patch.zones.map((z) => [z.id, z.name]));
  const zoneColor = new Map(
    patch.zones.map((z) => [z.id, `rgb(${z.display_color.join(",")})`]),
  );

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

  return (
    <div className="panel">
      <div className="panel-head">Fixtures</div>
      <ul className="fixture-list">
        {patch.fixtures.map((f) => (
          <li
            key={f.id}
            className={selection.has(f.id) ? "row selected" : "row"}
            onClick={(e) => click(f.id, e)}
          >
            <span className="swatch" style={{ background: zoneColor.get(f.zone) }} />
            <span className="fx-name">{f.name}</span>
            <span className="fx-meta">
              {f.pixels.length}px · {zoneName.get(f.zone) ?? "—"}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

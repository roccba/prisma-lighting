import { useState } from "react";
import type { Patch } from "./api";
import { Grid, Pencil, Plus, Minus } from "./icons";
import { Splitter, loadPaneHeight, savePaneHeight } from "./Splitter";

const ZONE_LIST_H_KEY = "prisma.editor.zoneListHeight";

// Left sector "Zonas" (§M7 restyle): the zone list is the organising control of the
// editor — pick a zone to make it active, and its fixtures show below. Zone management
// (add / rename / remove / render resolution) lives here now, not in the toolbar, so the
// whole zone concern is sectorised to the left, MADRIX-style. Selection of fixtures is
// two-way synced with the canvas.

interface Props {
  patch: Patch;
  activeZone: string;
  onActiveZone: (id: string) => void;
  onAddZone: () => void;
  onRenameZone: (id: string) => void;
  onRemoveZone: (id: string) => void;
  onEditMatrix: () => void;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
}

export function ZonesPanel(p: Props) {
  const { patch, activeZone } = p;
  const active = patch.zones.find((z) => z.id === activeZone);

  // The zone list has a user-draggable height so a rig with many zones stays fully
  // visible (the fixtures list below takes the rest). Persisted per-machine.
  const [zoneListH, setZoneListH] = useState(() => loadPaneHeight(ZONE_LIST_H_KEY, 180));
  const changeZoneListH = (v: number) => {
    setZoneListH(v);
    savePaneHeight(ZONE_LIST_H_KEY, v);
  };

  // Per-zone fixture counts in a single pass (avoids O(zones × fixtures) per render).
  const counts = new Map<string, number>();
  for (const f of patch.fixtures) counts.set(f.zone, (counts.get(f.zone) ?? 0) + 1);

  const shown = patch.fixtures.filter((f) => f.zone === activeZone);

  const clickFixture = (id: string, e: React.MouseEvent) => {
    const next = new Set(p.selection);
    if (e.shiftKey || e.ctrlKey) {
      next.has(id) ? next.delete(id) : next.add(id);
    } else {
      next.clear();
      next.add(id);
    }
    p.onSelectionChange(next);
  };

  return (
    <section className="left-sector zones-sector">
      <div className="sector-head">
        <span className="sector-title">Zonas</span>
        <div className="sector-actions">
          <button className="pr-btn pr-btn-icon" title="Añadir zona" onClick={p.onAddZone}>
            <Plus size={15} />
          </button>
          <button
            className="pr-btn pr-btn-icon"
            title="Renombrar zona"
            disabled={!active}
            onClick={() => active && p.onRenameZone(active.id)}
          >
            <Pencil size={15} />
          </button>
          <button
            className="pr-btn pr-btn-icon"
            title="Eliminar zona (debe estar vacía)"
            disabled={!active}
            onClick={() => active && p.onRemoveZone(active.id)}
          >
            <Minus size={15} />
          </button>
          <button
            className="pr-btn pr-btn-icon"
            title={
              active
                ? `Resolución de render de ${active.name} — ${active.mapping_size[0]}×${active.mapping_size[1]}`
                : "Resolución de render de la zona"
            }
            disabled={!active}
            onClick={p.onEditMatrix}
          >
            <Grid size={15} />
          </button>
        </div>
      </div>

      <div className="sector-scroll zone-list" style={{ height: zoneListH, flex: "0 0 auto" }}>
        {patch.zones.map((z) => (
          <div
            key={z.id}
            className={`pr-listrow${z.id === activeZone ? " is-sel" : ""}`}
            role="button"
            title={z.name}
            onClick={() => p.onActiveZone(z.id)}
          >
            <span className="zc" style={{ background: `rgb(${z.display_color.join(",")})` }} />
            <span className="nm">{z.name}</span>
            <span className="meta">{counts.get(z.id) ?? 0}</span>
          </div>
        ))}
        {patch.zones.length === 0 && <div className="sector-empty">Sin zonas todavía.</div>}
      </div>

      <Splitter value={zoneListH} onChange={changeZoneListH} min={60} max={640} />

      <div className="sub-head">
        Instalaciones{active ? ` · ${active.name}` : ""}
        {active && <span className="sub-count">{shown.length}</span>}
      </div>

      <div className="sector-scroll fixture-list">
        {shown.map((f) => (
          <div
            key={f.id}
            className={`pr-listrow${p.selection.has(f.id) ? " is-sel" : ""}`}
            onClick={(e) => clickFixture(f.id, e)}
          >
            <span className="nm">{f.name}</span>
            <span className="meta">{f.pixels.length}px</span>
          </div>
        ))}
        {active && shown.length === 0 && <div className="sector-empty">Sin instalaciones en esta zona.</div>}
      </div>
    </section>
  );
}

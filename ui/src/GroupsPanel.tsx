import { useState } from "react";
import { Plus, Minus } from "./icons";
import { Splitter, loadPaneHeight, savePaneHeight } from "./Splitter";

const GROUPS_H_KEY = "prisma.editor.groupsHeight";

// Left sector "Grupos" (§M7 restyle, §3.3 / §6). Groups — a saved convenience selection
// across fixtures — are NOT implemented yet (M7 gap): this is the sectorised placeholder
// that reserves the space and the affordances (add / remove) MADRIX puts at the bottom
// left, so the layout is complete and the feature has an obvious home. Controls are
// disabled until the feature lands. Its height is user-draggable (the splitter sits on
// the boundary with the Zonas sector above) so it never crowds the zone list.

export function GroupsPanel() {
  const [h, setH] = useState(() => loadPaneHeight(GROUPS_H_KEY, 120));
  const changeH = (v: number) => {
    setH(v);
    savePaneHeight(GROUPS_H_KEY, v);
  };

  return (
    <>
      <Splitter value={h} onChange={changeH} min={44} max={520} invert />
      <section className="left-sector groups-sector" style={{ height: h, flex: "0 0 auto" }}>
        <div className="sector-head">
          <span className="sector-title">Grupos</span>
          <div className="sector-actions">
            <button className="pr-btn pr-btn-icon" title="Crear grupo (próximamente)" disabled>
              <Plus size={15} />
            </button>
            <button className="pr-btn pr-btn-icon" title="Eliminar grupo (próximamente)" disabled>
              <Minus size={15} />
            </button>
          </div>
        </div>
        <div className="sector-scroll">
          <div className="sector-empty">Los grupos aún no están implementados.</div>
        </div>
      </section>
    </>
  );
}

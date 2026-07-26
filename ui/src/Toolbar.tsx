import type { Patch } from "./api";

// The zone selector is the organising control of the editor (§6): with a single
// zone it is a no-op and the editor feels like professional consoles; complexity appears only
// when there are several zones. It drives the three view modes (§5.3).

export type ViewMode = "all" | "highlight" | "isolate";

const MODES: { id: ViewMode; label: string; title: string }[] = [
  { id: "all", label: "All", title: "Show every zone" },
  { id: "highlight", label: "Highlight", title: "Emphasise the active zone, dim the rest" },
  { id: "isolate", label: "Isolate", title: "Show only the active zone" },
];

interface Props {
  patch: Patch;
  activeZone: string;
  onActiveZone: (id: string) => void;
  viewMode: ViewMode;
  onViewMode: (m: ViewMode) => void;
  onOpen: () => void;
  onSave: () => void;
  onAddFixtures: () => void;
  onEditMatrix: () => void;
  onLibrary: () => void;
  onDelete: () => void;
  canDelete: boolean;
  onUndo: () => void;
  canUndo: boolean;
  onRedo: () => void;
  canRedo: boolean;
}

export function Toolbar(p: Props) {
  const active = p.patch.zones.find((z) => z.id === p.activeZone);
  return (
    <div className="toolbar">
      <div className="tb-group">
        <button className="btn" onClick={p.onOpen} title="Open .prismap (Ctrl+O)">
          Open
        </button>
        <button className="btn" onClick={p.onSave} title="Save .prismap (Ctrl+S)">
          Save
        </button>
      </div>

      <div className="tb-sep" />

      <div className="tb-group">
        <button className="btn" onClick={p.onAddFixtures}>
          + Add fixtures
        </button>
        <button className="btn" onClick={p.onLibrary} title="Manage the fixture library (§3.2)">
          Library…
        </button>
        <button className="btn" onClick={p.onDelete} disabled={!p.canDelete}>
          Delete
        </button>
        <button className="btn" onClick={p.onUndo} disabled={!p.canUndo} title="Undo">
          ↶
        </button>
        <button className="btn" onClick={p.onRedo} disabled={!p.canRedo} title="Redo">
          ↷
        </button>
      </div>

      <div className="tb-sep" />

      <label className="tb-group">
        <span className="tb-label">Zone</span>
        <span className="tb-select">
          {active && (
            <span className="swatch" style={{ background: `rgb(${active.display_color.join(",")})` }} />
          )}
          <select value={p.activeZone} onChange={(e) => p.onActiveZone(e.target.value)}>
            {p.patch.zones.map((z) => (
              <option key={z.id} value={z.id}>
                {z.name}
              </option>
            ))}
          </select>
        </span>
      </label>
      {active && (
        <button
          className="btn"
          onClick={p.onEditMatrix}
          title={`Matrix settings — ${active.mapping_size[0]}×${active.mapping_size[1]} (render resolution)`}
        >
          Matrix… {active.mapping_size[0]}×{active.mapping_size[1]}
        </button>
      )}

      <div className="tb-group">
        <span className="tb-label">View</span>
        <div className="segmented">
          {MODES.map((m) => (
            <button
              key={m.id}
              title={m.title}
              className={m.id === p.viewMode ? "seg active" : "seg"}
              onClick={() => p.onViewMode(m.id)}
            >
              {m.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

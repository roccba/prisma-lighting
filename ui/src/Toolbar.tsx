import type { Patch } from "./api";
import { Dropdown } from "./Dropdown";

// The zone selector is the organising control of the editor (§6): with a single
// zone it is a no-op and the editor stays simple; complexity appears only
// when there are several zones. It drives the three view modes (§5.3).

export type ViewMode = "all" | "highlight" | "isolate";

/** Live-rig identify mode (§7): off, or blink/solid the current selection on the wire. */
export type HighlightMode = "off" | "solid" | "blink";

const MODES: { id: ViewMode; label: string; title: string }[] = [
  { id: "all", label: "All", title: "Show every zone" },
  { id: "highlight", label: "Highlight", title: "Emphasise the active zone, dim the rest" },
  { id: "isolate", label: "Isolate", title: "Show only the active zone" },
];

const HIGHLIGHT_MODES: { id: HighlightMode; label: string; title: string }[] = [
  { id: "off", label: "Off", title: "Do not light the rig" },
  { id: "solid", label: "Solid", title: "Light the selection solid (compare two without flicker)" },
  { id: "blink", label: "Blink", title: "Blink the selection (~2 Hz) to spot it among many" },
];

interface Props {
  patch: Patch;
  activeZone: string;
  onActiveZone: (id: string) => void;
  onAddZone: () => void;
  onRemoveZone: (id: string) => void;
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
  highlightMode: HighlightMode;
  onHighlightMode: (m: HighlightMode) => void;
}

// Segmented control (View / Highlight modes): a tight row of `.pr-tabbtn`, the active
// one on the accent. Presentation only.
function Segmented<T extends string>(props: {
  value: T;
  options: { id: T; label: string; title: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="pr-segmented">
      {props.options.map((o) => (
        <button
          key={o.id}
          title={o.title}
          className={`pr-btn pr-tabbtn${o.id === props.value ? " is-active" : ""}`}
          onClick={() => props.onChange(o.id)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Toolbar(p: Props) {
  const active = p.patch.zones.find((z) => z.id === p.activeZone);
  const zoneOptions = p.patch.zones.map((z) => ({
    value: z.id,
    label: z.name,
    swatch: `rgb(${z.display_color.join(",")})`,
  }));

  return (
    <div className="pr-toolbar">
      {/* File */}
      <div className="pr-toolbar-group">
        <button className="pr-btn" onClick={p.onOpen} title="Open .prismap (Ctrl+O)">
          Open
        </button>
        <button className="pr-btn" onClick={p.onSave} title="Save .prismap (Ctrl+S)">
          Save
        </button>
      </div>

      {/* Fixtures / edit */}
      <div className="pr-toolbar-group sep">
        <button
          className="pr-btn"
          onClick={p.onAddFixtures}
          disabled={!active}
          title={active ? "Add fixtures into the selected zone" : "Create a zone first — fixtures live in a zone"}
        >
          + Add fixtures
        </button>
        <button className="pr-btn" onClick={p.onLibrary} title="Manage the fixture library (§3.2)">
          Library…
        </button>
        <button className="pr-btn" onClick={p.onDelete} disabled={!p.canDelete}>
          Delete
        </button>
        <button className="pr-btn pr-btn-icon" onClick={p.onUndo} disabled={!p.canUndo} title="Undo">
          ↶
        </button>
        <button className="pr-btn pr-btn-icon" onClick={p.onRedo} disabled={!p.canRedo} title="Redo">
          ↷
        </button>
      </div>

      {/* Zone context — the organising control (§6) */}
      <div className="pr-toolbar-group sep">
        <span className="pr-toolbar-label">Zone</span>
        <Dropdown
          value={p.activeZone}
          options={zoneOptions}
          onChange={p.onActiveZone}
          minWidth={150}
          aria-label="Active zone"
        />
        <button className="pr-btn" onClick={p.onAddZone} title="Add a new zone">
          + Zona
        </button>
        <button
          className="pr-btn"
          onClick={() => p.onRemoveZone(p.activeZone)}
          disabled={!active}
          title="Remove the selected zone (must have no fixtures). The patch may have zero zones."
        >
          − Zona
        </button>
        {active && (
          <button
            className="pr-btn"
            onClick={p.onEditMatrix}
            title={`Render resolution of zone ${active.name} — ${active.mapping_size[0]}×${active.mapping_size[1]} (the detail its effects run at; the world auto-fits to the rig)`}
          >
            Resolution… {active.mapping_size[0]}×{active.mapping_size[1]}
          </button>
        )}
      </div>

      {/* View modes (§5.3) */}
      <div className="pr-toolbar-group sep">
        <span className="pr-toolbar-label">View</span>
        <Segmented value={p.viewMode} options={MODES} onChange={p.onViewMode} />
      </div>

      {/* Highlight on the real rig (§7) */}
      <div className="pr-toolbar-group sep">
        <span className="pr-toolbar-label" title="Light the selected fixtures on the real rig (§7)">
          Highlight
        </span>
        <Segmented value={p.highlightMode} options={HIGHLIGHT_MODES} onChange={p.onHighlightMode} />
      </div>
    </div>
  );
}

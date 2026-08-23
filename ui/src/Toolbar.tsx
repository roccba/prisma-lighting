import type { Patch } from "./api";
import {
  AddFixtures,
  Bolt,
  Bulb,
  Eye,
  EyeOff,
  FolderOpen,
  Isolate,
  Library,
  Redo,
  Save,
  Target,
  Trash,
  Undo,
} from "./icons";

// MADRIX-style icon-over-label toolbar (§M7 restyle): every action is a `.pr-tbtn`
// (icon on top, label under), grouped into labelled sections separated by `.sep`.
// Zone management lives in the left ZonesPanel now, not here — the toolbar is
// file / fixtures / edit / view / rig-highlight only.

export type ViewMode = "all" | "highlight" | "isolate";

/** Live-rig identify mode (§7): off, or blink/solid the current selection on the wire. */
export type HighlightMode = "off" | "solid" | "blink";

const MODES: { id: ViewMode; label: string; title: string; icon: React.ReactNode }[] = [
  { id: "all", label: "Todo", title: "Show every zone", icon: <Eye /> },
  { id: "highlight", label: "Resaltar", title: "Emphasise the active zone, dim the rest", icon: <Target /> },
  { id: "isolate", label: "Aislar", title: "Show only the active zone", icon: <Isolate /> },
];

const HIGHLIGHT_MODES: { id: HighlightMode; label: string; title: string; icon: React.ReactNode }[] = [
  { id: "off", label: "Off", title: "Do not light the rig", icon: <EyeOff /> },
  { id: "solid", label: "Fijo", title: "Light the selection solid (compare two without flicker)", icon: <Bulb /> },
  { id: "blink", label: "Parpadeo", title: "Blink the selection (~2 Hz) to spot it among many", icon: <Bolt size={18} /> },
];

interface Props {
  patch: Patch;
  activeZone: string;
  viewMode: ViewMode;
  onViewMode: (m: ViewMode) => void;
  onOpen: () => void;
  onSave: () => void;
  onAddFixtures: () => void;
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

/** One icon-over-label toolbar button. */
function TButton(props: {
  icon: React.ReactNode;
  label: string;
  title?: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      className={`pr-tbtn${props.active ? " is-active" : ""}`}
      title={props.title ?? props.label}
      onClick={props.onClick}
      disabled={props.disabled}
    >
      <span className="pr-ic">{props.icon}</span>
      {props.label}
    </button>
  );
}

export function Toolbar(p: Props) {
  const active = p.patch.zones.find((z) => z.id === p.activeZone);

  return (
    <div className="pr-toolbar">
      {/* File */}
      <div className="pr-toolbar-group">
        <TButton icon={<FolderOpen />} label="Abrir" title="Open .prismap (Ctrl+O)" onClick={p.onOpen} />
        <TButton icon={<Save />} label="Guardar" title="Save .prismap (Ctrl+S)" onClick={p.onSave} />
      </div>

      {/* Fixtures */}
      <div className="pr-toolbar-group sep">
        <TButton
          icon={<AddFixtures />}
          label="Añadir"
          title={active ? "Add fixtures into the selected zone" : "Create a zone first — fixtures live in a zone"}
          onClick={p.onAddFixtures}
          disabled={!active}
        />
        <TButton icon={<Library />} label="Librería" title="Manage the fixture library (§3.2)" onClick={p.onLibrary} />
        <TButton icon={<Trash />} label="Eliminar" onClick={p.onDelete} disabled={!p.canDelete} />
      </div>

      {/* Edit */}
      <div className="pr-toolbar-group sep">
        <TButton icon={<Undo />} label="Deshacer" onClick={p.onUndo} disabled={!p.canUndo} />
        <TButton icon={<Redo />} label="Rehacer" onClick={p.onRedo} disabled={!p.canRedo} />
      </div>

      {/* View modes (§5.3) */}
      <div className="pr-toolbar-group sep">
        {MODES.map((m) => (
          <TButton
            key={m.id}
            icon={m.icon}
            label={m.label}
            title={m.title}
            onClick={() => p.onViewMode(m.id)}
            active={p.viewMode === m.id}
          />
        ))}
      </div>

      {/* Highlight on the real rig (§7) */}
      <div className="pr-toolbar-group sep">
        {HIGHLIGHT_MODES.map((m) => (
          <TButton
            key={m.id}
            icon={m.icon}
            label={m.label}
            title={m.title}
            onClick={() => p.onHighlightMode(m.id)}
            active={p.highlightMode === m.id}
          />
        ))}
      </div>
    </div>
  );
}

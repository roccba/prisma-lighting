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
}

export function Toolbar({ patch, activeZone, onActiveZone, viewMode, onViewMode }: Props) {
  const active = patch.zones.find((z) => z.id === activeZone);
  return (
    <div className="toolbar">
      <label className="tb-group">
        <span className="tb-label">Zone</span>
        <span className="tb-select">
          {active && (
            <span
              className="swatch"
              style={{ background: `rgb(${active.display_color.join(",")})` }}
            />
          )}
          <select value={activeZone} onChange={(e) => onActiveZone(e.target.value)}>
            {patch.zones.map((z) => (
              <option key={z.id} value={z.id}>
                {z.name}
              </option>
            ))}
          </select>
        </span>
      </label>

      <div className="tb-group">
        <span className="tb-label">View</span>
        <div className="segmented">
          {MODES.map((m) => (
            <button
              key={m.id}
              title={m.title}
              className={m.id === viewMode ? "seg active" : "seg"}
              onClick={() => onViewMode(m.id)}
            >
              {m.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

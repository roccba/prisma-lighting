import { useContext, useEffect, useRef, useState } from "react";
import { PanelCellContext } from "./panelCell";
import { setParam, type ParamInfo } from "./api";

// the classic direction control for movement effects (Scroll de color): the 3×3 rose (eight
// arrows + ✕ in the centre) for the common directions, and below it a button showing the
// current one that opens a CASCADING menu with every mode, grouped
// (Lineal ▸ / Lineal a 2 vías ▸ / Forma 2D ▸ / …).
// Data-driven: the choices come from the manifest as "Group|Label" (index order = the
// value); the rose's buttons pick choices by label. Nothing here is effect-specific.

interface DirectionMenuProps {
  zoneId: string;
  param: ParamInfo;
}

/** The rose: [arrow glyph, choice label] in reading order; the centre is "Cruz". */
const ROSE: [string, string][] = [
  ["↖", "Superior izquierda"],
  ["↑", "Superior"],
  ["↗", "Superior derecha"],
  ["←", "Izquierda"],
  ["✕", "Cruz"],
  ["→", "Derecha"],
  ["↙", "Inferior izquierda"],
  ["↓", "Inferior"],
  ["↘", "Inferior derecha"],
];

const ARROW: Record<string, string> = Object.fromEntries(ROSE.map(([g, l]) => [l, g]));

/** A small glyph for any mode label (arrows for planar directions, symbols otherwise). */
function glyph(label: string): string {
  if (ARROW[label]) return ARROW[label];
  if (label === "Frontal") return "●";
  if (label === "Posterior") return "○";
  if (label.startsWith("Frontal") || label.startsWith("Posterior")) {
    const rest = label.replace(/^(Frontal|Posterior)\s*/, "");
    const planar = rest.charAt(0).toUpperCase() + rest.slice(1);
    return ARROW[planar] ?? (label.startsWith("Frontal") ? "●" : "○");
  }
  if (label.startsWith("Implosionar")) return "⇥";
  if (label.startsWith("Explosionar")) return "⇤";
  return "•";
}

export function DirectionMenu({ zoneId, param }: DirectionMenuProps) {
  const cell = useContext(PanelCellContext);
  const choices = param.choices ?? [];
  const steps = Math.max(1, choices.length - 1);
  const idxOf = (norm: number) => Math.min(choices.length - 1, Math.max(0, Math.round(norm * steps)));
  const [idx, setIdx] = useState(idxOf(param.value_norm));
  useEffect(() => setIdx(idxOf(param.value_norm)), [param.value_norm]);

  const parsed = choices.map((c) => {
    const bar = c.indexOf("|");
    return { group: bar >= 0 ? c.slice(0, bar) : "", label: bar >= 0 ? c.slice(bar + 1) : c };
  });
  const pick = (i: number) => {
    setIdx(i);
    setParam(zoneId, param.name, i / steps, cell).catch(() => {});
  };
  const byLabel = (label: string) => parsed.findIndex((p) => p.label === label);
  const current = parsed[idx];

  // The cascading menu: groups in first-appearance order; ungrouped choices are items.
  const groups: { name: string; items: number[] }[] = [];
  const loose: number[] = [];
  parsed.forEach((p, i) => {
    if (!p.group) return loose.push(i);
    let g = groups.find((x) => x.name === p.group);
    if (!g) groups.push((g = { name: p.group, items: [] }));
    g.items.push(i);
  });

  const btnRef = useRef<HTMLButtonElement>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const [openGroup, setOpenGroup] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menuAt) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current?.contains(e.target as Node) || btnRef.current?.contains(e.target as Node)) return;
      setMenuAt(null);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMenuAt(null);
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuAt]);

  const choose = (i: number) => {
    pick(i);
    setMenuAt(null);
    setOpenGroup(null);
  };

  // A submenu next to its group row, kept inside the window (it can be tall).
  const [subTop, setSubTop] = useState(0);
  const ITEM_H = 26;

  return (
    <div className="pr-dirmenu">
      <div className="pr-dirmenu-rose">
        {ROSE.map(([g, label]) => {
          const i = byLabel(label);
          return (
            <button
              key={label}
              type="button"
              className={`pr-btn${i === idx ? " is-active" : ""}`}
              disabled={i < 0}
              title={label}
              onClick={() => i >= 0 && pick(i)}
            >
              {g}
            </button>
          );
        })}
      </div>
      <button
        ref={btnRef}
        type="button"
        className={`pr-btn pr-dirmenu-current${menuAt ? " is-active" : ""}`}
        onClick={() => {
          const r = btnRef.current!.getBoundingClientRect();
          setMenuAt((cur) => (cur ? null : { x: r.left, y: r.bottom + 2 }));
          setOpenGroup(null);
        }}
      >
        <span className="pr-dirmenu-glyph">{current ? glyph(current.label) : ""}</span>
        <span className="pr-dirmenu-label">{current?.label ?? ""}</span>
      </button>

      {menuAt && (
        <div className="pr-dirmenu-pop" ref={menuRef} style={{ left: menuAt.x, top: menuAt.y }}>
          {groups.map((g) => (
            <div
              key={g.name}
              className={`pr-dirmenu-item is-group${openGroup === g.name || g.items.includes(idx) ? " is-current" : ""}${openGroup === g.name ? " is-open" : ""}`}
              onMouseEnter={(e) => {
                setOpenGroup(g.name);
                const r = e.currentTarget.getBoundingClientRect();
                const h = g.items.length * ITEM_H + 8;
                // Relative to this group row (the submenu is positioned inside it).
                setSubTop(Math.max(4, Math.min(r.top, window.innerHeight - h - 4)) - r.top - 4);
              }}
            >
              <span>{g.name}</span>
              <span className="pr-dirmenu-caret">▶</span>
              {openGroup === g.name && (
                <div className="pr-dirmenu-sub" style={{ top: subTop }}>
                  {g.items.map((i) => (
                    <div
                      key={i}
                      className={`pr-dirmenu-item${i === idx ? " is-sel" : ""}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        choose(i);
                      }}
                    >
                      <span className="pr-dirmenu-glyph">{glyph(parsed[i].label)}</span>
                      {parsed[i].label}
                    </div>
                  ))}
                </div>
              )}
            </div>
          ))}
          {loose.map((i) => (
            <div
              key={i}
              className={`pr-dirmenu-item${i === idx ? " is-sel" : ""}`}
              onMouseEnter={() => setOpenGroup(null)}
              onClick={() => choose(i)}
            >
              <span>{parsed[i].label}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

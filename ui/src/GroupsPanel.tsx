import { useRef, useState } from "react";
import type { EditorState, Group, Patch, Rgb } from "./api";
import { addGroup, removeGroup, renameGroup, setGroupColor, setGroupMembers } from "./api";
import { RenameInput } from "./cellParts";
import { ColorPicker } from "./ColorPicker";
import { ArrowDown, ArrowUp, Minus, Pencil, Plus } from "./icons";
import { Splitter, loadPaneHeight, savePaneHeight } from "./Splitter";

const GROUPS_H_KEY = "prisma.editor.groupsHeight";

// Left sector "Grupos" (LIVE-MODEL-SPEC §3.1, phase 5 step 1). A group is an ordered set of
// fixtures, saved in the patch; it can span zones. Created from the current selection, in
// the order the fixtures were selected in (that order is the element order of chasers,
// phase 6). Picking a group selects its fixtures on the canvas; its members are listed
// below, numbered, and can be added, removed and reordered from the selection. Every edit
// is undoable. The sector height is user-draggable, as before.

interface Props {
  patch: Patch;
  selection: Set<string>;
  onSelectionChange: (next: Set<string>) => void;
  onEditor: (s: EditorState) => void;
  onError: (msg: string) => void;
}

const css = (c: Rgb) => `rgb(${c.join(",")})`;

/** Move the members in `picked` one place earlier (`-1`) or later (`+1`), keeping their
 *  relative order; a block already at the edge stays. */
export function moveMembers(members: string[], picked: Set<string>, dir: -1 | 1): string[] {
  const out = [...members];
  const idx = out.map((m, i) => (picked.has(m) ? i : -1)).filter((i) => i >= 0);
  if (idx.length === 0) return out;
  if (dir < 0 ? idx[0] === 0 : idx[idx.length - 1] === out.length - 1) return out;
  for (const i of dir < 0 ? idx : [...idx].reverse()) {
    [out[i], out[i + dir]] = [out[i + dir], out[i]];
  }
  return out;
}

export function GroupsPanel(p: Props) {
  const { patch, selection } = p;
  const groups: Group[] = patch.groups ?? [];
  const [h, setH] = useState(() => loadPaneHeight(GROUPS_H_KEY, 300));
  const changeH = (v: number) => {
    setH(v);
    savePaneHeight(GROUPS_H_KEY, v);
  };
  const [activeId, setActiveId] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const active = groups.find((g) => g.id === activeId) ?? null;
  // The app's colour selector, open on one group's swatch. The swatch follows the drag
  // live; closing stores the colour once (one undo step). The ref holds the latest pick,
  // so a pick and a close in the same gesture are not lost.
  const [picker, setPicker] = useState<{ group: string; anchor: DOMRect; color: Rgb } | null>(null);
  const picked = useRef<Rgb | null>(null);
  const closePicker = () => {
    const open = picker;
    const color = picked.current;
    setPicker(null);
    picked.current = null;
    const g = open && groups.find((x) => x.id === open.group);
    if (g && color && color.join() !== g.display_color.join()) void run(() => setGroupColor(g.id, color));
  };

  const run = async (op: () => Promise<EditorState>) => {
    try {
      p.onEditor(await op());
    } catch (e) {
      p.onError(String(e));
    }
  };

  const pick = (g: Group) => {
    setActiveId(g.id);
    p.onSelectionChange(new Set(g.members));
  };
  const create = async () => {
    try {
      const [s, id] = await addGroup([...selection]);
      p.onEditor(s);
      setActiveId(id);
      setRenaming(id);
    } catch (e) {
      p.onError(String(e));
    }
  };
  const remove = () => {
    if (!active) return;
    setActiveId(null);
    void run(() => removeGroup(active.id));
  };
  const addSelection = () => {
    if (!active) return;
    const extra = [...selection].filter((id) => !active.members.includes(id));
    if (extra.length) void run(() => setGroupMembers(active.id, [...active.members, ...extra]));
  };
  const removeSelection = () => {
    if (!active) return;
    const kept = active.members.filter((id) => !selection.has(id));
    if (kept.length !== active.members.length) void run(() => setGroupMembers(active.id, kept));
  };
  const move = (dir: -1 | 1) => {
    if (!active) return;
    const next = moveMembers(active.members, selection, dir);
    if (next.some((m, i) => m !== active.members[i])) void run(() => setGroupMembers(active.id, next));
  };

  const fixtureName = new Map(patch.fixtures.map((f) => [f.id, f.name]));
  const zoneOf = new Map(patch.fixtures.map((f) => [f.id, f.zone]));
  const zoneName = new Map(patch.zones.map((z) => [z.id, z.name]));
  const selectedMembers = active ? active.members.filter((m) => selection.has(m)).length : 0;
  const newSelected = active ? [...selection].filter((id) => !active.members.includes(id)).length : 0;

  const clickMember = (id: string, e: React.MouseEvent) => {
    const next = new Set(selection);
    if (e.shiftKey || e.ctrlKey) {
      if (next.has(id)) next.delete(id);
      else next.add(id);
    } else {
      next.clear();
      next.add(id);
    }
    p.onSelectionChange(next);
  };

  return (
    <>
      <Splitter value={h} onChange={changeH} min={44} max={640} invert />
      <section className="left-sector groups-sector" style={{ height: h, flex: "0 0 auto" }}>
        <div className="sector-head">
          <span className="sector-title">Grupos</span>
          <div className="sector-actions">
            <button
              className="pr-btn pr-btn-icon"
              title={selection.size ? `Crear grupo con la selección (${selection.size})` : "Crear grupo vacío"}
              aria-label="Crear grupo"
              onClick={create}
            >
              <Plus size={15} />
            </button>
            <button
              className="pr-btn pr-btn-icon"
              title="Renombrar grupo"
              aria-label="Renombrar grupo"
              disabled={!active}
              onClick={() => active && setRenaming(active.id)}
            >
              <Pencil size={15} />
            </button>
            <button
              className="pr-btn pr-btn-icon"
              title="Eliminar grupo (las instalaciones no se tocan)"
              aria-label="Eliminar grupo"
              disabled={!active}
              onClick={remove}
            >
              <Minus size={15} />
            </button>
          </div>
        </div>

        <div className="sector-scroll group-list">
          {groups.map((g) => (
            <div
              key={g.id}
              className={`pr-listrow${g.id === activeId ? " is-sel" : ""}`}
              role="button"
              data-group={g.id}
              title={g.name}
              onClick={() => pick(g)}
              onDoubleClick={() => setRenaming(g.id)}
            >
              <button
                type="button"
                className="zc group-swatch"
                style={{ background: css(picker?.group === g.id ? picker.color : g.display_color) }}
                title="Color del grupo"
                aria-label={`Color de ${g.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  picked.current = null;
                  setPicker({ group: g.id, anchor: e.currentTarget.getBoundingClientRect(), color: g.display_color });
                }}
              />
              {renaming === g.id ? (
                <RenameInput
                  initial={g.name}
                  label="Nombre del grupo"
                  onCommit={(name) => {
                    setRenaming(null);
                    if (name.trim() !== g.name) void run(() => renameGroup(g.id, name));
                  }}
                  onCancel={() => setRenaming(null)}
                />
              ) : (
                <span className="nm">{g.name}</span>
              )}
              <span className="meta">{g.members.length}</span>
            </div>
          ))}
          {groups.length === 0 && (
            <div className="sector-empty">Sin grupos. Seleccioná instalaciones y creá uno con +.</div>
          )}
        </div>

        {active && (
          <>
            <div className="sub-head">
              Miembros · {active.name}
              <span className="sub-count">{active.members.length}</span>
            </div>
            <div className="group-member-actions">
              <button
                className="pr-btn"
                title="Agregar al final las instalaciones seleccionadas que no están en el grupo"
                disabled={newSelected === 0}
                onClick={addSelection}
              >
                <Plus size={13} /> Selección
              </button>
              <button
                className="pr-btn"
                title="Quitar del grupo las instalaciones seleccionadas"
                disabled={selectedMembers === 0}
                onClick={removeSelection}
              >
                <Minus size={13} /> Selección
              </button>
              <button
                className="pr-btn pr-btn-icon"
                title="Mover la selección antes en el orden"
                aria-label="Subir"
                disabled={selectedMembers === 0}
                onClick={() => move(-1)}
              >
                <ArrowUp size={14} />
              </button>
              <button
                className="pr-btn pr-btn-icon"
                title="Mover la selección después en el orden"
                aria-label="Bajar"
                disabled={selectedMembers === 0}
                onClick={() => move(1)}
              >
                <ArrowDown size={14} />
              </button>
            </div>
            <div className="sector-scroll group-members">
              {active.members.map((id, i) => (
                <div
                  key={id}
                  className={`pr-listrow${selection.has(id) ? " is-sel" : ""}`}
                  data-member={id}
                  onClick={(e) => clickMember(id, e)}
                >
                  <span className="meta group-pos">{i + 1}</span>
                  <span className="nm">{fixtureName.get(id) ?? id}</span>
                  <span className="meta">{zoneName.get(zoneOf.get(id) ?? "") ?? ""}</span>
                </div>
              ))}
              {active.members.length === 0 && <div className="sector-empty">Grupo vacío.</div>}
            </div>
          </>
        )}
      </section>
      {picker && (
        <ColorPicker
          anchor={picker.anchor}
          value={picker.color}
          onChange={(color) => {
            picked.current = color;
            setPicker((p) => (p ? { ...p, color } : p));
          }}
          onClose={closePicker}
        />
      )}
    </>
  );
}

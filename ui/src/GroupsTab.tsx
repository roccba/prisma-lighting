import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  addGroupPreset,
  applyGroupPreset,
  deleteGroupPreset,
  mainGroups,
  renameGroupPreset,
  rerecordGroupPreset,
  setGroupLevel,
  setGroupPresetFade,
  type GroupsInfo,
  type GroupPresetInfo,
} from "./api";
import { CellMenu, RenameInput, type MenuItem } from "./cellParts";
import { LearnTarget } from "./midiLearn";
import { scrubHandler } from "./scrub";
import { VFader } from "./VFader";

// The Grupos tab of the bottom panel (LIVE-MODEL-SPEC §8.5 step 4): one fader per group
// master (manual dimmers, applied at the output stage) and the group presets as buttons.
// A click on a preset applies it with its fade; right-click renames it, sets its fade,
// re-records it from the current masters or deletes it. "+ Preset" stores the current
// masters. Every fader and preset is a MIDI Learn target (its tree path).

/** The fades a preset's menu offers, in seconds (0 = cut). */
export const PRESET_FADES = [0, 0.5, 1, 2, 5];

export function fadeLabel(s: number): string {
  if (s <= 0) return "corte";
  return `${String(s).replace(".", ",")} s`;
}

const EMPTY: GroupsInfo = { groups: [], presets: [] };

export function GroupsTab({ onError }: { onError: (msg: string) => void }) {
  const [info, setInfo] = useState<GroupsInfo>(EMPTY);
  const [menu, setMenu] = useState<{ preset: GroupPresetInfo; x: number; y: number } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);

  const refresh = useCallback(() => {
    mainGroups()
      .then(setInfo)
      .catch(() => {});
  }, []);
  // A preset's fade moves the masters in the engine with no event of its own. While one
  // may be running — after a preset is applied here or from a controller — the faders
  // follow it (10 reads a second, only until the longest fade is over); otherwise
  // nothing is read until something changes.
  const follow = useRef<{ timer?: number; until: number }>({ until: 0 });
  const longestFade = useRef(0);
  longestFade.current = Math.max(0, ...info.presets.map((p) => p.fade));
  const followFades = useCallback(() => {
    const f = follow.current;
    f.until = Date.now() + longestFade.current * 1000 + 300;
    if (f.timer !== undefined) return;
    f.timer = window.setInterval(() => {
      refresh();
      if (Date.now() > f.until) {
        window.clearInterval(f.timer);
        f.timer = undefined;
      }
    }, 100);
  }, [refresh]);
  // Seeded from the engine and re-read when a controller moved something (a mapped
  // master, a preset pad) or the show / presets changed.
  useEffect(() => {
    refresh();
    const subs = [
      listen("midi-applied", () => {
        refresh();
        followFades();
      }),
      listen("layout-changed", refresh),
      listen("groups-changed", refresh),
    ];
    const f = follow.current;
    return () => {
      if (f.timer !== undefined) window.clearInterval(f.timer);
      f.timer = undefined;
      subs.forEach((un) => un.then((g) => g()).catch(() => {}));
    };
  }, [refresh, followFades]);

  const run = (p: Promise<unknown>) => p.then(refresh).catch((e) => onError(String(e)));

  const changeLevel = (id: string, level: number) => {
    setInfo((i) => ({ ...i, groups: i.groups.map((g) => (g.id === id ? { ...g, level } : g)) }));
    setGroupLevel(id, level).catch(() => {});
  };

  const presetMenu = (p: GroupPresetInfo): MenuItem[] => [
    { label: "Renombrar", onSelect: () => setRenaming(p.id) },
    { label: "Regrabar con los niveles actuales", onSelect: () => run(rerecordGroupPreset(p.id)) },
    "sep",
    ...PRESET_FADES.map((s) => ({
      label: `${s === p.fade ? "✓ " : ""}Fundido: ${fadeLabel(s)}`,
      onSelect: () => run(setGroupPresetFade(p.id, s)),
    })),
    "sep",
    { label: "Eliminar", onSelect: () => run(deleteGroupPreset(p.id)) },
  ];

  return (
    <div className="pr-groupstab">
      <div className="pr-groupstab-masters" role="group" aria-label="Masters de grupo">
        {info.groups.map((g) => (
          <div key={g.id} className="pr-groupstrip" data-group={g.id} title={g.name}>
            <span className="pr-groupstrip-color" style={{ background: `rgb(${g.color.join(",")})` }} />
            <span className="pr-groupstrip-name">{g.name}</span>
            <LearnTarget target={{ path: `/group/${g.id}/dimmer` }} name={`Master ${g.name}`}>
              <div className="pr-groupstrip-fader">
                <VFader value={g.level} onChange={(v) => changeLevel(g.id, v)} aria-label={`Master ${g.name}`} />
              </div>
            </LearnTarget>
            <span
              className="pr-value pr-groupstrip-value"
              onPointerDown={scrubHandler(() => ({
                get: () => g.level,
                set: (v: number) => changeLevel(g.id, v),
                min: 0,
                max: 1,
                perPixel: 1 / 255,
                step: 1 / 255,
              }))}
            >
              {Math.round(g.level * 255)}
            </span>
          </div>
        ))}
        {info.groups.length === 0 && (
          <div className="pr-placeholder-body">Sin grupos. Se crean en el editor de patch (F3).</div>
        )}
      </div>

      <div className="pr-groupstab-presets">
        <div className="pr-groupstab-head">
          <span className="pr-groupstab-title">Presets</span>
          <button
            type="button"
            className="pr-btn"
            disabled={info.groups.length === 0}
            title="Guardar los niveles actuales de los masters como un preset"
            onClick={() => run(addGroupPreset())}
          >
            + Preset
          </button>
        </div>
        <div className="pr-groupstab-list">
          {info.presets.map((p, i) =>
            renaming === p.id ? (
              <RenameInput
                key={p.id}
                initial={p.name}
                label="Nombre del preset"
                onCommit={(name) => {
                  setRenaming(null);
                  if (name.trim() && name.trim() !== p.name) run(renameGroupPreset(p.id, name));
                }}
                onCancel={() => setRenaming(null)}
              />
            ) : (
              <LearnTarget key={p.id} target={{ path: `/group-preset/${i}/apply` }} name={`Preset ${p.name}`} compact>
                <button
                  type="button"
                  className="pr-btn pr-preset-btn"
                  data-preset={p.id}
                  title={`${p.name} — fundido ${fadeLabel(p.fade)}. Clic derecho: renombrar, fundido, regrabar, eliminar`}
                  onClick={() => {
                    run(applyGroupPreset(p.id));
                    followFades();
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setMenu({ preset: p, x: e.clientX, y: e.clientY });
                  }}
                >
                  <span className="pr-preset-name">{p.name}</span>
                  <span className="pr-preset-fade">{fadeLabel(p.fade)}</span>
                </button>
              </LearnTarget>
            ),
          )}
          {info.presets.length === 0 && info.groups.length > 0 && (
            <div className="pr-groupstab-hint">Ajustá los masters y guardalos con + Preset.</div>
          )}
        </div>
      </div>

      {menu && (
        <CellMenu
          key={`${menu.preset.id}:${menu.x}:${menu.y}`}
          x={menu.x}
          y={menu.y}
          items={presetMenu(menu.preset)}
          onClose={() => setMenu(null)}
        />
      )}
    </div>
  );
}

import { PANEL_TAB_KEYS, PANEL_TABS } from "./panelTabs";
import { useEffect, useState } from "react";
import { controlKeys, controlResetKeys, controlSetKey, type KeyBinding, type KeyCombo } from "./api";
import { comboLabel, comboOf, isModifierOnly, isReserved } from "./keyboard";

// "Control" (LIVE-MODEL-SPEC §8.2): the operator's keyboard shortcuts. They are stored with
// the controller mappings — they follow the operator, not the show. Each action is a tree
// path; click its key to capture a new one (any key, with Ctrl / Alt / Mayús), "Quitar" to
// leave it without a key, "Restaurar" to go back to the defaults. Giving an action a key
// another one had moves it. The menu's keys (F3, F4, F7–F11, Ctrl+N/O/S/M) are refused.
// Shortcuts never fire while typing in a text or numeric field.

interface Action {
  path: string;
  label: string;
}

const ACTIONS: Action[] = [
  { path: "/arm/go", label: "GO" },
  { path: "/arm/clear", label: "Desarmar" },
  { path: "/tracks/stop", label: "Stop todo" },
  { path: "/master/blackout", label: "Blackout" },
  ...Array.from({ length: 9 }, (_, i) => ({ path: `/scene/${i}/launch`, label: `Escena ${i + 1}` })),
  { path: "/select/move/up", label: "Selección: arriba" },
  { path: "/select/move/down", label: "Selección: abajo" },
  { path: "/select/move/left", label: "Selección: track anterior" },
  { path: "/select/move/right", label: "Selección: track siguiente" },
  { path: "/select/launch", label: "Lanzar la celda seleccionada" },
  { path: "/select/arm", label: "Armar la celda seleccionada" },
  { path: "ui:panel", label: "Mostrar / ocultar el panel de parámetros" },
  ...Array.from({ length: PANEL_TAB_KEYS }, (_, i) => ({
    path: `ui:tab/${i + 1}`,
    label: `Panel: pestaña ${i + 1}${PANEL_TABS[i] ? ` (${PANEL_TABS[i].label})` : ""}`,
  })),
];

export function ControlDialog({ onClose }: { onClose: () => void }) {
  const [keys, setKeys] = useState<KeyBinding[]>([]);
  const [capturing, setCapturing] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    controlKeys()
      .then(setKeys)
      .catch(() => {});
  }, []);

  const save = (path: string, combo: KeyCombo | null) => {
    setCapturing(null);
    controlSetKey(path, combo)
      .then((k) => {
        setKeys(k);
        setMessage(null);
      })
      .catch((e) => setMessage(String(e)));
  };

  // While capturing, the next key (with its modifiers) is the new shortcut — Esc included,
  // since it is a bindable key; a click anywhere else cancels. Otherwise Esc closes.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!capturing) {
        if (e.key === "Escape") onClose();
        return;
      }
      if (isModifierOnly(e)) return;
      e.preventDefault();
      e.stopPropagation();
      const combo = comboOf(e);
      if (isReserved(combo)) {
        setMessage(`${comboLabel(combo)} la usa el menú; elegí otra tecla.`);
        return;
      }
      save(capturing, combo);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, onClose]);

  const keyOf = (path: string) => keys.find((k) => k.path === path)?.combo ?? null;
  // Shortcuts to other paths (a profile edited by hand) are listed too.
  const extra = keys.filter((k) => !ACTIONS.some((a) => a.path === k.path)).map((k) => ({ path: k.path, label: k.path }));

  return (
    <div className="pr-picker-backdrop" onPointerDown={onClose}>
      <div
        className="pr-picker pr-control"
        role="dialog"
        aria-label="Control"
        onPointerDown={(e) => {
          e.stopPropagation();
          if (!(e.target as HTMLElement).closest(".pr-keycap")) setCapturing(null);
        }}
      >
        <div className="pr-picker-head">Control</div>
        <div className="pr-devmgr-tabs">
          <button className="pr-wtab is-active" type="button">
            Teclado
          </button>
        </div>

        <div className="pr-control-body">
          <table className="pr-table pr-control-table">
            <thead>
              <tr>
                <th>Acción</th>
                <th>Tecla</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {[...ACTIONS, ...extra].map((a) => {
                const combo = keyOf(a.path);
                const isCapturing = capturing === a.path;
                return (
                  <tr key={a.path}>
                    <td>{a.label}</td>
                    <td>
                      <button
                        type="button"
                        className={`pr-btn pr-keycap${isCapturing ? " is-active" : ""}`}
                        onClick={() => setCapturing(isCapturing ? null : a.path)}
                        title="Clic y pulsá la tecla nueva"
                      >
                        {isCapturing ? "Pulsá una tecla…" : combo ? comboLabel(combo) : "—"}
                      </button>
                    </td>
                    <td>
                      {combo && (
                        <button type="button" className="pr-btn" onClick={() => save(a.path, null)} title="Dejar la acción sin tecla">
                          Quitar
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="pr-artnet-note">
            Los atajos no se disparan mientras escribís en un campo de texto o numérico. F3, F4, F7–F11 y Ctrl+N/O/S/M
            quedan para el menú.
          </div>
          {message && <div className="pr-artnet-warn">{message}</div>}
        </div>

        <div className="pr-devmgr-foot">
          <button
            className="pr-btn"
            type="button"
            onClick={() =>
              controlResetKeys()
                .then((k) => {
                  setKeys(k);
                  setMessage(null);
                })
                .catch((e) => setMessage(String(e)))
            }
          >
            Restaurar predeterminados
          </button>
          <span className="pr-devmgr-footstatus" />
          <button className="pr-btn" type="button" onClick={onClose}>
            Cerrar
          </button>
        </div>
      </div>
    </div>
  );
}

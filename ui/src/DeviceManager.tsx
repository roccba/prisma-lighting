import { useEffect, useState } from "react";
import { midiReconnect, midiSetInput, midiStatus, type MidiStatus, type ZoneInfo } from "./api";
import { ArtNetTab } from "./ArtNetTab";
import { Check } from "./Check";
import { Search } from "./icons";

// The device manager, modelled on professional consoles (docs/DEVICES-SPEC.md §9). The Art-Net tab
// (`ArtNetTab`) lists the show's devices — one row per node / BindIndex group — with
// Scan, staged edits and Aceptar / Aplicar / Cancelar.
//
// The MIDI tab mirrors the classic port table: each MIDI input, and which one is the
// "Remoto" (the device that controls Prisma). Only device configuration lives here —
// controls are mapped and unmapped with MIDI Learn on the main window (right-click a
// learned control), never from a list, so an assignment cannot be removed by accident.
// Changes apply immediately (they do not recompile the engine).

interface DeviceManagerProps {
  onClose: () => void;
  /** The running show's zones (kept for future per-zone MIDI views). */
  zones: ZoneInfo[];
}

function MidiTab({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<MidiStatus | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    midiStatus().then(setStatus).catch(() => {});
  }, []);

  const run = async (op: () => Promise<MidiStatus>) => {
    setBusy(true);
    try {
      setStatus(await op());
    } catch (e) {
      setStatus((s) => (s ? { ...s, error: String(e) } : s));
    } finally {
      setBusy(false);
    }
  };

  // Rows: every input port, plus the configured one if it is unplugged right now.
  const ports = status ? [...status.ports] : [];
  if (status?.selected && !ports.includes(status.selected)) ports.push(status.selected);
  const auto = status?.selected == null;

  return (
    <>
      <div className="pr-midi">
        <div className="pr-artnet-main">
          <div className="pr-artnet-tablewrap">
            <table className="pr-table pr-artnet-table">
              <thead>
                <tr>
                  <th>Puerto</th>
                  <th>Nombre del dispositivo</th>
                  <th className="col-on">Remoto</th>
                  <th>Estado</th>
                  <th>Feedback (LEDs)</th>
                </tr>
              </thead>
              <tbody>
                {ports.map((p, i) => {
                  const connected = status?.connected === p;
                  const remote = auto ? connected : status?.selected === p;
                  return (
                    <tr key={p} className={status?.ports.includes(p) ? "" : "is-off"}>
                      <td>Entrada {i + 1}</td>
                      <td>{p}</td>
                      <td className="col-on">
                        <Check checked={remote} disabled={busy} onChange={(v) => v && run(() => midiSetInput(p))} />
                      </td>
                      <td>{connected ? "Conectado" : status?.ports.includes(p) ? "—" : "Desconectado"}</td>
                      <td>{connected ? (status?.output ?? "Sin salida") : ""}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {status && ports.length === 0 && <div className="pr-artnet-empty">No hay dispositivos MIDI conectados.</div>}
          </div>
        </div>
        <div className="pr-artnet-side">
          <Check checked={auto} disabled={busy} onChange={(v) => v && run(() => midiSetInput(null))}>
            Elegir automáticamente
          </Check>
          <div className="pr-artnet-note">
            Con esta opción, Prisma usa el primer controlador conocido que encuentre (APC40).
          </div>
          <div className="pr-artnet-messages">
            {status?.connected && <div className="pr-artnet-note">Perfil: {status.device}</div>}
            {status?.error && <div className="pr-artnet-warn">{status.error}</div>}
            <div className="pr-artnet-note">Las asignaciones se hacen y se quitan con MIDI Learn (Ctrl+M) en la ventana principal.</div>
          </div>
          <div className="pr-artnet-order">
            <button className="pr-btn" type="button" disabled={busy} onClick={() => run(midiReconnect)} title="Buscar dispositivos MIDI y reconectar">
              <Search size={15} /> Buscar
            </button>
          </div>
        </div>
      </div>
      <div className="pr-devmgr-foot">
        <span className="pr-devmgr-footstatus" />
        <button className="pr-btn" type="button" onClick={onClose}>
          Cerrar
        </button>
      </div>
    </>
  );
}

export function DeviceManager({ onClose }: DeviceManagerProps) {
  const [tab, setTab] = useState<"artnet" | "midi">("artnet");
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Escape closes the manager — but not from under an open sub-dialog (Configurar,
      // Agregar, Scan), which would drop the staged edits with it.
      if (e.key === "Escape" && !document.querySelector(".pr-subdialog")) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="pr-picker-backdrop" onPointerDown={onClose}>
      <div className="pr-picker pr-devmgr" onPointerDown={(e) => e.stopPropagation()}>
        <div className="pr-picker-head">Gestor de dispositivos</div>

        <div className="pr-devmgr-tabs">
          <button className={`pr-wtab${tab === "artnet" ? " is-active" : ""}`} type="button" onClick={() => setTab("artnet")}>
            Art-Net
          </button>
          <button className={`pr-wtab${tab === "midi" ? " is-active" : ""}`} type="button" onClick={() => setTab("midi")}>
            MIDI
          </button>
        </div>

        {tab === "midi" ? (
          <MidiTab onClose={onClose} />
        ) : (
          <ArtNetTab onClose={onClose} />
        )}
      </div>
    </div>
  );
}

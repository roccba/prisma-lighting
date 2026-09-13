import { useEffect, useState } from "react";
import {
  controlLabel,
  midiReconnect,
  midiSetInput,
  midiStatus,
  midiUnmap,
  networkGet,
  networkSet,
  type MapTarget,
  type MidiStatus,
  type NetworkConfig,
  type ZoneInfo,
} from "./api";
import { Dropdown } from "./Dropdown";

// The device manager: where the engine sends DMX. First cut is the
// Art-Net tab — enable it and point it at a node IP or a subnet broadcast, Apply
// recompiles the engine onto that target (it was going to the loopback virtual sink).
// The per-node universe table is a later feature; today every universe goes to one
// target (a node that takes them all, or broadcast).
//
// The MIDI tab (M9) picks the input device (auto-detects the APC40 by default), reconnects
// after a late hot-plug, and lists the controller profile's mappings. Changes there apply
// immediately — they do not recompile the engine, so no Apply step.

interface DeviceManagerProps {
  onClose: () => void;
  /** The running show's zones in slot order, to name zone-slot mappings. */
  zones: ZoneInfo[];
}

const AUTO = "__auto__";

const SUB_LABEL: Record<string, string> = {
  dimmer: "Dimmer",
  speed: "Velocidad",
  solo: "Solo",
  blackout: "Mute",
};

function targetLabel(t: MapTarget, zones: ZoneInfo[]): string {
  if ("path" in t) return t.path === "/master/dimmer" ? "Master dimmer" : t.path;
  const { slot, sub } = t.zone_slot;
  const zone = zones[slot]?.name ?? `Zona ${slot + 1} (no está en este show)`;
  const what = typeof sub === "object" ? `Celda ${sub.cell + 1}` : (SUB_LABEL[sub] ?? sub);
  return `${what} — ${zone}`;
}

function MidiTab({ zones }: { zones: ZoneInfo[] }) {
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

  if (!status) return <div className="pr-devmgr-hint">Cargando…</div>;

  const options = [
    { value: AUTO, label: "Automático (APC40)" },
    ...status.ports.map((p) => ({ value: p, label: p })),
  ];
  // A configured port that is currently unplugged still shows as the selection.
  if (status.selected && !status.ports.includes(status.selected)) {
    options.push({ value: status.selected, label: `${status.selected} (desconectado)` });
  }

  return (
    <>
      <div className="pr-devmgr-field">
        <span className="pr-devmgr-label">Entrada</span>
        <div className="pr-devmgr-select">
          <Dropdown
            value={status.selected ?? AUTO}
            options={options}
            disabled={busy}
            onChange={(v) => run(() => midiSetInput(v === AUTO ? null : v))}
            aria-label="Entrada MIDI"
          />
        </div>
        <button className="pr-btn" type="button" disabled={busy} onClick={() => run(midiReconnect)}>
          Reconectar
        </button>
      </div>

      <div className={`pr-devmgr-status${status.connected ? " is-ok" : ""}`}>
        {status.connected ? `Conectado: ${status.connected}` : "Sin dispositivo MIDI conectado"}
      </div>
      {status.connected && (
        <div className="pr-devmgr-status">
          {status.output
            ? `Perfil: ${status.device} · LEDs por ${status.output}`
            : `Perfil: ${status.device} · sin salida (no hay feedback de LEDs)`}
        </div>
      )}
      {status.error && <div className="pr-devmgr-error">Error: {status.error}</div>}

      <div className="pr-devmgr-label">Asignaciones ({status.mappings.length})</div>
      <div className="pr-devmgr-maps">
        {status.mappings.length === 0 ? (
          <div className="pr-devmgr-hint">Ninguna. Usá MIDI Learn (Ctrl+M) en la ventana principal.</div>
        ) : (
          status.mappings.map((m) => (
            <div className="pr-devmgr-map" key={JSON.stringify(m.target)}>
              <code>{controlLabel(m.control)}</code>
              <span>{targetLabel(m.target, zones)}</span>
              <button
                type="button"
                title="Quitar asignación"
                onClick={() =>
                  midiUnmap(m.target)
                    .then((mappings) => setStatus((s) => (s ? { ...s, mappings } : s)))
                    .catch(() => {})
                }
              >
                ✕
              </button>
            </div>
          ))
        )}
      </div>
      <div className="pr-devmgr-hint">
        Las asignaciones son tuyas, no del show: siguen al operador en cualquier show. Los faders
        de zona se asignan por posición (zona 1, zona 2…).
      </div>
    </>
  );
}

const DEFAULT: NetworkConfig = { artnet_enabled: false, artnet_ip: "255.255.255.255", artnet_port: 6454 };

export function DeviceManager({ onClose, zones }: DeviceManagerProps) {
  const [tab, setTab] = useState<"artnet" | "midi">("artnet");
  const [cfg, setCfg] = useState<NetworkConfig>(DEFAULT);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    networkGet()
      .then(setCfg)
      .catch(() => setCfg(DEFAULT));
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const apply = async () => {
    setApplying(true);
    setError(null);
    try {
      await networkSet(cfg);
      onClose();
    } catch (e) {
      setError(String(e));
      setApplying(false);
    }
  };

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
          <>
            <div className="pr-picker-body">
              <MidiTab zones={zones} />
            </div>
            <div className="pr-devmgr-foot">
              <button className="pr-btn" type="button" onClick={onClose}>
                Cerrar
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="pr-picker-body">
              <label className="pr-devmgr-check">
                <input
                  type="checkbox"
                  checked={cfg.artnet_enabled}
                  onChange={(e) => setCfg({ ...cfg, artnet_enabled: e.target.checked })}
                />
                Habilitar Art-Net
              </label>

              <div className="pr-devmgr-field">
                <span className="pr-devmgr-label">IP destino</span>
                <input
                  className="pr-devmgr-input"
                  type="text"
                  value={cfg.artnet_ip}
                  disabled={!cfg.artnet_enabled}
                  placeholder="192.168.1.50  ·  broadcast: 2.255.255.255"
                  onChange={(e) => setCfg({ ...cfg, artnet_ip: e.target.value })}
                />
              </div>

              <div className="pr-devmgr-field">
                <span className="pr-devmgr-label">Puerto</span>
                <input
                  className="pr-devmgr-input pr-devmgr-port"
                  type="number"
                  min={1}
                  max={65535}
                  value={cfg.artnet_port}
                  disabled={!cfg.artnet_enabled}
                  onChange={(e) => setCfg({ ...cfg, artnet_port: Number(e.target.value) || 6454 })}
                />
              </div>

              <div className="pr-devmgr-hint">
                Deshabilitado → sink virtual (127.0.0.1:6454). Broadcast por subred:{" "}
                <code>x.255.255.255</code>. Todos los universos se envían a este destino; sus números
                van en cada paquete.
              </div>
              {error && <div className="pr-devmgr-error">Error: {error}</div>}
            </div>

            <div className="pr-devmgr-foot">
              <button className="pr-btn" type="button" onClick={onClose}>
                Cancelar
              </button>
              <button className="pr-btn is-primary" type="button" onClick={apply} disabled={applying}>
                {applying ? "Aplicando…" : "Aplicar"}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

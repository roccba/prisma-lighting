import { useEffect, useState } from "react";
import { networkGet, networkSet, type NetworkConfig } from "./api";

// The device manager: where the engine sends DMX. First cut is the
// Art-Net tab — enable it and point it at a node IP or a subnet broadcast, Apply
// recompiles the engine onto that target (it was going to the loopback virtual sink).
// The per-node universe table is a later feature; today every universe goes to one
// target (a node that takes them all, or broadcast).

interface DeviceManagerProps {
  onClose: () => void;
}

const DEFAULT: NetworkConfig = { artnet_enabled: false, artnet_ip: "255.255.255.255", artnet_port: 6454 };

export function DeviceManager({ onClose }: DeviceManagerProps) {
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
          <button className="pr-wtab is-active" type="button">
            Art-Net
          </button>
        </div>

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
      </div>
    </div>
  );
}

import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  artnetScan,
  hostName,
  networkInterfaces,
  nodeStatuses,
  openNodePage,
  outputGet,
  outputSet,
  outputStatus,
  type ArtNetNode,
  type InterfaceInfo,
  type NodeReach,
  type NodeStatusInfo,
  type OutputStatus,
  type ScannedNode,
  type ShowNetwork,
} from "./api";
import { Check } from "./Check";
import { Dropdown } from "./Dropdown";
import { Duplicate, Gear, Globe, Minus, Plus, Search } from "./icons";
import { NumberField } from "./NumberField";

// The Device Manager's Art-Net tab, modelled on professional consoles (docs/DEVICES-SPEC.md §9):
// one row per device — a node, or one BindIndex group of a node with more than 4 ports
// (the H802RA lists as 18 rows of 4 universes) — with the classic columns, a right-hand
// panel (Habilitar · ArtSync · interfaz de red · ▲▼) and a bottom bar (Scan · Agregar ·
// Duplicar · Eliminar · Configurar · HTTP · host · count). Edits are STAGED: nothing
// reaches the output until Aplicar (or Aceptar = Aplicar + close); Cancelar drops them.
//
// Universe numbers are Art-Net port addresses shown as is, like professional consoles (universe 1 is
// 0x0001 in its node config).

const EMPTY: ShowNetwork = {
  initialized: true,
  artnet_enabled: false,
  artsync: true,
  interface: null,
  nodes: [],
};

const NO_IP = "0.0.0.0";
const MAX_UNIVERSE = 32767;

const isIpv4 = (ip: string) =>
  /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.test(ip.trim()) &&
  ip
    .trim()
    .split(".")
    .every((o) => Number(o) <= 255);

/** Ports' universes as professional consoles list them ("1, 2, 3, 4"), with runs of 3+ consecutive
 *  universes compressed ("3–149") so a big device never makes the column kilometres wide. */
function formatPorts(ports: number[]): string {
  if (ports.length === 0) return "—";
  const out: string[] = [];
  let i = 0;
  while (i < ports.length) {
    let j = i;
    while (j + 1 < ports.length && ports[j + 1] === ports[j] + 1) j++;
    if (j - i >= 2) out.push(`${ports[i]}–${ports[j]}`);
    else for (let k = i; k <= j; k++) out.push(String(ports[k]));
    i = j + 1;
  }
  return out.join(", ");
}

const hex4 = (n: number) => `0x${n.toString(16).toUpperCase().padStart(4, "0")}`;

function newId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : "xxxxxxxx-xxxx-4xxx-8xxx-xxxxxxxxxxxx".replace(/x/g, () =>
        Math.floor(Math.random() * 16).toString(16),
      );
}

const ifaceKey = (address: string, prefix: number) => `${address}/${prefix}`;

const toU32 = (ip: string) => ip.split(".").reduce((a, o) => (a << 8) + Number(o), 0) >>> 0;
const maskOf = (prefix: number) => (prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0);
const fromU32 = (n: number) => [24, 16, 8, 0].map((s) => (n >>> s) & 255).join(".");

/** The directed broadcast of an interface network (e.g. 2.255.255.255). */
function broadcastOf(address: string, prefix: number): string {
  return fromU32((toU32(address) | ~maskOf(prefix)) >>> 0);
}

function sameNetwork(i: InterfaceInfo, address: string, prefix: number): boolean {
  const m = maskOf(prefix);
  return ((toU32(i.address) & m) >>> 0) === ((toU32(address) & m) >>> 0);
}

const REACH_LABEL: Record<NodeReach, string> = {
  online: "Responde",
  silent: "Sin respuesta (algunos equipos no contestan al Scan)",
  lost: "Dejó de responder",
};

/** The classic IP/MAC column: "ip / mac / BindIdxN (Envío …)". */
function addressText(n: ArtNetNode, broadcast: string): string {
  const parts = [n.ip === NO_IP ? "Sin IP" : n.ip];
  if (n.origin.kind === "scanned") {
    if (n.origin.mac) parts.push(n.origin.mac);
    parts.push(`BindIdx${n.origin.bind_index}`);
  }
  const how = n.unicast
    ? n.ip === NO_IP
      ? "(configurá la IP)"
      : `(Enviar solo a la dirección de unidifusión ${n.ip})`
    : `(Enviar a la dirección de difusión ${broadcast})`;
  return `${parts.join(" / ")} ${how}`;
}

export function ArtNetTab({ onClose }: { onClose: () => void }) {
  const [net, setNet] = useState<ShowNetwork>(EMPTY);
  const [applied, setApplied] = useState<ShowNetwork>(EMPTY);
  const [sel, setSel] = useState<number | null>(null);
  const [ifaces, setIfaces] = useState<InterfaceInfo[]>([]);
  const [status, setStatus] = useState<OutputStatus | null>(null);
  const [reach, setReach] = useState<Record<string, NodeReach>>({});
  const [host, setHost] = useState("");
  const [dialog, setDialog] = useState<{ kind: "add" } | { kind: "config"; index: number } | { kind: "scan" } | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshApplied = () => {
    outputGet()
      .then((n) => {
        setNet(n);
        setApplied(n);
      })
      .catch((e) => setError(String(e)));
    outputStatus().then(setStatus).catch(() => setStatus(null));
  };

  useEffect(() => {
    refreshApplied();
    networkInterfaces().then(setIfaces).catch(() => setIfaces([]));
    hostName().then(setHost).catch(() => {});
    const toMap = (list: NodeStatusInfo[]) =>
      setReach((r) => ({ ...r, ...Object.fromEntries(list.map((s) => [s.ip, s.status])) }));
    nodeStatuses().then(toMap).catch(() => {});
    const un = listen<NodeStatusInfo[]>("artnet-node-status", (e) => toMap(e.payload));
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  const dirty = useMemo(() => JSON.stringify(net) !== JSON.stringify(applied), [net, applied]);

  const setNodes = (f: (nodes: ArtNetNode[]) => ArtNetNode[]) => setNet((n) => ({ ...n, nodes: f(n.nodes) }));
  const updateNode = (i: number, node: ArtNetNode) => setNodes((ns) => ns.map((x, k) => (k === i ? node : x)));

  const apply = async (close: boolean) => {
    setBusy(true);
    setError(null);
    try {
      if (dirty) await outputSet({ ...net, initialized: true });
      if (close) {
        onClose();
        return;
      }
      refreshApplied();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const selected = sel !== null ? net.nodes[sel] : undefined;

  const duplicate = () => {
    if (!selected || sel === null) return;
    const copy: ArtNetNode = { ...selected, id: newId(), origin: { kind: "manual" } };
    setNodes((ns) => [...ns.slice(0, sel + 1), copy, ...ns.slice(sel + 1)]);
    setSel(sel + 1);
  };
  const remove = () => {
    if (sel === null) return;
    setNodes((ns) => ns.filter((_, k) => k !== sel));
    setSel(null);
  };
  const move = (dir: -1 | 1) => {
    if (sel === null) return;
    const to = sel + dir;
    if (to < 0 || to >= net.nodes.length) return;
    setNodes((ns) => {
      const out = [...ns];
      [out[sel], out[to]] = [out[to], out[sel]];
      return out;
    });
    setSel(to);
  };

  // Interface: the host's interfaces, "automatic" (OS routes), and the show's stored
  // network when this PC has no interface on it.
  const ifaceOptions = [
    { value: "", label: "Automática (la elige el sistema)" },
    ...ifaces.map((i) => ({ value: ifaceKey(i.address, i.prefix_len), label: `${i.name} — ${i.address}/${i.prefix_len}` })),
  ];
  const wanted = net.interface;
  const wantedKey = wanted ? ifaceKey(wanted.address, wanted.prefix_len) : "";
  const wantedHere = !wanted || ifaces.some((i) => sameNetwork(i, wanted.address, wanted.prefix_len));
  if (wanted && !ifaceOptions.some((o) => o.value === wantedKey)) {
    ifaceOptions.push({
      value: wantedKey,
      label: `${wanted.name_hint || "Red del show"} — ${wanted.address}/${wanted.prefix_len}${wantedHere ? "" : " (no está en esta PC)"}`,
    });
  }
  const pickIface = (key: string) => {
    if (!key) return setNet((n) => ({ ...n, interface: null }));
    const i = ifaces.find((x) => ifaceKey(x.address, x.prefix_len) === key);
    if (i) setNet((n) => ({ ...n, interface: { address: i.address, prefix_len: i.prefix_len, name_hint: i.name } }));
  };
  const broadcast = wanted ? broadcastOf(wanted.address, wanted.prefix_len) : "255.255.255.255";

  const uncovered = status && !dirty ? status.uncovered : [];

  return (
    <>
      <div className="pr-artnet">
        <div className="pr-artnet-main">
          <div className="pr-artnet-tablewrap">
            <table className="pr-table pr-artnet-table">
              <thead>
                <tr>
                  <th className="col-on" title="Activo">On</th>
                  <th className="col-state" title="Estado" />
                  <th>Nombre del dispositivo</th>
                  <th>Número/Red</th>
                  <th>Salida Universo</th>
                  <th>Nombre largo</th>
                  <th>Dirección IP / Dirección MAC</th>
                </tr>
              </thead>
              <tbody>
                {net.nodes.map((n, i) => {
                  const r = applied.nodes.some((a) => a.ip === n.ip) ? reach[n.ip] : undefined;
                  return (
                    <tr
                      key={n.id}
                      className={`${sel === i ? "is-sel" : ""}${n.enabled ? "" : " is-off"}`}
                      onClick={() => setSel(i)}
                      onDoubleClick={() => setDialog({ kind: "config", index: i })}
                    >
                      {/* The On checkbox only toggles: its clicks AND double-clicks stop
                          here, so toggling twice quickly never opens the config. */}
                      <td className="col-on" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
                        <Check checked={n.enabled} onChange={(v) => updateNode(i, { ...n, enabled: v })} />
                      </td>
                      <td className="col-state">
                        <span className={`pr-reach${r ? ` is-${r}` : ""}`} title={r ? REACH_LABEL[r] : "Se verá al aplicar"} />
                      </td>
                      <td>{n.name || "—"}</td>
                      <td className="num">{n.ports.length}</td>
                      <td className="num col-ports" title={n.ports.join(", ")}>
                        {formatPorts(n.ports)}
                      </td>
                      <td className="col-long" title={n.long_name}>
                        {n.long_name}
                      </td>
                      <td className="mono col-addr" title={addressText(n, broadcast)}>
                        {addressText(n, broadcast)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {net.nodes.length === 0 && (
              <div className="pr-artnet-empty">
                Sin dispositivos — la salida va al sink virtual. Usá <b>Scan</b> para buscar placas en la red, o{" "}
                <b>Agregar</b> para cargarlas a mano.
              </div>
            )}
          </div>

          <div className="pr-artnet-bar">
            <button className="pr-btn" type="button" onClick={() => setDialog({ kind: "scan" })} title="Buscar dispositivos Art-Net en la red (ArtPoll)">
              <Search size={15} /> Scan
            </button>
            <button className="pr-btn" type="button" onClick={() => setDialog({ kind: "add" })} title="Agregar dispositivos">
              <Plus size={15} /> Agregar
            </button>
            <button className="pr-btn" type="button" onClick={duplicate} disabled={!selected} title="Duplicar el dispositivo seleccionado">
              <Duplicate size={15} /> Duplicar
            </button>
            <button className="pr-btn" type="button" onClick={remove} disabled={!selected} title="Eliminar el dispositivo seleccionado">
              <Minus size={15} /> Eliminar
            </button>
            <button
              className="pr-btn"
              type="button"
              onClick={() => sel !== null && setDialog({ kind: "config", index: sel })}
              disabled={!selected}
              title="Abrir la configuración del dispositivo Art-Net"
            >
              <Gear size={15} /> Configurar
            </button>
            <button
              className="pr-btn"
              type="button"
              onClick={() => selected && openNodePage(selected.ip).catch((e) => setError(String(e)))}
              disabled={!selected || selected.ip === NO_IP}
              title="Abrir la página web del dispositivo en el navegador"
            >
              <Globe size={15} /> HTTP
            </button>
            <span className="pr-artnet-host">Nombre del host: {host || "—"}</span>
            <span className="pr-artnet-count">Número: {net.nodes.length}</span>
          </div>
        </div>

        <div className="pr-artnet-side">
          <Check checked={net.artnet_enabled} onChange={(v) => setNet((n) => ({ ...n, artnet_enabled: v }))}>
            Habilitar
          </Check>
          <Check checked={net.artsync} onChange={(v) => setNet((n) => ({ ...n, artsync: v }))}>
            ArtSync
          </Check>
          <div className="pr-artnet-side-label">Interfaz de red</div>
          <Dropdown value={wantedKey} options={ifaceOptions} onChange={pickIface} aria-label="Interfaz de red" />
          <div className="pr-artnet-messages">
            {!wantedHere && <div className="pr-artnet-warn">La red del show no está en esta PC; la salida usa la que elija el sistema.</div>}
            {!net.artnet_enabled && <div className="pr-artnet-note">Deshabilitado: la salida va al sink virtual.</div>}
            {uncovered.length > 0 && (
              <div className="pr-artnet-warn">
                {uncovered.length} universo{uncovered.length > 1 ? "s" : ""} del patch sin dispositivo: {uncovered.join(", ")}
              </div>
            )}
            {dirty && <div className="pr-artnet-note">Cambios sin aplicar.</div>}
          </div>
          <div className="pr-artnet-order">
            <button className="pr-btn" type="button" onClick={() => move(-1)} disabled={sel === null || sel === 0} title="Subir">
              ▲
            </button>
            <button className="pr-btn" type="button" onClick={() => move(1)} disabled={sel === null || sel === net.nodes.length - 1} title="Bajar">
              ▼
            </button>
          </div>
        </div>
      </div>

      <div className="pr-devmgr-foot">
        <span className="pr-devmgr-footstatus" title={error ?? undefined}>
          {error ? `Error: ${error}` : ""}
        </span>
        <button className="pr-btn is-primary" type="button" onClick={() => apply(true)} disabled={busy}>
          Aceptar
        </button>
        <button className="pr-btn" type="button" onClick={() => apply(false)} disabled={busy || !dirty}>
          Aplicar
        </button>
        <button className="pr-btn" type="button" onClick={onClose} disabled={busy}>
          Cancelar
        </button>
      </div>

      {dialog?.kind === "add" && (
        <AddDialog
          existing={net.nodes}
          onCancel={() => setDialog(null)}
          onAdd={(nodes) => {
            setNodes((ns) => [...ns, ...nodes]);
            setSel(net.nodes.length);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "config" && net.nodes[dialog.index] && (
        <ConfigDialog
          node={net.nodes[dialog.index]}
          reach={reach[net.nodes[dialog.index].ip]}
          broadcast={broadcast}
          onApply={(node) => updateNode(dialog.index, node)}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "scan" && (
        <ScanDialog
          existing={net.nodes}
          onCancel={() => setDialog(null)}
          onAdd={(found) => {
            setNodes((ns) => [...ns, ...found.map(fromScan)]);
            setDialog(null);
          }}
        />
      )}
    </>
  );
}

function fromScan(s: ScannedNode): ArtNetNode {
  return {
    id: newId(),
    name: s.short_name || s.ip,
    long_name: s.long_name,
    ip: s.ip,
    ports: s.ports,
    enabled: true,
    unicast: true,
    full_frames: false,
    origin: { kind: "scanned", mac: s.mac, bind_index: s.bind_index, esta: s.esta, oem: s.oem, firmware: s.firmware },
  };
}

/** A scanned row is already in the list if a device has the same IP and bind. */
function alreadyAdded(s: ScannedNode, nodes: ArtNetNode[]): boolean {
  return nodes.some(
    (n) => n.ip === s.ip && (n.origin.kind === "scanned" ? n.origin.bind_index === s.bind_index : n.ports.join() === s.ports.join()),
  );
}

// --- Agregar ("Opciones de nuevo dispositivo") ------------------------------------------

function AddDialog({
  existing,
  onAdd,
  onCancel,
}: {
  existing: ArtNetNode[];
  onAdd: (nodes: ArtNetNode[]) => void;
  onCancel: () => void;
}) {
  const [devices, setDevices] = useState(1);
  const [ports, setPorts] = useState(1);
  const [unicast, setUnicast] = useState(true);
  const [fullFrames, setFullFrames] = useState(false);

  const add = () => {
    // New devices take the next free universes, continuing after the highest in use,
    // and are numbered after the existing devices — like the classic "ArtNet 00001".
    let next = Math.max(0, ...existing.flatMap((n) => n.ports)) + 1;
    const nodes: ArtNetNode[] = [];
    for (let d = 0; d < devices; d++) {
      const nr = String(existing.length + d + 1).padStart(5, "0");
      const ps: number[] = [];
      for (let p = 0; p < ports; p++) ps.push(Math.min(next++, MAX_UNIVERSE));
      nodes.push({
        id: newId(),
        name: `ArtNet ${nr}`,
        long_name: `ArtNet device nr ${nr}`,
        ip: NO_IP,
        ports: ps,
        enabled: true,
        unicast,
        full_frames: fullFrames,
        origin: { kind: "manual" },
      });
    }
    onAdd(nodes);
  };

  return (
    <div className="pr-picker-backdrop pr-subdialog" onPointerDown={onCancel}>
      <div className="pr-picker pr-artnet-dialog pr-artnet-add" onPointerDown={(e) => e.stopPropagation()}>
        <div className="pr-picker-head">Opciones de nuevo dispositivo</div>
        <div className="pr-picker-body pr-artnet-form">
          <div className="pr-artnet-row">
            <span className="pr-artnet-lbl">Tipo de puerto:</span>
            <Dropdown value="out" options={[{ value: "out", label: "Salida" }]} onChange={() => {}} disabled aria-label="Tipo de puerto" />
          </div>
          <div className="pr-artnet-row">
            <NumberField variant="pm" value={devices} min={1} max={64} onChange={setDevices} width={40} aria-label="Número de dispositivos nuevos" />
            <span>Número de dispositivos nuevos</span>
          </div>
          <div className="pr-artnet-row">
            <NumberField variant="pm" value={ports} min={1} max={4} onChange={setPorts} width={40} aria-label="Número de puertos por dispositivo" />
            <span>Número de puertos por dispositivo</span>
          </div>
          <div className="pr-artnet-ports">
            {Array.from({ length: ports }, (_, p) => (
              <div className="pr-artnet-row" key={p}>
                <span className="pr-artnet-lbl">Puerto {p + 1}:</span>
                <Dropdown value="out" options={[{ value: "out", label: "Salida" }]} onChange={() => {}} disabled aria-label={`Puerto ${p + 1}`} />
              </div>
            ))}
          </div>
          <Check checked={unicast} onChange={setUnicast}>
            Enviar mediante modo unidifusión
          </Check>
          <Check checked={fullFrames} onChange={setFullFrames}>
            Enviar fotogramas completos
          </Check>
          <div className="pr-artnet-total">
            <b>{devices * ports}</b> Total de universos
          </div>
        </div>
        <div className="pr-devmgr-foot">
          <span className="pr-devmgr-footstatus" />
          <button className="pr-btn is-primary" type="button" onClick={add}>
            Aceptar
          </button>
          <button className="pr-btn" type="button" onClick={onCancel}>
            Cancelar
          </button>
        </div>
      </div>
    </div>
  );
}

// --- Configurar ("Configuración de dispositivo Art-Net") --------------------------------

function ConfigDialog({
  node,
  reach,
  broadcast,
  onApply,
  onClose,
}: {
  node: ArtNetNode;
  reach?: NodeReach;
  broadcast: string;
  onApply: (n: ArtNetNode) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<ArtNetNode>(node);
  const [octets, setOctets] = useState<string[]>(node.ip.split("."));
  useEffect(() => {
    setDraft(node);
    setOctets(node.ip.split("."));
  }, [node]);

  const ip = octets.map((o) => o.trim() || "0").join(".");
  const ipOk = isIpv4(ip);
  const error = !ipOk ? "IP inválida" : draft.unicast && ip === NO_IP ? "Falta la dirección IP de unidifusión" : null;
  const result: ArtNetNode = { ...draft, ip: ipOk ? ip : draft.ip };
  const changed = JSON.stringify(result) !== JSON.stringify(node);

  const setPortCount = (count: number) =>
    setDraft((d) => {
      const ports = [...d.ports];
      let next = Math.max(0, ...ports) + 1;
      while (ports.length < count) ports.push(Math.min(next++, MAX_UNIVERSE));
      return { ...d, ports: ports.slice(0, count) };
    });

  const scanned = draft.origin.kind === "scanned" ? draft.origin : null;
  const portOptions = Array.from({ length: Math.max(4, draft.ports.length) }, (_, i) => ({ value: String(i + 1), label: String(i + 1) }));

  return (
    <div className="pr-picker-backdrop pr-subdialog" onPointerDown={onClose}>
      <div className="pr-picker pr-artnet-dialog pr-artnet-config" onPointerDown={(e) => e.stopPropagation()}>
        <div className="pr-picker-head">Configuración de dispositivo Art-Net</div>
        <div className="pr-picker-body pr-artnet-config-body">
          <section className="pr-group">
            <div className="pr-group-head">
              <span>General</span>
              <button className="pr-btn" type="button" disabled={!ipOk || ip === NO_IP} onClick={() => openNodePage(ip).catch(() => {})}>
                Página web
              </button>
            </div>
            <div className="pr-group-body">
              {scanned && (
                <>
                  <div className="pr-artnet-kv">
                    <span>Fabricante:</span>
                    <b>ESTA {hex4(scanned.esta)}</b>
                  </div>
                  <div className="pr-artnet-kv">
                    <span>Firmware:</span>
                    <b>v{scanned.firmware}</b>
                  </div>
                </>
              )}
              <div className="pr-artnet-kv">
                <span>Nombre corto:</span>
                <input className="pr-devmgr-input" value={draft.name} maxLength={17} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
              </div>
              <div className="pr-artnet-kv">
                <span>Nombre largo:</span>
                <input className="pr-devmgr-input" value={draft.long_name} maxLength={63} onChange={(e) => setDraft({ ...draft, long_name: e.target.value })} />
              </div>
            </div>
          </section>

          <section className="pr-group">
            <div className="pr-group-head">
              <span>Puertos del dispositivo</span>
            </div>
            <div className="pr-group-body">
              <div className="pr-artnet-kv">
                <span>Número de puertos:</span>
                <Dropdown value={String(draft.ports.length)} options={portOptions} onChange={(v) => setPortCount(Number(v))} aria-label="Número de puertos" />
              </div>
              <div className="pr-artnet-kv">
                <span>Estado:</span>
                <span className="pr-artnet-state">
                  <span className={`pr-reach${reach ? ` is-${reach}` : ""}`} /> {reach ? REACH_LABEL[reach] : "Se verá al aplicar"}
                </span>
              </div>
              <div className="pr-artnet-portlist">
                {draft.ports.map((u, p) => (
                  <div className="pr-artnet-kv" key={p}>
                    <span>Puerto {p + 1} universo OUT:</span>
                    <span className="pr-artnet-portval">
                      <NumberField
                        variant="pm"
                        value={u}
                        min={0}
                        max={MAX_UNIVERSE}
                        width={56}
                        onChange={(v) => setDraft((d) => ({ ...d, ports: d.ports.map((x, k) => (k === p ? v : x)) }))}
                        aria-label={`Puerto ${p + 1} universo`}
                      />
                      <span className="pr-artnet-hex">{hex4(u)}</span>
                      <span className="pr-artnet-dim">ArtNet</span>
                    </span>
                  </div>
                ))}
              </div>
            </div>
          </section>

          <section className="pr-group">
            <div className="pr-group-head">
              <span>Configuración IP</span>
            </div>
            <div className="pr-group-body">
              <div className="pr-artnet-kv">
                <span>Envía modo broadcast:</span>
                <Check checked={!draft.unicast} onChange={(v) => setDraft({ ...draft, unicast: !v })}>
                  Enviar datos a la dirección de difusión dirigida {broadcast}
                </Check>
              </div>
              <div className="pr-artnet-kv">
                <span>Modo unidifusión de envío:</span>
                <Check checked={draft.unicast} onChange={(v) => setDraft({ ...draft, unicast: v })}>
                  Enviar datos únicamente a la dirección IP
                </Check>
              </div>
              <div className="pr-artnet-kv">
                <span />
                <span className="pr-ip">
                  {octets.map((o, k) => (
                    <span key={k} className="pr-ip-oct">
                      {k > 0 && <span className="pr-ip-dot">.</span>}
                      <input
                        value={o}
                        inputMode="numeric"
                        maxLength={3}
                        disabled={!draft.unicast}
                        onChange={(e) => setOctets((os) => os.map((x, i) => (i === k ? e.target.value.replace(/\D/g, "") : x)))}
                        aria-label={`Octeto ${k + 1}`}
                      />
                    </span>
                  ))}
                </span>
              </div>
              <div className="pr-artnet-kv">
                <span />
                <Check checked={draft.full_frames} onChange={(v) => setDraft({ ...draft, full_frames: v })}>
                  Enviar fotogramas completos (512 canales)
                </Check>
              </div>
              <div className="pr-artnet-kv pr-artnet-dim">
                <span>Dirección del puerto:</span>
                <span>6454 &nbsp; 0x1936</span>
              </div>
            </div>
          </section>
        </div>
        <div className="pr-devmgr-foot">
          <span className="pr-devmgr-footstatus">{error ?? ""}</span>
          <button
            className="pr-btn is-primary"
            type="button"
            disabled={!ipOk}
            onClick={() => {
              onApply(result);
              onClose();
            }}
          >
            Aceptar
          </button>
          <button className="pr-btn" type="button" disabled={!ipOk || !changed} onClick={() => onApply(result)}>
            Aplicar
          </button>
          <button className="pr-btn" type="button" onClick={onClose}>
            Cancelar
          </button>
        </div>
      </div>
    </div>
  );
}

// --- Scan ---------------------------------------------------------------------------------

function ScanDialog({
  existing,
  onAdd,
  onCancel,
}: {
  existing: ArtNetNode[];
  onAdd: (found: ScannedNode[]) => void;
  onCancel: () => void;
}) {
  const [found, setFound] = useState<ScannedNode[] | null>(null);
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);

  const run = () => {
    setFound(null);
    setError(null);
    artnetScan()
      .then((rows) => {
        setFound(rows);
        setPicked(new Set(rows.map((r, i) => (alreadyAdded(r, existing) ? -1 : i)).filter((i) => i >= 0)));
      })
      .catch((e) => {
        setError(String(e));
        setFound([]);
      });
  };
  useEffect(run, []);

  const toggle = (i: number) =>
    setPicked((p) => {
      const next = new Set(p);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  return (
    <div className="pr-picker-backdrop pr-subdialog" onPointerDown={onCancel}>
      <div className="pr-picker pr-artnet-dialog pr-artnet-scan" onPointerDown={(e) => e.stopPropagation()}>
        <div className="pr-picker-head">Scan Art-Net</div>
        <div className="pr-picker-body pr-artnet-scan-body">
          {found === null ? (
            <div className="pr-devmgr-hint">Buscando dispositivos en la red… (3 s)</div>
          ) : found.length === 0 ? (
            <div className="pr-devmgr-hint">
              {error ?? "Ningún dispositivo respondió. Revisá el cable y que esta PC tenga una IP en la red de las placas."}
            </div>
          ) : (
            <table className="pr-table pr-artnet-table">
              <thead>
                <tr>
                  <th className="col-on" />
                  <th>Nombre del dispositivo</th>
                  <th>Número/Red</th>
                  <th>Salida Universo</th>
                  <th>Nombre largo</th>
                  <th>Dirección IP / Dirección MAC</th>
                </tr>
              </thead>
              <tbody>
                {found.map((n, i) => {
                  const added = alreadyAdded(n, existing);
                  return (
                    <tr key={`${n.ip}/${n.bind_index}`} className={added ? "is-off" : ""} onClick={() => !added && toggle(i)}>
                      <td className="col-on" onClick={(e) => e.stopPropagation()}>
                        <Check checked={picked.has(i)} onChange={() => toggle(i)} disabled={added} />
                      </td>
                      <td>{n.short_name || "—"}</td>
                      <td className="num">{n.ports.length}</td>
                      <td className="num col-ports" title={n.ports.join(", ")}>
                        {formatPorts(n.ports)}
                      </td>
                      <td className="col-long" title={n.long_name}>
                        {n.long_name}
                      </td>
                      <td className="mono">
                        {n.ip}
                        {n.mac ? ` / ${n.mac}` : ""} / BindIdx{n.bind_index}
                        {added ? " (ya agregado)" : ""}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
        <div className="pr-devmgr-foot">
          <span className="pr-devmgr-footstatus">{found && found.length > 0 ? `Número: ${found.length}` : ""}</span>
          <button className="pr-btn" type="button" onClick={run} disabled={found === null}>
            Volver a buscar
          </button>
          <button
            className="pr-btn is-primary"
            type="button"
            disabled={!found || picked.size === 0}
            onClick={() => found && onAdd(found.filter((_, i) => picked.has(i)))}
          >
            Agregar seleccionados
          </button>
          <button className="pr-btn" type="button" onClick={onCancel}>
            Cancelar
          </button>
        </div>
      </div>
    </div>
  );
}

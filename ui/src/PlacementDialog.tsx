import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  fixtureTypes,
  nextFixtureId,
  nextFreeAddress,
  placeFixtures,
  previewPlacement,
  type EditorState,
  type FixtureType,
  type MainOrientation,
  type Patch,
  type PlacementSpec,
  type Rotation,
  type StartCorner,
} from "./api";
import { Check } from "./Check";
import { Dropdown } from "./Dropdown";
import { BUILTIN_FIXTURES } from "./LibraryManager";
import { NumberField } from "./NumberField";

// "Añadir instalaciones" — modelled on the classic Add Fixtures dialog (M7 spec §4.1).
//
// Two independent facts, kept apart exactly as professional consoles do:
//  · ROTACIÓN — which way the pixels run INSIDE each fixture (8 directions; the 45°
//    steps are Prisma's diagonals). The product preview draws the fixture as created
//    in the library, turned by this rotation, with pixel 1 (the signal input) lit.
//  · ESQUINA INICIAL + ORIENTACIÓN PRINCIPAL + SNAKE — the ORDER in which the fixtures
//    of the grid receive their DMX addresses. The addressing preview draws that path as
//    a fixed 5-line schematic: it shows the route of the signal, never the quantity.
//
// "Añadir" does not close the dialog: batches can be added one after another, and the
// ID / universe / channel refresh to the next free values after each one.

interface Props {
  patch: Patch;
  activeZone: string;
  onPlaced: (s: EditorState) => void;
  onClose: () => void;
}

const ROTATIONS: { value: Rotation; label: string }[] = [
  { value: "R0", label: "Original" },
  { value: "R45", label: "45°" },
  { value: "R90", label: "90°" },
  { value: "R135", label: "135°" },
  { value: "R180", label: "180°" },
  { value: "R225", label: "225°" },
  { value: "R270", label: "270°" },
  { value: "R315", label: "315°" },
];

const CORNERS: { value: StartCorner; label: string }[] = [
  { value: "TopLeft", label: "Superior izquierda" },
  { value: "TopRight", label: "Superior derecha" },
  { value: "BottomLeft", label: "Inferior izquierda" },
  { value: "BottomRight", label: "Inferior derecha" },
];

const ORIENTATIONS: { value: MainOrientation; label: string }[] = [
  { value: "Horizontal", label: "Horizontal" },
  { value: "Vertical", label: "Vertical" },
];

/** Per-pixel cell step of a strip at `r` — mirrors `Rotation::step` in the backend. */
const STEP: Record<Rotation, [number, number]> = {
  R0: [1, 0],
  R45: [1, 1],
  R90: [0, 1],
  R135: [-1, 1],
  R180: [-1, 0],
  R225: [-1, -1],
  R270: [0, -1],
  R315: [1, -1],
};

const componentCount = (t: FixtureType) => (t.components === "Rgbw" ? 4 : 3);

/** One entry of the "Producto" list: a built-in (standard) fixture or one of the user's. */
interface Product {
  key: string;
  type: FixtureType;
  builtin: boolean;
}

/** The product last chosen in this session (the window's lifetime) — reopening the dialog
 *  keeps it. Starts on the first built-in ("Píxel RGB"). */
let lastProductKey = `std:${BUILTIN_FIXTURES[0].name}`;

/** Cells a batch occupies when placed at (0, 0): every pixel cell along each fixture's
 *  run as the backend plans it (straight or 45°). Falls back to one fixture's strip,
 *  stepped by the rotation, if the backend cannot plan the spec (e.g. no addresses). */
function footprintCells(runs: { start: [number, number]; end: [number, number] }[], type: FixtureType, rotation: Rotation): [number, number][] {
  const cells: [number, number][] = [];
  if (runs.length === 0) {
    const [dx, dy] = STEP[rotation];
    for (let i = 0; i < Math.max(1, type.pixel_count); i++) cells.push([dx * i, dy * i]);
    return cells;
  }
  for (const r of runs) {
    const [x0, y0] = [Math.round(r.start[0]), Math.round(r.start[1])];
    const [x1, y1] = [Math.round(r.end[0]), Math.round(r.end[1])];
    const sx = Math.sign(x1 - x0);
    const sy = Math.sign(y1 - y0);
    const n = Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0));
    for (let i = 0; i <= n; i++) cells.push([x0 + sx * i, y0 + sy * i]);
  }
  return cells;
}

/** The first top-left position (0-based), scanning left → right, top → bottom, where the
 *  whole footprint lands on free cells (no pixel of any placed fixture). The scan is as
 *  wide as the rig (or the footprint, if wider); a row just below the rig always fits, so
 *  the scan ends there. An empty rig gives (0, 0). */
function firstFreePosition(patch: Patch, cells: [number, number][]): [number, number] {
  if (cells.length === 0) return [0, 0];
  const occupied = new Set<string>();
  let rigW = 0;
  let rigH = 0;
  for (const f of patch.fixtures) {
    for (const p of f.pixels) {
      const x = Math.round(p.position[0]);
      const y = Math.round(p.position[1]);
      occupied.add(`${x},${y}`);
      rigW = Math.max(rigW, x + 1);
      rigH = Math.max(rigH, y + 1);
    }
  }
  if (occupied.size === 0) return [0, 0];
  let minX = Infinity, minY = Infinity, maxX = -Infinity;
  for (const [x, y] of cells) {
    minX = Math.min(minX, x);
    minY = Math.min(minY, y);
    maxX = Math.max(maxX, x);
  }
  // Positions keep every footprint cell at x, y >= 0 and inside the scan width.
  const width = Math.max(rigW, maxX - minX + 1);
  for (let py = -minY; py <= rigH - minY; py++) {
    for (let px = -minX; px + maxX < width; px++) {
      if (cells.every(([x, y]) => !occupied.has(`${px + x},${py + y}`))) return [px, py];
    }
  }
  return [0, 0];
}

/** A canvas sized to its box at device-pixel resolution, redrawn when `deps` change. */
function useCanvas(draw: (ctx: CanvasRenderingContext2D, w: number, h: number, css: CSSStyleDeclaration) => void, deps: unknown[]) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    draw(ctx, w, h, getComputedStyle(document.documentElement));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return ref;
}

/** Shade a #rrggbb colour toward black (k < 0) or white (k > 0). */
function shade(hex: string, k: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  const ch = (c: number) => Math.round(k < 0 ? c * (1 + k) : c + (255 - c) * k);
  const r = ch((n >> 16) & 255),
    g = ch((n >> 8) & 255),
    b = ch(n & 255);
  return `rgb(${r},${g},${b})`;
}

/** "Vista previa de instalación": the fixture as created, one cube per pixel along its
 *  arrangement, turned by the rotation; pixel 1 (the signal input) in the accent colour. */
function ProductPreview({ type, rotation }: { type: FixtureType | undefined; rotation: Rotation }) {
  const ref = useCanvas(
    (ctx, w, h, css) => {
      if (!type || type.pixel_count <= 0) return;
      const n = type.pixel_count;
      const [dx, dy] = STEP[rotation];
      const cols = dx !== 0 ? n : 1;
      const rows = dy !== 0 ? n : 1;
      const DEPTH = 0.32; // oblique depth, as a fraction of the cube size
      const pad = 14;
      const s = Math.min(150, (w - 2 * pad) / (cols + DEPTH), (h - 2 * pad) / (rows + DEPTH));
      const d = s * DEPTH;
      const gap = n === 1 ? 0 : Math.max(1, s * 0.14);
      const face = s - gap;
      const ox = (w - (cols * s + d)) / 2;
      const oy = (h - (rows * s + d)) / 2 + d;
      const fx = dx < 0 ? cols - 1 : 0;
      const fy = dy < 0 ? rows - 1 : 0;

      const accent = css.getPropertyValue("--accent-bright").trim() || "#1F9CFF";
      const grey = "#d9d9d9";

      // Painter's order for an up-right oblique: lower rows first, then left to right.
      const cells = Array.from({ length: n }, (_, i) => ({ i, x: fx + dx * i, y: fy + dy * i }));
      cells.sort((a, b) => b.y - a.y || a.x - b.x);

      for (const c of cells) {
        const base = c.i === 0 ? accent : grey;
        const x = ox + c.x * s;
        const y = oy + c.y * s;
        // Top face.
        if (face > 5) {
          ctx.fillStyle = shade(base, 0.25);
          ctx.beginPath();
          ctx.moveTo(x, y);
          ctx.lineTo(x + d, y - d);
          ctx.lineTo(x + face + d, y - d);
          ctx.lineTo(x + face, y);
          ctx.closePath();
          ctx.fill();
          // Right face.
          ctx.fillStyle = shade(base, -0.28);
          ctx.beginPath();
          ctx.moveTo(x + face, y);
          ctx.lineTo(x + face + d, y - d);
          ctx.lineTo(x + face + d, y + face - d);
          ctx.lineTo(x + face, y + face);
          ctx.closePath();
          ctx.fill();
        }
        // Front face.
        ctx.fillStyle = base;
        ctx.fillRect(x, y, face, face);
      }
    },
    [type, rotation],
  );
  return <canvas ref={ref} className="pr-place-canvas" />;
}

/** "Vista previa de direccionamiento": the route the signal takes through the grid, as a
 *  fixed 5-line schematic — start corner + main orientation + snake, never the quantity. */
function AddressingPreview({ corner, orientation, snake }: { corner: StartCorner; orientation: MainOrientation; snake: boolean }) {
  const ref = useCanvas(
    (ctx, w, h, css) => {
      const LINES = 5;
      const t = 9; // stroke thickness
      const m = 22;
      const color = css.getPropertyValue("--text-label").trim() || "#aaa";
      const knobRing = css.getPropertyValue("--text").trim() || "#d4d4d4";
      const left = corner === "TopLeft" || corner === "BottomLeft";
      const top = corner === "TopLeft" || corner === "TopRight";
      const horizontal = orientation === "Horizontal";

      // Each line as [start, end] in pixels, in signal order.
      const lines: [number, number, number, number][] = [];
      for (let i = 0; i < LINES; i++) {
        const k = i / (LINES - 1); // 0..1 across the lines
        const reversed = snake && i % 2 === 1;
        if (horizontal) {
          const y = m + (top ? k : 1 - k) * (h - 2 * m);
          const fwd = left !== reversed; // runs left → right
          const x0 = fwd ? m : w - m;
          const x1 = fwd ? w - m : m;
          lines.push([x0, y, x1, y]);
        } else {
          const x = m + (left ? k : 1 - k) * (w - 2 * m);
          const fwd = top !== reversed; // runs top → bottom
          const y0 = fwd ? m : h - m;
          const y1 = fwd ? h - m : m;
          lines.push([x, y0, x, y1]);
        }
      }

      ctx.fillStyle = color;
      ctx.strokeStyle = color;
      ctx.lineWidth = t;
      ctx.lineJoin = "miter";
      ctx.lineCap = "butt";

      if (snake) {
        // One folded path: line, U-turn along the edge, next line...
        ctx.beginPath();
        ctx.moveTo(lines[0][0], lines[0][1]);
        for (const [x0, y0, x1, y1] of lines) {
          ctx.lineTo(x0, y0);
          ctx.lineTo(x1, y1);
        }
        ctx.stroke();
      } else {
        // Separate lines, each tapering to a point at its end (the classic arrow-like tips).
        for (const [x0, y0, x1, y1] of lines) {
          const len = Math.hypot(x1 - x0, y1 - y0) || 1;
          const ux = (x1 - x0) / len,
            uy = (y1 - y0) / len;
          const nx = -uy * (t / 2),
            ny = ux * (t / 2);
          const tip = t * 1.6;
          ctx.beginPath();
          ctx.moveTo(x0 + nx, y0 + ny);
          ctx.lineTo(x1 - ux * tip + nx, y1 - uy * tip + ny);
          ctx.lineTo(x1, y1);
          ctx.lineTo(x1 - ux * tip - nx, y1 - uy * tip - ny);
          ctx.lineTo(x0 - nx, y0 - ny);
          ctx.closePath();
          ctx.fill();
        }
      }

      // Start knob at the signal origin.
      const [sx, sy] = lines[0];
      ctx.beginPath();
      ctx.arc(sx, sy, t, 0, Math.PI * 2);
      ctx.fillStyle = color;
      ctx.fill();
      ctx.lineWidth = 2;
      ctx.strokeStyle = knobRing;
      ctx.stroke();
    },
    [corner, orientation, snake],
  );
  return <canvas ref={ref} className="pr-place-canvas" />;
}

function Group({ title, children, className }: { title: string; children: ReactNode; className?: string }) {
  return (
    <section className={`pr-group${className ? ` ${className}` : ""}`}>
      <div className="pr-group-head">
        <span>{title}</span>
      </div>
      <div className="pr-group-body">{children}</div>
    </section>
  );
}

function Row({ label, children, disabled }: { label: string; children: ReactNode; disabled?: boolean }) {
  return (
    <div className={`pr-place-row${disabled ? " is-off" : ""}`}>
      <span title={label}>{label}</span>
      <div className="pr-place-ctl">{children}</div>
    </div>
  );
}

/** Three console-style [value][−][+] fields for X/Y/Z. Z stays at its value and disabled
 *  while placement is 2-D (the field exists so 3-D does not reopen the dialog). */
function Xyz({ values, onChange, min, zLocked = true, label }: { values: [number, number, number]; onChange: (v: [number, number, number]) => void; min?: number; zLocked?: boolean; label: string }) {
  return (
    <div className="pr-place-xyz">
      {(["X", "Y", "Z"] as const).map((axis, k) => (
        <NumberField
          key={axis}
          variant="pm"
          width={42}
          value={values[k]}
          min={min}
          disabled={zLocked && k === 2}
          onChange={(v) => onChange(values.map((x, i) => (i === k ? v : x)) as [number, number, number])}
          aria-label={`${label} ${axis}`}
        />
      ))}
    </div>
  );
}

export function PlacementDialog({ patch, activeZone, onPlaced, onClose }: Props) {
  const [types, setTypes] = useState<FixtureType[]>([]);
  const [productKey, setProductKey] = useState(lastProductKey);
  const [startId, setStartId] = useState(1);
  const [rotation, setRotation] = useState<Rotation>("R0");
  const [count, setCount] = useState<[number, number, number]>([1, 1, 1]);
  const [offset, setOffset] = useState<[number, number, number]>([0, 0, 0]);
  // Shown 1-based like professional consoles; the spec is 0-based.
  const [position, setPosition] = useState<[number, number, number]>([1, 1, 1]);
  // While true the start position follows the first free spot for the current batch;
  // editing it by hand stops that until the next "Añadir".
  const [positionAuto, setPositionAuto] = useState(true);
  const [corner, setCorner] = useState<StartCorner>("TopLeft");
  const [orientation, setOrientation] = useState<MainOrientation>("Horizontal");
  const [snake, setSnake] = useState(false);
  const [snakeRotation, setSnakeRotation] = useState(false);
  const [universe, setUniverse] = useState(1);
  const [universeOffset, setUniverseOffset] = useState(0);
  const [channel, setChannel] = useState(1);
  const [channelOffset, setChannelOffset] = useState(0);
  const [channelEveryUniverse, setChannelEveryUniverse] = useState(false);
  const [perUniverse, setPerUniverse] = useState<number | null>(null); // null = the maximum
  const [status, setStatus] = useState<{ text: string; error: boolean } | null>(null);
  const [busy, setBusy] = useState(false);

  const zoneName = patch.zones.find((z) => z.id === activeZone)?.name ?? "—";
  const products: Product[] = [
    ...BUILTIN_FIXTURES.map((t) => ({ key: `std:${t.name}`, type: t, builtin: true })),
    ...types.map((t, i) => ({ key: `user:${i}:${t.name}`, type: t, builtin: false })),
  ];
  const product = products.find((p) => p.key === productKey) ?? products[0];
  const type = product?.type as FixtureType | undefined;

  const refreshNext = () => {
    nextFixtureId().then(setStartId).catch(() => {});
    nextFreeAddress()
      .then(([u, c]) => {
        setUniverse(u);
        setChannel(c);
      })
      .catch(() => {});
  };

  useEffect(() => {
    fixtureTypes().then(setTypes).catch(() => setTypes([]));
    refreshNext();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // How many fixtures of this product fit in one universe — the ceiling of
  // "Número límite de instalaciones por universo" (professional consoles grey their + at this value).
  const span = type ? type.pixel_count * componentCount(type) : 0;
  const firstChannel = channelEveryUniverse ? channel : 1;
  const maxPerUniverse = span > 0 ? Math.max(1, Math.floor((513 - firstChannel) / (span + channelOffset))) : 1;
  const limit = Math.min(perUniverse ?? maxPerUniverse, maxPerUniverse);

  const total = count[0] * count[1];

  // Suggest the first free position for the batch as it is set up right now (product,
  // rotation, count, gaps, addressing order) and re-suggest after the rig changes (e.g.
  // after "Añadir"). Only a suggestion: the operator can type any position, overlapping
  // included. A sequence number drops answers that arrive after a newer request.
  const suggestSeq = useRef(0);
  useEffect(() => {
    if (!positionAuto || !type) return;
    const seq = ++suggestSeq.current;
    const spec: PlacementSpec = {
      fixture_type: type,
      start_number: 1,
      rotation,
      count: [count[0], count[1], 1],
      offset: [offset[0], offset[1], 0],
      start_position: [0, 0, 0],
      start_corner: corner,
      orientation,
      snake,
      snake_rotation: snake && snakeRotation,
      start_universe: 1,
      start_channel: 1,
      channel_offset: 0,
      universe_offset: 0,
      start_channel_every_universe: false,
      fixtures_per_universe: Math.max(1, Math.floor(512 / Math.max(1, type.pixel_count * componentCount(type)))),
      zone: activeZone,
    };
    previewPlacement(spec)
      .catch(() => [])
      .then((runs) => {
        if (seq !== suggestSeq.current) return;
        const [x, y] = firstFreePosition(patch, footprintCells(runs, type, rotation));
        setPosition([x + 1, y + 1, 1]);
      });
  }, [positionAuto, type, rotation, count, offset, corner, orientation, snake, snakeRotation, patch, activeZone]);

  const submit = async () => {
    if (!type || total === 0) return;
    const spec: PlacementSpec = {
      fixture_type: type,
      start_number: startId,
      rotation,
      count: [count[0], count[1], 1],
      offset: [offset[0], offset[1], 0],
      start_position: [position[0] - 1, position[1] - 1, 0],
      start_corner: corner,
      orientation,
      snake,
      snake_rotation: snake && snakeRotation,
      start_universe: universe,
      start_channel: channel,
      channel_offset: channelOffset,
      universe_offset: universeOffset,
      start_channel_every_universe: channelEveryUniverse,
      fixtures_per_universe: limit,
      zone: activeZone,
    };
    setBusy(true);
    setStatus(null);
    try {
      onPlaced(await placeFixtures(spec));
      setPositionAuto(true); // the next batch goes to the next free spot
      setStatus({ text: `${total} ${total === 1 ? "instalación añadida" : "instalaciones añadidas"} a ${zoneName}`, error: false });
      refreshNext();
    } catch (e) {
      setStatus({ text: String(e), error: true });
    } finally {
      setBusy(false);
    }
  };

  const typeOptions = products.map((p) => ({ value: p.key, label: p.type.name, group: p.builtin ? "Genérico" : "Mis tipos" }));

  return (
    <div className="pr-picker-backdrop">
      <div className="pr-picker pr-place" role="dialog" aria-label="Añadir instalaciones">
        <div className="pr-picker-head pr-place-title">
          <span>Añadir instalaciones</span>
          <span className="pr-place-zone" title="Zona a la que se añaden">
            <b>{zoneName}</b>
          </span>
        </div>

        <div className="pr-place-body">
          <div className="pr-place-col">
            <Group title="Instalación">
              <Row label="Protocolo:">
                <Dropdown value="DMX" options={[{ value: "DMX", label: "DMX" }]} onChange={() => {}} aria-label="Protocolo" />
              </Row>
              <Row label="Producto:">
                <Dropdown
                  value={product?.key ?? ""}
                  options={typeOptions}
                  onChange={(v) => {
                    setProductKey(v);
                    lastProductKey = v;
                    setPerUniverse(null);
                  }}
                  aria-label="Producto"
                />
              </Row>
              <Row label="Iniciar ID de instalación:">
                <NumberField variant="pm" width={60} value={startId} min={1} onChange={setStartId} aria-label="Iniciar ID de instalación" />
              </Row>
            </Group>

            <Group title="Emplazamiento">
              <Row label="Rotación:">
                <Dropdown value={rotation} options={ROTATIONS} onChange={(v) => setRotation(v as Rotation)} aria-label="Rotación" />
              </Row>
              <Row label="Cuenta X/Y/Z:">
                <Xyz label="Cuenta" values={count} min={1} onChange={setCount} />
              </Row>
              <Row label="" disabled>
                <Check checked={false} onChange={() => {}} disabled>
                  Combinar instalaciones
                </Check>
              </Row>
              <Row label="Desfase X/Y/Z:">
                <Xyz label="Desfase" values={offset} onChange={setOffset} />
              </Row>
              <Row label="Posición inicial X/Y/Z:">
                <Xyz
                  label="Posición inicial"
                  values={position}
                  min={1}
                  onChange={(v) => {
                    setPositionAuto(false);
                    setPosition(v);
                  }}
                />
              </Row>
            </Group>

            <Group title="Direccionamiento">
              <Row label="Esquina inicial:">
                <Dropdown value={corner} options={CORNERS} onChange={(v) => setCorner(v as StartCorner)} aria-label="Esquina inicial" />
              </Row>
              <Row label="Orientación principal:">
                <Dropdown value={orientation} options={ORIENTATIONS} onChange={(v) => setOrientation(v as MainOrientation)} aria-label="Orientación principal" />
              </Row>
              <Row label="Modo Snake:">
                <Check checked={snake} onChange={setSnake} />
              </Row>
              <Row label="" disabled={!snake}>
                <Check checked={snake && snakeRotation} onChange={setSnakeRotation} disabled={!snake}>
                  Aplicar rotación de instalación
                </Check>
              </Row>
              <Row label="Orden-Z:" disabled>
                <Dropdown value="ft" options={[{ value: "ft", label: "De adelante hacia atrás" }]} onChange={() => {}} disabled aria-label="Orden-Z" />
              </Row>
              <Row label="Modo Snake Z:" disabled>
                <span className="pr-place-pair">
                  <Check checked={false} onChange={() => {}} disabled>
                    Voltear H
                  </Check>
                  <Check checked={false} onChange={() => {}} disabled>
                    Voltear V
                  </Check>
                </span>
              </Row>
              <Row label="Universo inicial DMX:">
                <span className="pr-place-pair">
                  <NumberField variant="pm" width={56} value={universe} min={1} max={32768} onChange={setUniverse} aria-label="Universo inicial DMX" />
                  <span className="pr-place-sublbl">Desfase:</span>
                  <NumberField variant="pm" width={48} value={universeOffset} min={0} onChange={setUniverseOffset} aria-label="Desfase de universo" />
                </span>
              </Row>
              <Row label="Canal inicial DMX:">
                <span className="pr-place-pair">
                  <NumberField variant="pm" width={56} value={channel} min={1} max={512} onChange={setChannel} aria-label="Canal inicial DMX" />
                  <span className="pr-place-sublbl">Desfase:</span>
                  <NumberField variant="pm" width={48} value={channelOffset} min={0} max={511} onChange={setChannelOffset} aria-label="Desfase de canal" />
                </span>
              </Row>
              <Row label="">
                <Check checked={channelEveryUniverse} onChange={setChannelEveryUniverse}>
                  Aplicar canal inicial DMX a cada universo
                </Check>
              </Row>
              <Row label="Número límite de instalaciones por universo:">
                <NumberField variant="pm" width={56} value={limit} min={1} max={maxPerUniverse} onChange={setPerUniverse} aria-label="Número límite de instalaciones por universo" />
              </Row>
            </Group>
          </div>

          <div className="pr-place-col pr-place-side">
            <Group title="Vista previa de instalación" className="pr-place-prev">
              <ProductPreview type={type} rotation={rotation} />
            </Group>
            <Group title="Vista previa de direccionamiento" className="pr-place-prev">
              <AddressingPreview corner={corner} orientation={orientation} snake={snake} />
            </Group>
          </div>
        </div>

        <div className="pr-devmgr-foot">
          <span className={`pr-devmgr-footstatus${status?.error ? " is-error" : ""}`} title={status?.text}>
            {status?.text ?? ""}
          </span>
          <button className="pr-btn is-primary" type="button" disabled={busy || !type || total === 0} onClick={submit}>
            Añadir
          </button>
          <button className="pr-btn" type="button" onClick={onClose}>
            Cerrar
          </button>
        </div>
      </div>
    </div>
  );
}

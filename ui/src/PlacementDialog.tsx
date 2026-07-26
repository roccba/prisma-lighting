import { useEffect, useMemo, useRef, useState } from "react";
import {
  fixtureTypes,
  nextFreeAddress,
  placeFixtures,
  previewPlacement,
  type Direction,
  type EditorState,
  type FixtureType,
  type Patch,
  type PlacementSpec,
  type PreviewRun,
} from "./api";

// The "Add fixtures" dialog (screenshot 06): Fixture · Placement · Addressing,
// into the ACTIVE zone (§4.1). Signal DIRECTION is separate from orientation and
// all eight are reachable (§4.1). A live signal-path preview shows the result
// before applying. Z and rotation exist but are inert while 2-D.

interface Props {
  patch: Patch;
  activeZone: string;
  onPlaced: (s: EditorState) => void;
  onClose: () => void;
}

const DIRECTIONS: { id: Direction; label: string }[] = [
  { id: "Right", label: "→  Horizontal (L→R)" },
  { id: "Left", label: "←  Horizontal (R→L)" },
  { id: "Down", label: "↓  Vertical (T→B)" },
  { id: "Up", label: "↑  Vertical (B→T)" },
  { id: "DownRight", label: "↘  Diagonal" },
  { id: "DownLeft", label: "↙  Diagonal" },
  { id: "UpRight", label: "↗  Diagonal" },
  { id: "UpLeft", label: "↖  Diagonal" },
];

function NumField(props: { label: string; value: number; onChange?: (n: number) => void; min?: number; disabled?: boolean }) {
  return (
    <label className="field">
      <span>{props.label}</span>
      <input
        type="number"
        value={Number.isFinite(props.value) ? props.value : 0}
        min={props.min}
        disabled={props.disabled}
        onChange={(e) => props.onChange?.(parseInt(e.target.value, 10) || 0)}
      />
    </label>
  );
}

/**
 * The signal path a placement would produce (§4.1), drawn like professional consoles's "Vista previa
 * de direccionamiento" (screenshot 12): ONE continuous serpentine stroke tracing the
 * cable's route through every fixture in signal order — up a column, across, down the
 * next — with a marker at the start end and an arrowhead at the finish. This is what
 * makes the snake and the starting corner legible; per-fixture arrows are gone.
 */
function PreviewCanvas({ runs }: { runs: PreviewRun[] }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth, h = canvas.clientHeight;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = "#0e0f13";
    ctx.fillRect(0, 0, w, h);
    if (runs.length === 0) {
      ctx.fillStyle = "#8a8f9c";
      ctx.font = "12px system-ui";
      ctx.textAlign = "center";
      ctx.fillText("nothing to place", w / 2, h / 2);
      return;
    }

    // Signal order: each run is one fixture's start→end; concatenated in order the
    // vertices form the whole route (the between-fixture segments are the folds).
    const ordered = [...runs].sort((a, b) => a.order - b.order);
    const pts: [number, number][] = [];
    for (const r of ordered) { pts.push(r.start); pts.push(r.end); }

    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const [x, y] of pts) {
      minX = Math.min(minX, x); minY = Math.min(minY, y);
      maxX = Math.max(maxX, x); maxY = Math.max(maxY, y);
    }
    const m = 18;
    const s = Math.max(1, Math.min((w - 2 * m) / Math.max(1, maxX - minX), (h - 2 * m) / Math.max(1, maxY - minY)));
    const X = (x: number) => m + (x - minX) * s;
    const Y = (y: number) => m + (y - minY) * s;

    // One continuous stroke, rounded joins like professional consoles's U-bends.
    ctx.strokeStyle = "#6aa8ff";
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(X(pts[0][0]), Y(pts[0][1]));
    for (let i = 1; i < pts.length; i++) ctx.lineTo(X(pts[i][0]), Y(pts[i][1]));
    ctx.stroke();

    // Start marker — a hollow knob at the signal's origin (professional consoles's rounded start).
    const [sx0, sy0] = [X(pts[0][0]), Y(pts[0][1])];
    ctx.fillStyle = "#0e0f13";
    ctx.strokeStyle = "#eaf1ff";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(sx0, sy0, 4, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();

    // Arrowhead at the very end, along the last segment.
    const [ex, ey] = [X(pts[pts.length - 1][0]), Y(pts[pts.length - 1][1])];
    const [px, py] = [X(pts[pts.length - 2][0]), Y(pts[pts.length - 2][1])];
    const dx = ex - px, dy = ey - py, len = Math.hypot(dx, dy) || 1;
    const ux = dx / len, uy = dy / len, hs = 7;
    ctx.strokeStyle = "#6aa8ff";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(ex, ey);
    ctx.lineTo(ex - ux * hs - uy * hs * 0.5, ey - uy * hs + ux * hs * 0.5);
    ctx.moveTo(ex, ey);
    ctx.lineTo(ex - ux * hs + uy * hs * 0.5, ey - uy * hs - ux * hs * 0.5);
    ctx.stroke();
  }, [runs]);
  return <canvas ref={ref} className="preview-canvas" />;
}

export function PlacementDialog({ patch, activeZone, onPlaced, onClose }: Props) {
  const [types, setTypes] = useState<FixtureType[]>([]);
  const [typeIndex, setTypeIndex] = useState(0);
  const [namePrefix, setNamePrefix] = useState("Tube");
  const [startNumber, setStartNumber] = useState(1);
  const [countX, setCountX] = useState(4);
  const [countY, setCountY] = useState(1);
  const [offsetX, setOffsetX] = useState(2);
  const [offsetY, setOffsetY] = useState(2);
  const [startX, setStartX] = useState(0);
  const [startY, setStartY] = useState(0);
  const [direction, setDirection] = useState<Direction>("Down");
  const [snake, setSnake] = useState(false);
  const [universe, setUniverse] = useState(1);
  const [channel, setChannel] = useState(1);
  const [channelOffset, setChannelOffset] = useState(0);
  const [universeOffset, setUniverseOffset] = useState(0);
  const [perUniverse, setPerUniverse] = useState(0);
  const [runs, setRuns] = useState<PreviewRun[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const zoneName = patch.zones.find((z) => z.id === activeZone)?.name ?? "—";

  useEffect(() => {
    fixtureTypes().then(setTypes).catch(() => setTypes([]));
    nextFreeAddress()
      .then(([u, c]) => { setUniverse(u); setChannel(c); })
      .catch(() => {});
  }, []);

  const spec: PlacementSpec | null = useMemo(() => {
    if (types.length === 0) return null;
    return {
      fixture_type: types[typeIndex],
      name_prefix: namePrefix,
      start_number: startNumber,
      rotation: "R0",
      count: [countX, countY, 1],
      offset: [offsetX, offsetY, 0],
      start_position: [startX, startY, 0],
      direction,
      snake,
      start_universe: universe,
      start_channel: channel,
      channel_offset: channelOffset,
      universe_offset: universeOffset,
      fixtures_per_universe: perUniverse > 0 ? perUniverse : null,
      zone: activeZone,
    };
  }, [types, typeIndex, namePrefix, startNumber, countX, countY, offsetX, offsetY, startX, startY, direction, snake, universe, channel, channelOffset, universeOffset, perUniverse, activeZone]);

  // Live signal-path preview.
  useEffect(() => {
    if (!spec) return;
    previewPlacement(spec).then(setRuns).catch(() => setRuns([]));
  }, [spec]);

  const submit = async () => {
    if (!spec) return;
    setBusy(true);
    setError(null);
    try {
      onPlaced(await placeFixtures(spec));
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const total = Math.max(0, countX) * Math.max(0, countY);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          Add fixtures
          <span className="modal-zone">into zone <strong>{zoneName}</strong></span>
        </div>

        <div className="modal-body">
          <section>
            <h4>Fixture</h4>
            <label className="field">
              <span>Type</span>
              <select value={typeIndex} onChange={(e) => setTypeIndex(parseInt(e.target.value, 10))}>
                {types.map((t, i) => (<option key={i} value={i}>{t.name}</option>))}
              </select>
            </label>
            <label className="field">
              <span>Name prefix</span>
              <input value={namePrefix} onChange={(e) => setNamePrefix(e.target.value)} />
            </label>
            <NumField label="Start #" value={startNumber} onChange={setStartNumber} min={0} />
            <label className="field">
              <span>Rotation (2-D only)</span>
              <select disabled value="R0"><option value="R0">Original</option></select>
            </label>
          </section>

          <section>
            <h4>Placement</h4>
            <div className="field-row">
              <NumField label="Count X" value={countX} onChange={setCountX} min={0} />
              <NumField label="Count Y" value={countY} onChange={setCountY} min={0} />
              <NumField label="Count Z" value={1} disabled />
            </div>
            <div className="field-row">
              <NumField label="Offset X" value={offsetX} onChange={setOffsetX} />
              <NumField label="Offset Y" value={offsetY} onChange={setOffsetY} />
              <NumField label="Offset Z" value={0} disabled />
            </div>
            <div className="field-row">
              <NumField label="Start X" value={startX} onChange={setStartX} />
              <NumField label="Start Y" value={startY} onChange={setStartY} />
              <NumField label="Start Z" value={0} disabled />
            </div>
            <label className="field">
              <span>Signal direction</span>
              <select value={direction} onChange={(e) => setDirection(e.target.value as Direction)}>
                {DIRECTIONS.map((d) => (<option key={d.id} value={d.id}>{d.label}</option>))}
              </select>
            </label>
          </section>

          <section>
            <h4>Addressing</h4>
            <div className="field-row">
              <NumField label="Universe" value={universe} onChange={setUniverse} min={1} />
              <NumField label="+ offset" value={universeOffset} onChange={setUniverseOffset} min={0} />
            </div>
            <div className="field-row">
              <NumField label="Channel" value={channel} onChange={setChannel} min={1} />
              <NumField label="+ offset" value={channelOffset} onChange={setChannelOffset} min={0} />
            </div>
            <NumField label="Fixtures / universe (0 = no limit)" value={perUniverse} onChange={setPerUniverse} min={0} />
            <label className="check">
              <input type="checkbox" checked={snake} onChange={(e) => setSnake(e.target.checked)} />
              <span>Snake mode</span>
            </label>
          </section>
        </div>

        <div className="preview-section">
          <div className="preview-label">Signal path preview</div>
          <PreviewCanvas runs={runs} />
        </div>

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-foot">
          <span className="modal-count">{total} fixtures</span>
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn primary" onClick={submit} disabled={busy || total === 0}>Add</button>
        </div>
      </div>
    </div>
  );
}

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

/** Small live diagram of the runs a placement would produce (§4.1). */
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
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const r of runs)
      for (const p of [r.start, r.end]) {
        minX = Math.min(minX, p[0]); minY = Math.min(minY, p[1]);
        maxX = Math.max(maxX, p[0]); maxY = Math.max(maxY, p[1]);
      }
    const m = 16;
    const s = Math.max(1, Math.min((w - 2 * m) / Math.max(1, maxX - minX), (h - 2 * m) / Math.max(1, maxY - minY)));
    const X = (x: number) => m + (x - minX) * s;
    const Y = (y: number) => m + (y - minY) * s;

    // Snake/signal path between runs (end of one → start of next), faint.
    ctx.strokeStyle = "rgba(150,190,255,0.35)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let i = 0; i < runs.length - 1; i++) {
      ctx.moveTo(X(runs[i].end[0]), Y(runs[i].end[1]));
      ctx.lineTo(X(runs[i + 1].start[0]), Y(runs[i + 1].start[1]));
    }
    ctx.stroke();

    // Each run as an arrow start→end (its signal direction).
    for (const r of runs) {
      const x0 = X(r.start[0]), y0 = Y(r.start[1]), x1 = X(r.end[0]), y1 = Y(r.end[1]);
      ctx.strokeStyle = "#6aa8ff";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
      // start dot
      ctx.fillStyle = "#eaf1ff";
      ctx.beginPath();
      ctx.arc(x0, y0, 2, 0, Math.PI * 2);
      ctx.fill();
      // arrowhead at end
      const dx = x1 - x0, dy = y1 - y0, len = Math.hypot(dx, dy) || 1;
      const ux = dx / len, uy = dy / len, hs = 6;
      ctx.beginPath();
      ctx.moveTo(x1, y1);
      ctx.lineTo(x1 - ux * hs - uy * hs * 0.5, y1 - uy * hs + ux * hs * 0.5);
      ctx.moveTo(x1, y1);
      ctx.lineTo(x1 - ux * hs + uy * hs * 0.5, y1 - uy * hs - ux * hs * 0.5);
      ctx.strokeStyle = "#6aa8ff";
      ctx.stroke();
    }
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

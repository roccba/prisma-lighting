import { useEffect, useMemo, useRef, useState } from "react";
import {
  fixtureTypes,
  nextFreeAddress,
  placeFixtures,
  type Direction,
  type EditorState,
  type FixtureType,
  type Patch,
  type PlacementSpec,
} from "./api";

// The "Add fixtures" dialog (screenshot 06): Fixture · Placement · Addressing,
// into the ACTIVE zone (§4.1). Signal DIRECTION is separate from orientation and
// all eight are reachable (§4.1). A schematic signal-direction preview shows the
// routing before applying. Z and rotation exist but are inert while 2-D.

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

/** A numeric field that can be CLEARED and typed over (a default 0 you cannot delete
 *  is what made these fields annoying). Keeps a local text draft: while it is empty the
 *  parent value is untouched, and it reverts to the last value on blur if still empty. */
function NumField(props: {
  label: string;
  value: number;
  onChange?: (n: number) => void;
  min?: number;
  disabled?: boolean;
}) {
  const [draft, setDraft] = useState(String(props.value));
  useEffect(() => {
    if (parseInt(draft, 10) !== props.value) setDraft(String(props.value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.value]);

  const clampEmit = (raw: string) => {
    const n = parseInt(raw, 10);
    if (Number.isNaN(n)) return;
    const c = props.min != null && n < props.min ? props.min : n;
    props.onChange?.(c);
  };

  return (
    <label className="field">
      <span>{props.label}</span>
      <input
        type="number"
        value={draft}
        min={props.min}
        disabled={props.disabled}
        onChange={(e) => {
          setDraft(e.target.value);
          clampEmit(e.target.value);
        }}
        onBlur={() => {
          // Empty (or garbage) on blur → snap back to the parent's current value.
          if (Number.isNaN(parseInt(draft, 10))) setDraft(String(props.value));
        }}
      />
    </label>
  );
}

/** Normalised vertices (0..1) of a SCHEMATIC signal route for a given direction + snake
 *  — a fixed-resolution illustration of *where the signal starts and which way it runs*,
 *  NOT one mark per fixture. This is the professional consoles "Vista previa de direccionamiento" idea:
 *  the picture shows direction, never quantity, so a 4-pixel and a 10 000-pixel placement
 *  draw the same cheap schematic (and the dialog never chokes rendering thousands). */
function schematicPoints(direction: Direction, snake: boolean): { x: number; y: number }[] {
  const diagonal: Partial<Record<Direction, { x: number; y: number }[]>> = {
    DownRight: [{ x: 0, y: 0 }, { x: 1, y: 1 }],
    DownLeft: [{ x: 1, y: 0 }, { x: 0, y: 1 }],
    UpRight: [{ x: 0, y: 1 }, { x: 1, y: 0 }],
    UpLeft: [{ x: 1, y: 1 }, { x: 0, y: 0 }],
  };
  if (diagonal[direction]) return diagonal[direction]!;

  const vertical = direction === "Up" || direction === "Down";
  const N = 4; // representative lines — fixed, so the schematic never scales with count
  const pts: { x: number; y: number }[] = [];
  for (let i = 0; i < N; i++) {
    const cross = i / (N - 1); // 0..1 across the lines
    const forward = !snake || i % 2 === 0; // snake reverses alternate lines (U-bends)
    const a = forward ? 0 : 1;
    const b = forward ? 1 : 0;
    if (vertical) {
      pts.push({ x: cross, y: a }, { x: cross, y: b });
    } else {
      pts.push({ x: a, y: cross }, { x: b, y: cross });
    }
  }
  // Direction sign: Up flips the vertical travel, Left flips the horizontal travel.
  const flipX = direction === "Left";
  const flipY = direction === "Up";
  return pts.map((p) => ({ x: flipX ? 1 - p.x : p.x, y: flipY ? 1 - p.y : p.y }));
}

/** Draws the schematic route: one blue stroke, a hollow knob at the signal start, an
 *  arrowhead at the finish (§4.1). Cheap and constant-cost — depends only on direction. */
function DirectionPreview({ direction, snake }: { direction: Direction; snake: boolean }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth,
      h = canvas.clientHeight;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const css = getComputedStyle(document.documentElement);
    const accent = css.getPropertyValue("--accent-bright").trim() || "#1F9CFF";
    const bg = css.getPropertyValue("--bg-input").trim() || "#000";
    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, w, h);

    const pts = schematicPoints(direction, snake);
    const m = 22;
    const X = (x: number) => m + x * (w - 2 * m);
    const Y = (y: number) => m + y * (h - 2 * m);

    ctx.strokeStyle = accent;
    ctx.lineWidth = 3;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(X(pts[0].x), Y(pts[0].y));
    for (let i = 1; i < pts.length; i++) ctx.lineTo(X(pts[i].x), Y(pts[i].y));
    ctx.stroke();

    // Start knob — a hollow circle at the signal origin.
    ctx.fillStyle = bg;
    ctx.strokeStyle = "#eaf1ff";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(X(pts[0].x), Y(pts[0].y), 6, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();

    // Arrowhead at the finish, along the last segment.
    const a = pts[pts.length - 1],
      b = pts[pts.length - 2];
    const ex = X(a.x),
      ey = Y(a.y);
    const dx = ex - X(b.x),
      dy = ey - Y(b.y);
    const len = Math.hypot(dx, dy) || 1;
    const ux = dx / len,
      uy = dy / len,
      hs = 12;
    ctx.strokeStyle = accent;
    ctx.lineWidth = 3;
    ctx.beginPath();
    ctx.moveTo(ex, ey);
    ctx.lineTo(ex - ux * hs - uy * hs * 0.5, ey - uy * hs + ux * hs * 0.5);
    ctx.moveTo(ex, ey);
    ctx.lineTo(ex - ux * hs + uy * hs * 0.5, ey - uy * hs - ux * hs * 0.5);
    ctx.stroke();
  }, [direction, snake]);
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
  // Default the start row to a FREE band below the existing rig, so a new zone's fixtures
  // land in empty workspace instead of stacking on top of what is already placed (the
  // operator can still aim anywhere). Computed once from the patch when the dialog opens.
  const [startY, setStartY] = useState(() => {
    let maxY = -Infinity;
    for (const f of patch.fixtures) for (const p of f.pixels) maxY = Math.max(maxY, p.position[1]);
    return Number.isFinite(maxY) ? Math.ceil(maxY) + 2 : 0;
  });
  const [direction, setDirection] = useState<Direction>("Down");
  const [snake, setSnake] = useState(false);
  const [universe, setUniverse] = useState(1);
  const [channel, setChannel] = useState(1);
  const [channelOffset, setChannelOffset] = useState(0);
  const [universeOffset, setUniverseOffset] = useState(0);
  const [perUniverse, setPerUniverse] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const zoneName = patch.zones.find((z) => z.id === activeZone)?.name ?? "—";

  useEffect(() => {
    fixtureTypes().then(setTypes).catch(() => setTypes([]));
    nextFreeAddress()
      .then(([u, c]) => {
        setUniverse(u);
        setChannel(c);
      })
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
          <div className="preview-label">Signal direction preview</div>
          <DirectionPreview direction={direction} snake={snake} />
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

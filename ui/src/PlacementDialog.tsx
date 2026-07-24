import { useEffect, useState } from "react";
import {
  fixtureTypes,
  nextFreeAddress,
  placeFixtures,
  type EditorState,
  type FixtureType,
  type Orientation,
  type Patch,
  type PlacementSpec,
} from "./api";

// The "Add fixtures" dialog, modelled on professional consoles's (screenshot 06) — three sections:
// Fixture, Placement, Addressing. Placed fixtures go into the ACTIVE zone (§4.1),
// which is what finally makes the zone selector the editor's organising control (§6).

interface Props {
  patch: Patch;
  activeZone: string;
  onPlaced: (s: EditorState) => void;
  onClose: () => void;
}

const ORIENTATIONS: { id: Orientation; label: string }[] = [
  { id: "Horizontal", label: "Horizontal" },
  { id: "Vertical", label: "Vertical" },
  { id: "DiagonalDown", label: "Diagonal ↘" },
  { id: "DiagonalUp", label: "Diagonal ↗" },
];

function NumField({ label, value, onChange, min }: { label: string; value: number; onChange: (n: number) => void; min?: number }) {
  return (
    <label className="field">
      <span>{label}</span>
      <input
        type="number"
        value={Number.isFinite(value) ? value : 0}
        min={min}
        onChange={(e) => onChange(parseInt(e.target.value, 10) || 0)}
      />
    </label>
  );
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
  const [orientation, setOrientation] = useState<Orientation>("Vertical");
  const [snake, setSnake] = useState(false);
  const [universe, setUniverse] = useState(1);
  const [channel, setChannel] = useState(1);
  const [perUniverse, setPerUniverse] = useState(0); // 0 = no limit
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

  const submit = async () => {
    if (types.length === 0) return;
    setBusy(true);
    setError(null);
    const spec: PlacementSpec = {
      fixture_type: types[typeIndex],
      name_prefix: namePrefix,
      start_number: startNumber,
      count: [countX, countY],
      offset: [offsetX, offsetY],
      start_position: [startX, startY],
      orientation,
      snake,
      start_universe: universe,
      start_channel: channel,
      fixtures_per_universe: perUniverse > 0 ? perUniverse : null,
      zone: activeZone,
    };
    try {
      const state = await placeFixtures(spec);
      onPlaced(state);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const total = Math.max(0, countX) * Math.max(0, countY);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          Add fixtures
          <span className="modal-zone">
            into zone <strong>{zoneName}</strong>
          </span>
        </div>

        <div className="modal-body">
          <section>
            <h4>Fixture</h4>
            <label className="field">
              <span>Type</span>
              <select value={typeIndex} onChange={(e) => setTypeIndex(parseInt(e.target.value, 10))}>
                {types.map((t, i) => (
                  <option key={i} value={i}>
                    {t.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>Name prefix</span>
              <input value={namePrefix} onChange={(e) => setNamePrefix(e.target.value)} />
            </label>
            <NumField label="Start #" value={startNumber} onChange={setStartNumber} min={0} />
          </section>

          <section>
            <h4>Placement</h4>
            <div className="field-row">
              <NumField label="Count X" value={countX} onChange={setCountX} min={0} />
              <NumField label="Count Y" value={countY} onChange={setCountY} min={0} />
            </div>
            <div className="field-row">
              <NumField label="Offset X" value={offsetX} onChange={setOffsetX} />
              <NumField label="Offset Y" value={offsetY} onChange={setOffsetY} />
            </div>
            <div className="field-row">
              <NumField label="Start X" value={startX} onChange={setStartX} />
              <NumField label="Start Y" value={startY} onChange={setStartY} />
            </div>
            <label className="field">
              <span>Orientation</span>
              <select value={orientation} onChange={(e) => setOrientation(e.target.value as Orientation)}>
                {ORIENTATIONS.map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.label}
                  </option>
                ))}
              </select>
            </label>
          </section>

          <section>
            <h4>Addressing</h4>
            <div className="field-row">
              <NumField label="Universe" value={universe} onChange={setUniverse} min={1} />
              <NumField label="Channel" value={channel} onChange={setChannel} min={1} />
            </div>
            <NumField label="Fixtures / universe (0 = no limit)" value={perUniverse} onChange={setPerUniverse} min={0} />
            <label className="check">
              <input type="checkbox" checked={snake} onChange={(e) => setSnake(e.target.checked)} />
              <span>Snake mode</span>
            </label>
          </section>
        </div>

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-foot">
          <span className="modal-count">{total} fixtures</span>
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={submit} disabled={busy || total === 0}>
            Add
          </button>
        </div>
      </div>
    </div>
  );
}

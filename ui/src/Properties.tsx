import type { Patch, SelectionAggregate } from "./api";

// The right panel switches on selection state (screenshot 05): a whole-patch
// summary when nothing is selected, the selection aggregate otherwise (§5.4).

interface Props {
  patch: Patch;
  selection: Set<string>;
  aggregate: SelectionAggregate | null;
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="prop-row">
      <span className="prop-label">{label}</span>
      <span className="prop-value">{value}</span>
    </div>
  );
}

const range = (a: number, b: number) => (a === b ? `${a}` : `${a} – ${b}`);
const frange = (a: number, b: number) => (a === b ? `${a}` : `${a} – ${b}`);

export function Properties({ patch, selection, aggregate }: Props) {
  if (selection.size > 0 && aggregate) {
    const a = aggregate;
    return (
      <div className="panel">
        <div className="panel-head">Selection</div>
        <Row label="Fixtures" value={`${a.fixture_count}`} />
        <Row label="Pixels" value={`${a.pixel_count}`} />
        <Row label="X" value={frange(a.position_min[0], a.position_max[0])} />
        <Row label="Y" value={frange(a.position_min[1], a.position_max[1])} />
        <Row label="Z" value={frange(a.position_min[2], a.position_max[2])} />
        <Row label="Universes" value={range(a.universe_min, a.universe_max)} />
        <Row label="Start ch." value={range(a.channel_min, a.channel_max)} />
      </div>
    );
  }

  // Whole-patch summary.
  const pixels = patch.fixtures.reduce((n, f) => n + f.pixels.length, 0);
  const channels = patch.fixtures.reduce(
    (n, f) => n + f.pixels.reduce((m) => m + 3, 0),
    0,
  );
  return (
    <div className="panel">
      <div className="panel-head">Patch</div>
      <Row label="Name" value={patch.name} />
      <Row label="Fixtures" value={`${patch.fixtures.length}`} />
      <Row label="Zones" value={`${patch.zones.length}`} />
      <Row label="Pixels" value={`${pixels}`} />
      <Row label="DMX channels" value={`${channels}`} />
      {/* The world/venue canvas — auto-fitted to the rig (not an operator knob). Shown
          as info only; distinct from a zone's own render resolution (toolbar → Resolution…). */}
      <Row label="World (auto)" value={`${patch.matrix_size[0]} × ${patch.matrix_size[1]}`} />
      <div className="zones-legend">
        {patch.zones.map((z) => (
          <div key={z.id} className="legend-row">
            <span className="swatch" style={{ background: `rgb(${z.display_color.join(",")})` }} />
            <span>{z.name}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

import { useMemo, useState } from "react";
import { diagnosticFixtures, type Diagnostic, type Patch } from "./api";

// S6 (replanned): DMX wire faults surfaced DISCREETLY — a subtle notice, not a red
// banner (§1, §4.3). The two faults are an address collision and an out-of-range
// channel; both are also marked on the canvas. Everything else the old S6 flagged
// (out-of-matrix, shared cells) is a valid choice and no longer surfaced.

interface Props {
  patch: Patch;
  diagnostics: Diagnostic[];
  onSelectFixtures: (ids: string[]) => void;
}

function describe(d: Diagnostic, fx: Map<string, string>): string {
  const name = (id: string) => fx.get(id) ?? "unknown fixture";
  if (d.kind === "AddressCollision") {
    const chans =
      d.channel_start === d.channel_end
        ? `channel ${d.channel_start}`
        : `channels ${d.channel_start}–${d.channel_end}`;
    return `Collision — universe ${d.universe}, ${chans}: ${d.fixtures.map(name).join(" & ")}`;
  }
  return `Channel out of range — ${name(d.mark.fixture)}: DMX ${d.start}–${
    d.start + d.span - 1
  } exceeds 512`;
}

function summary(diagnostics: Diagnostic[]): string {
  const collisions = diagnostics.filter((d) => d.kind === "AddressCollision").length;
  const ranges = diagnostics.length - collisions;
  const parts: string[] = [];
  if (collisions > 0) parts.push(`${collisions} address ${collisions === 1 ? "collision" : "collisions"}`);
  if (ranges > 0) parts.push(`${ranges} channel out of range`);
  return parts.join(" · ");
}

export function WireFaults({ patch, diagnostics, onSelectFixtures }: Props) {
  const [open, setOpen] = useState(false);
  const fx = useMemo(() => new Map(patch.fixtures.map((f) => [f.id, f.name])), [patch]);

  if (diagnostics.length === 0) return null;

  const allFixtures = [...new Set(diagnostics.flatMap(diagnosticFixtures))];

  return (
    <div className="wirefaults">
      <button
        className="wirefaults-bar"
        onClick={() => setOpen((o) => !o)}
        title="DMX wire faults — click to list; each row selects the fixtures involved"
      >
        <span className="wirefaults-dot" />
        <span className="wirefaults-text">{summary(diagnostics)}</span>
        <span
          className="wirefaults-select"
          onClick={(e) => {
            e.stopPropagation();
            onSelectFixtures(allFixtures);
          }}
          title="Select all involved fixtures"
        >
          select
        </span>
        <span className="wirefaults-toggle">{open ? "▾" : "▸"}</span>
      </button>

      {open && (
        <ul className="wirefaults-list">
          {diagnostics.map((d, i) => (
            <li
              key={i}
              className="wirefaults-item"
              onClick={() => onSelectFixtures(diagnosticFixtures(d))}
              title="Select the fixtures involved"
            >
              {describe(d, fx)}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

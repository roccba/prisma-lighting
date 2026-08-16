import { ZoneCard } from "./ZoneCard";
import type { ZoneInfo } from "./api";

// The zone row (M8-SPEC §3.1): every zone at once, horizontally, so the operator sees
// the whole venue at a glance. Selecting a card is a UI state (which zone the grid /
// preview / parameters will show, in later steps) — it never changes what a zone
// outputs (selected ≠ active, §3.2). Presentational: the selected id and the zones
// data are owned by the main window.

interface ZoneRowProps {
  zones: ZoneInfo[];
  selected: string | null;
  onSelect: (id: string) => void;
}

export function ZoneRow({ zones, selected, onSelect }: ZoneRowProps) {
  return (
    <div className="pr-zonerow">
      {zones.length === 0 ? (
        <div className="pr-zonerow-empty">No zones in the show</div>
      ) : (
        zones.map((z) => (
          <ZoneCard
            key={z.id}
            zone={z}
            selected={z.id === selected}
            onSelect={() => onSelect(z.id)}
          />
        ))
      )}
    </div>
  );
}

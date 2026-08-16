import { useEffect, useState } from "react";
import { assignCell, clearCell, triggerCell, zoneGrid, type CellInfo } from "./api";
import { EffectPicker } from "./EffectPicker";

// The scene grid of the SELECTED zone (M8-SPEC §3.3): a grid of assignable cells (the
// zone's "scenes"). Empty cells open the effect picker; assigned cells trigger — which
// makes that cell the zone's active content, enqueued through the frame-boundary queue
// (step 2), so the change shows in the zone's mini-preview and on the wire. The active
// cell is marked with the accent. Per zone, not cross-zone (the product's thesis): this
// is the selected zone's own grid; switching the selected zone swaps to its cells.

interface SceneGridProps {
  zoneId: string;
}

export function SceneGrid({ zoneId }: SceneGridProps) {
  const [cells, setCells] = useState<CellInfo[]>([]);
  const [pickerFor, setPickerFor] = useState<number | null>(null);

  useEffect(() => {
    zoneGrid(zoneId)
      .then(setCells)
      .catch(() => setCells([]));
    setPickerFor(null);
  }, [zoneId]);

  const onCellClick = (cell: CellInfo) => {
    if (cell.effect_id) {
      triggerCell(zoneId, cell.index)
        .then(setCells)
        .catch(() => {});
    } else {
      setPickerFor(cell.index);
    }
  };

  const onPick = (effectId: string) => {
    const idx = pickerFor;
    setPickerFor(null);
    if (idx === null) return;
    assignCell(zoneId, idx, effectId)
      .then(setCells)
      .catch(() => {});
  };

  const onClear = (e: React.PointerEvent, cell: CellInfo) => {
    e.stopPropagation();
    clearCell(zoneId, cell.index)
      .then(setCells)
      .catch(() => {});
  };

  return (
    <div className="pr-grid-wrap">
      <div className="pr-grid">
        {cells.map((cell) => (
          <div
            key={cell.index}
            className={`pr-cell ${cell.effect_id ? "is-assigned" : "is-empty"}${cell.active ? " is-active" : ""}`}
            onPointerDown={() => onCellClick(cell)}
            role="button"
            tabIndex={0}
            title={cell.effect_name ?? "Vacío — asignar un efecto"}
          >
            <span className="pr-cell-num">{cell.index + 1}</span>
            {cell.effect_id ? (
              <span className="pr-cell-name">{cell.effect_name}</span>
            ) : (
              <span className="pr-cell-plus">+</span>
            )}
            {cell.effect_id && (
              <button
                className="pr-cell-clear"
                onPointerDown={(e) => onClear(e, cell)}
                title="Vaciar celda"
              >
                ×
              </button>
            )}
          </div>
        ))}
      </div>
      {pickerFor !== null && <EffectPicker onPick={onPick} onClose={() => setPickerFor(null)} />}
    </div>
  );
}

import { useEffect, useMemo, useState } from "react";
import { setZoneMappingSize, type EditorState, type Patch, type Zone } from "./api";
import { NumberField } from "./NumberField";

// "Ajustes de matriz" — modelled on the classic Matrix Settings. Edits the selected zone's
// mapping_size: the resolution its effects RENDER at (§2.4), which the render then places
// over the zone's footprint in the world. "Igualar el área del mapa de voxels" sets it to
// that footprint (the bounding box of the zone's pixels), i.e. one cell per pixel — the
// sharpest result; a smaller matrix is stretched over the rig and looks blocky.
// X/Y only (Z stays 1 while 2-D); capped at 512×512.

const MAX = 512;

interface Props {
  zone: Zone;
  patch: Patch;
  onApplied: (s: EditorState) => void;
  onClose: () => void;
}

/** The zone's footprint in cells: the bounding box of its fixtures' pixels (nearest cell,
 *  as the gather samples them). `null` when the zone has no pixels yet. */
function zoneFootprint(patch: Patch, zoneId: string): [number, number] | null {
  let minX = Infinity,
    minY = Infinity,
    maxX = -Infinity,
    maxY = -Infinity;
  for (const f of patch.fixtures) {
    if (f.zone !== zoneId) continue;
    for (const p of f.pixels) {
      const x = Math.round(p.position[0]);
      const y = Math.round(p.position[1]);
      minX = Math.min(minX, x);
      minY = Math.min(minY, y);
      maxX = Math.max(maxX, x);
      maxY = Math.max(maxY, y);
    }
  }
  if (!Number.isFinite(minX)) return null;
  return [Math.min(MAX, maxX - minX + 1), Math.min(MAX, maxY - minY + 1)];
}

export function MatrixDialog({ zone, patch, onApplied, onClose }: Props) {
  const [x, setX] = useState(zone.mapping_size[0]);
  const [y, setY] = useState(zone.mapping_size[1]);
  const footprint = useMemo(() => zoneFootprint(patch, zone.id), [patch, zone.id]);

  const apply = async () => {
    onApplied(await setZoneMappingSize(zone.id, [x, y]));
    onClose();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="pr-picker-backdrop">
      <div className="pr-picker pr-matrix" role="dialog" aria-label="Ajustes de matriz">
        <div className="pr-picker-head pr-place-title">
          <span>Ajustes de matriz</span>
          <span className="pr-place-zone">
            Zona: <b>{zone.name}</b>
          </span>
        </div>
        <div className="pr-picker-body pr-matrix-body">
          <button
            className="pr-btn pr-matrix-fit"
            type="button"
            disabled={!footprint}
            onClick={() => {
              if (!footprint) return;
              setX(footprint[0]);
              setY(footprint[1]);
            }}
          >
            Igualar el área del mapa de voxels
          </button>
          <div className="pr-place-row">
            <span>Tamaño X:</span>
            <NumberField variant="pm" width={56} value={x} min={1} max={MAX} onChange={setX} aria-label="Tamaño X" />
          </div>
          <div className="pr-place-row">
            <span>Tamaño Y:</span>
            <NumberField variant="pm" width={56} value={y} min={1} max={MAX} onChange={setY} aria-label="Tamaño Y" />
          </div>
          <div className="pr-place-row is-off">
            <span>Tamaño Z:</span>
            <NumberField variant="pm" width={56} value={zone.mapping_size[2]} disabled onChange={() => {}} aria-label="Tamaño Z" />
          </div>
        </div>
        <div className="pr-devmgr-foot">
          <button className="pr-btn is-primary" type="button" onClick={apply}>
            Aceptar
          </button>
          <button className="pr-btn" type="button" onClick={onClose}>
            Cancelar
          </button>
        </div>
      </div>
    </div>
  );
}

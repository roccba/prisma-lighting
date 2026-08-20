import { useState } from "react";
import { setZoneMappingSize, type EditorState, type Zone } from "./api";

// Matrix settings dialog (§2.5), modelled on professional consoles's "Ajustes de Matriz" (screenshot
// 11). Edits the selected zone's mapping_size — its effect RENDER RESOLUTION (§2.4),
// not a visual bound. X/Y only (Z stays 1 while 2-D); capped at 512×512.

const MAX = 512;

interface Props {
  zone: Zone;
  onApplied: (s: EditorState) => void;
  onClose: () => void;
}

export function MatrixDialog({ zone, onApplied, onClose }: Props) {
  // Text drafts so a field can be CLEARED and typed over (a default you cannot delete is
  // what made these annoying). Parsed + clamped on Apply.
  const [x, setX] = useState(String(zone.mapping_size[0]));
  const [y, setY] = useState(String(zone.mapping_size[1]));

  const clamp = (v: string) => {
    const n = parseInt(v, 10);
    return Number.isNaN(n) ? 1 : Math.max(1, Math.min(MAX, n));
  };

  const apply = async () => {
    onApplied(await setZoneMappingSize(zone.id, [clamp(x), clamp(y)]));
    onClose();
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal matrix-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          Render resolution
          <span className="modal-zone">
            zone <strong>{zone.name}</strong>
          </span>
        </div>
        <div className="modal-message">
          This is the zone's <strong>render resolution</strong> — the detail its effects run
          at, independent of every other zone. It is <em>not</em> the number of pixels that
          light (your fixtures decide that), and it is separate from the <strong>world</strong>,
          which auto-fits to the whole rig.
        </div>
        <div className="matrix-fields">
          <label className="field">
            <span>Size X</span>
            <input
              type="number"
              min={1}
              max={MAX}
              value={x}
              onChange={(e) => setX(e.target.value)}
              autoFocus
            />
          </label>
          <span className="matrix-x">×</span>
          <label className="field">
            <span>Size Y</span>
            <input
              type="number"
              min={1}
              max={MAX}
              value={y}
              onChange={(e) => setY(e.target.value)}
            />
          </label>
          <span className="matrix-cap">max {MAX}×{MAX}</span>
        </div>
        <div className="modal-foot">
          <button className="btn" onClick={onClose}>Cancel</button>
          <div className="spacer" />
          <button className="btn primary" onClick={apply}>Apply</button>
        </div>
      </div>
    </div>
  );
}

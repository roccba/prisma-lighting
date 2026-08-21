import { useEffect, useState } from "react";
import { effectCatalogue, type EffectListing } from "./api";

// The effect picker (M8-SPEC §4): assigning a cell opens this, an
// effect list organised by category. Built from the engine's catalogue, never a
// hardcoded list (§5.3) — the day Noise (or an audio-reactive category) is added on
// the Rust side it appears here with no UI change. M8 offers the M5 generators.

interface EffectPickerProps {
  onPick: (effectId: string) => void;
  onClose: () => void;
}

export function EffectPicker({ onPick, onClose }: EffectPickerProps) {
  const [items, setItems] = useState<EffectListing[]>([]);

  useEffect(() => {
    effectCatalogue()
      .then(setItems)
      .catch(() => setItems([]));
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const categories = Array.from(new Set(items.map((i) => i.category)));

  return (
    <div className="pr-picker-backdrop" onPointerDown={onClose}>
      <div className="pr-picker" onPointerDown={(e) => e.stopPropagation()}>
        <div className="pr-picker-head">Elegir efecto</div>
        <div className="pr-picker-body">
          {categories.length === 0 ? (
            <div className="pr-picker-empty">Sin efectos disponibles</div>
          ) : (
            categories.map((cat) => (
              <div key={cat} className="pr-picker-cat">
                <div className="pr-picker-cat-title">{cat}</div>
                <div className="pr-picker-items">
                  {items
                    .filter((i) => i.category === cat)
                    .map((i) => (
                      <button
                        key={i.id}
                        className="pr-btn pr-picker-item"
                        onClick={() => onPick(i.id)}
                      >
                        {i.name}
                      </button>
                    ))}
                </div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

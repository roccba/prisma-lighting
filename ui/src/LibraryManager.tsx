import { useEffect, useState } from "react";
import {
  addFixtureType,
  getLibrary,
  removeFixtureType,
  updateFixtureType,
  type ColorOrder,
  type ComponentSet,
  type FixtureLibrary,
  type FixtureType,
} from "./api";

// Fixture library manager (§3.2): a simple CRUD over the operator's reusable pixel-LED
// fixture types. Our domain is only pixel LED — N pixels + colour order — so a type is
// just name / pixel count / arrangement / component set / colour order. NO DMX
// personalities, channel modes or macros (this is deliberately not a full-blown
// fixture editor). Intra-fixture roles (§3.2 hexagon) exist in the model but have no UI
// yet. Every change persists to the library file via the backend.

const COMPONENTS: ComponentSet[] = ["Rgb", "Rgbw"];
const COLOR_ORDERS: ColorOrder[] = ["Rgb", "Rbg", "Grb", "Gbr", "Brg", "Bgr"];

interface Props {
  onClose: () => void;
}

function blankType(): FixtureType {
  return {
    name: "New fixture",
    pixel_count: 60,
    components: "Rgb",
    color_order: "Grb",
    arrangement: "Strip",
    roles: [],
  };
}

export function LibraryManager({ onClose }: Props) {
  const [lib, setLib] = useState<FixtureLibrary | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The type being added (index null) or edited (index set). Null = list view.
  const [form, setForm] = useState<{ index: number | null; value: FixtureType } | null>(null);

  useEffect(() => {
    getLibrary().then(setLib).catch((e) => setError(String(e)));
  }, []);

  const run = async (p: Promise<FixtureLibrary>) => {
    try {
      setLib(await p);
      setForm(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const save = () => {
    if (!form) return;
    const value = { ...form.value, name: form.value.name.trim() || "Unnamed", pixel_count: Math.max(1, Math.round(form.value.pixel_count || 1)) };
    run(form.index === null ? addFixtureType(value) : updateFixtureType(form.index, value));
  };

  const set = (patch: Partial<FixtureType>) => setForm((f) => (f ? { ...f, value: { ...f.value, ...patch } } : f));

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal library-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          Fixture library
          {lib && <span className="modal-zone">{lib.types.length} types</span>}
        </div>

        {error && <div className="modal-error">{error}</div>}

        {!lib ? (
          <div className="modal-message">Loading…</div>
        ) : form ? (
          <div className="library-form">
            <label className="field">
              <span>Name</span>
              <input value={form.value.name} autoFocus onChange={(e) => set({ name: e.target.value })} />
            </label>
            <div className="field-row">
              <label className="field">
                <span>Pixel count</span>
                <input
                  type="number"
                  min={1}
                  value={form.value.pixel_count}
                  onChange={(e) => set({ pixel_count: parseInt(e.target.value, 10) || 0 })}
                />
              </label>
              <label className="field">
                <span>Component set</span>
                <select value={form.value.components} onChange={(e) => set({ components: e.target.value as ComponentSet })}>
                  {COMPONENTS.map((c) => (<option key={c} value={c}>{c.toUpperCase()}</option>))}
                </select>
              </label>
            </div>
            <div className="field-row">
              <label className="field">
                <span>Colour order</span>
                <select value={form.value.color_order} onChange={(e) => set({ color_order: e.target.value as ColorOrder })}>
                  {COLOR_ORDERS.map((c) => (<option key={c} value={c}>{c.toUpperCase()}</option>))}
                </select>
              </label>
              <label className="field">
                <span>Arrangement</span>
                <select value={form.value.arrangement} disabled>
                  <option value="Strip">Strip (1-D)</option>
                </select>
              </label>
            </div>
            <div className="modal-foot">
              <button className="btn" onClick={() => setForm(null)}>Cancel</button>
              <div className="spacer" />
              <button className="btn primary" onClick={save}>
                {form.index === null ? "Add" : "Save"}
              </button>
            </div>
          </div>
        ) : (
          <>
            <ul className="library-list">
              {lib.types.map((t, i) => (
                <li key={i} className="library-item">
                  <div className="library-item-main">
                    <span className="library-item-name">{t.name}</span>
                    <span className="library-item-meta">
                      {t.pixel_count}px · {t.components.toUpperCase()} · {t.color_order.toUpperCase()}
                    </span>
                  </div>
                  <button className="btn small" onClick={() => setForm({ index: i, value: { ...t } })}>Edit</button>
                  <button className="btn small" onClick={() => run(removeFixtureType(i))}>Delete</button>
                </li>
              ))}
              {lib.types.length === 0 && <li className="library-empty">No fixture types yet.</li>}
            </ul>
            <div className="modal-foot">
              <button className="btn" onClick={() => setForm({ index: null, value: blankType() })}>+ Add type</button>
              <div className="spacer" />
              <button className="btn primary" onClick={onClose}>Done</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

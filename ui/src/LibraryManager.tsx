import { useEffect, useState } from "react";
import {
  addFixtureType,
  DEFAULT_WHITE,
  getLibrary,
  removeFixtureType,
  updateFixtureType,
  type ColorOrder,
  type ComponentSet,
  type FixtureLibrary,
  type FixtureType,
  type WhiteMode,
} from "./api";

// Fixture library manager (§3.2): a simple CRUD over the operator's reusable pixel-LED
// fixture types. Our domain is only pixel LED — N pixels + colour order — so a type is
// just name / pixel count / arrangement / component set / colour order. NO DMX
// personalities, channel modes or macros (this is deliberately not a full-blown
// fixture editor). Intra-fixture roles (§3.2 hexagon) exist in the model but have no UI
// yet. Every change persists to the library file via the backend.

/** Standard fixtures every install ships with: generic single pixels and 20-pixel tubes
 *  in the common component sets / colour orders. Frontend constants — not in the user's
 *  library file, so they can be used but never edited or deleted. */
const builtin = (name: string, pixel_count: number, components: ComponentSet, color_order: ColorOrder): FixtureType => ({
  name,
  pixel_count,
  components,
  color_order,
  arrangement: "Strip",
  roles: [],
  white: DEFAULT_WHITE,
});
export const BUILTIN_FIXTURES: FixtureType[] = [
  builtin("Píxel RGB", 1, "Rgb", "Rgb"),
  builtin("Píxel GRB", 1, "Rgb", "Grb"),
  builtin("Píxel RGBW", 1, "Rgbw", "Rgb"),
  builtin("Píxel GRBW", 1, "Rgbw", "Grb"),
  builtin("Tubo RGB 20", 20, "Rgb", "Rgb"),
  builtin("Tubo GRB 20", 20, "Rgb", "Grb"),
  builtin("Tubo RGBW 20", 20, "Rgbw", "Rgb"),
  builtin("Tubo GRBW 20", 20, "Rgbw", "Grb"),
  builtin("Píxel RGBWW", 1, "Rgbww", "Rgb"),
  builtin("Tubo RGBWW 20", 20, "Rgbww", "Rgb"),
];

/** One readable line per fixture: its size and the channel order on the wire, e.g.
 *  "20 píxeles · orden GRBW" (an RGBW fixture sends its colour order, then white). */
const fixtureMeta = (t: FixtureType) =>
  `${t.pixel_count} ${t.pixel_count === 1 ? "píxel" : "píxeles"} · orden ${t.color_order.toUpperCase()}${
    t.components === "Rgbww" ? " + blanco cálido y frío" : t.components === "Rgbw" ? "W" : ""
  }`;

const COMPONENTS: ComponentSet[] = ["Rgb", "Rgbw", "Rgbww"];
const WHITE_MODES: { value: WhiteMode; label: string }[] = [
  { value: "extract", label: "Extraer (blanco = mín RGB, se resta)" },
  { value: "add", label: "Sumar (blanco = mín RGB, sin restar)" },
  { value: "off", label: "Apagado" },
];
const COLOR_ORDERS: ColorOrder[] = ["Rgb", "Rbg", "Grb", "Gbr", "Brg", "Bgr"];

interface Props {
  onClose: () => void;
}

function blankType(): FixtureType {
  return {
    name: "Nuevo fixture",
    pixel_count: 60,
    components: "Rgb",
    color_order: "Grb",
    arrangement: "Strip",
    roles: [],
    white: DEFAULT_WHITE,
  };
}

export function LibraryManager({ onClose }: Props) {
  const [lib, setLib] = useState<FixtureLibrary | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The type being added (index null) or edited (index set). Null = list view.
  const [form, setForm] = useState<{ index: number | null; value: FixtureType } | null>(null);
  // Collapsed groups of the list (by heading), like the classic per-manufacturer tree.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const toggleGroup = (g: string) =>
    setCollapsed((c) => {
      const next = new Set(c);
      if (next.has(g)) next.delete(g);
      else next.add(g);
      return next;
    });
  const groupHead = (g: string) => (
    <li className="library-group">
      <button type="button" className="library-group-head" aria-expanded={!collapsed.has(g)} onClick={() => toggleGroup(g)}>
        <span className="library-group-sign">{collapsed.has(g) ? "+" : "−"}</span>
        {g}
      </button>
    </li>
  );

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
    const value = { ...form.value, name: form.value.name.trim() || "Sin nombre", pixel_count: Math.max(1, Math.round(form.value.pixel_count || 1)) };
    run(form.index === null ? addFixtureType(value) : updateFixtureType(form.index, value));
  };

  const set = (patch: Partial<FixtureType>) => setForm((f) => (f ? { ...f, value: { ...f.value, ...patch } } : f));

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal library-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          Biblioteca de fixtures
        </div>

        {error && <div className="modal-error">{error}</div>}

        {!lib ? (
          <div className="modal-message">Cargando…</div>
        ) : form ? (
          <div className="library-form">
            <label className="field">
              <span>Nombre</span>
              <input value={form.value.name} autoFocus onChange={(e) => set({ name: e.target.value })} />
            </label>
            <div className="field-row">
              <label className="field">
                <span>Cantidad de píxeles</span>
                <input
                  type="number"
                  min={1}
                  value={form.value.pixel_count}
                  onChange={(e) => set({ pixel_count: parseInt(e.target.value, 10) || 0 })}
                />
              </label>
              <label className="field">
                <span>Componentes</span>
                <select value={form.value.components} onChange={(e) => set({ components: e.target.value as ComponentSet })}>
                  {COMPONENTS.map((c) => (<option key={c} value={c}>{c.toUpperCase()}</option>))}
                </select>
              </label>
            </div>
            <div className="field-row">
              <label className="field">
                <span>Orden de color</span>
                <select value={form.value.color_order} onChange={(e) => set({ color_order: e.target.value as ColorOrder })}>
                  {COLOR_ORDERS.map((c) => (<option key={c} value={c}>{c.toUpperCase()}</option>))}
                </select>
              </label>
              <label className="field">
                <span>Disposición</span>
                <select value={form.value.arrangement} disabled>
                  <option value="Strip">Tira (1-D)</option>
                </select>
              </label>
            </div>
            {form.value.components !== "Rgb" && (
              <div className="field-row">
                <label className="field">
                  <span>Blanco</span>
                  <select
                    value={(form.value.white ?? DEFAULT_WHITE).mode}
                    onChange={(e) => set({ white: { ...(form.value.white ?? DEFAULT_WHITE), mode: e.target.value as WhiteMode } })}
                  >
                    {WHITE_MODES.map((m) => (<option key={m.value} value={m.value}>{m.label}</option>))}
                  </select>
                </label>
                {form.value.components === "Rgbw" ? (
                  <label className="field">
                    <span>Temperatura del blanco (K)</span>
                    <input
                      type="number"
                      min={1500}
                      max={12000}
                      step={100}
                      value={(form.value.white ?? DEFAULT_WHITE).kelvin}
                      onChange={(e) => set({ white: { ...(form.value.white ?? DEFAULT_WHITE), kelvin: parseInt(e.target.value, 10) || 4000 } })}
                    />
                  </label>
                ) : (
                  <>
                    <label className="field">
                      <span>Blanco cálido (K)</span>
                      <input
                        type="number"
                        min={1500}
                        max={12000}
                        step={100}
                        value={(form.value.white ?? DEFAULT_WHITE).warm_kelvin}
                        onChange={(e) => set({ white: { ...(form.value.white ?? DEFAULT_WHITE), warm_kelvin: parseInt(e.target.value, 10) || 2700 } })}
                      />
                    </label>
                    <label className="field">
                      <span>Blanco frío (K)</span>
                      <input
                        type="number"
                        min={1500}
                        max={12000}
                        step={100}
                        value={(form.value.white ?? DEFAULT_WHITE).cool_kelvin}
                        onChange={(e) => set({ white: { ...(form.value.white ?? DEFAULT_WHITE), cool_kelvin: parseInt(e.target.value, 10) || 6500 } })}
                      />
                    </label>
                  </>
                )}
              </div>
            )}
            <div className="modal-foot">
              <button className="btn" onClick={() => setForm(null)}>Cancelar</button>
              <div className="spacer" />
              <button className="btn primary" onClick={save}>
                {form.index === null ? "Añadir" : "Guardar"}
              </button>
            </div>
          </div>
        ) : (
          <>
            <ul className="library-list">
              {groupHead("Genérico")}
              {!collapsed.has("Genérico") &&
                BUILTIN_FIXTURES.map((t) => (
                  <li key={`std:${t.name}`} className="library-item in-group">
                    <div className="library-item-main">
                      <span className="library-item-name">{t.name}</span>
                      <span className="library-item-meta">{fixtureMeta(t)}</span>
                    </div>
                  </li>
                ))}
              {groupHead("Mis tipos")}
              {!collapsed.has("Mis tipos") && lib.types.map((t, i) => (
                <li key={i} className="library-item in-group">
                  <div className="library-item-main">
                    <span className="library-item-name">{t.name}</span>
                    <span className="library-item-meta">{fixtureMeta(t)}</span>
                  </div>
                  <button className="btn small" onClick={() => setForm({ index: i, value: { ...t } })}>Editar</button>
                  <button className="btn small" onClick={() => run(removeFixtureType(i))}>Borrar</button>
                </li>
              ))}
              {!collapsed.has("Mis tipos") && lib.types.length === 0 && (
                <li className="library-empty in-group">Todavía no creaste tipos propios.</li>
              )}
            </ul>
            <div className="modal-foot">
              <button className="btn" onClick={() => setForm({ index: null, value: blankType() })}>+ Añadir tipo</button>
              <div className="spacer" />
              <button className="btn primary" onClick={onClose}>Listo</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

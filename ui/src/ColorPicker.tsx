import { useEffect, useRef, useState } from "react";

// "Selector de color" — modelled on the classic colour picker, opened from a colour swatch.
//
//   [ quick colours: white · 12 hues · black                                  ]
//   [grey]  [ hue field: white at the top → pure hue in the middle → black ] [now]
//   [strip] [ (hue runs left → right)                                      ] [was]
//   [ favourites 1…n · +                                                     ]
//
// Momentary, like professional consoles: press on a swatch, keep the button held and drag over the
// selector; release to keep the colour (see `ColorPicker`). All values
// are sRGB 0–255 (what the operator sees); the caller converts to the engine's linear
// light. Favourites are a per-operator convenience kept in this machine's browser storage.

export type Rgb = [number, number, number];

interface ColorPickerProps {
  /** The swatch the selector opens from; it always opens below it. */
  anchor: DOMRect;
  value: Rgb;
  onChange: (rgb: Rgb) => void;
  onClose: () => void;
}

const QUICK: Rgb[] = [
  [255, 255, 255],
  [255, 0, 0],
  [255, 127, 0],
  [255, 255, 0],
  [127, 255, 0],
  [0, 255, 0],
  [0, 255, 127],
  [0, 255, 255],
  [0, 127, 255],
  [0, 0, 255],
  [127, 0, 255],
  [255, 0, 255],
  [255, 0, 127],
  [0, 0, 0],
];

const DEFAULT_FAVORITES: Rgb[] = [
  [255, 255, 255],
  [0, 0, 0],
  [255, 0, 0],
  [255, 255, 0],
  [0, 255, 0],
  [0, 255, 255],
  [0, 0, 255],
  [255, 0, 255],
];
const MAX_FAVORITES = 13;
const FAVORITES_KEY = "prisma.colorFavorites";

function loadFavorites(): Rgb[] {
  try {
    const raw = localStorage.getItem(FAVORITES_KEY);
    if (raw) {
      const list = JSON.parse(raw);
      if (Array.isArray(list)) return list.filter((c) => Array.isArray(c) && c.length === 3);
    }
  } catch {
    /* storage unavailable: defaults */
  }
  return DEFAULT_FAVORITES;
}

function saveFavorites(list: Rgb[]) {
  try {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(list));
  } catch {
    /* storage unavailable: favourites last for this session only */
  }
}

const css = ([r, g, b]: Rgb) => `rgb(${r},${g},${b})`;
const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
const same = (a: Rgb, b: Rgb) => a[0] === b[0] && a[1] === b[1] && a[2] === b[2];
/** Black or white text, whichever reads on `c`. */
const ink = ([r, g, b]: Rgb) => (r * 0.299 + g * 0.587 + b * 0.114 > 150 ? "#000" : "#fff");

/** Fully saturated colour at hue `h` (0..1). */
function hueRgb(h: number): Rgb {
  const x = (h % 1) * 6;
  const f = x - Math.floor(x);
  const seg = Math.floor(x) % 6;
  const up = Math.round(255 * f);
  const down = 255 - up;
  return (
    [
      [255, up, 0],
      [down, 255, 0],
      [0, 255, up],
      [0, down, 255],
      [up, 0, 255],
      [255, 0, down],
    ] as Rgb[]
  )[seg];
}

/** The field's vertical layout (professional consoles): white → hue over the top band, the PURE hue
 *  between the two lines, hue → black over the bottom band. */
const PURE_TOP = 0.4;
const PURE_BOTTOM = 0.6;

/** The field's colour at (x, y), both 0..1. */
function fieldColor(x: number, y: number): Rgb {
  const h = hueRgb(clamp01(x));
  if (y < PURE_TOP) {
    const t = y / PURE_TOP; // 0 = white, 1 = pure hue
    return h.map((c) => Math.round(255 + (c - 255) * t)) as Rgb;
  }
  if (y <= PURE_BOTTOM) return h;
  const t = (y - PURE_BOTTOM) / (1 - PURE_BOTTOM); // 0 = pure hue, 1 = black
  return h.map((c) => Math.round(c * (1 - t))) as Rgb;
}

/** Where `rgb` sits on the field, or null for a grey (no hue to place it on). */
function fieldPos([r, g, b]: Rgb): { x: number; y: number } | null {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  if (max === min) return null;
  const d = max - min;
  let h: number;
  if (max === r) h = ((g - b) / d + 6) % 6;
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  const x = h / 6;
  // Some white in it → top band; dimmed → bottom band; otherwise the pure band's middle.
  let y: number;
  if (min > 0 && max >= 250) y = PURE_TOP * (1 - min / 255);
  else if (max < 250) y = PURE_BOTTOM + (1 - PURE_BOTTOM) * (1 - max / 255);
  else y = (PURE_TOP + PURE_BOTTOM) / 2;
  return { x, y };
}

const PICKER_W = 640;
const PICKER_H = 330;

/** The colour (or favourite action) under a screen point, if it is inside the selector. */
type Hit =
  | { kind: "color"; rgb: Rgb; marker: { x: number; y: number } | null }
  | { kind: "add" }
  | null;

function hitAt(cx: number, cy: number): Hit {
  const el = document.elementFromPoint(cx, cy)?.closest<HTMLElement>("[data-cpick]");
  if (!el) return null;
  const r = el.getBoundingClientRect();
  const x = clamp01((cx - r.left) / r.width);
  const y = clamp01((cy - r.top) / r.height);
  switch (el.dataset.cpick) {
    case "field":
      return { kind: "color", rgb: fieldColor(x, y), marker: { x, y } };
    case "grey": {
      const v = Math.round(255 * (1 - y));
      return { kind: "color", rgb: [v, v, v], marker: null };
    }
    case "rgb": {
      const rgb = (el.dataset.rgb ?? "0,0,0").split(",").map(Number) as Rgb;
      return { kind: "color", rgb, marker: fieldPos(rgb) };
    }
    case "add":
      return { kind: "add" };
    default:
      return null;
  }
}

/** The selector is MOMENTARY, like professional consoles: it opens on the press on a swatch, follows the
 *  pointer while the button is held (the colour changes live as you pass over it), and on
 *  release keeps the colour under the pointer and closes. Releasing over "+" stores the
 *  current colour as a favourite. Escape closes it. It always opens BELOW the swatch. */
export function ColorPicker({ anchor, value, onChange, onClose }: ColorPickerProps) {
  const left = Math.max(4, Math.min(anchor.left, window.innerWidth - PICKER_W - 4));
  // Opens BELOW the swatch; only if that would run off the window does it rise just
  // enough to stay fully visible (e.g. the Fader Box preview near the screen's bottom).
  const top = Math.max(4, Math.min(anchor.bottom + 2, window.innerHeight - PICKER_H - 4));
  const [previous] = useState<Rgb>(value); // the colour when the selector opened
  const [favorites] = useState<Rgb[]>(loadFavorites);
  const [marker, setMarker] = useState(() => fieldPos(value));
  const [hoverAdd, setHoverAdd] = useState(false);

  // Latest values for the window listeners (registered once for the whole hold).
  const live = useRef({ value, favorites, onChange, onClose });
  live.current = { value, favorites, onChange, onClose };

  useEffect(() => {
    const apply = (hit: Hit) => {
      if (hit?.kind !== "color") return;
      setMarker(hit.marker);
      const cur = live.current.value;
      if (!same(hit.rgb, cur)) live.current.onChange(hit.rgb);
    };
    const move = (e: PointerEvent) => {
      e.preventDefault();
      const hit = hitAt(e.clientX, e.clientY);
      setHoverAdd(hit?.kind === "add");
      apply(hit);
    };
    const up = (e: PointerEvent) => {
      const hit = hitAt(e.clientX, e.clientY);
      apply(hit);
      if (hit?.kind === "add") {
        const { favorites: favs, value: cur } = live.current;
        if (favs.length < MAX_FAVORITES && !favs.some((f) => same(f, cur))) saveFavorites([...favs, cur]);
      }
      live.current.onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") live.current.onClose();
    };
    const cancel = () => live.current.onClose();
    document.body.classList.add("is-picking");
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key);
    return () => {
      document.body.classList.remove("is-picking");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key);
    };
  }, []);

  // Follow the value when it changes from elsewhere.
  useEffect(() => {
    if (marker && same(fieldColor(marker.x, marker.y), value)) return;
    setMarker(fieldPos(value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value[0], value[1], value[2]]);

  return (
    <div className="pr-cpick" role="dialog" aria-label="Selector de color" style={{ left, top }}>
      <div className="pr-cpick-quick">
        {QUICK.map((c, i) => (
          <div key={i} data-cpick="rgb" data-rgb={c.join(",")} style={{ background: css(c) }} />
        ))}
      </div>

      <div className="pr-cpick-mid">
        <div className="pr-cpick-grey" data-cpick="grey" />
        <div className="pr-cpick-field" data-cpick="field">
          <div className="pr-cpick-line" style={{ top: `${PURE_TOP * 100}%` }} />
          <div className="pr-cpick-line" style={{ top: `${PURE_BOTTOM * 100}%` }} />
          {marker && (
            <div className="pr-cpick-marker" style={{ left: `${marker.x * 100}%`, top: `${marker.y * 100}%`, background: css(value) }} />
          )}
        </div>
        <div className="pr-cpick-compare">
          <div style={{ background: css(value) }} />
          <div data-cpick="rgb" data-rgb={previous.join(",")} style={{ background: css(previous) }} />
        </div>
      </div>

      <div className="pr-cpick-favs">
        {favorites.map((c, i) => (
          <div key={i} data-cpick="rgb" data-rgb={c.join(",")} style={{ background: css(c), color: ink(c) }}>
            {i + 1}
          </div>
        ))}
        {favorites.length < MAX_FAVORITES && (
          <div data-cpick="add" className={`pr-cpick-add${hoverAdd ? " is-hover" : ""}`} style={{ background: css(value), color: ink(value) }}>
            +
          </div>
        )}
      </div>
    </div>
  );
}

/** A colour swatch that opens the "Selector de color" on press — use it wherever the
 *  mixed colour is shown (effect colour, Fader Box preview, …) so every one of them can
 *  pick. Press, hold and drag over the selector; release to keep the colour. */
export function ColorSwatch({
  value,
  onChange,
  className,
  title = "Selector de color — mantené apretado y arrastrá",
}: {
  value: Rgb;
  onChange: (rgb: Rgb) => void;
  className?: string;
  title?: string;
}) {
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  return (
    <>
      <div
        className={`pr-swatch-pick${className ? ` ${className}` : ""}`}
        style={{ background: css(value) }}
        title={title}
        role="button"
        aria-label="Selector de color"
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          e.preventDefault();
          setAnchor(e.currentTarget.getBoundingClientRect());
        }}
      />
      {anchor && <ColorPicker anchor={anchor} value={value} onChange={onChange} onClose={() => setAnchor(null)} />}
    </>
  );
}

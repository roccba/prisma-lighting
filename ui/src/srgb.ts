// sRGB ↔ linear-light conversion for the colour picker (M8 parameter types).
//
// The engine keeps a colour's channels in LINEAR light `0..1` (§2.3, "render linear,
// gamma at the end") — that is the value in the Parameter Tree and the uniform, and it
// must not change. The operator, though, thinks in ordinary 0–255 sRGB colour values
// (the same space a native colour picker and any design tool use, and the same curve
// the M6 preview encodes with). So the panel presents sRGB and converts on the way in
// and out; the model stays linear. This file is that conversion, in one place.

/** Linear-light `0..1` → sRGB `0..255` (rounded), the standard IEC 61966-2-1 curve. */
export function linearToSrgb255(linear: number): number {
  const c = Math.min(1, Math.max(0, linear));
  const s = c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
  return Math.round(s * 255);
}

/** sRGB `0..255` → linear-light `0..1`. Inverse of {@link linearToSrgb255}. */
export function srgb255ToLinear(v255: number): number {
  const s = Math.min(1, Math.max(0, v255 / 255));
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/** Two hex digits for a `0..255` byte. */
function hex2(v: number): string {
  return Math.min(255, Math.max(0, Math.round(v))).toString(16).padStart(2, "0");
}

/** Linear-light `{r,g,b}` (each `0..1`) → an sRGB `#rrggbb` for a swatch / picker. */
export function linearToHex(r: number, g: number, b: number): string {
  return `#${hex2(linearToSrgb255(r))}${hex2(linearToSrgb255(g))}${hex2(linearToSrgb255(b))}`;
}

/** An sRGB `#rrggbb` → linear-light `{r,g,b}` (each `0..1`) for the model. */
export function hexToLinear(hex: string): { r: number; g: number; b: number } {
  const n = hex.replace("#", "");
  const byte = (i: number) => parseInt(n.slice(i, i + 2), 16) || 0;
  return {
    r: srgb255ToLinear(byte(0)),
    g: srgb255ToLinear(byte(2)),
    b: srgb255ToLinear(byte(4)),
  };
}

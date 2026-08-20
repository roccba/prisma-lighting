// Small inline SVG icons (themeable via `currentColor`), replacing emoji glyphs that
// render inconsistently and read as clip-art. Kept tiny and stroke/fill on currentColor
// so a button's `color` token drives them.

/** A lightning bolt — the strobe glyph. */
export function Bolt({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d="M13 2 L4 14 h6 l-1 8 9-12 h-6 z" fill="currentColor" />
    </svg>
  );
}

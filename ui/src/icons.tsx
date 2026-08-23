// Small inline SVG icons (themeable via `currentColor`), replacing emoji glyphs that
// render inconsistently and read as clip-art. Kept tiny and stroke/fill on currentColor
// so a button's `color` token drives them. The toolbar icons are the MADRIX-style
// icon-over-label set (§M7 restyle): line icons on a 24×24 grid, 1.8 stroke, round caps.

type IconProps = { size?: number };

// Shared <svg> wrapper: line icons (fill none, stroke currentColor). Filled glyphs pass
// their own fills and override stroke locally.
function Svg({ size = 18, children }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

/** A lightning bolt — the strobe glyph / "blink" highlight. */
export function Bolt({ size = 13 }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d="M13 2 L4 14 h6 l-1 8 9-12 h-6 z" fill="currentColor" />
    </svg>
  );
}

/** Open a file — folder with a lifted flap. */
export function FolderOpen(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4l2 2.5h7A1.5 1.5 0 0 1 19 9" />
      <path d="M3 9h16.5a1 1 0 0 1 .97 1.24l-1.5 6A1.5 1.5 0 0 1 17.5 17.5H5A1.5 1.5 0 0 1 3.5 16z" />
    </Svg>
  );
}

/** Save — a floppy disk. */
export function Save(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M5 3h11l3 3v13a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" />
      <path d="M8 3v5h7V3" />
      <rect x="7.5" y="12.5" width="9" height="6" rx="0.5" />
    </Svg>
  );
}

/** Add fixtures — a pixel grid with a plus. */
export function AddFixtures(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="3.5" y="3.5" width="6" height="6" rx="1" />
      <rect x="3.5" y="14.5" width="6" height="6" rx="1" />
      <rect x="14.5" y="14.5" width="6" height="6" rx="1" />
      <path d="M17.5 4v7M14 7.5h7" />
    </Svg>
  );
}

/** Library — stacked layers. */
export function Library(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M12 3 3 7.5l9 4.5 9-4.5z" />
      <path d="M3 12l9 4.5L21 12" />
      <path d="M3 16.5 12 21l9-4.5" />
    </Svg>
  );
}

/** Delete — a trash can. */
export function Trash(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 6.5h16" />
      <path d="M9 6.5V4.5A1 1 0 0 1 10 3.5h4a1 1 0 0 1 1 1v2" />
      <path d="M6 6.5 6.8 20a1.5 1.5 0 0 0 1.5 1.4h7.4a1.5 1.5 0 0 0 1.5-1.4L18 6.5" />
      <path d="M10 10.5v7M14 10.5v7" />
    </Svg>
  );
}

/** Undo — a curved arrow to the left. */
export function Undo(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M9 7 4 12l5 5" />
      <path d="M4 12h10a6 6 0 0 1 6 6v1" />
    </Svg>
  );
}

/** Redo — a curved arrow to the right. */
export function Redo(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M15 7l5 5-5 5" />
      <path d="M20 12H10a6 6 0 0 0-6 6v1" />
    </Svg>
  );
}

/** All zones visible — an eye. */
export function Eye(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" />
      <circle cx="12" cy="12" r="3" />
    </Svg>
  );
}

/** Highlight active zone — a target / focus reticle. */
export function Target(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="12" r="3.5" />
      <path d="M12 1.5v3M12 19.5v3M1.5 12h3M19.5 12h3" />
    </Svg>
  );
}

/** Isolate a zone — a crop / single frame. */
export function Isolate(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M6 2.5v15.5a1 1 0 0 0 1 1h15.5" />
      <path d="M2.5 6H18a1 1 0 0 1 1 1v15.5" />
    </Svg>
  );
}

/** Highlight off — an eye with a slash. */
export function EyeOff(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 4l16 16" />
      <path d="M9.5 5.6A9.7 9.7 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a17 17 0 0 1-2.7 3.4" />
      <path d="M6.3 7.9A16.6 16.6 0 0 0 2.5 12S6 18.5 12 18.5a9.4 9.4 0 0 0 3.2-.55" />
      <path d="M9.9 9.9a3 3 0 0 0 4.2 4.2" />
    </Svg>
  );
}

/** Solid highlight — a lit bulb. */
export function Bulb(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M9 17.5h6" />
      <path d="M10 20.5h4" />
      <path d="M12 2.5a6.5 6.5 0 0 0-4 11.6c.7.55 1 1 1 2.4h6c0-1.4.3-1.85 1-2.4A6.5 6.5 0 0 0 12 2.5z" />
    </Svg>
  );
}

/** Plus — add. */
export function Plus(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M12 5v14M5 12h14" />
    </Svg>
  );
}

/** Minus — remove. */
export function Minus(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M5 12h14" />
    </Svg>
  );
}

/** Pencil — rename / edit. */
export function Pencil(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M14.5 4.5l5 5" />
      <path d="M4 20l1-4L16 5l3 3L8 19z" />
    </Svg>
  );
}

/** Grid — matrix / render resolution. */
export function Grid(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="3.5" y="3.5" width="17" height="17" rx="1.5" />
      <path d="M9 3.5v17M15 3.5v17M3.5 9h17M3.5 15h17" />
    </Svg>
  );
}

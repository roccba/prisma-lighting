# Prisma — Design System

The visual language for Prisma's UI. Modelled on professional consoles's density and
control style, with the lime-green accent replaced by **VS Code blue**. The
structural greys are kept close to professional consoles because they read well for dense,
professional lighting-control work.

`theme.css` is the authoritative implementation — every value below lives
there as a CSS custom property or a `.pr-*` class. **Reference tokens; never
hardcode a colour in a component.** To retheme (light build, per-client
accent) you override `:root` only.

Reference screenshots for the source look are in `Screenshots/` (professional consoles). The
throwaway HTML explorations that produced this system are gitignored.

---

## 1. Principles

- **Dense, not sparse.** This is a professional tool. Controls are compact and
  information-rich, closer to a DAW or professional consoles than a consumer app. Padding is
  tight; font sizes are small (11–12px base).
- **The accent is a scalpel, not a wash.** Blue marks what is *active*,
  *selected*, or a *live value* — nothing else. Most of the UI is grey. When
  everything is blue, nothing is.
- **Greys carry structure.** Depth comes from the grey ramp and 1px borders
  (a light top edge, a dark bottom edge = raised; inverted = recessed), not
  from shadows or colour.
- **One knob to rule them all.** Every slider — vertical fader or horizontal —
  uses the same metallic console knob. Consistency of the signature control
  matters more than saving pixels.

---

## 2. Colour

### Accent — VS Code blue
| Token | Hex | Use |
|---|---|---|
| `--accent` | `#007ACC` | active state, selection, fills, primary buttons |
| `--accent-bright` | `#1F9CFF` | hover, active borders, grabbed knob outline |
| `--accent-dim` | `#005A99` | pressed, secondary hover backgrounds |
| `--accent-text` | `#4FC1FF` | numeric values & data on dark (reads brighter) |

Rule of thumb: **structural blue** (states, borders, fills) uses `--accent`;
**text/number blue** uses `--accent-text`, which is lighter so digits stay
legible on black input fields.

### Structural greys
| Token | Hex | Use |
|---|---|---|
| `--bg-app` | `#0a0a0a` | app background |
| `--bg-panel` | `#1e1e1e` | panel surface |
| `--bg-panel-2` | `#2a2a2a` | raised surface (dropdown lists, popovers) |
| `--bg-canvas` | `#000000` | canvas / preview |
| `--bg-input` | `#000000` | inputs & value fields |
| `--menu-bg` | `#252525` | menu bar & submenus |
| `--row-a` / `--row-b` | `#202020` / `#262626` | zebra table/list rows |
| `--header-bg` | `#333` | table headers |

### Borders
- `--btn-border` `#555` — standard control edge
- `--btn-border-dark` `#111` — recessed / bottom edge

### Text
- `--text` `#d4d4d4` — primary
- `--text-dim` `#888` — secondary / meta
- `--text-label` `#aaa` — field labels

---

## 3. Type

- UI: `--font-ui` — Segoe UI / Roboto Condensed / Tahoma. Condensed-ish
  sans; dense.
- Numbers & data: `--font-mono` — Consolas / DejaVu Sans Mono. **All numeric
  values, DMX addresses, channels and coordinates are monospace** so columns
  align and digits don't jump.
- Sizes: `--fs-base` 12 · `--fs-small` 11 · `--fs-tiny` 10 · `--fs-value` 12.

---

## 4. Components

Each maps to a `.pr-*` class in `theme.css`.

- **Panel** (`.pr-panel`, `.pr-panel-title`) — bordered surface with an
  optional gradient title bar.
- **Buttons** (`.pr-btn`) — grey raised face; `.is-active` fills blue,
  `.is-primary` is a blue call-to-action. `.pr-tabbtn` = segmented tabs;
  `.pr-tbtn` = icon-over-label toolbar button.
- **Numeric input** (`.pr-num`) — black field, blue mono value. Two spinner
  variants: `.pr-spin` (stacked ▲▼) and `.pr-pm` (flanking − / +). Both come
  straight from professional consoles.
- **Dropdown** (`.pr-dd`) — custom, never the native `<select>`. Black head
  with blue value + grey caret box; list drops with a blue top border, hover
  and selected options fill blue.
- **Checkbox** (`.pr-chk`) / **Toggle** (`.pr-tgl`) — blue when on.
- **Window tabs** (`.pr-wtab`) — pestañas for multi-tab windows (device
  manager: DMX / Art-Net / sACN / MIDI / Audio).
- **Menu bar** (`.pr-menubar`) — top application menu with submenus and
  keyboard-shortcut hints.
- **Toolbar** (`.pr-toolbar`) — container for `.pr-tbtn`s with `.sep`
  dividers.
- **Sliders** — the signature control:
  - `.pr-fader` (vertical) and `.pr-hslider` (horizontal) share `.pr-knob`.
  - The knob is a **console-style metallic slider cap**: taller/wider than the
    slot, a metallic vertical (or horizontal) gradient, and **grip ridges**
    like a real mixing-desk fader.
  - **Grabbed state** (`.is-grabbed`) draws an accent outline around the knob.
    professional consoles turns its knob lime while dragging; we use the accent blue.
  - Both have scale ticks along the rail (major every 5).
  - Value readout uses `.pr-value` (black field, blue mono).
- **Transport** (`.pr-transport`) + **meter** (`.pr-meter`) — for live
  operation (M8).
- **Colour field** (`.pr-colorfield`) — swatch + R/G/B mono channels, each
  channel underlined in its own colour.
- **Data table** (`.pr-table`) — zebra rows, grey header, blue mono numeric
  cells, blue selected row.
- **List rows** (`.pr-listrow`, `.pr-listhead`) — zebra rows with a `.meta`
  secondary column; blue when selected. Used by the fixture list.

---

## 5. Logo

**Wordmark:** "PRISMA" where the **P is built from pixels** (a nod to pixel
mapping) and "RISMA" is set in the bold UI type — read as one word. Below,
"LIGHTING CONTROL" spaced out.

Direction only, not finalised: the pixel-P should sit at the **cap-height of
the text** and match its **stroke weight** (single-pixel stroke, but pixels
large enough to equal the letterforms' thickness). Needs a designer pass to
get the P unambiguous and the weight matched — treat as open, not locked.

Accent-bright pixels scattered in the P give a "signal scanning" feel and
could animate on a splash screen later. Not required.

---

## 6. What is NOT decided yet

Deferred to when the relevant M8 surface is built, with its own professional consoles
reference:
- Live-operation specifics: the zone row with live thumbnails, the scene grid,
  the global operation bar (master/strobe/chaser).
- Exact menu contents.
- Device-manager layout beyond the tab style.
- Final logo geometry.

Everything a component needs to look right is in `theme.css`; layout of whole
screens is defined per-surface during M8.

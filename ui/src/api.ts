// Typed wrappers over the Tauri IPC command surface (Rust side: app/src/main.rs).

import { invoke } from "@tauri-apps/api/core";

export type Vec3 = [number, number, number];
export type Rgb = [number, number, number];

export interface Pixel {
  index: number;
  position: Vec3;
  universe: number;
  start_channel: number;
  color_order: string;
}

export interface Fixture {
  id: string;
  name: string;
  pixels: Pixel[];
  zone: string; // owning zone id (§2.2)
}

export interface Zone {
  id: string;
  name: string;
  display_color: Rgb;
  /** The zone's own local matrix = its effect render resolution (§2.4). [x, y, z]. */
  mapping_size: Vec3;
}

export interface Patch {
  id: string;
  name: string;
  matrix_size: Vec3;
  fixtures: Fixture[];
  zones: Zone[];
}

/** One pixel a diagnostic points at (owning fixture + pixel index within it). */
export interface Mark {
  fixture: string;
  pixel: number;
}

/**
 * A physical DMX wire fault the editor surfaces (discreetly), annotated with the pixels
 * it points at. Discriminated on `kind` — mirrors Rust `prisma_patch::Diagnostic`.
 * After the S6 replan there are exactly two: an address collision and an out-of-range
 * channel. Out-of-matrix positions and shared cells are valid choices and are NOT faults
 * (§2.4/§4.4).
 */
export type Diagnostic =
  | {
      kind: "AddressCollision";
      universe: number;
      channel_start: number;
      channel_end: number;
      fixtures: string[];
      marks: Mark[];
    }
  | { kind: "ChannelOutOfRange"; mark: Mark; start: number; span: number };

/** The pixels a diagnostic points at (for canvas marks). */
export function diagnosticMarks(d: Diagnostic): Mark[] {
  return d.kind === "AddressCollision" ? d.marks : [d.mark];
}

/** The fixtures a diagnostic involves (for click-to-select). */
export function diagnosticFixtures(d: Diagnostic): string[] {
  return d.kind === "AddressCollision" ? d.fixtures : [d.mark.fixture];
}

/** The editor snapshot after any command (patch + toolbar state + all problems). */
export interface EditorState {
  patch: Patch;
  can_undo: boolean;
  can_redo: boolean;
  dirty: boolean;
  diagnostics: Diagnostic[];
}

export interface SelectionAggregate {
  fixture_count: number;
  pixel_count: number;
  position_min: Vec3;
  position_max: Vec3;
  universe_min: number;
  universe_max: number;
  channel_min: number;
  channel_max: number;
}

export type ComponentSet = "Rgb" | "Rgbw";
export type ColorOrder = "Rgb" | "Rbg" | "Grb" | "Gbr" | "Brg" | "Bgr";

/** Physical pixel layout of a fixture type (§3.2). Only `Strip` in M7. */
export type FixtureArrangement = "Strip";

/** A named subset of a fixture type's pixel indices (§3.2, hexagon). Unused in M7. */
export interface PixelRole {
  name: string;
  pixels: number[];
}

/** Signal direction — the way a fixture's pixels are wired (§4.1), all eight. */
export type Direction =
  | "Right"
  | "Left"
  | "Down"
  | "Up"
  | "DownRight"
  | "DownLeft"
  | "UpRight"
  | "UpLeft";

export type Rotation = "R0" | "R90" | "R180" | "R270";

export interface FixtureType {
  name: string;
  pixel_count: number;
  components: ComponentSet;
  color_order: ColorOrder;
  arrangement: FixtureArrangement;
  roles: PixelRole[];
}

/** The operator's reusable fixture library (§3.2), stored in its own file. */
export interface FixtureLibrary {
  id: string;
  name: string;
  types: FixtureType[];
}

export interface PlacementSpec {
  fixture_type: FixtureType;
  name_prefix: string;
  start_number: number;
  rotation: Rotation;
  count: [number, number, number];
  offset: [number, number, number];
  start_position: [number, number, number];
  direction: Direction;
  snake: boolean;
  start_universe: number;
  start_channel: number;
  channel_offset: number;
  universe_offset: number;
  fixtures_per_universe: number | null;
  zone: string;
}

/** One instance's run for the placement preview (§4.1). */
export interface PreviewRun {
  start: [number, number];
  end: [number, number];
  order: number;
}

export const getState = () => invoke<EditorState>("editor_get_state");

export const selectionAggregate = (ids: string[]) =>
  invoke<SelectionAggregate | null>("editor_selection_aggregate", { ids });

export const fixtureTypes = () => invoke<FixtureType[]>("editor_fixture_types");

// --- Fixture library CRUD (§3.2). Each mutation persists and returns the library. ---
export const getLibrary = () => invoke<FixtureLibrary>("editor_library_get");
export const addFixtureType = (ty: FixtureType) =>
  invoke<FixtureLibrary>("editor_library_add_type", { ty });
export const updateFixtureType = (index: number, ty: FixtureType) =>
  invoke<FixtureLibrary>("editor_library_update_type", { index, ty });
export const removeFixtureType = (index: number) =>
  invoke<FixtureLibrary>("editor_library_remove_type", { index });

export const nextFreeAddress = () => invoke<[number, number]>("editor_next_free_address");

export const previewPlacement = (spec: PlacementSpec) =>
  invoke<PreviewRun[]>("editor_preview_placement", { spec });

export const placeFixtures = (spec: PlacementSpec) =>
  invoke<EditorState>("editor_place_fixtures", { spec });

export const translateFixtures = (ids: string[], delta: [number, number, number]) =>
  invoke<EditorState>("editor_translate_fixtures", { ids, delta });

/** Set a zone's matrix (render resolution, §2.4/§2.5). X/Y clamped to 1..=512 in Rust. */
export const setZoneMappingSize = (zone: string, size: [number, number]) =>
  invoke<EditorState>("editor_set_zone_mapping_size", { zone, size });

export const removeFixtures = (ids: string[]) =>
  invoke<EditorState>("editor_remove_fixtures", { ids });

export const undo = () => invoke<EditorState>("editor_undo");
export const redo = () => invoke<EditorState>("editor_redo");

// --- Live engine: Highlight + apply (§7 / S9) ---
/** Light `ids` on the live rig to identify them; `blink` vs solid. Empty = clear. */
export const highlight = (ids: string[], blink: boolean) =>
  invoke<void>("editor_highlight", { ids, blink });

/** Recompile the current draft into the live engine (the show resumes on the new layout). */
export const applyToLive = () => invoke<void>("editor_apply_to_live");

/** The port the live-preview WebSocket is bound to (§M6/S10). Re-query on reconnect. */
export const previewPort = () => invoke<number>("editor_preview_port");

/** Serialize the mapping to `path` (a `.prismap`) and clear the dirty flag (§9). */
export const saveTo = (path: string) => invoke<EditorState>("editor_save_to", { path });

/** Open a `.prismap` from `path` into the editor, migrating an older schema (§9). */
export const openFrom = (path: string) => invoke<EditorState>("editor_open_from", { path });

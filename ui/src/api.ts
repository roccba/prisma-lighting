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

/** Add a new (empty) zone to the patch (§7). Returns the new editor state. */
export const addZone = (name: string) => invoke<EditorState>("editor_add_zone", { name });

/** Remove a zone (§7). Rejected (throws) if fixtures still belong to it. */
export const removeZone = (zone: string) => invoke<EditorState>("editor_remove_zone", { zone });

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

// --- Main operation window (M8 step 3): the zone row ---

/** A zone card's data: identity + its current live controls (read from the engine). */
export interface ZoneInfo {
  id: string;
  name: string;
  color: Rgb;
  dimmer: number; // 0..1
  solo: boolean;
  blackout: boolean;
}

/** Every zone of the running show, in slot order, with live controls. */
export const mainZones = () => invoke<ZoneInfo[]>("main_zones");

/** Set a zone's dimmer (the zone-row fader) — changes the output, not the selection. */
export const setZoneDimmer = (zone: string, value: number) =>
  invoke<void>("main_set_zone_dimmer", { zone, value });

/** Toggle a zone's solo (§3.1). */
export const setZoneSolo = (zone: string, on: boolean) =>
  invoke<void>("main_set_zone_solo", { zone, on });

/** Toggle a zone's blackout/mute (§3.1). */
export const setZoneBlackout = (zone: string, on: boolean) =>
  invoke<void>("main_set_zone_blackout", { zone, on });

/** Set a zone's strobe: `rate` in Hz (0 = off), `duty` 0..1 (§3.6). */
export const setZoneStrobe = (zone: string, rate: number, duty: number) =>
  invoke<void>("main_set_zone_strobe", { zone, rate, duty });

// --- Global operation bar (M8 step 6) ---

/** The master section's live state, for the global bar. */
export interface MasterInfo {
  dimmer: number; // 0..1
  blackout: boolean;
  strobe_rate: number; // Hz (0 = off)
  strobe_duty: number; // 0..1
}

/** Read the master section (dimmer, blackout, global strobe). */
export const masterState = () => invoke<MasterInfo>("main_master");

/** Set the master dimmer (real 0..1). */
export const setMasterDimmer = (value: number) =>
  invoke<void>("main_set_master_dimmer", { value });

/** Toggle the master blackout (kills all output while holding the fader). */
export const setMasterBlackout = (on: boolean) =>
  invoke<void>("main_set_master_blackout", { on });

/** Set the global strobe: `rate` Hz (0 = off), `duty` 0..1. */
export const setMasterStrobe = (rate: number, duty: number) =>
  invoke<void>("main_set_master_strobe", { rate, duty });

/** Open the patch editor as a secondary window (§6/§7). Freezes the show while open;
 *  applies the edits to the running show on close. */
export const openEditor = () => invoke<void>("main_open_editor");

/** Save the live show (with its embedded patch) to a `.prisma` file (§5). */
export const saveShow = (path: string) => invoke<void>("main_save_show", { path });
/** Open a `.prisma` show and run it (recompiles the engine onto its embedded patch). */
export const openShow = (path: string) => invoke<void>("main_open_show", { path });

// --- Main operation window (M8 step 4): the scene grid ---

/** One scene-grid cell of a zone: empty (assignable) or an assigned effect, and
 *  whether it is the zone's running (active) cell. */
export interface CellInfo {
  index: number;
  effect_id: string | null;
  effect_name: string | null;
  active: boolean;
}

/** One entry the effect picker offers (screenshot 14). */
export interface EffectListing {
  id: string;
  name: string;
  category: string;
}

/** The effect catalogue for the picker, grouped by `category` in the UI. */
export const effectCatalogue = () => invoke<EffectListing[]>("main_effect_catalogue");

/** The scene grid of a zone (its cells + which is running). */
export const zoneGrid = (zone: string) => invoke<CellInfo[]>("main_zone_grid", { zone });

/** Assign an effect to a cell AND fire it (picking plays it). Returns the new grid. */
export const assignCell = (zone: string, index: number, effectId: string) =>
  invoke<CellInfo[]>("main_assign_cell", { zone, index, effectId });

/** Trigger an already-assigned cell (make it the zone's active content). */
export const triggerCell = (zone: string, index: number) =>
  invoke<CellInfo[]>("main_trigger_cell", { zone, index });

/** Empty a cell (and stop the zone if it was the running cell). */
export const clearCell = (zone: string, index: number) =>
  invoke<CellInfo[]>("main_clear_cell", { zone, index });

// --- Main operation window (M8 step 5): the effect parameter panel ---

/** One manifest-driven parameter row (§3.5): its key, display label, real range and
 *  current normalized value. The panel builds itself from these — never hardcoded. */
export interface ParamInfo {
  name: string;
  label: string;
  min: number;
  max: number;
  value_norm: number; // 0..1
  /** Presentation type the panel switches on (§3.5): a plain slider, a channel of a
   *  colour picker, or the direction rose. Value model is scalar regardless. */
  role: "scalar" | "color" | "direction";
  /** Colour group id — set only for `role === "color"`; channels sharing it are one
   *  picker (two colours in an effect use distinct groups). */
  group: string | null;
  /** Colour channel — set only for `role === "color"`. */
  channel: "r" | "g" | "b" | null;
}

/** The effect work panel for a zone: its active cell's effect + its parameters.
 *  `effect_id` is null when the zone has no running cell (empty state). */
export interface EffectPanelInfo {
  zone_id: string;
  cell_index: number | null;
  effect_id: string | null;
  effect_name: string | null;
  params: ParamInfo[];
}

/** The active effect and its manifest-driven parameters for a zone (§3.5). */
export const effectPanel = (zone: string) =>
  invoke<EffectPanelInfo>("main_effect_panel", { zone });

/** Set one parameter of a zone's active effect (normalized 0..1). Drives it live. */
export const setParam = (zone: string, name: string, value: number) =>
  invoke<void>("main_set_param", { zone, name, value });

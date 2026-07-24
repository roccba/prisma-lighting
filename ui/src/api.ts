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
}

export interface Patch {
  id: string;
  name: string;
  matrix_size: Vec3;
  fixtures: Fixture[];
  zones: Zone[];
}

/** The editor snapshot after any command (patch + toolbar/validation state). */
export interface EditorState {
  patch: Patch;
  can_undo: boolean;
  can_redo: boolean;
  dirty: boolean;
  validation: string | null;
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
  color_order: string;
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

export const nextFreeAddress = () => invoke<[number, number]>("editor_next_free_address");

export const previewPlacement = (spec: PlacementSpec) =>
  invoke<PreviewRun[]>("editor_preview_placement", { spec });

export const placeFixtures = (spec: PlacementSpec) =>
  invoke<EditorState>("editor_place_fixtures", { spec });

export const translateFixtures = (ids: string[], delta: [number, number, number]) =>
  invoke<EditorState>("editor_translate_fixtures", { ids, delta });

export const removeFixtures = (ids: string[]) =>
  invoke<EditorState>("editor_remove_fixtures", { ids });

export const undo = () => invoke<EditorState>("editor_undo");
export const redo = () => invoke<EditorState>("editor_redo");

/** Serialize the mapping to `path` (a `.prismap`) and clear the dirty flag (§9). */
export const saveTo = (path: string) => invoke<EditorState>("editor_save_to", { path });

/** Open a `.prismap` from `path` into the editor, migrating an older schema (§9). */
export const openFrom = (path: string) => invoke<EditorState>("editor_open_from", { path });

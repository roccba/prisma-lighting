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

/** An ordered set of fixtures (LIVE-MODEL-SPEC §3.1). Can span zones. */
export interface Group {
  id: string;
  name: string;
  /** Fixture ids, in element order. */
  members: string[];
  display_color: Rgb;
}

export interface Patch {
  id: string;
  name: string;
  matrix_size: Vec3;
  fixtures: Fixture[];
  zones: Zone[];
  /** Absent in a patch without groups. */
  groups?: Group[];
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

export type ComponentSet = "Rgb" | "Rgbw" | "Rgbww";

/** How a profile drives its white channel(s) from RGB at the output stage. */
export type WhiteMode = "off" | "extract" | "add";

/** A profile's whites: mode and temperatures (kelvin). RGBW uses `kelvin`; RGBWW
 *  splits between `warm_kelvin` and `cool_kelvin`. */
export interface WhiteProfile {
  mode: WhiteMode;
  kelvin: number;
  warm_kelvin: number;
  cool_kelvin: number;
}

export const DEFAULT_WHITE: WhiteProfile = { mode: "extract", kelvin: 4000, warm_kelvin: 2700, cool_kelvin: 6500 };
export type ColorOrder = "Rgb" | "Rbg" | "Grb" | "Gbr" | "Brg" | "Bgr";

/** Physical pixel layout of a fixture type (§3.2). Only `Strip` in M7. */
export type FixtureArrangement = "Strip";

/** A named subset of a fixture type's pixel indices (§3.2, hexagon). Unused in M7. */
export interface PixelRole {
  name: string;
  pixels: number[];
}

/** Direction of the pixels inside a fixture: clockwise from "Original" (a strip runs
 *  left → right). The 45° steps are Prisma's diagonals. */
export type Rotation = "R0" | "R45" | "R90" | "R135" | "R180" | "R225" | "R270" | "R315";

/** Grid corner that receives the first DMX address (classic "Esquina inicial"). */
export type StartCorner = "TopLeft" | "TopRight" | "BottomLeft" | "BottomRight";

/** Addressing walks rows (Horizontal) or columns (Vertical) first. */
export type MainOrientation = "Horizontal" | "Vertical";

export interface FixtureType {
  name: string;
  pixel_count: number;
  components: ComponentSet;
  color_order: ColorOrder;
  arrangement: FixtureArrangement;
  roles: PixelRole[];
  /** How its white channel(s) are driven (RGBW / RGBWW). */
  white: WhiteProfile;
}

/** The operator's reusable fixture library (§3.2), stored in its own file. */
export interface FixtureLibrary {
  id: string;
  name: string;
  types: FixtureType[];
}

export interface PlacementSpec {
  fixture_type: FixtureType;
  start_number: number;
  rotation: Rotation;
  count: [number, number, number];
  /** Gap in cells BETWEEN neighbouring fixtures (0 = touching). */
  offset: [number, number, number];
  /** Top-left cell of the grid, 0-based (the dialog shows it 1-based). */
  start_position: [number, number, number];
  start_corner: StartCorner;
  orientation: MainOrientation;
  snake: boolean;
  snake_rotation: boolean;
  start_universe: number;
  start_channel: number;
  channel_offset: number;
  universe_offset: number;
  start_channel_every_universe: boolean;
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

export const nextFixtureId = () => invoke<number>("editor_next_fixture_id");

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

export const renameZone = (zone: string, name: string) =>
  invoke<EditorState>("editor_rename_zone", { zone, name });

// --- Groups (LIVE-MODEL-SPEC §3.1) — every edit undoable ---
/** Create a group from `members` (selection order). Returns the state and the new id. */
export const addGroup = (members: string[]) =>
  invoke<[EditorState, string]>("editor_add_group", { members });
export const renameGroup = (group: string, name: string) =>
  invoke<EditorState>("editor_rename_group", { group, name });
export const setGroupColor = (group: string, color: Rgb) =>
  invoke<EditorState>("editor_set_group_color", { group, color });
/** Replace the members, in element order (reorder / add / remove). */
export const setGroupMembers = (group: string, members: string[]) =>
  invoke<EditorState>("editor_set_group_members", { group, members });
export const removeGroup = (group: string) => invoke<EditorState>("editor_remove_group", { group });

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
  speed: number; // real -10..10 (0 = static, negative = reverse)
  solo: boolean;
  blackout: boolean;
  strobe_on: boolean;
  strobe_rate: number; // 0..1 over 1–25 Hz
  /** White temperature for two-white fixtures: automatic, or `white_kelvin`. */
  white_auto: boolean;
  white_kelvin: number;
  /** The zone has RGBWW fixtures (only then is the white control shown). */
  has_dual_white: boolean;
}

/** Every zone of the running show, in slot order, with live controls. */
export const mainZones = () => invoke<ZoneInfo[]>("main_zones");

/** Set a zone's dimmer (the zone-row fader) — changes the output, not the selection. */
export const setZoneDimmer = (zone: string, value: number) =>
  invoke<void>("main_set_zone_dimmer", { zone, value });

/** Set a zone's velocity multiplier (the zone-card speed fader), real -10..10:
 *  0 = static, negative = reverse. */
export const setZoneSpeed = (zone: string, value: number) =>
  invoke<void>("main_set_zone_speed", { zone, value });

/** Toggle a zone's solo (§3.1). */
export const setZoneSolo = (zone: string, on: boolean) =>
  invoke<void>("main_set_zone_solo", { zone, on });

/** Toggle a zone's blackout/mute (§3.1). */
export const setZoneBlackout = (zone: string, on: boolean) =>
  invoke<void>("main_set_zone_blackout", { zone, on });

/** Engage / release a zone's strobe (§3.6). */
export const setZoneStrobeOn = (zone: string, on: boolean) =>
  invoke<void>("main_set_zone_strobe_on", { zone, on });

/** Set a zone's strobe speed in Hz (1–25). */
export const setZoneStrobeRate = (zone: string, rate: number) =>
  invoke<void>("main_set_zone_strobe_rate", { zone, rate });

/** A zone's white temperature (RGBWW): automatic, or a fixed `kelvin`. */
export const setZoneWhite = (zone: string, auto: boolean, kelvin: number) =>
  invoke<void>("main_set_zone_white", { zone, auto, kelvin });

// --- Global operation bar (M8 step 6) ---

/** The master section's live state, for the global bar. */
export interface MasterInfo {
  dimmer: number; // 0..1
  blackout: boolean;
  strobe_on: boolean;
  strobe_rate: number; // 0..1 over 1–25 Hz
}

/** Read the master section (dimmer, blackout, global strobe). */
export const masterState = () => invoke<MasterInfo>("main_master");

/** Set the master dimmer (real 0..1). */
export const setMasterDimmer = (value: number) =>
  invoke<void>("main_set_master_dimmer", { value });

/** Toggle the master blackout (kills all output while holding the fader). */
export const setMasterBlackout = (on: boolean) =>
  invoke<void>("main_set_master_blackout", { on });

/** Engage / release the global strobe. */
export const setMasterStrobeOn = (on: boolean) => invoke<void>("main_set_master_strobe_on", { on });

/** Set the global strobe speed in Hz (1–25). */
export const setMasterStrobeRate = (rate: number) =>
  invoke<void>("main_set_master_strobe_rate", { rate });

/** Open the patch editor as a secondary window (§6/§7). Freezes the show while open;
 *  applies the edits to the running show on close. */
export const openEditor = () => invoke<void>("main_open_editor");

// --- Device manager (network output, docs/DEVICES-SPEC.md) ---
//
// Universe numbers are Art-Net port addresses, shown AS IS — like professional consoles, whose node
// config shows universe 1 as 0x0001.

export type NodeOrigin =
  | { kind: "manual" }
  | { kind: "scanned"; mac: string; bind_index: number; esta: number; oem: number; firmware: number };

/** One Art-Net device row, as professional consoles list it: a node or one BindIndex group of it. */
export interface ArtNetNode {
  id: string;
  /** Short name ("Nombre del dispositivo"). */
  name: string;
  long_name: string;
  /** "0.0.0.0" = not set yet. */
  ip: string;
  /** The universe of each output port, in port order. */
  ports: number[];
  enabled: boolean;
  /** false = send to the interface's subnet broadcast instead of the node's IP. */
  unicast: boolean;
  /** Pad every ArtDmx to 512 channels. */
  full_frames: boolean;
  origin: NodeOrigin;
}

/** The output interface, stored by network (address + prefix); name is only a hint. */
export interface InterfaceRef {
  address: string;
  prefix_len: number;
  name_hint: string;
}

/** The show's whole network output config. */
export interface ShowNetwork {
  initialized: boolean;
  artnet_enabled: boolean;
  artsync: boolean;
  interface: InterfaceRef | null;
  nodes: ArtNetNode[];
}

/** A host network interface (for the interface dropdown). */
export interface InterfaceInfo {
  name: string;
  address: string;
  prefix_len: number;
}

/** How the running show's output resolved: interface + universes no node receives. */
export interface OutputStatus {
  interface_state: "unset" | "resolved" | "missing";
  interface: InterfaceInfo | null;
  uncovered: number[];
}

/** One device row found by a Scan (one per BindIndex group, like professional consoles). */
export interface ScannedNode {
  ip: string;
  short_name: string;
  long_name: string;
  mac: string;
  bind_index: number;
  ports: number[];
  esta: number;
  oem: number;
  firmware: number;
}

export type NodeReach = "online" | "silent" | "lost";
export interface NodeStatusInfo {
  ip: string;
  status: NodeReach;
}

export const outputGet = () => invoke<ShowNetwork>("main_output_get");
/** Apply a staged config to the running show (Aplicar / Aceptar). */
export const outputSet = (network: ShowNetwork) => invoke<void>("main_output_set", { network });
export const outputStatus = () => invoke<OutputStatus>("main_output_status");
export const networkInterfaces = () => invoke<InterfaceInfo[]>("main_network_interfaces");
/** Scan (~3 s): ArtPoll the network, replies merged per board. */
export const artnetScan = () => invoke<ScannedNode[]>("main_artnet_scan");
export const nodeStatuses = () => invoke<NodeStatusInfo[]>("main_node_statuses");
/** Open the node's web configuration page in the system browser. */
export const openNodePage = (ip: string) => invoke<void>("main_open_node_page", { ip });
export const hostName = () => invoke<string>("main_host_name");

/** Save the live show (with its embedded patch) to a `.prisma` file (§5). */
export const saveShow = (path: string) => invoke<void>("main_save_show", { path });
/** Open a `.prisma` show and run it (recompiles the engine onto its embedded patch). */
export const openShow = (path: string) => invoke<void>("main_open_show", { path });

/** Start a clean show over the current patch (keeps zones/fixtures, clears all effects). */
export const newShow = () => invoke<void>("main_new_show");

/** "Archivo → Abrir show de demo": the built-in demo show, unsaved like a new one. */
export const openDemoShow = () => invoke<void>("main_open_demo_show");

/** Set once at boot when the session was recovered from an autosave. */
export interface RecoveryInfo {
  /** The last saved show to fall back to, if any. */
  last_saved: string | null;
}
export const recoveryInfo = () => invoke<RecoveryInfo | null>("main_recovery_info");
/** The last opened or saved show file, if any. */
export const mainLastSaved = () => invoke<string | null>("main_last_saved");

// --- The track grid (live model, docs/LIVE-MODEL-SPEC.md §7) ---

/** One clip (grid cell) of a track: empty (assignable) or an assigned effect, and
 *  whether it is the track's running or armed clip. */
export interface CellInfo {
  index: number;
  effect_id: string | null;
  effect_name: string | null;
  active: boolean;
  /** Armed for GO. */
  armed: boolean;
  /** The clip's own fade in seconds (0 = the track's transition). */
  fade: number;
  /** The operator's label for the cell (null = unnamed: show the effect's name). */
  name: string | null;
  /** The cell's grid colour, sRGB 0–255 (null = default panel grey). */
  color: [number, number, number] | null;
  /** The effect's stored parameter overrides (normalized), null for an empty cell. */
  params: Record<string, number> | null;
  /** The effect's own colour table (linear light), if it has one. */
  palette: [number, number, number][] | null;
  /** The effect's main colour (linear light), shown when the cell has no label colour. */
  effect_color: [number, number, number] | null;
}

/** How a track blends onto the ones before it (the Rust enum's names). */
export type BlendMode =
  | "Normal"
  | "Add"
  | "Subtract"
  | "Multiply"
  | "Screen"
  | "Max"
  | "Min"
  | "Mask"
  | "LumaKey";

export type TimeBase = "seconds" | "beats";
export type Easing = "linear" | "ease_in" | "ease_out" | "ease_in_out";
export type LaunchMode = "direct" | "arm";

export interface TrackTransition {
  time: number;
  base: TimeBase;
  easing: Easing;
}

/** One column of the grid. */
export interface TrackInfo {
  id: string;
  name: string;
  zones: string[];
  /** The destination zones' names, joined. */
  destination: string;
  /** The first destination zone's display colour (sRGB 0–255): pads and cells. */
  color: [number, number, number];
  /** Every destination zone's display colour, parallel to `zones`: the header strip. */
  colors: [number, number, number][];
  blend_mode: BlendMode;
  /** LumaKey: where the source starts to show, and how far above it it is full (0..1). */
  key_threshold: number;
  key_softness: number;
  opacity: number;
  mute: boolean;
  solo: boolean;
  transition: TrackTransition;
  launch: LaunchMode;
  active: number | null;
  armed: number | null;
  clips: CellInfo[];
}

export interface SceneInfo {
  index: number;
  name: string | null;
  color: [number, number, number] | null;
}

export interface GridInfo {
  tracks: TrackInfo[];
  scenes: SceneInfo[];
}

/** One entry the effect picker offers. */
export interface EffectListing {
  id: string;
  name: string;
  category: string;
}

/** The effect catalogue for the picker, grouped by `category` in the UI. */
export const effectCatalogue = () => invoke<EffectListing[]>("main_effect_catalogue");

/** The whole grid: tracks (compositing order) with their clips, and the scene rows. */
export const grid = () => invoke<GridInfo>("main_grid");

/** One track's clips (from the grid). */
export const trackClips = async (track: string) =>
  (await grid()).tracks.find((t) => t.id === track)?.clips ?? [];

/** Assign an effect to a clip AND play it. `params` / `palette` default to the effect's. */
export const assignCell = (
  track: string,
  index: number,
  effectId: string,
  params: Record<string, number> | null = null,
  palette: [number, number, number][] | null = null,
) => invoke<CellInfo[]>("main_assign_cell", { track, index, effectId, params, palette });

/** Assign an effect (with its stored params / colour table) to a clip WITHOUT playing it. */
export const assignCellSilent = (
  track: string,
  index: number,
  effectId: string,
  params: Record<string, number> | null,
  palette: [number, number, number][] | null,
) => invoke<CellInfo[]>("main_assign_cell_silent", { track, index, effectId, params, palette });

/** Label a clip (blank clears the label). */
export const renameCell = (track: string, index: number, name: string) =>
  invoke<CellInfo[]>("main_rename_cell", { track, index, name });

/** Give a clip its own grid colour (sRGB 0–255). */
export const setCellColor = (track: string, index: number, color: [number, number, number]) =>
  invoke<CellInfo[]>("main_set_cell_color", { track, index, color });

/** Launch a clip (the track's `launch` target: plays it, or arms it on an Arm track). */
export const triggerCell = (track: string, index: number) =>
  invoke<CellInfo[]>("main_trigger_cell", { track, index });

/** Empty a clip (and stop the track if it was running). */
export const clearCell = (track: string, index: number) =>
  invoke<CellInfo[]>("main_clear_cell", { track, index });

/** Arm (or disarm) a clip for GO. */
export const armCell = (track: string, index: number) => invoke<void>("main_arm_cell", { track, index });

/** A clip's own fade in seconds (0 = the track's transition). */
export const setCellFade = (track: string, index: number, seconds: number) =>
  invoke<void>("main_set_cell_fade", { track, index, seconds });

/** Stop a track (fades out with its transition; a second stop during the fade cuts). */
export const stopTrack = (track: string) => invoke<void>("main_stop_track", { track });

/** Launch (or arm) grid row `row` on every track that has a clip there. */
export const launchScene = (row: number, arm = false) => invoke<void>("main_launch_scene", { row, arm });

/** Rename a scene row (empty = back to its number). A label only. */
export const renameScene = (row: number, name: string) => invoke<void>("main_rename_scene", { row, name });

/** GO: play every armed clip in the same frame. */
export const armGo = () => invoke<void>("main_arm_go");

export const setTrackOpacity = (track: string, value: number) =>
  invoke<void>("main_set_track_opacity", { track, value });
export const setTrackMute = (track: string, on: boolean) => invoke<void>("main_set_track_mute", { track, on });
export const setTrackSolo = (track: string, on: boolean) => invoke<void>("main_set_track_solo", { track, on });

/** The track popover: transition + launch mode (applies at the next launch). */
export const setTrackBehaviour = (track: string, transition: TrackTransition, launch: LaunchMode) =>
  invoke<void>("main_set_track_behaviour", { track, transition, launch });

export const setTrackBlend = (
  track: string,
  mode: BlendMode,
  keyThreshold: number | null = null,
  keySoftness: number | null = null,
) => invoke<void>("main_set_track_blend", { track, mode, keyThreshold, keySoftness });

/** Disarm every armed clip (`/arm/clear`). */
export const armClear = () => invoke<void>("main_arm_clear");

/** Stop every track (each with its transition; a second press during the fades cuts). */
export const stopAll = () => invoke<void>("main_stop_all");

/** Move a track just before another of the same zone (`before` null = last). */
export const moveTrack = (track: string, before: string | null) =>
  invoke<void>("main_move_track", { track, before });

export const renameTrack = (track: string, name: string) => invoke<void>("main_rename_track", { track, name });

/** "Aplicar destino": the zones a track reaches (at least one). Recompiles the running
 *  show; the window re-fetches on `layout-changed`. */
export const setTrackDestination = (track: string, zones: string[]) =>
  invoke<void>("main_set_track_destination", { track, zones });

/** "+ Track" on a zone (recompiles; the window re-fetches on `layout-changed`). */
/** Resolves to the new track's id. */
export const addTrack = (zone: string) => invoke<string>("main_add_track", { zone });
export const removeTrack = (track: string) => invoke<void>("main_remove_track", { track });

// --- Tempo (global bar) ---

export const clockInfo = () => invoke<{ bpm: number }>("main_clock");
export const setBpm = (bpm: number) => invoke<void>("main_set_bpm", { bpm });
export const tap = () => invoke<void>("main_tap");

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
   *  colour picker, the direction rose, a channel of a palette slot, a palette's
   *  active-length, or a discrete choice → a dropdown. Value model is scalar regardless
   *  (an `enum` is an index 0..max). */
  role: "scalar" | "color" | "direction" | "enum" | "toggle" | "bpm" | "direction_menu" | "palette_mode";
  /** A grouping key: the colour group id for `role === "color"`. Null otherwise. */
  group: string | null;
  /** Colour channel — set for `role === "color"`. */
  channel: "r" | "g" | "b" | null;
  /** The named options for `role === "enum"` — the dropdown labels, in index order.
   *  Absent for every other role (the shape effects that declare enums land in a later
   *  step; the panel already renders them). */
  choices?: string[] | null;
  /** Greyed out unless `param` has the choice labelled `choice` (or, when `choice` is
   *  null, unless `param` is above zero). Console-style dependent controls. */
  requires?: { param: string; choice: string | null } | null;
  /** The device knob (`/select/param/{n}`, 0-based) that drives it while this clip is
   *  selected: the n-th continuous parameter. null for colours, choices and switches. */
  knob?: number | null;
}

/** The effect work panel for a zone: one cell's effect + its parameters — the grid's
 *  selected cell, else the zone's running cell. `effect_id` is null when that cell is
 *  empty (or the zone has no cell to show). */
export interface EffectPanelInfo {
  track_id: string;
  /** The cell is armed for GO. */
  armed: boolean;
  cell_index: number | null;
  /** Whether `cell_index` is the zone's running cell (its edits drive the output live). */
  active: boolean;
  /** The cell is EMPTY: this is the UI's draft effect at its defaults, nothing stored. */
  draft: boolean;
  effect_id: string | null;
  effect_name: string | null;
  params: ParamInfo[];
  /** The colour table (linear-light RGB) for an effect that reads one, else null. */
  palette: [number, number, number][] | null;
  /** Most colours a table may hold (255). */
  palette_max: number;
  /** The clip's own fade in seconds (0 = the track's transition). */
  fade: number;
}

/** A cell's effect and its manifest-driven parameters (§3.5). `cell` is the grid's
 *  selection; null = the zone's running cell. For an EMPTY cell, `draft` is the effect
 *  to show at its defaults (read-only — the cell stays empty). */
export const effectPanel = (track: string, cell: number | null = null, draft: string | null = null) =>
  invoke<EffectPanelInfo>("main_effect_panel", { track, cell, draft });

/** Replace the colour table of a cell's effect (linear-light RGB list). Live only when
 *  that cell is the running one; `cell` null = the running cell. */
export const setPalette = (track: string, colors: [number, number, number][], cell: number | null = null) =>
  invoke<void>("main_set_palette", { track, colors, cell });

/** Fired on `window` for every `setParam`, so the effect panel keeps its copy of the
 *  values current (a control opened later — e.g. the Tabla de color — must see what was
 *  set, not what was loaded when the cell was selected). */
export const PARAM_WRITTEN = "prisma-param-written";
export interface ParamWritten {
  track: string;
  name: string;
  value: number;
}

/** Set one parameter of a zone's active effect (normalized 0..1). Drives it live. */
/** After the app stored a parameter (the grid re-reads: a cell shows its effect's main
 *  colour, computed from the current values). */
export const PARAM_STORED = "prisma-param-stored";

export const setParam = (track: string, name: string, value: number, cell: number | null = null) => {
  window.dispatchEvent(new CustomEvent<ParamWritten>(PARAM_WRITTEN, { detail: { track, name, value } }));
  return invoke<void>("main_set_param", { track, name, value, cell }).then(() => {
    window.dispatchEvent(new CustomEvent<ParamWritten>(PARAM_STORED, { detail: { track, name, value } }));
  });
};

// --- MIDI (M9 Slice 1): input device + MIDI Learn ---

/** The per-zone controls a mapping can address by position (zone slot). */
export type ZoneSub = "dimmer" | "speed" | "solo" | "blackout" | "strobe_on" | "strobe_rate";

/** The per-track controls a mapping can address by position in the controller's bank
 *  window; `{ clip: i }` is row i of the window. */
export type TrackSub = "opacity" | "mute" | "solo" | "stop" | "select" | { clip: number };

/** How a control acts. Faders are `absolute`; buttons/pads use the other four. */
export type ControlMode = "absolute" | "toggle" | "latch" | "trigger" | "momentary";

export type LedRate = "n24" | "n16" | "n8" | "n4" | "n2";
export type LedAnimation = "solid" | { pulse: LedRate } | { blink: LedRate };

/** The operator's LED look for one control: palette colour indices + motion. */
export interface LedStyle {
  on?: number;
  idle?: number;
  animation?: LedAnimation;
}

/** Where a MIDI mapping writes: a literal tree path (globals such as `/master/dimmer`), a
 *  zone control by slot (slot 0 = the first zone of whichever show is running), a track
 *  control or scene by position in the bank window, or parameter n of the selected clip.
 *  The shape mirrors the Rust enum's serde form. */
export type MapTarget =
  | { path: string }
  | { zone_slot: { slot: number; sub: ZoneSub } }
  | { track_slot: { slot: number; sub: TrackSub } }
  | { scene: number }
  | { selected_param: number };

/** Where the controller's grid window sits over the track grid. */
export interface Bank {
  track: number;
  row: number;
}

/** A physical control: 0-based channel + note or CC number. */
export interface MidiControl {
  channel: number;
  kind: { note: number } | { cc: number };
}

export interface MidiMapping {
  control: MidiControl;
  target: MapTarget;
  mode: ControlMode;
  curve: unknown;
  led?: LedStyle | null;
  /** How a fader / knob takes over a value it does not match: "pickup" waits until the
   *  control meets it (the default), "jump" applies every position at once. */
  takeover?: Takeover;
}

export type Takeover = "pickup" | "jump";

export interface MidiStatus {
  ports: string[];
  /** Configured input; null = auto-detect (APC40). */
  selected: string | null;
  /** The port open right now, if any. */
  connected: string | null;
  /** Output open for LED feedback, if the device has one. */
  output: string | null;
  /** Connected device profile, e.g. "Akai APC40 mkII" / "Generic MIDI". */
  device: string;
  error: string | null;
  learning: MapTarget | null;
  mappings: MidiMapping[];
  /** Device LED palette as `#rrggbb` (index = colour value); empty if none. */
  palette: string[];
  /** The controller's grid window. */
  bank: Bank;
  /** How many default mappings the connected device's profile ships (0 = none). */
  device_defaults: number;
  /** The device's grid window (tracks × rows) when it has bank buttons, else null. */
  grid_window: [number, number] | null;
}

export const midiStatus = () => invoke<MidiStatus>("midi_status");
export const midiSetInput = (port: string | null) => invoke<MidiStatus>("midi_set_input", { port });
export const midiReconnect = () => invoke<MidiStatus>("midi_reconnect");
export const midiLearnArm = (target: MapTarget) => invoke<void>("midi_learn_arm", { target });
export const midiLearnCancel = () => invoke<void>("midi_learn_cancel");
export const midiUnmap = (target: MapTarget) => invoke<MidiMapping[]>("midi_unmap", { target });
export const midiConfigure = (target: MapTarget, mode: ControlMode, led: LedStyle | null) =>
  invoke<MidiMapping[]>("midi_configure", { target, mode, led });

export const midiSetTakeover = (target: MapTarget, takeover: Takeover) =>
  invoke<MidiMapping[]>("midi_set_takeover", { target, takeover });

/** "Cargar mapeos del perfil": add the device's defaults for unmapped controls only. */
export const midiLoadDeviceDefaults = () =>
  invoke<{ added: number; mappings: MidiMapping[] }>("midi_load_device_defaults");

// --- Control (LIVE-MODEL-SPEC §8.2): selection in the tree, keyboard shortcuts ---

/** The selection as the engine holds it; `clip` null = the track's running clip. */
export interface Selection {
  track: string | null;
  clip: number | null;
}

export const mainSelect = (track: string | null, clip: number | null) =>
  invoke<void>("main_select", { track, clip });
export const mainSelection = () => invoke<Selection>("main_selection");

/** A key with its modifiers, as `keyboard.ts` normalizes it. */
export interface KeyCombo {
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
}

/** What has the focus: shortcuts never fire in a text or numeric field. */
export type FocusKind = "other" | "text" | "number";

export interface KeyBinding {
  combo: KeyCombo;
  path: string;
}

/** A key press: the shortcut's path if one ran (a `ui:…` window action is returned for
 *  the UI to perform), else null. */
export const mainKey = (combo: KeyCombo, focus: FocusKind) => invoke<string | null>("main_key", { combo, focus });
export const controlKeys = () => invoke<KeyBinding[]>("control_keys");
export const controlSetKey = (path: string, combo: KeyCombo | null) =>
  invoke<KeyBinding[]>("control_set_key", { path, combo });
export const controlResetKeys = () => invoke<KeyBinding[]>("control_reset_keys");

/** Structural equality of two targets (they are small plain objects). */
export const sameTarget = (a: MapTarget, b: MapTarget) => JSON.stringify(a) === JSON.stringify(b);

/** Short human label for a control, e.g. "CC 7 · Ch 2" or "Nota 36 · Ch 1". */
export const controlLabel = (c: MidiControl) =>
  "cc" in c.kind ? `CC ${c.kind.cc} · Ch ${c.channel + 1}` : `Nota ${c.kind.note} · Ch ${c.channel + 1}`;

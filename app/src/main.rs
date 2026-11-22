//! Prisma desktop shell.
//!
//! Hosts the engine (started once, shut down in order on exit — S1) and serves the
//! patch-editor UI. The editor works against a Rust-side [`PatchDraft`] (the single
//! source of truth, S2); the webview renders it and sends structural edits back as
//! commands. S5 adds the first edits: fixture placement (into the active zone) plus
//! delete / undo / redo, each an undoable [`EditOp`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autosave;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glam::Vec3;
use prisma_editor::{
    aggregate, load_library, next_fixture_id, next_free_address, plan, plan_preview, save_library,
    EditOp, FixtureLibrary, FixtureType, PatchDraft, PlacementError, PlacementSpec, PreviewRun,
    SelectionAggregate,
};
use prisma_engine::{
    host_interfaces, load_mappings, load_show, save_mappings, save_show, ArtNetNode, Cell,
    ClipEffect, ClipHost, ControlMode, ControllerMappings, DiscoveredNode, Engine, EngineConfig,
    EngineHandle, HostInterface, InterfaceResolution, Layer, LedStyle, MapTarget, MidiMapping,
    MidiNotice, MidiRuntime, NodeMonitor, NodeOrigin, NodeStatus, ParamSet, ScanError, Show,
    ShowNetwork, Source, Takeover, Track, TrackBehaviour, ZoneLiveReadout, ZoneShow,
};
use prisma_engine::{LaunchMode, TrackTransition};
use prisma_midi::io::{input_port_names, MidiInputLink, MidiIoError, MidiOutputLink};
use prisma_midi::{DeviceProfile, APC40_KEYWORDS};
use prisma_patch::{load_patch, save_patch, BlendMode, Diagnostic, Patch, Zone};
use prisma_preview::sink::WebSocketSink;
use prisma_render::{AdapterPreference, ColorChannel, ParamRole, Requires};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use uuid::Uuid;

/// A zone card's data for the zone row: identity + its current live controls.
#[derive(Serialize)]
struct ZoneInfo {
    id: Uuid,
    name: String,
    color: [u8; 3],
    dimmer: f32,
    /// Velocity multiplier, real `-10..=10` (0 = static, negative = reverse). The
    /// zone-card speed fader seeds from this and writes it back live.
    speed: f32,
    solo: bool,
    blackout: bool,
    strobe_on: bool,
    /// Strobe speed, `0..=1` over 1–25 Hz.
    strobe_rate: f32,
}

/// One scene-grid cell of the selected zone (M8 step 4): empty (assignable) or an
/// assigned effect, and whether it is the zone's running (active) cell.
#[derive(Serialize)]
struct CellInfo {
    index: usize,
    effect_id: Option<String>,
    effect_name: Option<String>,
    active: bool,
    /// Armed for GO (LIVE-MODEL-SPEC §5).
    armed: bool,
    /// The clip's own fade in seconds (0 = the track's transition).
    fade: f64,
    /// The operator's label for the cell (`None` = unnamed).
    name: Option<String>,
    /// The cell's grid colour, sRGB 0–255 (`None` = default panel grey).
    color: Option<[u8; 3]>,
    /// The effect's stored parameter overrides — carried so the grid's copy/paste can
    /// duplicate a cell whole (effect + parameters + colour table + label).
    params: Option<ParamSet>,
    /// The effect's stored colour table (linear light), if it has its own.
    palette: Option<Vec<[f32; 3]>>,
    /// The effect's main colour (its first colour group, linear light) — what the grid
    /// shows on a cell without a label colour. `None` for an effect without one.
    effect_color: Option<[f32; 3]>,
}

/// One entry the effect picker offers, catalogue-driven (§5.3).
#[derive(Serialize)]
struct EffectListingInfo {
    id: String,
    name: String,
    category: String,
}

/// One row of the effect parameter panel (M8 step 5, §3.5), built from the effect's
/// manifest — never a hardcoded per-effect control (§5.3). `value_norm` is the cell's
/// current normalized value (its stored override, else the manifest default); `min`/
/// `max` are the real range so the panel can show a value the operator reads.
#[derive(Serialize)]
struct ParamInfo {
    name: String,
    label: String,
    min: f32,
    max: f32,
    value_norm: f32,
    /// Presentation hint the panel switches on (§3.5): `"scalar"` (a slider), `"color"`
    /// (a channel of the `group` colour picker), `"direction"` (the rose), `"enum"`,
    /// `"toggle"`, `"bpm"` or `"direction_menu"`. A palette is not a parameter: it travels
    /// as `EffectPanelInfo::palette`.
    role: &'static str,
    /// A grouping key: for `role == "color"` the colour group id. `Some` only then.
    group: Option<String>,
    /// The colour channel (`"r"`/`"g"`/`"b"`), `Some` for `role == "color"`.
    channel: Option<&'static str>,
    /// Dropdown labels for `role == "enum"` (or `"Group|Label"` for `"direction_menu"`),
    /// in index order; `None` for every other role.
    choices: Option<Vec<String>>,
    /// Greyed out unless this holds (console-style dependent controls); `None` = always on.
    requires: Option<RequiresInfo>,
}

/// [`prisma_render::Requires`] for the panel: enabled while `param` has the choice
/// labelled `choice`, or (when `choice` is `None`) while `param` is above zero.
#[derive(Serialize)]
struct RequiresInfo {
    param: &'static str,
    choice: Option<&'static str>,
}

/// The effect work panel's data for the selected zone (M8 step 5): the zone's active
/// cell effect and its manifest-driven parameters. `effect_id` is `None` when the zone
/// has no running cell (empty selection) — the panel then shows an empty state.
#[derive(Serialize)]
struct EffectPanelInfo {
    track_id: Uuid,
    /// The cell is armed for GO.
    armed: bool,
    cell_index: Option<usize>,
    /// Whether `cell_index` is the zone's running cell — its edits drive the output live;
    /// any other cell's edits are only stored, for when it is played.
    active: bool,
    /// The cell is EMPTY and this panel shows the UI's draft effect at its defaults (a
    /// read of the manifest only — the cell stays empty until the UI assigns it).
    draft: bool,
    effect_id: Option<String>,
    effect_name: Option<String>,
    params: Vec<ParamInfo>,
    /// The colour table (linear RGB) for an effect that reads one; `None` otherwise.
    palette: Option<Vec<[f32; 3]>>,
    /// Most colours a table may hold.
    palette_max: usize,
    /// The clip's own fade in seconds (0 = the track's transition).
    fade: f64,
}

/// A display label from a manifest parameter key: capitalise the first letter, leaving
/// the rest as-is. Generic on purpose — the panel stays manifest-driven, with no
/// per-effect label table (§5.3). Real i18n resolution of the manifest's `label_key`
/// is deferred (the UI has no locale resolver yet); this is a legible stand-in.
fn humanize(name: &str) -> String {
    // Spanish names already settled with Bauti (console wording). A stand-in for the
    // deferred i18n catalogue, keyed by parameter name — not a per-effect table.
    if let Some(es) = match name {
        "fade" => Some("Atenuar"),
        "bpm" => Some("BPM"),
        "step" => Some("Anchura del paso"),
        "color_width" => Some("Anchura del color"),
        "cross_width" => Some("Anchura de la cruz"),
        "displacement" => Some("Desplazamiento"),
        "disp_speed" => Some("Vel. del desplazamiento"),
        "disp_dist" => Some("Distribución"),
        "cross_axes" => Some("Ejes de la cruz"),
        "mode" => Some("Dirección"),
        "ratio" => Some("Proporción"),
        "fade_out" => Some("Difuminado"),
        "color_mode" => Some("Modo de color"),
        "background" => Some("Fondo"),
        _ => None,
    } {
        return es.to_string();
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Flatten a manifest [`ParamRole`] into the serialisable `(role, group, channel)` the
/// UI switches on. Presentation only — it never changes the value the tree carries.
fn role_dto(role: &ParamRole) -> (&'static str, Option<String>, Option<&'static str>) {
    fn ch_str(channel: &ColorChannel) -> &'static str {
        match channel {
            ColorChannel::R => "r",
            ColorChannel::G => "g",
            ColorChannel::B => "b",
        }
    }
    match role {
        ParamRole::Scalar => ("scalar", None, None),
        ParamRole::Direction => ("direction", None, None),
        ParamRole::Color { group, channel } => {
            ("color", Some((*group).to_string()), Some(ch_str(channel)))
        }
        // The labels travel in `ParamInfo::choices`, computed at the call site; the
        // group/channel slots are unused for an enum.
        ParamRole::Enum { .. } => ("enum", None, None),
        ParamRole::Toggle => ("toggle", None, None),
        ParamRole::Bpm => ("bpm", None, None),
        // Choices travel in `ParamInfo::choices` ("Group|Label"), like an enum's.
        ParamRole::DirectionMenu { .. } => ("direction_menu", None, None),
        // Choices travel in `ParamInfo::choices`, like an enum's.
        ParamRole::PaletteMode { .. } => ("palette_mode", None, None),
    }
}

/// How many assignable clips each track has (8 columns × 5 rows — the common
/// pad-controller layout, shown as the zone's grid until the track grid lands in
/// phase 2). Shorter tracks are padded up to this on load.
const GRID_CELLS: usize = 40;

/// The picker's display name for an `effect_id`, from the catalogue.
fn effect_display_name(id: &str) -> Option<String> {
    prisma_render::catalogue()
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.name.to_string())
}

/// An effect's main colour: its manifest's FIRST colour group, each channel the stored
/// value or the manifest default (linear light `0..1`). `None` if it declares none.
fn effect_main_color(effect_id: &str, params: &ParamSet) -> Option<[f32; 3]> {
    let defs = prisma_render::effect_params(effect_id);
    let group = defs.iter().find_map(|p| match p.role {
        ParamRole::Color { group, .. } => Some(group),
        _ => None,
    })?;
    let mut rgb = [0.0f32; 3];
    for p in &defs {
        if let ParamRole::Color { group: g, channel } = p.role {
            if g == group {
                let i = match channel {
                    ColorChannel::R => 0,
                    ColorChannel::G => 1,
                    ColorChannel::B => 2,
                };
                rgb[i] = params.get(&p.name).unwrap_or(p.default_norm);
            }
        }
    }
    Some(rgb)
}

/// A track's clips as cell DTOs (`armed` = its armed clip).
fn grid_dto(zs: &Track, armed: Option<usize>) -> Vec<CellInfo> {
    zs.clips
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let shader = cell.layers.first().map(|l| match &l.source {
                Source::Shader {
                    effect_id,
                    params,
                    palette,
                } => (effect_id.clone(), params.clone(), palette.clone()),
            });
            let (effect_id, params, palette) = match shader {
                Some((id, params, palette)) => (Some(id), Some(params), palette),
                None => (None, None, None),
            };
            let effect_name = effect_id.as_deref().and_then(effect_display_name);
            let effect_color = effect_id
                .as_deref()
                .zip(params.as_ref())
                .and_then(|(id, p)| effect_main_color(id, p));
            CellInfo {
                index: i,
                effect_id,
                effect_name,
                active: zs.active == Some(i),
                armed: armed == Some(i),
                fade: cell.fade_time.as_secs_f64(),
                name: cell.cell_name.clone(),
                color: cell.cell_color,
                params,
                palette,
                effect_color,
            }
        })
        .collect()
}

/// Side of the default rig (like professional consoles, a fresh start is a mapped square, not a blank).
const DEFAULT_RIG: u32 = 50;

/// A fresh patch for a new show: ONE zone ("Zona 1") already mapped with a
/// `DEFAULT_RIG`×`DEFAULT_RIG` matrix of 1-pixel RGB fixtures, addressed from universe 1
/// channel 1 (170 per universe), and the zone's render matrix set to that same size so the
/// content is sharp from the first frame. Unrouted universes go to the virtual sink, so
/// this lights nothing on the network until a device is configured.
fn empty_patch() -> Patch {
    let zone_id = Uuid::new_v4();
    let side = DEFAULT_RIG;
    let spec = PlacementSpec {
        fixture_type: FixtureType {
            name: "Generic RGB 1 pixel".into(),
            pixel_count: 1,
            components: prisma_patch::ComponentSet::Rgb,
            color_order: prisma_patch::ColorOrder::Rgb,
            arrangement: prisma_editor::FixtureArrangement::Strip,
            roles: Vec::new(),
        },
        start_number: 1,
        rotation: prisma_editor::Rotation::R0,
        count: [side, side, 1],
        offset: [0, 0, 0],
        start_position: [0, 0, 0],
        start_corner: prisma_editor::StartCorner::TopLeft,
        orientation: prisma_editor::MainOrientation::Horizontal,
        snake: false,
        snake_rotation: false,
        start_universe: 1,
        start_channel: 1,
        channel_offset: 0,
        universe_offset: 0,
        start_channel_every_universe: false,
        fixtures_per_universe: Some(170),
        zone: zone_id,
    };
    let mut patch = Patch {
        id: Uuid::new_v4(),
        name: "Sin título".into(),
        matrix_size: [1, 1, 1],
        fixtures: plan(&spec).unwrap_or_default(),
        zones: vec![Zone {
            id: zone_id,
            name: "Zona 1".into(),
            members: Vec::new(),
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [side, side, 1],
            display_color: ZONE_COLORS[0],
            world_transform: None,
        }],
    };
    patch.fill_zone_members_from_ownership();
    patch.fit_matrix_size();
    patch
}

/// Pad every track up to `GRID_CELLS` assignable (empty) clips — `Show::new` creates
/// tracks with no clips, and every row needs a cell in every track. Existing clips keep
/// their index and a track is never truncated, so real content is never wiped.
/// Idempotent.
fn pad_grids(show: &mut Show) {
    for track in &mut show.tracks {
        if track.clips.len() < GRID_CELLS {
            track.clips.resize_with(GRID_CELLS, Cell::empty);
        }
    }
}

/// A new show: the default 50×50 zone with its own track of empty clips (nothing
/// running — lazy assignment: a clip is assigned only when the operator edits it or
/// plays it with GO) and the dimmer up, so the operator starts from a mapped rig
/// (matching a freshly added zone).
fn empty_show() -> Show {
    let mut show = Show::new("Sin título", empty_patch());
    pad_grids(&mut show);
    // The default zone starts at full dimmer (like the reconcile path's new-zone branch),
    // so the first clip the operator plays lights the rig at once.
    for zs in &mut show.zones {
        zs.live.dimmer = 1.0;
    }
    show
}

/// Read the remembered last-show path, if any. Empty/missing file ⇒ `None` ⇒ boot empty.
fn read_last_show(file: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(file).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// Remember (or, with `None`, forget) the last-show path so the next launch reopens it.
fn write_last_show(file: &Path, path: Option<&Path>) {
    let content = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = std::fs::write(file, content);
}

/// Everything the shell owns for the life of the process.
struct AppState {
    /// The live output engine (S1). `None` once shut down.
    engine: Mutex<Option<EngineHandle>>,
    /// Autosave (see `autosave`): its files, and the clean baseline + last snapshot the
    /// autosave thread compares against.
    autosave: autosave::Autosave,
    autosave_state: Mutex<(autosave::Baseline, Option<String>)>,
    /// Set at boot when the session was recovered from an autosave; taken once by the UI
    /// to show "Sesión recuperada".
    recovered: Mutex<Option<RecoveryInfo>>,
    /// Each track's armed clip, as the clip controller reports it (live state, not
    /// saved with the show).
    armed: Mutex<std::collections::HashMap<Uuid, usize>>,
    /// The editable Show — the source of truth for each zone's scene grid (M8 step 4)
    /// AND for the zone set/identity (its embedded patch; §7 derives the zone row and
    /// slot mapping from it, so a hot patch edit needs no separate list kept in sync).
    /// The engine ran a copy at boot; grid edits mutate this and are pushed to the
    /// engine as effect triggers through the frame-boundary queue.
    show: Mutex<Show>,
    /// The editor's working patch and its undo history (S2).
    draft: Mutex<PatchDraft>,
    /// The operator's reusable fixture library (S7, §3.2). Feeds the placement dialog
    /// and the library manager; persisted to [`library_path`](AppState::library_path)
    /// on every edit.
    library: Mutex<FixtureLibrary>,
    /// Where the library file lives — the per-user app-config dir, not the project or
    /// the `.prismap`.
    library_path: PathBuf,
    /// TCP port the live-preview WebSocket is bound to (§M6/S10). The React preview
    /// queries it and connects to `ws://127.0.0.1:{port}`. A fresh (OS-assigned) port
    /// is bound per engine, so a recompile never fights the old bind for a fixed port;
    /// the viewer re-queries and reconnects.
    preview_port: Mutex<u16>,
    /// The LEGACY app-wide Art-Net target (`network.json`, pre device manager). Read-only:
    /// it only seeds shows that predate the per-show network ([`prepare_show_network`]).
    network: Mutex<NetworkConfig>,
    /// The Art-Net node monitor (status + Scan), the single owner of UDP 6454. `None`
    /// when another program holds the port; retried on the next Scan.
    artnet_monitor: Mutex<Option<Arc<NodeMonitor>>>,
    /// File holding the path of the last opened/saved show, so the next launch reopens it.
    /// Empty/missing ⇒ boot empty. Cleared by "Nueva configuración".
    last_show_file: PathBuf,
    /// MIDI (M9): a clone of the engine's runtime. It outlives every recompile (the engine
    /// carries it across), so the open input link below keeps feeding the live tree.
    midi: MidiRuntime,
    /// The open hardware input, if any. Dropping it closes the port.
    midi_link: Mutex<Option<MidiInputLink>>,
    /// The same device's output (LED feedback, M9 Slice 2), if it has one.
    midi_out_link: Mutex<Option<MidiOutputLink>>,
    /// Which input to open (persisted) and why the last open failed (transient).
    midi_config: Mutex<MidiConfig>,
    midi_config_path: PathBuf,
    midi_error: Mutex<Option<String>>,
}

/// Which MIDI input the app opens. `None` = auto-detect the reference controller (APC40)
/// by port-name keyword, so plugging it in just works on any OS.
#[derive(Serialize, Deserialize, Clone, Default)]
struct MidiConfig {
    input_port: Option<String>,
}

/// LEGACY app-wide Art-Net target (`network.json`), from before the device manager: one
/// IP for every universe. The output now follows each Show's own `network` (nodes); this
/// is only read to migrate shows that predate it ([`prepare_show_network`]) and is never
/// written any more.
#[derive(Serialize, Deserialize, Clone)]
struct NetworkConfig {
    artnet_enabled: bool,
    artnet_ip: String,
    artnet_port: u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        // Disabled by default → the virtual sink, so a fresh install behaves exactly as
        // before. A subnet broadcast is the safe first target to try against real gear.
        Self {
            artnet_enabled: false,
            artnet_ip: "255.255.255.255".into(),
            artnet_port: 6454,
        }
    }
}

/// The loopback virtual sink: where DMX goes when the show's output is not routed
/// (Art-Net off or no nodes). Routed shows send each universe to its node instead —
/// the engine compiles that from `Show::network` (docs/DEVICES-SPEC.md §5).
fn virtual_sink() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454)
}

/// Bring a loaded Show's network output up to date: one-time migration from the old
/// app-wide Art-Net target (`network.json`) for shows that predate the device manager,
/// then the ranges → one-universe-per-port migration. Idempotent.
///
/// The legacy migration keeps an old show behaving exactly as before: an enabled legacy
/// target becomes one node receiving the patch's universes; a disabled one leaves the
/// show on the virtual sink. A show already initialized (even deliberately without
/// nodes) is never re-migrated.
fn prepare_show_network(show: &mut Show, legacy: &NetworkConfig) {
    let universes = patch_universes(&show.patch);
    let network = &mut show.network;
    if !network.initialized {
        network.initialized = true;
        let ip = legacy.artnet_ip.trim().parse::<Ipv4Addr>();
        network.artnet_enabled = legacy.artnet_enabled && ip.is_ok();
        if let (true, Ok(ip)) = (legacy.artnet_enabled, ip) {
            network.nodes.push(ArtNetNode {
                id: Uuid::new_v4(),
                name: "Destino Art-Net".into(),
                long_name: String::new(),
                ip,
                ports: universes.clone(),
                universes: Vec::new(),
                enabled: true,
                unicast: true,
                full_frames: false,
                origin: NodeOrigin::Manual,
            });
        }
    }
    network.normalize(&universes);
}

/// The patch's universes (raw Art-Net port addresses), sorted and unique.
fn patch_universes(patch: &Patch) -> Vec<u16> {
    let mut u: Vec<u16> = patch
        .fixtures
        .iter()
        .flat_map(|f| f.pixels.iter().map(|p| p.universe))
        .collect();
    u.sort_unstable();
    u.dedup();
    u
}

/// Fold the engine's CURRENT live values (master + per-zone) back into the show model, so
/// a recompile (which rebuilds the Parameter Tree from the show) does not reset the
/// operator's faders/colours. Keyed by zone slot, valid while `show.zones` still matches
/// the engine's zone order (true here — the patch is unchanged).
fn fold_live_into_show(engine: &EngineHandle, show: &mut Show) {
    let m = engine.live().master();
    show.master.dimmer = m.dimmer.read_real().unwrap_or(show.master.dimmer);
    for i in 0..3 {
        show.master.color[i] = m.color[i].read_real().unwrap_or(show.master.color[i]);
    }
    for (slot, zs) in show.zones.iter_mut().enumerate() {
        if slot >= engine.zone_count() {
            break;
        }
        let z = engine.live().zone(slot);
        zs.live.dimmer = z.dimmer.read_real().unwrap_or(zs.live.dimmer);
        for i in 0..3 {
            zs.live.color_filter[i] = z.color[i].read_real().unwrap_or(zs.live.color_filter[i]);
        }
        zs.live.speed_multiplier = z.speed.read_real().unwrap_or(zs.live.speed_multiplier);
        zs.live.solo = z.solo.read_real().map(|v| v > 0.5).unwrap_or(zs.live.solo);
        zs.live.blackout = z
            .blackout
            .read_real()
            .map(|v| v > 0.5)
            .unwrap_or(zs.live.blackout);
    }
    // Tracks, by the slot order the engine was assembled in.
    for (slot, t) in show.tracks.iter_mut().enumerate() {
        if let Some(l) = engine.track_live(slot) {
            t.opacity = l.opacity;
            t.mute = l.mute;
            t.solo = l.solo;
        }
    }
}

/// Bind a live-preview WebSocket on an OS-assigned port and package it as an engine
/// config (adapter ignored on a recompile — the GPU is reused). Returns the config and
/// the bound port, or `None` if the bind failed (the engine then runs without a
/// preview — never fatal).
fn preview_config(adapter: AdapterPreference, target: SocketAddr) -> (EngineConfig, Option<u16>) {
    let mut config = EngineConfig::new(adapter, target);
    config.output_rate_hz = 44;
    config.preview_rate_hz = 30;
    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    match WebSocketSink::bind(loopback) {
        Ok(ws) => {
            let port = ws.local_addr().port();
            config.preview_sink = Some(Box::new(ws));
            (config, Some(port))
        }
        Err(e) => {
            eprintln!("prisma: live preview unavailable (WebSocket bind failed: {e})");
            (config, None)
        }
    }
}

/// Write the library to its file, creating the config directory if needed.
fn persist_library(path: &Path, library: &FixtureLibrary) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = save_library(library).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// The editor snapshot the UI renders after any command: the patch plus the
/// history/dirty flags (toolbar state) and the patch's physical DMX **wire faults**.
/// Faults are surfaced, never enforced — the editor marks them on the canvas and shows
/// a discreet notice, it does not block edits (§1, §4.3). `diagnostics` carries each
/// fault (an address collision or an out-of-range channel) annotated with the pixels it
/// points at, so a duplicated channel among thousands of pixels cannot hide. Out-of-
/// matrix positions and shared cells are valid choices and are not reported (§2.4/§4.4).
#[derive(Serialize)]
struct EditorState {
    patch: Patch,
    can_undo: bool,
    can_redo: bool,
    dirty: bool,
    diagnostics: Vec<Diagnostic>,
}

fn state_of(draft: &PatchDraft) -> EditorState {
    EditorState {
        patch: draft.patch().clone(),
        can_undo: draft.can_undo(),
        can_redo: draft.can_redo(),
        dirty: draft.is_dirty(),
        diagnostics: draft.wire_faults(),
    }
}

// --- IPC command surface ---

#[tauri::command]
fn editor_get_state(state: State<AppState>) -> EditorState {
    state_of(&state.draft.lock().unwrap())
}

/// Aggregate properties of a fixture selection (§5.4), or `null` if empty.
#[tauri::command]
fn editor_selection_aggregate(
    state: State<AppState>,
    ids: Vec<Uuid>,
) -> Option<SelectionAggregate> {
    aggregate(state.draft.lock().unwrap().patch(), &ids)
}

/// The fixture types the placement dialog offers — the operator's library (§3.2).
#[tauri::command]
fn editor_fixture_types(state: State<AppState>) -> Vec<FixtureType> {
    state.library.lock().unwrap().types.clone()
}

/// The whole fixture library, for the manager UI (§3.2).
#[tauri::command]
fn editor_library_get(state: State<AppState>) -> FixtureLibrary {
    state.library.lock().unwrap().clone()
}

/// Add a fixture type to the library; persist and return the updated library.
#[tauri::command]
fn editor_library_add_type(
    state: State<AppState>,
    ty: FixtureType,
) -> Result<FixtureLibrary, String> {
    let mut lib = state.library.lock().unwrap();
    lib.add_type(ty);
    persist_library(&state.library_path, &lib)?;
    Ok(lib.clone())
}

/// Replace the fixture type at `index`; persist and return the updated library.
#[tauri::command]
fn editor_library_update_type(
    state: State<AppState>,
    index: usize,
    ty: FixtureType,
) -> Result<FixtureLibrary, String> {
    let mut lib = state.library.lock().unwrap();
    lib.update_type(index, ty).map_err(|e| e.to_string())?;
    persist_library(&state.library_path, &lib)?;
    Ok(lib.clone())
}

/// Remove the fixture type at `index`; persist and return the updated library.
#[tauri::command]
fn editor_library_remove_type(
    state: State<AppState>,
    index: usize,
) -> Result<FixtureLibrary, String> {
    let mut lib = state.library.lock().unwrap();
    lib.remove_type(index).map_err(|e| e.to_string())?;
    persist_library(&state.library_path, &lib)?;
    Ok(lib.clone())
}

/// Suggest the next free DMX address, to prefill the placement dialog (§4.3).
#[tauri::command]
fn editor_next_free_address(state: State<AppState>) -> [u16; 2] {
    let (u, c) = next_free_address(state.draft.lock().unwrap().patch());
    [u, c]
}

/// The next free fixture ID, to prefill "Iniciar ID de instalación".
#[tauri::command]
fn editor_next_fixture_id(state: State<AppState>) -> u32 {
    next_fixture_id(state.draft.lock().unwrap().patch())
}

/// A placement error in the operator's words (the dialog shows it verbatim).
fn placement_error_text(e: &PlacementError) -> String {
    match e {
        PlacementError::Empty => "No hay nada que añadir (cuenta o píxeles en cero).".into(),
        PlacementError::FixtureTooBig { span } => {
            format!("La instalación necesita {span} canales: no entra en un universo (512).")
        }
        PlacementError::StartChannelTooHigh { start, span } => format!(
            "La instalación necesita {span} canales y no entra desde el canal {start} hasta el final del universo."
        ),
        PlacementError::OutOfUniverses => "Se acabaron los universos DMX.".into(),
    }
}

/// Place fixtures from the dialog spec (§4). They go into `spec.zone` — the active
/// zone (§4.1/§6). Appends via an undoable `InsertFixtures` op.
#[tauri::command]
fn editor_place_fixtures(
    state: State<AppState>,
    spec: PlacementSpec,
) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    let fixtures = plan(&spec).map_err(|e| placement_error_text(&e))?;
    let base = draft.patch().fixtures.len();
    let at = fixtures
        .into_iter()
        .enumerate()
        .map(|(i, f)| (base + i, f))
        .collect();
    draft
        .apply(EditOp::InsertFixtures { at }, None)
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Live preview of the signal path a placement would produce (§4.1) — no mutation.
#[tauri::command]
fn editor_preview_placement(spec: PlacementSpec) -> Vec<PreviewRun> {
    plan_preview(&spec)
}

/// Translate fixtures by a cell delta — the canvas drag-move (§5.5). One undoable
/// op for the whole selection, so a drag is a single undo step.
#[tauri::command]
fn editor_translate_fixtures(
    state: State<AppState>,
    ids: Vec<Uuid>,
    delta: [f32; 3],
) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    if ids.is_empty() {
        return Ok(state_of(&draft));
    }
    draft
        .apply(
            EditOp::TranslateFixtures {
                ids,
                delta: Vec3::from(delta),
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Set a zone's mapping-matrix size — its effect render resolution (§2.4/§2.5). The
/// dialog edits X and Y; Z is preserved (it stays 1 while the editor is 2-D). Values are
/// clamped to `1..=512` (the workspace ceiling, §2.5) so no caller can set a zero axis
/// (which the model rejects) or an oversized matrix. Undoable.
#[tauri::command]
fn editor_set_zone_mapping_size(
    state: State<AppState>,
    zone: Uuid,
    size: [u32; 2],
) -> Result<EditorState, String> {
    const MAX_MATRIX: u32 = 512;
    let mut draft = state.draft.lock().unwrap();
    let z = draft
        .patch()
        .zones
        .iter()
        .find(|z| z.id == zone)
        .map(|z| z.mapping_size[2])
        .unwrap_or(1);
    let clamp = |v: u32| v.clamp(1, MAX_MATRIX);
    let size3 = [clamp(size[0]), clamp(size[1]), z];
    draft
        .apply(EditOp::SetZoneMappingSize { zone, size: size3 }, None)
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// A grey palette to seed a new zone's display colour, cycled by index so successive
/// zones are visually distinct without asking the operator to pick a colour up front.
const ZONE_COLORS: [[u8; 3]; 6] = [
    [69, 123, 157],  // blue
    [42, 157, 143],  // teal
    [233, 196, 106], // gold
    [142, 125, 190], // violet
    [231, 111, 81],  // coral
    [138, 177, 125], // sage
];

/// Add a new zone to the patch (undoable). The zone starts empty (no fixtures) with a
/// modest default matrix and a palette colour; fixtures are assigned to it afterward via
/// the canvas / active-zone placement. Returns the updated editor state.
#[tauri::command]
fn editor_add_zone(state: State<AppState>, name: String) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    // Zone names must be unique (case-insensitive, trimmed). An empty name auto-generates
    // a unique "Zona N" (picking the first free N, not `len()+1` which collided after a
    // delete); a provided name that already exists is rejected so two zones never share a
    // name and get confused for one another.
    let existing: Vec<String> = draft
        .patch()
        .zones
        .iter()
        .map(|z| z.name.trim().to_lowercase())
        .collect();
    let name = {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            let mut n = existing.len() + 1;
            loop {
                let cand = format!("Zona {n}");
                if !existing.contains(&cand.to_lowercase()) {
                    break cand;
                }
                n += 1;
            }
        } else if existing.contains(&trimmed.to_lowercase()) {
            return Err(format!("Ya existe una zona llamada «{trimmed}»."));
        } else {
            trimmed.to_string()
        }
    };
    let at = draft.patch().zones.len();
    let zone = Zone {
        id: Uuid::new_v4(),
        name,
        members: vec![],
        bounds: None,
        z_order: 0,
        blend_mode: BlendMode::Normal,
        mapping_size: [16, 16, 1],
        display_color: ZONE_COLORS[at % ZONE_COLORS.len()],
        world_transform: None,
    };
    draft
        .apply(EditOp::AddZone { at, zone }, None)
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Rename a zone (undoable). The name must be non-empty and unique (case-insensitive),
/// excluding the zone itself, so two zones never share a name.
#[tauri::command]
fn editor_rename_zone(
    state: State<AppState>,
    zone: Uuid,
    name: String,
) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("El nombre no puede estar vacío.".into());
    }
    let clash = draft
        .patch()
        .zones
        .iter()
        .any(|z| z.id != zone && z.name.trim().to_lowercase() == trimmed.to_lowercase());
    if clash {
        return Err(format!("Ya existe una zona llamada «{trimmed}»."));
    }
    draft
        .apply(
            EditOp::RenameZone {
                zone,
                name: trimmed.to_string(),
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Remove a zone (undoable). Refused if fixtures still belong to it — the error names
/// how many, so the UI can tell the operator to reassign or delete them first.
#[tauri::command]
fn editor_remove_zone(state: State<AppState>, zone: Uuid) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    draft
        .apply(EditOp::RemoveZone { zone }, None)
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Highlight fixtures on the live output to identify them on the rig (§7 / S9). `blink`
/// chooses blink vs. solid. An empty selection clears the highlight. Writes to the live
/// engine, not the draft — this is the editor↔engine seam that makes Highlight possible.
#[tauri::command]
fn editor_highlight(state: State<AppState>, ids: Vec<Uuid>, blink: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_mut() {
        engine.highlight(&ids, blink);
    }
}

/// The TCP port the live-preview WebSocket is bound to (§M6/S10). The React preview
/// connects to `ws://127.0.0.1:{port}` and re-queries on reconnect (the port changes
/// when a recompile rebinds a fresh one).
#[tauri::command]
fn editor_preview_port(state: State<AppState>) -> u16 {
    *state.preview_port.lock().unwrap()
}

/// Apply the editor's current draft to the **running show** (§7, M8 hot patch editing):
/// adopt the edited patch into the live show — keeping each zone's live state and the
/// tracks, keyed by zone id — and recompile on the same GPU so the show resumes on the new layout.
/// `resume` unfreezes (the editor window closed); `resume == false` keeps it frozen
/// (an explicit "Apply to live" while the editor is still open, §7).
///
/// Because recompile rebuilds the Parameter Tree, live values reset to the show's saved
/// state (a known limitation; live tweaks made since load are not preserved). Zone
/// **add/remove** IS supported: the show's zones and tracks are reconciled to the new
/// patch's zone set (a new zone gets its own track, a removed zone leaves every
/// destination, and a track left reaching nothing is dropped), and a
/// `layout-changed` event tells the main window to re-fetch the (possibly changed) zones.
fn apply_editor_draft(app: &AppHandle, resume: bool) -> Result<(), String> {
    let state = app.state::<AppState>();

    // Build the candidate patch and VALIDATE it BEFORE touching the running engine or
    // the show. `recompile_show` tears the old engine down *before* it assembles the new
    // one, so a patch that fails to compile would leave the app with no engine at all — a
    // dead show. Compiling here first means an invalid edit is rejected with the live
    // engine untouched (it keeps running); only a genuine post-validation assembly fault
    // (e.g. GPU) can now reach the teardown path. The render path needs Zone::members
    // (the editor only tracks fixture→zone ownership), so derive them first.
    let mut patch = state.draft.lock().unwrap().patch().clone();
    patch.fill_zone_members_from_ownership();
    patch.fit_matrix_size(); // world stays derived from the rig on every apply-to-live
    patch.compile().map_err(|e| e.to_string())?;

    // Adopt the edited patch into the running show.
    let show = {
        let mut show = state.show.lock().unwrap();
        // Preserve live state across the recompile (which rebuilds the Parameter Tree):
        // fold the engine's CURRENT live values back into the show model, keyed by the
        // OLD zone order (still matching the engine + `show.zones` here, before the
        // reconcile). The reconcile keeps these by zone id, and `recompile_show` re-seeds
        // from them — so auto-applying an edit does not reset the operator's master /
        // colour / dimmers.
        if let Some(engine) = state.engine.lock().unwrap().as_ref() {
            fold_live_into_show(engine, &mut show);
        }
        // Reconcile to the new zone set: one ZoneShow per patch zone, in patch order,
        // keeping an existing zone's live state by id, a default one for a newly added
        // zone, and dropping a removed zone's. This keeps `Show` a bijection with its
        // patch (what the engine and `Show::validate` expect). Tracks follow below.
        let mut old = std::mem::take(&mut show.zones);
        let old_zone_ids: Vec<Uuid> = old.iter().map(|zs| zs.zone_id).collect();
        show.zones = patch
            .zones
            .iter()
            .map(|pz| match old.iter().position(|zs| zs.zone_id == pz.id) {
                Some(pos) => old.remove(pos),
                // A newly added zone; its own track is added below.
                None => {
                    let mut zs = ZoneShow::new(pz.id);
                    // A freshly created zone starts LIT (dimmer full) — master defaults to
                    // full too, so the operator sees the zone's content the moment they
                    // assign an effect, instead of a black zone that looks broken.
                    zs.live.dimmer = 1.0;
                    zs
                }
            })
            .collect();
        // Tracks: a removed zone leaves every destination; a track left reaching
        // nothing is dropped (as a removed zone's grid used to be).
        for track in &mut show.tracks {
            track
                .destination
                .zones
                .retain(|z| patch.zones.iter().any(|pz| pz.id == *z));
        }
        show.tracks.retain(|t| !t.destination.is_empty());
        // A newly added zone gets its own track, like `Show::new` gives every zone.
        for pz in &patch.zones {
            if !old_zone_ids.contains(&pz.id) {
                show.tracks.push(Track::new(
                    pz.name.clone(),
                    prisma_engine::Destination::zone(pz.id),
                ));
            }
        }
        show.patch = patch;
        pad_grids(&mut show);
        show.clone()
    };
    // A FRESH preview WebSocket on a new port: port 0 never collides with the still-
    // running old engine's sink, and recompile drops the old one on teardown. The
    // viewer reconnects to the new port (re-queried on reconnect).
    let target = virtual_sink();
    let (config, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    let bpm = engine.bpm();
    match engine.recompile_show(show, config) {
        Ok(new) => {
            new.set_frozen(!resume);
            new.set_bpm(bpm);
            state.armed.lock().unwrap().clear();
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
            drop(guard);
            // Tell the main window to re-fetch zones/grids — the zone set, names or
            // fixtures may have changed.
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Apply the current editor draft to the live show without closing the editor (§7):
/// an explicit "Apply to live" that keeps content frozen while the editor stays open.
#[tauri::command]
fn editor_apply_to_live(app: AppHandle) -> Result<(), String> {
    apply_editor_draft(&app, false)
}

/// The show as it would be saved right now: the document with the engine's live values
/// folded in, serialized like a `.prisma`. Never holds the show and engine locks
/// together (the show is cloned first).
fn show_snapshot(state: &AppState) -> Result<String, String> {
    let mut show = state.show.lock().unwrap().clone();
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        fold_live_into_show(engine, &mut show);
    }
    save_show(&show).map_err(|e| e.to_string())
}

/// The operator saved or opened `snapshot`: it is the clean state the autosave compares
/// against from now on.
fn mark_saved(state: &AppState, snapshot: String) {
    let mut g = state.autosave_state.lock().unwrap();
    let (baseline, last) = &mut *g;
    autosave::mark_clean(baseline, last, snapshot);
}

/// One autosave step: snapshot, write or clear the autosave, and mark the window title
/// when there are unsaved changes. Run by the autosave thread and once on exit.
fn autosave_tick(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Ok(snap) = show_snapshot(&state) else {
        return;
    };
    let (step, dirty) = {
        let mut g = state.autosave_state.lock().unwrap();
        let (baseline, last) = &mut *g;
        autosave::step(baseline, last, snap.clone())
    };
    match step {
        autosave::Step::Idle => {}
        autosave::Step::Write => {
            if let Err(e) = state.autosave.write(&snap) {
                eprintln!("prisma: autosave failed: {e}");
            }
        }
        autosave::Step::Clear => state.autosave.clear(),
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_title(if dirty { "Prisma •" } else { "Prisma" });
    }
}

/// The session was recovered from an autosave at boot.
#[derive(Serialize, Clone)]
struct RecoveryInfo {
    /// The last saved show, if there is one to fall back to.
    last_saved: Option<String>,
}

/// Taken once by the main window at boot: `Some` if this session was recovered.
#[tauri::command]
fn main_recovery_info(state: State<AppState>) -> Option<RecoveryInfo> {
    state.recovered.lock().unwrap().take()
}

/// Save the current live show to a `.prisma` file (M8-SPEC §5): serialize the Show — with
/// its embedded Patch — through the versioned envelope. The frontend picks the path with
/// the native dialog.
#[tauri::command]
fn main_save_show(state: State<AppState>, path: String) -> Result<(), String> {
    // The saved file carries the live values (faders, opacities) as they are now.
    let json = show_snapshot(&state)?;
    std::fs::write(&path, &json).map_err(|e| e.to_string())?;
    mark_saved(&state, json);
    // Remember it so the next launch reopens this show.
    write_last_show(&state.last_show_file, Some(Path::new(&path)));
    Ok(())
}

/// "Nueva configuración": reset to the default state — exactly as a from-zero launch (the
/// default 50×50 zone with its Color cell). The last-show memory is cleared, so the next
/// launch also starts empty until a show is opened or saved. Validated before the engine is
/// torn down, so a failure leaves the running show intact. The main window re-fetches on
/// `layout-changed`.
#[tauri::command]
fn main_new_show(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let show = empty_show();
    let target = virtual_sink();
    let (config, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    match engine.recompile_show(show.clone(), config) {
        Ok(new) => {
            new.set_frozen(false);
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
            drop(guard);
            *state.draft.lock().unwrap() = PatchDraft::new(show.patch.clone());
            *state.show.lock().unwrap() = show;
            watch_show_nodes(&state);
            if let Some(engine) = state.engine.lock().unwrap().as_ref() {
                refresh_all_cells(engine);
            }
            write_last_show(&state.last_show_file, None);
            if let Ok(snap) = show_snapshot(&state) {
                mark_saved(&state, snap);
            }
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Open a `.prisma` show and run it: load the Show, recompile the engine onto its embedded
/// Patch, and adopt it as the live + editable state. Like [`apply_editor_draft`], the patch
/// is validated (compile) BEFORE the engine is torn down, so a bad file is rejected with the
/// running show intact rather than leaving the app with no engine. The main window re-fetches
/// on the `layout-changed` event.

#[tauri::command]
fn main_open_show(app: AppHandle, path: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut show = load_show(&json).map_err(|e| e.to_string())?;
    // Derive the render-critical data and validate before touching the running engine.
    show.patch.fill_zone_members_from_ownership();
    show.patch.fit_matrix_size();
    show.patch.compile().map_err(|e| e.to_string())?;
    pad_grids(&mut show);
    prepare_show_network(&mut show, &state.network.lock().unwrap());

    let target = virtual_sink();
    let (config, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    match engine.recompile_show(show.clone(), config) {
        Ok(new) => {
            new.set_frozen(false); // a freshly opened show plays
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
            drop(guard);
            // Adopt the loaded show + its patch as the editable state (so the editor, if
            // opened next, shows this rig), then tell the main window to re-fetch.
            *state.draft.lock().unwrap() = PatchDraft::new(show.patch.clone());
            *state.show.lock().unwrap() = show;
            watch_show_nodes(&state);
            if let Some(engine) = state.engine.lock().unwrap().as_ref() {
                refresh_all_cells(engine);
            }
            // Remember it so the next launch reopens this show.
            write_last_show(&state.last_show_file, Some(Path::new(&path)));
            if let Ok(snap) = show_snapshot(&state) {
                mark_saved(&state, snap);
            }
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Delete the given fixtures (undoable).
#[tauri::command]
fn editor_remove_fixtures(state: State<AppState>, ids: Vec<Uuid>) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    if ids.is_empty() {
        return Ok(state_of(&draft));
    }
    draft
        .apply(EditOp::RemoveFixtures { ids }, None)
        .map_err(|e| e.to_string())?;
    Ok(state_of(&draft))
}

/// Save the current mapping to a `.prismap` file (§9). The frontend picks the path
/// with the native dialog; here we serialize through the M2 envelope (which carries
/// EVERY field — zones' mapping_size/world_transform/colour, fixture ownership) and
/// write it, then clear the dirty flag.
#[tauri::command]
fn editor_save_to(state: State<AppState>, path: String) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    let json = save_patch(draft.patch()).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    draft.mark_saved();
    Ok(state_of(&draft))
}

/// Open a `.prismap` into the editor (§9). Migrates an older schema on the way in
/// (M2 migration chain). Replaces the draft with a fresh, clean history.
///
/// Note: this loads into the *editor draft* only; the live engine keeps running the
/// patch it started with until a recompile (Highlight/close, S9).
#[tauri::command]
fn editor_open_from(state: State<AppState>, path: String) -> Result<EditorState, String> {
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let patch = load_patch(&json).map_err(|e| e.to_string())?;
    let mut draft = state.draft.lock().unwrap();
    *draft = PatchDraft::new(patch);
    Ok(state_of(&draft))
}

// --- Main operation window (M8 step 3): the zone row ---

/// The slot (index in `Patch::zones`) of a zone id, or `None` if unknown. Derived from
/// the running show's embedded patch — the single source of truth for the zone set, so
/// after a hot patch edit (add/remove zone) the slots the engine's `LiveParams` use and
/// the app's addressing stay in lockstep with no separate list to keep in sync.
/// The app's side of clip playback: the engine's clip controller plays clips from the
/// Parameter Tree (pads, clicks — every transport the same way) and asks here what a
/// clip holds and how a track transitions; it reports back which clip each track is
/// running or has armed, which this folds into the app state and announces to the UI
/// (`cells-changed`, the track id).
///
/// Lock discipline: it only ever takes the `show` lock, never `engine`, so an engine
/// recompile (which holds `engine` while it stops the controller) cannot deadlock on it.
struct AppCellHost {
    app: AppHandle,
}

impl ClipHost for AppCellHost {
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect> {
        let state = self.app.try_state::<AppState>()?;
        let show = state.show.lock().unwrap();
        let cell = show.track(track)?.clips.get(clip)?;
        let layer = cell.layers.first()?;
        match &layer.source {
            Source::Shader {
                effect_id,
                params,
                palette,
            } => Some(ClipEffect {
                effect_id: effect_id.clone(),
                params: params.clone(),
                palette: palette.clone(),
                fade_time: cell.fade_time,
            }),
        }
    }

    fn track_behaviour(&self, track: Uuid) -> Option<TrackBehaviour> {
        let state = self.app.try_state::<AppState>()?;
        let show = state.show.lock().unwrap();
        let t = show.track(track)?;
        Some(TrackBehaviour {
            transition: t.transition,
            launch: t.launch,
        })
    }

    fn armed_changed(&self, track: Uuid, armed: Option<usize>) {
        if let Some(state) = self.app.try_state::<AppState>() {
            let mut map = state.armed.lock().unwrap();
            match armed {
                Some(clip) => map.insert(track, clip),
                None => map.remove(&track),
            };
        }
        let _ = self.app.emit("cells-changed", track);
    }

    fn active_changed(&self, track: Uuid, active: Option<usize>) {
        if let Some(state) = self.app.try_state::<AppState>() {
            if let Some(t) = state.show.lock().unwrap().track_mut(track) {
                t.active = active;
            }
        }
        let _ = self.app.emit("cells-changed", track);
    }
}

/// Re-publish every track's clip states — after the show document was swapped under a
/// running engine (open / new), whose controller started against the previous one.
fn refresh_all_cells(engine: &EngineHandle) {
    for slot in 0..engine.track_count() {
        engine.refresh_clips(slot);
    }
}

fn zone_slot(state: &AppState, id: Uuid) -> Option<usize> {
    state
        .show
        .lock()
        .unwrap()
        .patch
        .zones
        .iter()
        .position(|z| z.id == id)
}

/// Every zone with its identity and current live controls, for the zone row. The
/// dimmer/solo/blackout are read live from the engine so the cards reflect state set
/// by any transport (or the loaded show), not just the UI's own writes.
#[tauri::command]
fn main_zones(state: State<AppState>) -> Vec<ZoneInfo> {
    // Identity comes from the show's embedded patch (the single source of truth, so a
    // hot patch edit is reflected here); the live controls come from the engine. Collect
    // identity first, then drop the show lock before taking the engine lock.
    let metas: Vec<(Uuid, String, [u8; 3])> = {
        let show = state.show.lock().unwrap();
        show.patch
            .zones
            .iter()
            .map(|z| (z.id, z.name.clone(), z.display_color))
            .collect()
    };
    let guard = state.engine.lock().unwrap();
    metas
        .into_iter()
        .enumerate()
        .map(|(slot, (id, name, color))| {
            let live = guard
                .as_ref()
                .map(|e| e.zone_live(slot))
                .unwrap_or(ZoneLiveReadout {
                    dimmer: 0.0,
                    speed: 1.0,
                    solo: false,
                    blackout: false,
                    strobe_on: false,
                    strobe_rate: 0.4,
                });
            ZoneInfo {
                id,
                name,
                color,
                dimmer: live.dimmer,
                speed: live.speed,
                solo: live.solo,
                blackout: live.blackout,
                strobe_on: live.strobe_on,
                strobe_rate: live.strobe_rate,
            }
        })
        .collect()
}

/// Set a zone's dimmer (the zone-row fader) — a live control that changes the output,
/// not the UI's selection. `selected ≠ active`: this is the "active" side.
#[tauri::command]
fn main_set_zone_dimmer(state: State<AppState>, zone: Uuid, value: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_dimmer(slot, value);
    }
}

/// Set a zone's velocity multiplier (the zone-card speed fader), real `-10..=10`:
/// 0 = static, negative = reverse. Written live; the render integrates it per frame.
#[tauri::command]
fn main_set_zone_speed(state: State<AppState>, zone: Uuid, value: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_speed(slot, value);
    }
}

/// Toggle a zone's solo (M8-SPEC §3.1).
#[tauri::command]
fn main_set_zone_solo(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_solo(slot, on);
    }
}

/// Toggle a zone's blackout/mute (M8-SPEC §3.1).
#[tauri::command]
fn main_set_zone_blackout(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_blackout(slot, on);
    }
}

/// Engage/release a zone's strobe (M8-SPEC §3.6; a tree toggle since M9, so a pad can
/// drive it too). Gates the zone's non-overlapped pixels at the output stage.
#[tauri::command]
fn main_set_zone_strobe_on(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_strobe_on(slot, on);
    }
}

/// Set a zone's strobe speed in Hz (1–25), engaged or not.
#[tauri::command]
fn main_set_zone_strobe_rate(state: State<AppState>, zone: Uuid, rate: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_strobe_rate(slot, rate);
    }
}

// --- Device manager (network output) ---

/// Edit the running show and recompile the engine onto it (reusing the GPU). Live values
/// are folded into the show FIRST — by the engine's current zone/track order, before the
/// edit can add or remove a track and shift the slots — so the recompile does not reset
/// faders; the tempo and freeze carry over; the preview WebSocket moves to a fresh port,
/// so the UI is nudged to re-fetch.
fn recompile_running_show(app: &AppHandle, edit: impl FnOnce(&mut Show)) -> Result<(), String> {
    let state = app.state::<AppState>();
    let show = {
        let mut show = state.show.lock().unwrap();
        if let Some(engine) = state.engine.lock().unwrap().as_ref() {
            fold_live_into_show(engine, &mut show);
        }
        edit(&mut show);
        pad_grids(&mut show);
        show.clone()
    };
    let (cfg, port) = preview_config(AdapterPreference::Software, virtual_sink());
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    let (bpm, frozen) = (engine.bpm(), engine.is_frozen());
    match engine.recompile_show(show, cfg) {
        Ok(new) => {
            new.set_frozen(frozen);
            new.set_bpm(bpm);
            state.armed.lock().unwrap().clear();
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
            drop(guard);
            watch_show_nodes(&state);
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The running show's whole network output config (nodes, interface, enable, ArtSync),
/// for the Device Manager. Universe numbers are raw port addresses (the UI shows +1).
#[tauri::command]
fn main_output_get(state: State<AppState>) -> ShowNetwork {
    state.show.lock().unwrap().network.clone()
}

/// Apply the Device Manager's staged config (Aplicar / Aceptar) to the running show and
/// recompile the output onto it. Validated first; a bad config changes nothing.
#[tauri::command]
fn main_output_set(app: AppHandle, network: ShowNetwork) -> Result<(), String> {
    network.validate().map_err(|e| e.to_string())?;
    recompile_running_show(&app, |show| {
        show.network = network;
        show.network.initialized = true;
    })
}

/// This machine's host name, shown in the Device Manager like professional consoles ("Nombre del
/// host"). From the OS environment / kernel; empty if unknown.
#[tauri::command]
fn main_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        })
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

/// Open a node's web configuration page (the classic "HTTP" button) in the system browser.
/// Only `http://<IPv4>` is accepted — the command never opens arbitrary URLs.
#[tauri::command]
fn main_open_node_page(ip: String) -> Result<(), String> {
    let ip: Ipv4Addr = ip
        .trim()
        .parse()
        .map_err(|_| format!("IP inválida: {ip}"))?;
    let url = format!("http://{ip}/");
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&url).spawn();
    spawned.map(|_| ()).map_err(|e| e.to_string())
}

/// A host network interface, for the Device Manager's interface dropdown.
#[derive(Serialize)]
struct InterfaceInfo {
    name: String,
    address: String,
    prefix_len: u8,
}

impl From<&HostInterface> for InterfaceInfo {
    fn from(h: &HostInterface) -> Self {
        Self {
            name: h.name.clone(),
            address: h.address.to_string(),
            prefix_len: h.prefix_len,
        }
    }
}

/// This machine's IPv4 interfaces (loopback excluded), for choosing the show's output
/// interface (docs/DEVICES-SPEC.md §6). Setup path only — never the output thread.
#[tauri::command]
fn main_network_interfaces() -> Vec<InterfaceInfo> {
    host_interfaces().iter().map(InterfaceInfo::from).collect()
}

/// How the running show's output resolved: the interface (or why none), and the patch
/// universes no enabled node receives — the Device Manager's warnings (§9).
#[derive(Serialize)]
struct OutputStatusInfo {
    /// "unset" (OS routes), "resolved" or "missing" (the show's network is not here).
    interface_state: &'static str,
    /// The resolved host interface, or the show's wanted network when missing.
    interface: Option<InterfaceInfo>,
    /// Uncovered universes, as raw Art-Net port addresses (the UI shows them +1).
    uncovered: Vec<u16>,
}

#[tauri::command]
fn main_output_status(state: State<AppState>) -> Result<OutputStatusInfo, String> {
    let guard = state.engine.lock().unwrap();
    let engine = guard.as_ref().ok_or("engine is not running")?;
    let routing = engine.output_routing();
    let (interface_state, interface) = match &routing.interface {
        InterfaceResolution::Unset => ("unset", None),
        InterfaceResolution::Resolved(h) => ("resolved", Some(InterfaceInfo::from(h))),
        InterfaceResolution::Missing(r) => (
            "missing",
            Some(InterfaceInfo {
                name: r.name_hint.clone(),
                address: r.address.to_string(),
                prefix_len: r.prefix_len,
            }),
        ),
    };
    Ok(OutputStatusInfo {
        interface_state,
        interface,
        uncovered: routing.uncovered.clone(),
    })
}

/// One device row found by a Scan (one per `BindIndex` group, like professional consoles).
#[derive(Serialize)]
struct ScannedNodeInfo {
    ip: String,
    short_name: String,
    long_name: String,
    mac: String,
    bind_index: u8,
    /// The universe (raw Art-Net port address) of each output port.
    ports: Vec<u16>,
    esta: u16,
    oem: u16,
    firmware: u16,
}

impl From<&DiscoveredNode> for ScannedNodeInfo {
    fn from(n: &DiscoveredNode) -> Self {
        Self {
            ip: n.ip.to_string(),
            short_name: n.short_name.clone(),
            long_name: n.long_name.clone(),
            mac: n.mac_string(),
            bind_index: n.bind_index,
            ports: n.ports.clone(),
            esta: n.esta,
            oem: n.oem,
            firmware: n.firmware,
        }
    }
}

/// Start the Art-Net node monitor if it is not running, and return it. Its status
/// changes reach the UI as `artnet-node-status` events (`[{ip, status}]`).
fn ensure_artnet_monitor(app: &AppHandle) -> Result<Arc<NodeMonitor>, String> {
    let state = app.state::<AppState>();
    let mut slot = state.artnet_monitor.lock().unwrap();
    if let Some(m) = slot.as_ref() {
        return Ok(Arc::clone(m));
    }
    let emitter = app.clone();
    let monitor = NodeMonitor::start(Box::new(move |changed| {
        let payload: Vec<NodeStatusInfo> = changed
            .iter()
            .map(|&(ip, st)| NodeStatusInfo::new(ip, st))
            .collect();
        let _ = emitter.emit("artnet-node-status", payload);
    }))
    .map_err(|e| match e {
        ScanError::PortBusy(_) => "El puerto UDP 6454 está ocupado por otro programa \
             (¿otro programa de iluminación abierto?). Podés agregar los nodos a mano."
            .to_string(),
        other => other.to_string(),
    })?;
    let monitor = Arc::new(monitor);
    *slot = Some(Arc::clone(&monitor));
    drop(slot);
    watch_show_nodes(&state);
    Ok(monitor)
}

/// Point the monitor at the running show's nodes (their status is what the UI shows).
fn watch_show_nodes(state: &AppState) {
    let ips: Vec<Ipv4Addr> = state
        .show
        .lock()
        .unwrap()
        .network
        .nodes
        .iter()
        .map(|n| n.ip)
        .collect();
    if let Some(m) = state.artnet_monitor.lock().unwrap().as_ref() {
        m.set_watched(ips);
    }
}

/// A node's reachability for the UI: "online" (responde), "silent" (sin respuesta —
/// neutral, some devices never answer ArtPoll) or "lost" (dejó de responder — alarm).
#[derive(Serialize, Clone)]
struct NodeStatusInfo {
    ip: String,
    status: &'static str,
}

impl NodeStatusInfo {
    fn new(ip: Ipv4Addr, status: NodeStatus) -> Self {
        Self {
            ip: ip.to_string(),
            status: match status {
                NodeStatus::Online => "online",
                NodeStatus::Silent => "silent",
                NodeStatus::Lost => "lost",
            },
        }
    }
}

/// Current status of every node of the running show (initial fill for the Device
/// Manager; later changes arrive as `artnet-node-status` events). Empty if the monitor
/// is not running (port held).
#[tauri::command]
fn main_node_statuses(state: State<AppState>) -> Vec<NodeStatusInfo> {
    let Some(monitor) = state.artnet_monitor.lock().unwrap().clone() else {
        return Vec::new();
    };
    let ips: Vec<Ipv4Addr> = state
        .show
        .lock()
        .unwrap()
        .network
        .nodes
        .iter()
        .map(|n| n.ip)
        .collect();
    ips.into_iter()
        .map(|ip| NodeStatusInfo::new(ip, monitor.status(ip)))
        .collect()
}

/// How long a Scan listens for replies (spec §7: ~3 s, with a re-poll halfway).
const SCAN_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// Scan the network for Art-Net nodes (ArtPoll → ArtPollReply), like the classic Scan,
/// through the node monitor (the one owner of UDP 6454). It polls every host
/// interface's subnet broadcast plus 255.255.255.255, so a board on a link-local
/// network (the H802RA at 169.254.x) is found without picking an interface first. Runs off the UI/event thread. The raw replies are also written to
/// `artnet-scan.json` in the app-config dir — real device bytes for the parser tests.
#[tauri::command]
async fn main_artnet_scan(app: AppHandle) -> Result<Vec<ScannedNodeInfo>, String> {
    let monitor = ensure_artnet_monitor(&app)?;
    let nodes = tauri::async_runtime::spawn_blocking(move || monitor.scan(SCAN_WINDOW))
        .await
        .map_err(|e| e.to_string())?;
    if let Ok(dir) = app.path().app_config_dir() {
        let dump: Vec<serde_json::Value> = nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "ip": n.ip.to_string(),
                    "short_name": n.short_name,
                    "bind_index": n.bind_index,
                    "reply_hex": n.raw_reply.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                })
            })
            .collect();
        let _ = persist_text(
            &dir.join("artnet-scan.json"),
            &serde_json::to_string_pretty(&dump).unwrap_or_default(),
        );
    }
    Ok(nodes.iter().map(ScannedNodeInfo::from).collect())
}

// --- MIDI (M9): device I/O, MIDI Learn, LED feedback ---

/// Write a JSON-able config next to the others, creating the config directory if needed.
fn persist_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// (Re)open the configured MIDI input, closing the previous link first. A missing device
/// is not an error worth shouting about in auto mode; it is recorded for the device
/// manager to show.
///
/// The output (LEDs) is the port with the input's name — how ALSA, CoreMUSIC and WinMM
/// all name the two halves of one USB device — falling back to the keywords. A device
/// without an output simply gets no feedback. Its device profile (LED semantics) is
/// picked from the port name.
fn connect_midi(state: &AppState) {
    let mut link = state.midi_link.lock().unwrap();
    let mut out_link = state.midi_out_link.lock().unwrap();
    state.midi.set_output(None);
    *link = None;
    *out_link = None;
    let port = state.midi_config.lock().unwrap().input_port.clone();
    let opened = match &port {
        Some(name) => MidiInputLink::open(state.midi.clone(), name),
        None => MidiInputLink::open_by_keywords(state.midi.clone(), APC40_KEYWORDS),
    };
    *state.midi_error.lock().unwrap() = match opened {
        Ok(l) => {
            let name = l.port_name().to_string();
            *link = Some(l);
            state.midi.set_device(DeviceProfile::for_port(&name));
            *out_link = MidiOutputLink::open(&name)
                .or_else(|_| MidiOutputLink::open_by_keywords(APC40_KEYWORDS))
                .ok();
            if let Some(out) = out_link.as_ref() {
                state.midi.set_output(Some(out.sink()));
            }
            None
        }
        Err(MidiIoError::NotFound) if port.is_none() => None,
        Err(e) => Some(e.to_string()),
    };
}

/// Forward runtime notices to the UI. Learn persists the profile and emits `midi-learned`
/// at once. `Applied` (a mapped control moved) arrives at controller rate, so it only
/// marks a flag; a notifier thread emits `midi-applied` at most every 50 ms and always
/// once more after the last move, so the UI's faders land on the final value.
///
/// A control that drives a zone also emits `midi-focus` (the zone slot) so the main
/// window can follow the operator's hands when "follow MIDI focus" is on. It fires on the
/// first message of a gesture — a different zone than last time, or the same zone after a
/// pause — not at controller rate: re-selecting the zone on every CC would fight a click
/// the operator makes on another zone mid-gesture.
fn install_midi_listener(app: &AppHandle, midi: &MidiRuntime, mappings_path: PathBuf) {
    let dirty = Arc::new(AtomicBool::new(false));
    let notifier = {
        let app = app.clone();
        let dirty = Arc::clone(&dirty);
        std::thread::Builder::new()
            .name("prisma-midi-ui".into())
            .spawn(move || loop {
                std::thread::park();
                while dirty.swap(false, Ordering::Relaxed) {
                    let _ = app.emit("midi-applied", ());
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
            .expect("spawn MIDI UI notifier")
    };
    let app = app.clone();
    let last_focus: Mutex<Option<(u16, std::time::Instant)>> = Mutex::new(None);
    midi.set_listener(Some(Arc::new(move |notice| match notice {
        MidiNotice::Learned { mapping, profile } => {
            match save_mappings(&profile) {
                Ok(json) => {
                    if let Err(e) = persist_text(&mappings_path, &json) {
                        eprintln!("prisma: could not save controller mappings: {e}");
                    }
                }
                Err(e) => eprintln!("prisma: could not serialize controller mappings: {e}"),
            }
            let _ = app.emit("midi-learned", mapping);
        }
        MidiNotice::Applied { zone, .. } => {
            if !dirty.swap(true, Ordering::Relaxed) {
                notifier.thread().unpark();
            }
            if let Some(slot) = zone {
                let now = std::time::Instant::now();
                let mut last = last_focus.lock().unwrap();
                let new_gesture = match *last {
                    Some((prev, at)) => prev != slot || now.duration_since(at) > FOCUS_GESTURE_GAP,
                    None => true,
                };
                if new_gesture {
                    let _ = app.emit("midi-focus", slot);
                }
                *last = Some((slot, now));
            }
        }
    })));
}

/// A pause longer than this between messages for the same zone starts a new gesture,
/// which re-focuses that zone (e.g. after the operator clicked elsewhere).
const FOCUS_GESTURE_GAP: std::time::Duration = std::time::Duration::from_millis(400);

/// Everything the MIDI tab and the Learn overlay need.
#[derive(Serialize)]
struct MidiStatus {
    /// Present input ports (empty if the MIDI system is unavailable).
    ports: Vec<String>,
    /// The configured port; `None` = auto-detect (APC40).
    selected: Option<String>,
    /// The port actually open right now.
    connected: Option<String>,
    /// The output open for LED feedback, if the device has one.
    output: Option<String>,
    /// The connected device's profile name (e.g. "Akai APC40 mkII", "Generic MIDI").
    device: String,
    error: Option<String>,
    /// The target waiting for a control, if Learn is armed.
    learning: Option<MapTarget>,
    mappings: Vec<MidiMapping>,
    /// The connected device's LED palette as `#rrggbb`, index = colour value; empty
    /// when the device has none (colour choices are then hidden).
    palette: Vec<String>,
}

#[tauri::command]
fn midi_status(state: State<AppState>) -> MidiStatus {
    let (ports, list_error) = match input_port_names() {
        Ok(p) => (p, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    MidiStatus {
        ports,
        selected: state.midi_config.lock().unwrap().input_port.clone(),
        connected: state
            .midi_link
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| l.port_name().to_string()),
        output: state
            .midi_out_link
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| l.port_name().to_string()),
        device: state.midi.device().display_name,
        error: state.midi_error.lock().unwrap().clone().or(list_error),
        learning: state.midi.learning(),
        mappings: state.midi.profile().mappings,
        palette: state
            .midi
            .device()
            .palette()
            .map(|p| p.iter().map(|c| format!("#{c:06x}")).collect())
            .unwrap_or_default(),
    }
}

/// Choose the input (`None` = auto-detect), persist it, and reopen.
#[tauri::command]
fn midi_set_input(state: State<AppState>, port: Option<String>) -> Result<MidiStatus, String> {
    let config = MidiConfig { input_port: port };
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    persist_text(&state.midi_config_path, &json)?;
    *state.midi_config.lock().unwrap() = config;
    connect_midi(&state);
    Ok(midi_status(state))
}

/// Re-scan and reopen the configured input (hot-plug: the device was connected late).
#[tauri::command]
fn midi_reconnect(state: State<AppState>) -> MidiStatus {
    connect_midi(&state);
    midi_status(state)
}

/// Arm MIDI Learn for a target: the next pad/fader touched on the hardware binds to it.
#[tauri::command]
fn midi_learn_arm(state: State<AppState>, target: MapTarget) {
    state.midi.arm_learn(target);
}

#[tauri::command]
fn midi_learn_cancel(state: State<AppState>) {
    state.midi.cancel_learn();
}

/// Remove the mapping for a target and persist the profile.
#[tauri::command]
fn midi_unmap(app: AppHandle, target: MapTarget) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    if let Some(profile) = state.midi.unmap(&target) {
        persist_mappings(&app, &profile)?;
    }
    Ok(state.midi.profile().mappings)
}

/// Change a mapped control's pad style and LED look (the Learn right-click menu, M9
/// Slice 3), persist the profile, and return the mappings.
#[tauri::command]
fn midi_configure(
    app: AppHandle,
    target: MapTarget,
    mode: ControlMode,
    led: Option<LedStyle>,
) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    let profile = state
        .midi
        .configure(&target, mode, led)
        .ok_or("that control is not mapped")?;
    persist_mappings(&app, &profile)?;
    Ok(profile.mappings)
}

/// Change how a mapped fader / knob takes over its value — soft takeover ("pickup",
/// the default) or "jump" — persist the profile, and return the mappings.
#[tauri::command]
fn midi_set_takeover(
    app: AppHandle,
    target: MapTarget,
    takeover: Takeover,
) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    let profile = state
        .midi
        .set_takeover(&target, takeover)
        .ok_or("that control is not mapped")?;
    persist_mappings(&app, &profile)?;
    Ok(profile.mappings)
}

fn persist_mappings(app: &AppHandle, profile: &ControllerMappings) -> Result<(), String> {
    let json = save_mappings(profile).map_err(|e| e.to_string())?;
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join(MAPPINGS_FILE);
    persist_text(&path, &json)
}

/// The operator's controller profile, in the per-user app-config dir (CLAUDE.md §4.6 —
/// separate from any Show).
const MAPPINGS_FILE: &str = "controller-mappings.json";

// --- Main operation window (M8 step 6): the global operation bar ---

/// The global bar's current master state, so the bar reflects what the engine holds
/// (a loaded show, or another transport) rather than only the UI's own writes.
#[derive(Serialize)]
struct MasterInfo {
    dimmer: f32,
    blackout: bool,
    strobe_on: bool,
    /// Strobe speed, `0..=1` over 1–25 Hz.
    strobe_rate: f32,
}

/// Read the master section (dimmer, blackout, global strobe) for the global bar.
#[tauri::command]
fn main_master(state: State<AppState>) -> MasterInfo {
    let guard = state.engine.lock().unwrap();
    let Some(engine) = guard.as_ref() else {
        return MasterInfo {
            dimmer: 1.0,
            blackout: false,
            strobe_on: false,
            strobe_rate: 0.4,
        };
    };
    let m = engine.live().master();
    MasterInfo {
        dimmer: m.dimmer.read_real().unwrap_or(1.0),
        blackout: m.blackout.read_real().unwrap_or(0.0) > 0.5,
        strobe_on: m.strobe.on.read_bool().unwrap_or(false),
        strobe_rate: m.strobe.rate.read_norm().unwrap_or(0.4),
    }
}

/// Set the master dimmer (the global-bar fader), real 0..1.
#[tauri::command]
fn main_set_master_dimmer(state: State<AppState>, value: f32) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_dimmer(value);
    }
}

/// Toggle the master blackout (kills all output while holding the fader value).
#[tauri::command]
fn main_set_master_blackout(state: State<AppState>, on: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_blackout(on);
    }
}

/// Engage/release the global strobe.
#[tauri::command]
fn main_set_master_strobe_on(state: State<AppState>, on: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_strobe_on(on);
    }
}

/// Set the global strobe speed in Hz (1–25), engaged or not.
#[tauri::command]
fn main_set_master_strobe_rate(state: State<AppState>, rate: f32) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_strobe_rate(rate);
    }
}

// --- Main operation window (M8 step 7): the patch editor as a secondary window ---

/// Open the M7 patch editor as a **secondary window** (M8-SPEC §6), from the main
/// window's menu. Ownership is already B (this process owns the engine; the editor
/// window talks to it over the same command surface), so opening just brackets the
/// show: the content **freezes** while the editor is open (output stays alive so
/// Highlight works, §7), and on **close** the editor's edits are applied to the running
/// show and it resumes on the new layout ([`apply_editor_draft`]). A second open focuses
/// the existing window.
/// Boot handover, step 1: show the console BEHIND the splash. Called by the splash once
/// the engine and UI are ready and its minimum on-screen time has passed. The splash is
/// always-on-top, so the operator still sees only the splash while the window manager
/// brings the console to its final (maximized) size — on Linux a hidden window ignores
/// size, position and "maximized" until it is shown, so it cannot be sized beforehand.
/// The console keeps its content invisible until that size has settled, then emits
/// `prisma://shown` and the splash fades and calls [`close_splash`].
#[tauri::command]
fn show_main(app: AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        reveal_main(&main);
    }
    let _ = app.emit("prisma://show-main", ());
}

/// Boot handover, step 2: drop the splash and focus the console. Also reveals the console
/// itself if step 1 never happened (the splash's safety cap), so the app can never be
/// left with nothing on screen. Idempotent.
#[tauri::command]
fn close_splash(app: AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        reveal_main(&main);
        let _ = main.set_focus();
    }
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.close();
    }
}

/// Show and maximize the console. No-op once visible.
fn reveal_main(main: &tauri::WebviewWindow) {
    if main.is_visible().unwrap_or(false) {
        return;
    }
    let _ = main.show();
    let _ = main.maximize();
}

/// The patch-editor window, opened to MATCH the main window: on its monitor, fullscreen
/// if it is fullscreen (maximized otherwise), and as its child so it stays above it and
/// travels with it. A plain maximized window left a fullscreen operator with an editor
/// that had a title bar and the desktop panel, possibly on the other monitor.
/// (On Wayland the compositor ignores client positions; the parent link is what keeps the
/// editor with the main window there.)
fn editor_window_builder(
    app: &AppHandle,
) -> tauri::Result<WebviewWindowBuilder<'_, tauri::Wry, AppHandle>> {
    let mut builder =
        WebviewWindowBuilder::new(app, "editor", WebviewUrl::App("index.html".into()))
            .title("Prisma — Editor de Patch")
            .inner_size(1280.0, 800.0)
            .min_inner_size(1280.0, 800.0);
    let Some(main) = app.get_webview_window("main") else {
        return Ok(builder.maximized(true));
    };
    if let Ok(Some(monitor)) = main.current_monitor() {
        let scale = monitor.scale_factor();
        let pos = monitor.position();
        builder = builder.position(pos.x as f64 / scale, pos.y as f64 / scale);
    }
    let fullscreen = main.is_fullscreen().unwrap_or(false);
    builder
        .fullscreen(fullscreen)
        .maximized(!fullscreen)
        .parent(&main)
}

// `async` on purpose: a Tauri v2 sync command runs on the main (event-loop) thread,
// and `WebviewWindowBuilder::build()` blocks waiting for that same event loop to create
// the window — a deadlock that hangs on Windows/WebView2 (GTK tolerated it, so it only
// showed up on Windows). An async command runs off the main thread, so `build()` can be
// serviced by the event loop and the editor window actually opens.
#[tauri::command]
async fn main_open_editor(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("editor") {
        let _ = win.set_focus();
        return Ok(());
    }
    // Freeze the show content while the editor is open (§7).
    if let Some(engine) = app.state::<AppState>().engine.lock().unwrap().as_ref() {
        engine.set_frozen(true);
    }
    let win = match editor_window_builder(&app).and_then(|b| b.build()) {
        Ok(win) => win,
        Err(e) => {
            let msg = format!("editor window failed to open: {e}");
            // Release builds have no console; record why and unfreeze so the show is
            // not left stuck frozen with no editor.
            if let Ok(dir) = app.path().app_config_dir() {
                let _ = std::fs::write(dir.join("editor-error.txt"), &msg);
            }
            if let Some(engine) = app.state::<AppState>().engine.lock().unwrap().as_ref() {
                engine.set_frozen(false);
            }
            return Err(msg);
        }
    };

    // On **Destroyed** (not CloseRequested): apply the edits to the running show and
    // resume it. `Destroyed` is the reliable "the window is truly gone" event across
    // every path — the editor's own webview intercepts `CloseRequested` for its
    // unsaved-changes guard and then closes via `destroy()`, which skips CloseRequested.
    // A failed recompile still unfreezes so the show never stays stuck frozen.
    let handle = app.clone();
    win.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            if let Err(e) = apply_editor_draft(&handle, true) {
                eprintln!("prisma: editor-close recompile failed: {e}");
                if let Some(engine) = handle.state::<AppState>().engine.lock().unwrap().as_ref() {
                    engine.set_frozen(false);
                }
                // The previous layout keeps running (fail-safe), but that must NOT be
                // silent — tell the main window the edits did not take, with the reason.
                let _ = handle.emit("editor-apply-failed", e);
            }
        }
    });
    Ok(())
}

// --- Main operation window (M8 step 4): the scene grid ---

/// The effects the picker offers, grouped by category — the M5 GPU
/// effects. Catalogue-driven, so the picker is never hardcoded (§5.3).
#[tauri::command]
fn main_effect_catalogue() -> Vec<EffectListingInfo> {
    prisma_render::catalogue()
        .into_iter()
        .map(|e| EffectListingInfo {
            id: e.id.to_string(),
            name: e.name.to_string(),
            category: e.category.to_string(),
        })
        .collect()
}

/// Write an effect (with optional stored `params` / colour table) into a zone's cell,
/// replacing whatever it held. Show state only — firing is the caller's choice. `None`
/// when the zone does not exist; an out-of-range index leaves the grid unchanged.
fn write_cell(
    state: &AppState,
    track: Uuid,
    index: usize,
    effect_id: String,
    params: Option<ParamSet>,
    palette: Option<Vec<[f32; 3]>>,
) -> Option<Vec<CellInfo>> {
    let mut show = state.show.lock().unwrap();
    let zs = show.track_mut(track)?;
    if index < zs.clips.len() {
        let mut layer = Layer::shader(effect_id, params.unwrap_or_default());
        let Source::Shader { palette: p, .. } = &mut layer.source;
        *p = palette
            .map(|mut p| {
                p.truncate(prisma_render::PALETTE_MAX);
                p
            })
            .filter(|p| !p.is_empty());
        zs.clips[index] = Cell::single(layer);
    }
    Some(grid_dto(zs, armed_of(state, track)))
}

/// Assign an effect to a cell AND fire it (picking an effect for a cell plays it):
/// fill the cell's layer, make it the zone's active cell, and trigger it live through
/// the frame-boundary queue. Returns the updated grid.
///
/// `params` / `palette` are optional: the effect picker sends neither (manifest
/// defaults).
#[tauri::command]
fn main_assign_cell(
    state: State<AppState>,
    track: Uuid,
    index: usize,
    effect_id: String,
    params: Option<ParamSet>,
    palette: Option<Vec<[f32; 3]>>,
) -> Vec<CellInfo> {
    let Some(slot) = track_slot(&state, track) else {
        return Vec::new();
    };
    let Some(dto) = write_cell(&state, track, index, effect_id, params, palette) else {
        return Vec::new();
    };
    // Play it through the cell's `trigger` target (restart: the cell may already be the
    // running one, now with a different effect). The controller reads the new content
    // from the show, runs it and reports the running cell back (`cells-changed`).
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.trigger_clip(slot, index);
    }
    dto
}

/// Assign an effect to a cell WITHOUT firing it — the grid's paste: the operator readies
/// a cell and plays it later. The zone's active cell does not change; only the pads are
/// refreshed so a mapped pad shows the cell as assigned. Returns the updated grid.
#[tauri::command]
fn main_assign_cell_silent(
    state: State<AppState>,
    track: Uuid,
    index: usize,
    effect_id: String,
    params: Option<ParamSet>,
    palette: Option<Vec<[f32; 3]>>,
) -> Vec<CellInfo> {
    let Some(slot) = track_slot(&state, track) else {
        return Vec::new();
    };
    let Some(dto) = write_cell(&state, track, index, effect_id, params, palette) else {
        return Vec::new();
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.refresh_clips(slot);
    }
    dto
}

/// Trigger an already-assigned cell (make it the zone's active content). An empty
/// cell clears the zone (stops it). Returns the updated grid.
#[tauri::command]
fn main_trigger_cell(state: State<AppState>, track: Uuid, index: usize) -> Vec<CellInfo> {
    let Some(slot) = track_slot(&state, track) else {
        return Vec::new();
    };
    let dto = {
        let show = state.show.lock().unwrap();
        let Some(zs) = show.track(track) else {
            return Vec::new();
        };
        grid_dto(zs, armed_of(&state, track))
    };
    // The same tree target a pad in latch mode fires (M9): one way to play a cell. The
    // running cell updates asynchronously and arrives as `cells-changed`.
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.fire_clip(slot, index);
    }
    dto
}

/// Empty a cell. If it was the running cell, the zone stops (clears its output).
#[tauri::command]
fn main_clear_cell(state: State<AppState>, track: Uuid, index: usize) -> Vec<CellInfo> {
    let Some(slot) = track_slot(&state, track) else {
        return Vec::new();
    };
    let (dto, was_active) = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.track_mut(track) else {
            return Vec::new();
        };
        if index >= zs.clips.len() {
            return grid_dto(zs, armed_of(&state, track));
        }
        zs.clips[index] = Cell::empty();
        let was_active = zs.active == Some(index);
        (grid_dto(zs, armed_of(&state, track)), was_active)
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        if was_active {
            // Re-run the now-empty running cell: the zone stops, and its state follows.
            engine.trigger_clip(slot, index);
        } else {
            engine.refresh_clips(slot); // its pad goes dark
        }
    }
    dto
}

/// Label a cell on the grid. Blank (after trimming) clears the label, so the grid falls
/// back to the effect's name. A UI label only — the engine never reads it, so nothing
/// is pushed live; it persists with the show. Returns the updated grid.
#[tauri::command]
fn main_rename_cell(
    state: State<AppState>,
    track: Uuid,
    index: usize,
    name: String,
) -> Vec<CellInfo> {
    let mut show = state.show.lock().unwrap();
    let Some(zs) = show.track_mut(track) else {
        return Vec::new();
    };
    if let Some(cell) = zs.clips.get_mut(index) {
        let name = name.trim();
        cell.cell_name = (!name.is_empty()).then(|| name.to_string());
    }
    grid_dto(zs, armed_of(&state, track))
}

/// Give a cell its own grid colour (sRGB 0–255). A UI label like the name: not light,
/// never sent to the engine; it persists with the show. Returns the updated grid.
#[tauri::command]
fn main_set_cell_color(
    state: State<AppState>,
    track: Uuid,
    index: usize,
    color: [u8; 3],
) -> Vec<CellInfo> {
    let mut show = state.show.lock().unwrap();
    let Some(zs) = show.track_mut(track) else {
        return Vec::new();
    };
    if let Some(cell) = zs.clips.get_mut(index) {
        cell.cell_color = Some(color);
    }
    grid_dto(zs, armed_of(&state, track))
}

// --- Main operation window (M8 step 5): the effect parameter panel ---

/// The cell the effect panel works on: the one the operator selected on the grid, else
/// the zone's running cell. Selecting a cell never plays it (selected ≠ active).
fn panel_cell(zs: &Track, cell: Option<usize>) -> Option<usize> {
    cell.filter(|i| *i < zs.clips.len()).or(zs.active)
}

/// The effect work panel for a zone (§3.5): the effect of the given `cell` (the grid's
/// selection; `None` = the zone's running cell) and the effect's manifest-driven
/// parameters, each merged with the cell's stored value (else the manifest default).
///
/// For an EMPTY cell, `draft` names the effect the UI offers for it (the panel's
/// dropdown): the panel then shows that effect at its manifest defaults, flagged
/// `draft`. Read-only — the cell is not touched; the UI assigns it on the first edit or
/// GO. Without a (known) draft an empty cell's panel is empty (`effect_id: None`).
#[tauri::command]
fn main_effect_panel(
    state: State<AppState>,
    track: Uuid,
    cell: Option<usize>,
    draft: Option<String>,
) -> EffectPanelInfo {
    let show = state.show.lock().unwrap();
    let armed_clip = armed_of(&state, track);
    let empty = |cell_index: Option<usize>, active: bool| EffectPanelInfo {
        track_id: track,
        armed: false,
        cell_index,
        active,
        draft: false,
        effect_id: None,
        effect_name: None,
        params: Vec::new(),
        palette: None,
        palette_max: prisma_render::PALETTE_MAX,
        fade: 0.0,
    };
    let Some(zs) = show.track(track) else {
        return empty(None, false);
    };
    let index = panel_cell(zs, cell);
    let active = index.is_some() && index == zs.active;
    let fade = index
        .and_then(|i| zs.clips.get(i))
        .map_or(0.0, |c| c.fade_time.as_secs_f64());
    // The cell's single layer (M8: one effect per cell). An empty cell has no layer, so
    // the panel shows its empty state.
    let effect = index
        .and_then(|i| zs.clips.get(i))
        .and_then(|c| c.layers.first())
        .map(|l| match &l.source {
            Source::Shader {
                effect_id,
                params,
                palette,
            } => (effect_id.clone(), params, palette.clone()),
        });
    // An empty cell: the UI's draft effect, at its defaults (nothing stored).
    let no_params = ParamSet::new();
    let is_draft = effect.is_none();
    let effect = effect.or_else(|| {
        index?;
        let id = draft.filter(|d| prisma_render::manifest_by_id(d).is_some())?;
        Some((id, &no_params, None))
    });
    let Some((effect_id, stored, stored_palette)) = effect else {
        return empty(index, active);
    };
    let params = prisma_render::effect_params(&effect_id)
        .into_iter()
        .map(|p| {
            let (role, group, channel) = role_dto(&p.role);
            let choices = match &p.role {
                ParamRole::Enum { choices }
                | ParamRole::DirectionMenu { choices }
                | ParamRole::PaletteMode { choices } => {
                    Some(choices.iter().map(|s| s.to_string()).collect())
                }
                _ => None,
            };
            let requires = p.requires.map(|r| match r {
                Requires::Choice { param, choice } => RequiresInfo {
                    param,
                    choice: Some(choice),
                },
                Requires::NonZero { param } => RequiresInfo {
                    param,
                    choice: None,
                },
            });
            ParamInfo {
                value_norm: stored.0.get(&p.name).copied().unwrap_or(p.default_norm),
                label: humanize(&p.name),
                name: p.name,
                min: p.min,
                max: p.max,
                role,
                group,
                channel,
                choices,
                requires,
            }
        })
        .collect();
    // The colour table, for an effect that reads one: the cell's own list, else the
    // effect's factory colours.
    let palette = prisma_render::manifest_by_id(&effect_id)
        .and_then(|m| m.palette)
        .map(|defaults| stored_palette.unwrap_or_else(|| defaults.to_vec()));
    EffectPanelInfo {
        track_id: track,
        armed: index.is_some() && index == armed_clip,
        cell_index: index,
        active,
        draft: is_draft,
        effect_name: effect_display_name(&effect_id),
        effect_id: Some(effect_id),
        params,
        palette,
        palette_max: prisma_render::PALETTE_MAX,
        fade,
    }
}

/// Replace the colour table of a cell's effect (the "Tabla de color"): persist it into
/// the show's cell (linear-light RGB, capped at [`prisma_render::PALETTE_MAX`]) and, when
/// that cell is the zone's running one, push it live at the next frame boundary. `cell`
/// is the grid's selection (`None` = the running cell).
#[tauri::command]
fn main_set_palette(
    state: State<AppState>,
    track: Uuid,
    colors: Vec<[f32; 3]>,
    cell: Option<usize>,
) {
    let mut colors = colors;
    colors.truncate(prisma_render::PALETTE_MAX);
    if colors.is_empty() {
        return; // a colour table always keeps at least one colour
    }
    let live = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.track_mut(track) else {
            return;
        };
        let index = panel_cell(zs, cell);
        if let Some(c) = index.and_then(|i| zs.clips.get_mut(i)) {
            if let Some(layer) = c.layers.first_mut() {
                let Source::Shader { palette, .. } = &mut layer.source;
                *palette = Some(colors.clone());
            }
        }
        index.is_some() && index == zs.active
    };
    if !live {
        return; // a cell that is not playing: stored for when it is played
    }
    if let Some(slot) = track_slot(&state, track) {
        if let Some(engine) = state.engine.lock().unwrap().as_ref() {
            engine.set_track_palette(slot, colors);
        }
    }
}

/// Set one parameter of a cell's effect (§3.5). `cell` is the grid's selection (`None` =
/// the zone's running cell). The normalized value is always persisted into the show
/// (source of truth for save and for a later trigger); only when that cell is the
/// running one is it also driven **live** through the engine (so the preview and wire
/// follow at once) — editing a cell that is not playing never touches the output.
/// `value` is normalized `0..=1`.
#[tauri::command]
fn main_set_param(
    state: State<AppState>,
    track: Uuid,
    name: String,
    value: f32,
    cell: Option<usize>,
) {
    let (live, track) = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.track_mut(track) else {
            return;
        };
        let index = panel_cell(zs, cell);
        if let Some(c) = index.and_then(|i| zs.clips.get_mut(i)) {
            if let Some(layer) = c.layers.first_mut() {
                let Source::Shader { params, .. } = &mut layer.source;
                params.set(name.as_str(), value);
            }
        }
        (index.is_some() && index == zs.active, zs.id)
    };
    if !live {
        return;
    }
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_track_param(track, &name, value);
    }
}

// --- Live model phase 2: the track grid, scenes, Arm / GO, tempo ---

/// One track column of the grid (LIVE-MODEL-SPEC §7).
#[derive(Serialize)]
struct TrackInfo {
    id: Uuid,
    name: String,
    /// Destination zones (phase 2: exactly the one it was created for).
    zones: Vec<Uuid>,
    /// The destination zones' names, joined for the header.
    destination: String,
    /// The first destination zone's display colour (the header strip, the pads).
    color: [u8; 3],
    blend_mode: BlendMode,
    /// `BlendMode::LumaKey`'s threshold and softness (linear luminance, 0..1).
    key_threshold: f32,
    key_softness: f32,
    opacity: f32,
    mute: bool,
    solo: bool,
    transition: TrackTransition,
    launch: LaunchMode,
    active: Option<usize>,
    armed: Option<usize>,
    clips: Vec<CellInfo>,
}

/// One grid row's label.
#[derive(Serialize)]
struct SceneInfo {
    index: usize,
    name: Option<String>,
    color: Option<[u8; 3]>,
}

/// The whole grid: tracks in compositing order, one scene per row.
#[derive(Serialize)]
struct GridInfo {
    tracks: Vec<TrackInfo>,
    scenes: Vec<SceneInfo>,
}

/// A track's armed clip, as the clip controller last reported it.
fn armed_of(state: &AppState, track: Uuid) -> Option<usize> {
    state.armed.lock().unwrap().get(&track).copied()
}

/// The engine slot of a track. The engine numbers tracks in `Show::tracks` order, and
/// every engine is assembled from this same show.
fn track_slot(state: &AppState, track: Uuid) -> Option<usize> {
    state
        .show
        .lock()
        .unwrap()
        .tracks
        .iter()
        .position(|t| t.id == track)
}

/// Run `f` on the engine with a track's slot, if both exist.
fn with_track_engine(state: &AppState, track: Uuid, f: impl FnOnce(&EngineHandle, usize)) {
    let Some(slot) = track_slot(state, track) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        f(engine, slot);
    }
}

/// The grid of the running show: every track with its clips and live controls, and
/// one scene per row.
#[tauri::command]
fn main_grid(state: State<AppState>) -> GridInfo {
    let show = state.show.lock().unwrap();
    let engine = state.engine.lock().unwrap();
    let tracks: Vec<TrackInfo> = show
        .tracks
        .iter()
        .enumerate()
        .map(|(slot, t)| {
            let zone = |id: &Uuid| show.patch.zones.iter().find(|z| z.id == *id);
            let live = engine.as_ref().and_then(|e| e.track_live(slot));
            TrackInfo {
                id: t.id,
                name: t.name.clone(),
                zones: t.destination.zones.clone(),
                destination: t
                    .destination
                    .zones
                    .iter()
                    .filter_map(|id| zone(id).map(|z| z.name.clone()))
                    .collect::<Vec<_>>()
                    .join(", "),
                color: t
                    .destination
                    .zones
                    .first()
                    .and_then(zone)
                    .map_or([128, 128, 128], |z| z.display_color),
                blend_mode: t.blend_mode,
                key_threshold: t.key_threshold,
                key_softness: t.key_softness,
                opacity: live.map_or(t.opacity, |l| l.opacity),
                mute: live.map_or(t.mute, |l| l.mute),
                solo: live.map_or(t.solo, |l| l.solo),
                transition: t.transition,
                launch: t.launch,
                active: t.active,
                armed: armed_of(&state, t.id),
                clips: grid_dto(t, armed_of(&state, t.id)),
            }
        })
        .collect();
    let rows = show.tracks.iter().map(|t| t.clips.len()).max().unwrap_or(0);
    let scenes = (0..rows)
        .map(|i| {
            let label = show.scenes.get(i);
            SceneInfo {
                index: i,
                name: label.and_then(|l| l.name.clone()),
                color: label.and_then(|l| l.color),
            }
        })
        .collect();
    GridInfo { tracks, scenes }
}

/// Arm (or disarm) a clip for GO — Shift+click, the global "Modo armar", or an
/// Arm-mode track's launch all reach this same state.
#[tauri::command]
fn main_arm_cell(state: State<AppState>, track: Uuid, index: usize) {
    with_track_engine(&state, track, |e, slot| {
        e.arm_clip(slot, index);
    });
}

/// Stop a track (fade out with its transition; a second stop during it cuts).
#[tauri::command]
fn main_stop_track(state: State<AppState>, track: Uuid) {
    with_track_engine(&state, track, |e, slot| {
        e.stop_track(slot);
    });
}

/// Launch grid row `row` on every track that has a clip there (`arm` = arm instead).
#[tauri::command]
fn main_launch_scene(state: State<AppState>, row: usize, arm: bool) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        if arm {
            e.arm_scene(row);
        } else {
            e.launch_scene(row);
        }
    }
}

/// Disarm every armed clip (`/arm/clear`).
#[tauri::command]
fn main_arm_clear(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.clear_arm();
    }
}

/// Stop every track (each with its transition; a second press during the fades cuts).
#[tauri::command]
fn main_stop_all(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.stop_all();
    }
}

/// GO: play every armed clip in the same frame.
#[tauri::command]
fn main_arm_go(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.go();
    }
}

/// A track's opacity (`0..=1`), live, and stored in the show.
#[tauri::command]
fn main_set_track_opacity(state: State<AppState>, track: Uuid, value: f32) {
    let value = value.clamp(0.0, 1.0);
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.opacity = value;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_opacity(slot, value));
}

/// Mute a track, live, and store it in the show.
#[tauri::command]
fn main_set_track_mute(state: State<AppState>, track: Uuid, on: bool) {
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.mute = on;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_mute(slot, on));
}

/// Solo a track, live, and store it in the show.
#[tauri::command]
fn main_set_track_solo(state: State<AppState>, track: Uuid, on: bool) {
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.solo = on;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_solo(slot, on));
}

/// The track popover: its transition and launch mode. Read by the clip controller at
/// each launch, so it applies at once — no recompile.
#[tauri::command]
fn main_set_track_behaviour(
    state: State<AppState>,
    track: Uuid,
    transition: TrackTransition,
    launch: LaunchMode,
) {
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.transition = TrackTransition {
            time: transition.time.clamp(0.0, 600.0),
            ..transition
        };
        t.launch = launch;
    }
}

/// How a track blends onto the tracks before it (the track popover). Live, no recompile.
#[tauri::command]
fn main_set_track_blend(
    state: State<AppState>,
    track: Uuid,
    mode: BlendMode,
    key_threshold: Option<f32>,
    key_softness: Option<f32>,
) {
    let key = {
        let mut show = state.show.lock().unwrap();
        let Some(t) = show.track_mut(track) else {
            return;
        };
        t.blend_mode = mode;
        if let Some(v) = key_threshold {
            t.key_threshold = v.clamp(0.0, 1.0);
        }
        if let Some(v) = key_softness {
            t.key_softness = v.clamp(0.0, 1.0);
        }
        [t.key_threshold, t.key_softness]
    };
    with_track_engine(&state, track, |e, slot| {
        e.set_track_blend(slot, mode, key);
    });
}

/// A clip's own fade in seconds (0 = the track's transition).
#[tauri::command]
fn main_set_cell_fade(state: State<AppState>, track: Uuid, index: usize, seconds: f64) {
    if let Some(cell) = state
        .show
        .lock()
        .unwrap()
        .track_mut(track)
        .and_then(|t| t.clips.get_mut(index))
    {
        cell.fade_time = std::time::Duration::from_secs_f64(seconds.clamp(0.0, 600.0));
    }
}

/// Rename a track (a label only: no recompile).
#[tauri::command]
fn main_rename_track(state: State<AppState>, track: Uuid, name: String) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.name = name.to_string();
    }
}

/// "+ Track" on a zone: a new track "Track n" reaching only that zone, after the zone's
/// last track. A destination change recompiles the running show (LIVE-MODEL-SPEC §4.4).
/// Returns the new track's id, so the UI can open its name for editing.
#[tauri::command]
fn main_add_track(app: AppHandle, zone: Uuid) -> Result<Uuid, String> {
    let known = {
        let state = app.state::<AppState>();
        let show = state.show.lock().unwrap();
        show.patch.zones.iter().any(|z| z.id == zone)
    };
    if !known {
        return Err("no such zone".into());
    }
    let mut new_id = Uuid::nil();
    recompile_running_show(&app, |show| {
        let mut track = Track::new(
            next_track_name(show),
            prisma_engine::Destination::zone(zone),
        );
        track.clips.resize_with(GRID_CELLS, Cell::empty);
        new_id = track.id;
        let at = show
            .tracks
            .iter()
            .rposition(|t| t.destination.reaches(zone))
            .map_or(show.tracks.len(), |i| i + 1);
        show.tracks.insert(at, track);
    })?;
    Ok(new_id)
}

/// "Track n" with the smallest `n` no track of the show is named after.
fn next_track_name(show: &Show) -> String {
    (1..)
        .map(|n| format!("Track {n}"))
        .find(|name| !show.tracks.iter().any(|t| &t.name == name))
        .expect("an unused number exists")
}

/// Reorder: move `track` to sit just before `before` (or last when `None`). Only among the
/// tracks of the same zone — order is compositing order inside a zone (later = on top);
/// across zones it means nothing. Recompiles the running show.
#[tauri::command]
fn main_move_track(app: AppHandle, track: Uuid, before: Option<Uuid>) -> Result<(), String> {
    recompile_running_show(&app, |show| move_track(&mut show.tracks, track, before))
}

/// Move `track` just before `before` (or after the last track of its zone when `None`),
/// only when both reach the same zones; anything else leaves the order unchanged.
fn move_track(tracks: &mut Vec<Track>, track: Uuid, before: Option<Uuid>) {
    let Some(from) = tracks.iter().position(|t| t.id == track) else {
        return;
    };
    let zones = tracks[from].destination.zones.clone();
    let same = |t: &Track| t.destination.zones == zones;
    if let Some(b) = before {
        if b == track || !tracks.iter().any(|t| t.id == b && same(t)) {
            return;
        }
    }
    let moved = tracks.remove(from);
    let at = match before {
        Some(b) => tracks
            .iter()
            .position(|t| t.id == b)
            .expect("checked above"),
        None => tracks
            .iter()
            .rposition(same)
            .map_or(tracks.len(), |i| i + 1),
    };
    tracks.insert(at, moved);
}

/// Delete a track and its clips. Recompiles the running show.
#[tauri::command]
fn main_remove_track(app: AppHandle, track: Uuid) -> Result<(), String> {
    recompile_running_show(&app, |show| show.tracks.retain(|t| t.id != track))
}

/// The tempo in the global bar.
#[derive(Serialize)]
struct ClockInfo {
    bpm: f64,
}

#[tauri::command]
fn main_clock(state: State<AppState>) -> ClockInfo {
    let bpm = state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .map_or(120.0, |e| e.bpm());
    ClockInfo { bpm }
}

#[tauri::command]
fn main_set_bpm(state: State<AppState>, bpm: f64) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_bpm(bpm);
    }
}

#[tauri::command]
fn main_tap(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.tap();
    }
}

#[tauri::command]
fn editor_undo(state: State<AppState>) -> EditorState {
    let mut draft = state.draft.lock().unwrap();
    draft.undo();
    state_of(&draft)
}

#[tauri::command]
fn editor_redo(state: State<AppState>) -> EditorState {
    let mut draft = state.draft.lock().unwrap();
    draft.redo();
    state_of(&draft)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            editor_get_state,
            editor_selection_aggregate,
            editor_fixture_types,
            editor_library_get,
            editor_library_add_type,
            editor_library_update_type,
            editor_library_remove_type,
            editor_next_free_address,
            editor_next_fixture_id,
            editor_preview_placement,
            editor_place_fixtures,
            editor_translate_fixtures,
            editor_set_zone_mapping_size,
            editor_add_zone,
            editor_rename_zone,
            editor_remove_zone,
            editor_highlight,
            editor_apply_to_live,
            editor_preview_port,
            editor_remove_fixtures,
            editor_save_to,
            editor_open_from,
            editor_undo,
            editor_redo,
            main_zones,
            main_set_zone_dimmer,
            main_set_zone_speed,
            main_network_interfaces,
            main_output_get,
            main_output_set,
            main_open_node_page,
            main_host_name,
            main_output_status,
            main_artnet_scan,
            main_node_statuses,
            main_set_zone_solo,
            main_set_zone_blackout,
            main_set_zone_strobe_on,
            main_set_zone_strobe_rate,
            main_master,
            main_set_master_dimmer,
            main_set_master_blackout,
            main_set_master_strobe_on,
            main_set_master_strobe_rate,
            main_open_editor,
            main_save_show,
            main_new_show,
            main_open_show,
            main_effect_catalogue,
            main_assign_cell,
            main_assign_cell_silent,
            main_trigger_cell,
            main_recovery_info,
            main_set_track_blend,
            main_stop_all,
            main_move_track,
            main_arm_clear,
            main_grid,
            main_arm_cell,
            main_stop_track,
            main_launch_scene,
            main_arm_go,
            main_set_track_opacity,
            main_set_track_mute,
            main_set_track_solo,
            main_set_track_behaviour,
            main_set_cell_fade,
            main_rename_track,
            main_add_track,
            main_remove_track,
            main_clock,
            main_set_bpm,
            main_tap,
            main_clear_cell,
            main_rename_cell,
            main_set_cell_color,
            main_effect_panel,
            main_set_param,
            main_set_palette,
            midi_status,
            midi_set_input,
            midi_reconnect,
            midi_learn_arm,
            midi_learn_cancel,
            midi_unmap,
            midi_configure,
            midi_set_takeover,
            close_splash,
            show_main,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The app boots EMPTY (no zones, no patch, nothing running) unless a last show
            // is remembered — then it reopens that `.prisma` (which carries its own embedded
            // patch). This is the "start from zero, or resume the last show" behaviour.
            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?;
            // Network output config (device manager): where DMX goes. Loaded from the
            // per-user app-config dir; a missing/invalid file → defaults (virtual sink),
            // so a fresh install behaves exactly as before.
            let network_path = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?
                .join("network.json");
            let network = std::fs::read_to_string(&network_path)
                .ok()
                .and_then(|json| serde_json::from_str::<NetworkConfig>(&json).ok())
                .unwrap_or_default();

            let last_show_file = config_dir.join("last-show.txt");
            let prepare = |json: String| {
                load_show(&json)
                    .ok()
                    .map(|mut s| {
                        s.patch.fill_zone_members_from_ownership();
                        s.patch.fit_matrix_size();
                        // An old save (empty or 16-cell grids) is padded to the full grid.
                        pad_grids(&mut s);
                        s
                    })
                    .filter(|s| s.patch.compile().is_ok())
            };
            // An autosave on disk means the last session ended with unsaved changes:
            // recover it (and keep a backup); otherwise reopen the last saved show.
            let autosave = autosave::Autosave::new(&config_dir);
            let last_saved = read_last_show(&last_show_file);
            let recovered_show = autosave.read().and_then(prepare);
            let recovered = recovered_show.is_some().then(|| {
                autosave.rotate_backups();
                RecoveryInfo {
                    last_saved: last_saved
                        .as_ref()
                        .filter(|p| autosave::exists(p))
                        .map(|p| p.to_string_lossy().into_owned()),
                }
            });
            let mut show = recovered_show
                .or_else(|| {
                    last_saved
                        .as_ref()
                        .and_then(|p| std::fs::read_to_string(p).ok())
                        .and_then(prepare)
                })
                .unwrap_or_else(empty_show);
            // A show from before the device manager inherits the old app-wide target.
            prepare_show_network(&mut show, &network);
            // Keep an editable copy as the grid's + zone-set source of truth; the engine
            // runs its own copy and follows our triggers through the queue.
            let show_state = show.clone();
            // The editor draft opens on the same patch (empty until the operator builds a
            // rig, or the loaded show's rig).
            let draft_patch = show.patch.clone();

            // Prefer the real GPU, fall back to software so the window still opens without
            // a configured Vulkan driver. Each attempt binds its own preview WebSocket (a
            // failed attempt frees its port before the next binds).
            let target = virtual_sink();
            let (config_hp, port_hp) = preview_config(AdapterPreference::HighPerformance, target);
            let (engine, preview_port) = match Engine::start_show(show.clone(), config_hp) {
                Ok(engine) => (engine, port_hp),
                Err(e_hp) => {
                    let (config_sw, port_sw) = preview_config(AdapterPreference::Software, target);
                    match Engine::start_show(show, config_sw) {
                        Ok(engine) => (engine, port_sw),
                        Err(e_sw) => {
                            // No GPU at all (a bare VM with neither a Vulkan driver nor a
                            // DX12/WARP fallback). Release builds have no console
                            // (`windows_subsystem = "windows"`), so record why the boot
                            // failed to a file the operator can read, then surface the error.
                            let msg = format!(
                                "Prisma could not start the render engine — no usable GPU.\n\
                                 HighPerformance adapter: {e_hp}\n\
                                 Software adapter: {e_sw}\n"
                            );
                            let _ = std::fs::write(config_dir.join("boot-error.txt"), &msg);
                            return Err(msg.into());
                        }
                    }
                }
            };

            // The show runs LIVE (not frozen) — the main window operates content. The
            // editor's freeze bracket returns with the editor window in a later step.

            // Load the operator's fixture library from the per-user app-config dir
            // (§3.2). First run: seed a few generic types and write the file, so the
            // placement dialog is never empty. A present-but-invalid file is left
            // untouched (so a hand-edit mistake is recoverable) and a seeded library is
            // used in memory for the session.
            let library_path = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?
                .join("fixture-library.json");
            let library = match std::fs::read_to_string(&library_path) {
                Ok(json) => load_library(&json).unwrap_or_else(|e| {
                    eprintln!(
                        "prisma: fixture library at {} is invalid ({e}); \
                         using a seeded library in memory (file left untouched)",
                        library_path.display()
                    );
                    FixtureLibrary::seed()
                }),
                Err(_) => {
                    let lib = FixtureLibrary::seed();
                    if let Err(e) = persist_library(&library_path, &lib) {
                        eprintln!("prisma: could not write seed fixture library: {e}");
                    }
                    lib
                }
            };

            // MIDI (M9): the operator's controller profile + which input to open, both
            // per-user. A present-but-invalid profile is left untouched on disk (so a
            // hand-edit mistake is recoverable) and an empty one is used for the session.
            let mappings_path = config_dir.join(MAPPINGS_FILE);
            let profile = match std::fs::read_to_string(&mappings_path) {
                Ok(json) => load_mappings(&json).unwrap_or_else(|e| {
                    eprintln!(
                        "prisma: controller mappings at {} are invalid ({e}); \
                         using an empty profile in memory (file left untouched)",
                        mappings_path.display()
                    );
                    ControllerMappings::new("default")
                }),
                Err(_) => ControllerMappings::new("default"),
            };
            let midi = engine.midi().clone();
            midi.set_profile(profile);
            install_midi_listener(app.handle(), &midi, mappings_path);
            let midi_config_path = config_dir.join("midi.json");
            let midi_config = std::fs::read_to_string(&midi_config_path)
                .ok()
                .and_then(|json| serde_json::from_str::<MidiConfig>(&json).ok())
                .unwrap_or_default();

            app.manage(AppState {
                engine: Mutex::new(Some(engine)),
                show: Mutex::new(show_state),
                draft: Mutex::new(PatchDraft::new(draft_patch)),
                library: Mutex::new(library),
                library_path,
                preview_port: Mutex::new(preview_port.unwrap_or(0)),
                network: Mutex::new(network),
                artnet_monitor: Mutex::new(None),
                last_show_file,
                midi,
                midi_link: Mutex::new(None),
                midi_out_link: Mutex::new(None),
                midi_config: Mutex::new(midi_config),
                midi_config_path,
                midi_error: Mutex::new(None),
                armed: Mutex::new(Default::default()),
                autosave,
                autosave_state: Mutex::new((
                    if recovered.is_some() {
                        autosave::Baseline::Recovered
                    } else {
                        autosave::Baseline::Pending
                    },
                    None,
                )),
                recovered: Mutex::new(recovered),
            });
            // Autosave: a snapshot every couple of seconds, off every hot path.
            let autosaver = app.handle().clone();
            std::thread::Builder::new()
                .name("prisma-autosave".into())
                .spawn(move || loop {
                    std::thread::sleep(autosave::INTERVAL);
                    autosave_tick(&autosaver);
                })
                .expect("spawn autosave thread");
            // Track clips play from the Parameter Tree; the engine keeps this host
            // across every recompile. Set after `manage` — the host reads the state.
            if let Some(engine) = app.state::<AppState>().engine.lock().unwrap().as_ref() {
                engine.set_clip_host(Arc::new(AppCellHost {
                    app: app.handle().clone(),
                }));
            }
            // Open the MIDI input off the boot path's critical section: a missing device
            // just leaves the link empty (the device manager can reconnect later).
            connect_midi(&app.state::<AppState>());
            // Art-Net node monitor (status dots + Scan). A held 6454 (another lighting program open on this
            // PC) just leaves it off; the Scan retries and explains.
            let _ = ensure_artnet_monitor(app.handle());

            // Boot splash safety net: the splash webview normally reveals the console (via
            // `close_splash`) once the UI signals `prisma://ready`, and it has its own hard
            // cap. This is the backstop for a splash whose JS never runs: after a generous
            // timeout, reveal the console the same way and drop the splash, so the app can
            // never boot to nothing on screen. No-op if the normal path got there first.
            let boot = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(20));
                if let Some(main) = boot.get_webview_window("main") {
                    reveal_main(&main);
                    let _ = main.set_focus();
                }
                if let Some(splash) = boot.get_webview_window("splash") {
                    let _ = splash.close();
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Prisma app")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                // Last autosave step while the engine still holds the live values.
                autosave_tick(app_handle);
                if let Some(state) = app_handle.try_state::<AppState>() {
                    // Close the MIDI input before the engine, so no input thread is writing
                    // while it tears down; darken the LEDs, then close the output.
                    state.midi_link.lock().unwrap().take();
                    state.midi.clear_leds();
                    state.midi.set_output(None);
                    state.midi_out_link.lock().unwrap().take();
                    if let Some(engine) = state.engine.lock().unwrap().take() {
                        let report = engine.shutdown();
                        eprintln!(
                            "prisma: engine shut down cleanly \
                             ({} render frames, gpu refs at release = {})",
                            report.render.frames_published, report.gpu_strong_refs_at_release
                        );
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(enabled: bool, ip: &str) -> NetworkConfig {
        NetworkConfig {
            artnet_enabled: enabled,
            artnet_ip: ip.into(),
            artnet_port: 6454,
        }
    }

    #[test]
    fn an_enabled_legacy_target_becomes_one_node_with_the_patch_universes() {
        let mut show = empty_show();
        show.network = ShowNetwork::default();
        prepare_show_network(&mut show, &legacy(true, "2.255.255.255"));
        let net = &show.network;
        assert!(net.initialized && net.artnet_enabled);
        assert_eq!(net.nodes.len(), 1);
        assert_eq!(net.nodes[0].ip, Ipv4Addr::new(2, 255, 255, 255));
        assert_eq!(net.nodes[0].ports, patch_universes(&show.patch));
    }

    #[test]
    fn a_disabled_legacy_stays_on_the_virtual_sink_and_migrates_once() {
        let mut show = empty_show();
        show.network = ShowNetwork::default();
        prepare_show_network(&mut show, &legacy(false, "10.0.0.1"));
        assert!(show.network.initialized && !show.network.artnet_enabled);
        assert!(show.network.nodes.is_empty());
        // Already initialized: a later enabled legacy config must not add a node.
        prepare_show_network(&mut show, &legacy(true, "10.0.0.1"));
        assert!(show.network.nodes.is_empty());
    }

    #[test]
    fn empty_show_default_zone_has_a_seeded_lit_grid() {
        // Regression: the default boot zone must have assignable scene cells and start lit,
        // otherwise mapping pixels into it leaves the effect grid disabled (the reconcile
        // path preserves an existing zone's grid, so an empty one would never fill in).
        let show = empty_show();
        assert_eq!(show.zones.len(), 1, "empty show has one default zone");
        assert_eq!(show.tracks.len(), 1, "the default zone has its own track");
        assert_eq!(
            show.tracks[0].clips.len(),
            GRID_CELLS,
            "default zone's scene grid must be seeded"
        );
        assert_eq!(show.zones[0].live.dimmer, 1.0, "default zone starts lit");
    }

    #[test]
    fn a_new_show_is_a_mapped_50x50_zone_with_an_empty_grid() {
        let show = empty_show();
        let patch = &show.patch;
        assert_eq!(patch.fixtures.len(), (DEFAULT_RIG * DEFAULT_RIG) as usize);
        assert_eq!(patch.zones[0].mapping_size, [DEFAULT_RIG, DEFAULT_RIG, 1]);
        assert_eq!(patch.matrix_size[..2], [DEFAULT_RIG, DEFAULT_RIG]);
        let universes: std::collections::BTreeSet<u16> = patch
            .fixtures
            .iter()
            .flat_map(|f| f.pixels.iter().map(|p| p.universe))
            .collect();
        assert_eq!(universes.len(), 15, "2500 RGB px at 170 per universe");
        assert_eq!(patch.fixtures[0].name, "Generic RGB 1 pixel #1");
        patch.clone().compile().expect("default patch compiles");

        // Every cell empty and nothing running: cells are assigned lazily (first edit / GO).
        let track = show.home_track(patch.zones[0].id).expect("its own track");
        assert_eq!(track.active, None);
        assert_eq!(track.clips.len(), GRID_CELLS);
        assert!(
            track.clips.iter().all(|c| c.layers.is_empty()),
            "no pre-assigned cell"
        );
    }

    #[test]
    fn pad_grids_fills_short_tracks_and_keeps_authored_clips() {
        // Idempotent: it fills a track with no clips and pads a short one, keeping its
        // authored clips at their index (so a loaded show's real content survives). It
        // never adds tracks: removing a zone's only track must stick.
        let mut show = Show::new("t", empty_patch());
        assert!(show.tracks[0].clips.is_empty());
        let mut authored = Track::new("extra", prisma_engine::Destination::default());
        authored.clips = vec![Cell::empty()];
        authored.clips[0].fade_time = std::time::Duration::from_millis(250);
        show.tracks.push(authored);

        pad_grids(&mut show);
        pad_grids(&mut show);

        assert_eq!(show.tracks.len(), 2, "no track added or removed");
        assert_eq!(show.tracks[0].clips.len(), GRID_CELLS, "empty track seeded");
        assert_eq!(show.tracks[1].clips.len(), GRID_CELLS, "short track padded");
        assert_eq!(
            show.tracks[1].clips[0].fade_time,
            std::time::Duration::from_millis(250),
            "authored clip kept at its index"
        );

        show.tracks.clear();
        pad_grids(&mut show);
        assert!(
            show.tracks.is_empty(),
            "a zone with no track stays without one"
        );
    }

    #[test]
    fn new_tracks_are_named_track_n_with_the_first_free_number() {
        let mut show = Show::new("t", empty_patch());
        assert_eq!(next_track_name(&show), "Track 1");
        show.tracks[0].name = "Track 1".into();
        let mut t = Track::new("Track 3", prisma_engine::Destination::default());
        show.tracks.push(t.clone());
        assert_eq!(next_track_name(&show), "Track 2");
        t.name = "Track 2".into();
        show.tracks.push(t);
        assert_eq!(next_track_name(&show), "Track 4");
    }

    #[test]
    fn a_cell_shows_its_effects_first_colour_group() {
        let mut p = ParamSet::new();
        p.set("r", 0.5);
        p.set("g", 0.25);
        p.set("b", 0.0);
        assert_eq!(effect_main_color("solid", &p), Some([0.5, 0.25, 0.0]));
        assert_eq!(effect_main_color("no-such-effect", &p), None);
    }

    #[test]
    fn tracks_reorder_only_within_their_zone() {
        let za = Uuid::new_v4();
        let zb = Uuid::new_v4();
        let t = |name: &str, z: Uuid| Track::new(name, prisma_engine::Destination::zone(z));
        let mut tracks = vec![t("a1", za), t("a2", za), t("a3", za), t("b1", zb)];
        let ids: Vec<Uuid> = tracks.iter().map(|t| t.id).collect();
        let names = |ts: &[Track]| ts.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        move_track(&mut tracks, ids[2], Some(ids[0]));
        assert_eq!(names(&tracks), ["a3", "a1", "a2", "b1"]);
        move_track(&mut tracks, ids[2], None);
        assert_eq!(
            names(&tracks),
            ["a1", "a2", "a3", "b1"],
            "last of its zone, not of the show"
        );
        move_track(&mut tracks, ids[0], Some(ids[3]));
        assert_eq!(
            names(&tracks),
            ["a1", "a2", "a3", "b1"],
            "another zone: unchanged"
        );
    }
}

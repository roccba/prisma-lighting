//! Prisma desktop shell.
//!
//! Hosts the engine (started once, shut down in order on exit — S1) and serves the
//! patch-editor UI. The editor works against a Rust-side [`PatchDraft`] (the single
//! source of truth, S2); the webview renders it and sends structural edits back as
//! commands. S5 adds the first edits: fixture placement (into the active zone) plus
//! delete / undo / redo, each an undoable [`EditOp`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use glam::Vec3;
use prisma_editor::{
    aggregate, load_library, next_free_address, plan, plan_preview, save_library, EditOp,
    FixtureLibrary, FixtureType, PatchDraft, PlacementSpec, PreviewRun, SelectionAggregate,
};
use prisma_engine::{
    load_show, save_show, Cell, Engine, EngineConfig, EngineHandle, Layer, ParamSet, Show, Source,
    ZoneLiveReadout, ZoneShow,
};
use prisma_patch::{load_patch, save_patch, BlendMode, Diagnostic, Patch, Zone};
use prisma_preview::sink::WebSocketSink;
use prisma_render::{AdapterPreference, ColorChannel, ParamRole};
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
}

/// One scene-grid cell of the selected zone (M8 step 4): empty (assignable) or an
/// assigned effect, and whether it is the zone's running (active) cell.
#[derive(Serialize)]
struct CellInfo {
    index: usize,
    effect_id: Option<String>,
    effect_name: Option<String>,
    active: bool,
}

/// One entry the effect picker offers (screenshot 14), catalogue-driven (§5.3).
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
    /// (a channel of the `group` colour picker), `"direction"` (the rose), `"palette"`
    /// (a channel of palette slot `group`, collected into the palette editor) or
    /// `"palette_count"` (the palette's active length).
    role: &'static str,
    /// A grouping key: for `role == "color"` the colour group id; for `role ==
    /// "palette"` the slot index as a string. `Some` only for those roles.
    group: Option<String>,
    /// The colour channel (`"r"`/`"g"`/`"b"`), `Some` for `role == "color"` or
    /// `"palette"`.
    channel: Option<&'static str>,
}

/// The effect work panel's data for the selected zone (M8 step 5): the zone's active
/// cell effect and its manifest-driven parameters. `effect_id` is `None` when the zone
/// has no running cell (empty selection) — the panel then shows an empty state.
#[derive(Serialize)]
struct EffectPanelInfo {
    zone_id: Uuid,
    cell_index: Option<usize>,
    effect_id: Option<String>,
    effect_name: Option<String>,
    params: Vec<ParamInfo>,
}

/// A display label from a manifest parameter key: capitalise the first letter, leaving
/// the rest as-is. Generic on purpose — the panel stays manifest-driven, with no
/// per-effect label table (§5.3). Real i18n resolution of the manifest's `label_key`
/// is deferred (the UI has no locale resolver yet); this is a legible stand-in.
fn humanize(name: &str) -> String {
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
        // A palette slot reuses `group` to carry its slot index as a string; the panel
        // collects all `palette` params by that index into one editor. `palette_count`
        // is the plain length param the editor reads for the active number of slots.
        ParamRole::Palette { index, channel } => {
            ("palette", Some(index.to_string()), Some(ch_str(channel)))
        }
        ParamRole::PaletteCount => ("palette_count", None, None),
    }
}

/// How many assignable cells each zone's grid seeds with (8 columns × 2 rows). The
/// first cell holds the zone's initial effect; the rest are empty and assignable.
const GRID_CELLS: usize = 16;

/// The picker's display name for an `effect_id`, from the catalogue.
fn effect_display_name(id: &str) -> Option<String> {
    prisma_render::catalogue()
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.name.to_string())
}

/// The scene grid of one zone as cell DTOs.
fn grid_dto(zs: &ZoneShow) -> Vec<CellInfo> {
    zs.grid
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let effect_id = cell.layers.first().map(|l| match &l.source {
                Source::Shader { effect_id, .. } => effect_id.clone(),
            });
            let effect_name = effect_id.as_deref().and_then(effect_display_name);
            CellInfo {
                index: i,
                effect_id,
                effect_name,
                active: zs.active == Some(i),
            }
        })
        .collect()
}

/// The M8 test show: the wedding rig with a distinct effect running per zone, so the
/// zone row's live mini-previews show something worth looking at (not two trivial
/// zones). The editor writes only fixture→zone *ownership*, so the overlap membership
/// the render path needs is derived first ([`Patch::fill_zone_members_from_ownership`]).
fn build_show() -> Show {
    let mut patch = prisma_editor::samples::wedding_rig();
    patch.fill_zone_members_from_ownership();
    patch.fit_matrix_size(); // world = the rig's bounding box, a derived fact (not stored)
    let mut show = Show::new("Wedding", patch);
    // One effect per zone, cycled — enough variety to judge the row at a glance. Each
    // zone gets a grid of assignable cells with its initial effect in cell 0 (active).
    let catalogue = [
        "color_scroll",
        "plasma",
        "fire",
        "noise",
        "radial",
        "sparkle",
    ];
    for (i, zs) in show.zones.iter_mut().enumerate() {
        let effect_id = catalogue[i % catalogue.len()];
        let mut params = ParamSet::new();
        if effect_id == "solid" {
            params.set("r", 0.85);
            params.set("g", 0.25);
            params.set("b", 0.5);
        }
        zs.grid = (0..GRID_CELLS)
            .map(|c| {
                if c == 0 {
                    Cell::single(Layer::shader(effect_id, params.clone()))
                } else {
                    Cell::empty()
                }
            })
            .collect();
        zs.active = Some(0);
        zs.live.dimmer = 0.8; // lit, so the mini-previews are visible
    }
    show
}

/// Everything the shell owns for the life of the process.
struct AppState {
    /// The live output engine (S1). `None` once shut down.
    engine: Mutex<Option<EngineHandle>>,
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
    /// The network output config (device manager, screenshot 16): where DMX is sent.
    network: Mutex<NetworkConfig>,
    /// Where the network config file lives — the per-user app-config dir.
    network_path: PathBuf,
}

/// Network output configuration — the device manager (screenshot 16). It decides WHERE
/// the engine sends DMX: the loopback virtual sink (disabled, the dev default) or a real
/// **Art-Net** target (enabled). Persisted to the per-user app-config dir so it survives
/// restarts. First cut: one Art-Net target for **every** universe — a node that takes them
/// all, or a subnet broadcast address (e.g. `2.255.255.255`); the per-node universe table
/// (screenshot 16) is the richer later feature (CLAUDE.md Deferred). Universe numbers are
/// in the packets, so a broadcast or an all-universes node receives every one.
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

/// The DMX destination for a network config: the configured Art-Net target when enabled
/// and its IP parses, otherwise the loopback virtual sink. Every universe is sent here.
fn resolve_target(net: &NetworkConfig) -> SocketAddr {
    if net.artnet_enabled {
        if let Ok(ip) = net.artnet_ip.trim().parse::<IpAddr>() {
            return SocketAddr::new(ip, net.artnet_port);
        }
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454)
}

/// Write the network config to its file, creating the config directory if needed.
fn persist_network(path: &Path, net: &NetworkConfig) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(net).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
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

/// Place fixtures from the dialog spec (§4). They go into `spec.zone` — the active
/// zone (§4.1/§6). Appends via an undoable `InsertFixtures` op.
#[tauri::command]
fn editor_place_fixtures(
    state: State<AppState>,
    spec: PlacementSpec,
) -> Result<EditorState, String> {
    let mut draft = state.draft.lock().unwrap();
    let fixtures = plan(&spec).map_err(|e| e.to_string())?;
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
/// adopt the edited patch into the live show — keeping each zone's grid/effects, keyed
/// by zone id — and recompile on the same GPU so the show resumes on the new layout.
/// `resume` unfreezes (the editor window closed); `resume == false` keeps it frozen
/// (an explicit "Apply to live" while the editor is still open, §7).
///
/// Because recompile rebuilds the Parameter Tree, live values reset to the show's saved
/// state (a known limitation; live tweaks made since load are not preserved). Zone
/// **add/remove** IS supported: the show's per-zone grids are reconciled to the new
/// patch's zone set (kept by id, empty for a new zone, dropped for a removed one), and a
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
        {
            let guard = state.engine.lock().unwrap();
            if let Some(engine) = guard.as_ref() {
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
                        zs.live.color_filter[i] =
                            z.color[i].read_real().unwrap_or(zs.live.color_filter[i]);
                    }
                    zs.live.speed_multiplier =
                        z.speed.read_real().unwrap_or(zs.live.speed_multiplier);
                    zs.live.solo = z.solo.read_real().map(|v| v > 0.5).unwrap_or(zs.live.solo);
                    zs.live.blackout = z
                        .blackout
                        .read_real()
                        .map(|v| v > 0.5)
                        .unwrap_or(zs.live.blackout);
                }
            }
        }
        // Reconcile the per-zone grids to the new zone set: one ZoneShow per patch zone,
        // in patch order, keeping an existing zone's grid/live by id, an empty one for a
        // newly added zone, and dropping a removed zone's. This keeps `Show` a bijection
        // with its patch (what the engine and `Show::validate` expect).
        let mut old = std::mem::take(&mut show.zones);
        show.zones = patch
            .zones
            .iter()
            .map(|pz| match old.iter().position(|zs| zs.zone_id == pz.id) {
                Some(pos) => old.remove(pos),
                // A newly added zone gets a grid of assignable (empty) cells, like the
                // seeded zones — otherwise its scene grid would render nothing.
                None => {
                    let mut zs = ZoneShow::new(pz.id);
                    zs.grid = (0..GRID_CELLS).map(|_| Cell::empty()).collect();
                    // A freshly created zone starts LIT (dimmer full) — master defaults to
                    // full too, so the operator sees the zone's content the moment they
                    // assign an effect, instead of a black zone that looks broken.
                    zs.live.dimmer = 1.0;
                    zs
                }
            })
            .collect();
        show.patch = patch;
        show.clone()
    };
    // A FRESH preview WebSocket on a new port: port 0 never collides with the still-
    // running old engine's sink, and recompile drops the old one on teardown. The
    // viewer reconnects to the new port (re-queried on reconnect).
    let target = resolve_target(&state.network.lock().unwrap());
    let (config, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    match engine.recompile_show(show, config) {
        Ok(new) => {
            new.set_frozen(!resume);
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

/// Save the current live show to a `.prisma` file (M8-SPEC §5): serialize the Show — with
/// its embedded Patch — through the versioned envelope. The frontend picks the path with
/// the native dialog.
#[tauri::command]
fn main_save_show(state: State<AppState>, path: String) -> Result<(), String> {
    let show = state.show.lock().unwrap();
    let json = save_show(&show).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
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

    let target = resolve_target(&state.network.lock().unwrap());
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
                });
            ZoneInfo {
                id,
                name,
                color,
                dimmer: live.dimmer,
                speed: live.speed,
                solo: live.solo,
                blackout: live.blackout,
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

/// Set a zone's strobe: `rate` in Hz (0 = off), `duty` 0..1 (M8-SPEC §3.6). Gates the
/// zone's non-overlapped pixels at the output stage.
#[tauri::command]
fn main_set_zone_strobe(state: State<AppState>, zone: Uuid, rate: f32, duty: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_strobe(slot, rate, duty);
    }
}

// --- Device manager (network output, screenshot 16) ---

/// The current network output config, for the device manager UI.
#[tauri::command]
fn main_network_get(state: State<AppState>) -> NetworkConfig {
    state.network.lock().unwrap().clone()
}

/// Apply a network output config: persist it, then **recompile the engine onto the new
/// target** (reusing the GPU) so DMX starts flowing to the configured Art-Net node
/// instead of the virtual sink — the point of the device manager (screenshot 16). Live
/// values are folded into the show first so the retarget does not reset faders. A bad IP
/// falls back to the virtual sink (never leaves the engine down).
#[tauri::command]
fn main_network_set(app: AppHandle, config: NetworkConfig) -> Result<(), String> {
    let state = app.state::<AppState>();
    persist_network(&state.network_path, &config)?;
    *state.network.lock().unwrap() = config.clone();

    let target = resolve_target(&config);
    let show = {
        let mut show = state.show.lock().unwrap();
        if let Some(engine) = state.engine.lock().unwrap().as_ref() {
            fold_live_into_show(engine, &mut show);
        }
        show.clone()
    };
    let (cfg, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    match engine.recompile_show(show, cfg) {
        Ok(new) => {
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
            drop(guard);
            // The preview WebSocket moved to a fresh port; nudge the UI to re-fetch.
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

// --- Main operation window (M8 step 6): the global operation bar ---

/// The global bar's current master state, so the bar reflects what the engine holds
/// (a loaded show, or another transport) rather than only the UI's own writes.
#[derive(Serialize)]
struct MasterInfo {
    dimmer: f32,
    blackout: bool,
    strobe_rate: f32,
    strobe_duty: f32,
}

/// Read the master section (dimmer, blackout, global strobe) for the global bar.
#[tauri::command]
fn main_master(state: State<AppState>) -> MasterInfo {
    let guard = state.engine.lock().unwrap();
    let Some(engine) = guard.as_ref() else {
        return MasterInfo {
            dimmer: 1.0,
            blackout: false,
            strobe_rate: 0.0,
            strobe_duty: 0.5,
        };
    };
    let m = engine.live().master();
    MasterInfo {
        dimmer: m.dimmer.read_real().unwrap_or(1.0),
        blackout: m.blackout.read_real().unwrap_or(0.0) > 0.5,
        strobe_rate: m.strobe.rate.read_real().unwrap_or(0.0),
        strobe_duty: m.strobe.duty.read_real().unwrap_or(0.5),
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

/// Set the global strobe: `rate` Hz (0 = off), `duty` 0..1.
#[tauri::command]
fn main_set_master_strobe(state: State<AppState>, rate: f32, duty: f32) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_strobe(rate, duty);
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
#[tauri::command]
fn main_open_editor(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("editor") {
        let _ = win.set_focus();
        return Ok(());
    }
    // Freeze the show content while the editor is open (§7).
    if let Some(engine) = app.state::<AppState>().engine.lock().unwrap().as_ref() {
        engine.set_frozen(true);
    }
    let win = WebviewWindowBuilder::new(&app, "editor", WebviewUrl::App("index.html".into()))
        .title("Prisma — Editor de Patch")
        .inner_size(1280.0, 800.0)
        .min_inner_size(1280.0, 800.0)
        .maximized(true)
        .build()
        .map_err(|e| e.to_string())?;

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
            }
        }
    });
    Ok(())
}

// --- Main operation window (M8 step 4): the scene grid ---

/// The effects the picker offers (screenshot 14), grouped by category — the M5 GPU
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

/// The scene grid of a zone (its assignable cells and which is running).
#[tauri::command]
fn main_zone_grid(state: State<AppState>, zone: Uuid) -> Vec<CellInfo> {
    let show = state.show.lock().unwrap();
    show.zone(zone).map(grid_dto).unwrap_or_default()
}

/// Assign an effect to a cell AND fire it (picking an effect for a cell plays it):
/// fill the cell's layer, make it the zone's active cell, and trigger it live through
/// the frame-boundary queue. Returns the updated grid.
#[tauri::command]
fn main_assign_cell(
    state: State<AppState>,
    zone: Uuid,
    index: usize,
    effect_id: String,
) -> Vec<CellInfo> {
    let Some(slot) = zone_slot(&state, zone) else {
        return Vec::new();
    };
    let params = ParamSet::new(); // default params in step 4; parameter editing is step 5
    let dto = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.zones.iter_mut().find(|z| z.zone_id == zone) else {
            return Vec::new();
        };
        if index >= zs.grid.len() {
            return grid_dto(zs);
        }
        zs.grid[index] = Cell::single(Layer::shader(effect_id.clone(), params.clone()));
        zs.active = Some(index);
        grid_dto(zs)
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.trigger_zone_effect(slot, zone, &effect_id, &params);
    }
    dto
}

/// Trigger an already-assigned cell (make it the zone's active content). An empty
/// cell clears the zone (stops it). Returns the updated grid.
#[tauri::command]
fn main_trigger_cell(state: State<AppState>, zone: Uuid, index: usize) -> Vec<CellInfo> {
    let Some(slot) = zone_slot(&state, zone) else {
        return Vec::new();
    };
    let (dto, effect) = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.zones.iter_mut().find(|z| z.zone_id == zone) else {
            return Vec::new();
        };
        if index >= zs.grid.len() {
            return grid_dto(zs);
        }
        zs.active = Some(index);
        let effect = zs.grid[index].layers.first().map(|l| match &l.source {
            Source::Shader { effect_id, params } => (effect_id.clone(), params.clone()),
        });
        (grid_dto(zs), effect)
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        match effect {
            Some((id, params)) => {
                engine.trigger_zone_effect(slot, zone, &id, &params);
            }
            None => {
                engine.clear_zone_effect(slot);
            }
        }
    }
    dto
}

/// Empty a cell. If it was the running cell, the zone stops (clears its output).
#[tauri::command]
fn main_clear_cell(state: State<AppState>, zone: Uuid, index: usize) -> Vec<CellInfo> {
    let Some(slot) = zone_slot(&state, zone) else {
        return Vec::new();
    };
    let (dto, was_active) = {
        let mut show = state.show.lock().unwrap();
        let Some(zs) = show.zones.iter_mut().find(|z| z.zone_id == zone) else {
            return Vec::new();
        };
        if index >= zs.grid.len() {
            return grid_dto(zs);
        }
        zs.grid[index] = Cell::empty();
        let was_active = zs.active == Some(index);
        if was_active {
            zs.active = None;
        }
        (grid_dto(zs), was_active)
    };
    if was_active {
        if let Some(engine) = state.engine.lock().unwrap().as_ref() {
            engine.clear_zone_effect(slot);
        }
    }
    dto
}

// --- Main operation window (M8 step 5): the effect parameter panel ---

/// The effect work panel for a zone (§3.5): its active cell's effect and the effect's
/// manifest-driven parameters, each merged with the cell's stored value (else the
/// manifest default). Empty (`effect_id: None`) when the zone has no running cell.
#[tauri::command]
fn main_effect_panel(state: State<AppState>, zone: Uuid) -> EffectPanelInfo {
    let show = state.show.lock().unwrap();
    let empty = |cell_index| EffectPanelInfo {
        zone_id: zone,
        cell_index,
        effect_id: None,
        effect_name: None,
        params: Vec::new(),
    };
    let Some(zs) = show.zone(zone) else {
        return empty(None);
    };
    // The active cell's single layer (M8: one effect per cell). An active-but-empty
    // cell (cleared) has no layer, so the panel shows its empty state.
    let effect = zs
        .active
        .and_then(|i| zs.grid.get(i))
        .and_then(|c| c.layers.first())
        .map(|l| match &l.source {
            Source::Shader { effect_id, params } => (effect_id.clone(), params),
        });
    let Some((effect_id, stored)) = effect else {
        return empty(zs.active);
    };
    let params = prisma_render::effect_params(&effect_id)
        .into_iter()
        .map(|p| {
            let (role, group, channel) = role_dto(&p.role);
            ParamInfo {
                value_norm: stored.0.get(&p.name).copied().unwrap_or(p.default_norm),
                label: humanize(&p.name),
                name: p.name,
                min: p.min,
                max: p.max,
                role,
                group,
                channel,
            }
        })
        .collect();
    EffectPanelInfo {
        zone_id: zone,
        cell_index: zs.active,
        effect_name: effect_display_name(&effect_id),
        effect_id: Some(effect_id),
        params,
    }
}

/// Set one parameter of a zone's active effect (§3.5). Two writes, both needed:
/// persist the normalized value into the show (source of truth for save and for a
/// later re-trigger), and drive it **live** on the running effect through the engine
/// (so the preview and wire follow at once). `value` is normalized `0..=1`.
#[tauri::command]
fn main_set_param(state: State<AppState>, zone: Uuid, name: String, value: f32) {
    {
        let mut show = state.show.lock().unwrap();
        if let Some(zs) = show.zones.iter_mut().find(|z| z.zone_id == zone) {
            if let Some(cell) = zs.active.and_then(|i| zs.grid.get_mut(i)) {
                if let Some(layer) = cell.layers.first_mut() {
                    let Source::Shader { params, .. } = &mut layer.source;
                    params.set(name.as_str(), value);
                }
            }
        }
    }
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_param(zone, &name, value);
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
            editor_preview_placement,
            editor_place_fixtures,
            editor_translate_fixtures,
            editor_set_zone_mapping_size,
            editor_add_zone,
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
            main_network_get,
            main_network_set,
            main_set_zone_solo,
            main_set_zone_blackout,
            main_set_zone_strobe,
            main_master,
            main_set_master_dimmer,
            main_set_master_blackout,
            main_set_master_strobe,
            main_open_editor,
            main_save_show,
            main_open_show,
            main_effect_catalogue,
            main_zone_grid,
            main_assign_cell,
            main_trigger_cell,
            main_clear_cell,
            main_effect_panel,
            main_set_param,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // M8: the app boots into the main operation window running a real Show (the
            // wedding rig with a distinct effect per zone). This is where the engine
            // stops rendering IdleContent and runs per-zone content — the zone row's
            // live mini-previews light up. The editor draft below stays for the patch
            // editor (a secondary window in a later step).
            let show = build_show();
            // Keep an editable copy as the grid's + zone-set source of truth; the engine
            // runs its own copy and follows our triggers through the queue.
            let show_state = show.clone();

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

            // Prefer the real GPU, fall back to software so the window still opens without
            // a configured Vulkan driver. Each attempt binds its own preview WebSocket (a
            // failed attempt frees its port before the next binds).
            let target = resolve_target(&network);
            let (config_hp, port_hp) = preview_config(AdapterPreference::HighPerformance, target);
            let (engine, preview_port) = match Engine::start_show(show.clone(), config_hp) {
                Ok(engine) => (engine, port_hp),
                Err(_) => {
                    let (config_sw, port_sw) = preview_config(AdapterPreference::Software, target);
                    (Engine::start_show(show, config_sw)?, port_sw)
                }
            };

            // The show runs LIVE (not frozen) — the main window operates content. The
            // editor's freeze bracket returns with the editor window in a later step.

            // The editor draft opens on the same rig (its own copy; the editor works in
            // ownership terms and never needs the derived overlap membership).
            let patch = prisma_editor::samples::wedding_rig();

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

            app.manage(AppState {
                engine: Mutex::new(Some(engine)),
                show: Mutex::new(show_state),
                draft: Mutex::new(PatchDraft::new(patch)),
                library: Mutex::new(library),
                library_path,
                preview_port: Mutex::new(preview_port.unwrap_or(0)),
                network: Mutex::new(network),
                network_path,
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Prisma app")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
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

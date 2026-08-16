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
    Cell, Engine, EngineConfig, EngineHandle, Layer, ParamSet, Show, ZoneLiveReadout,
};
use prisma_patch::{load_patch, save_patch, Diagnostic, Patch};
use prisma_preview::sink::WebSocketSink;
use prisma_render::AdapterPreference;
use serde::Serialize;
use tauri::{Manager, RunEvent, State};
use uuid::Uuid;

/// A zone's identity for the main operation window (M8 step 3), in `Patch::zones`
/// order — the slot the engine's per-zone live controls are addressed by.
struct ZoneMeta {
    id: Uuid,
    name: String,
    color: [u8; 3],
}

/// A zone card's data for the zone row: identity + its current live controls.
#[derive(Serialize)]
struct ZoneInfo {
    id: Uuid,
    name: String,
    color: [u8; 3],
    dimmer: f32,
    solo: bool,
    blackout: bool,
}

/// The M8 test show: the wedding rig with a distinct effect running per zone, so the
/// zone row's live mini-previews show something worth looking at (not two trivial
/// zones). The editor writes only fixture→zone *ownership*, so the overlap membership
/// the render path needs is derived first ([`Patch::fill_zone_members_from_ownership`]).
fn build_show() -> Show {
    let mut patch = prisma_editor::samples::wedding_rig();
    patch.fill_zone_members_from_ownership();
    let mut show = Show::new("Wedding", patch);
    // One effect per zone, cycled — enough variety to judge the row at a glance.
    let catalogue = ["plasma", "wave", "gradient", "solid"];
    for (i, zs) in show.zones.iter_mut().enumerate() {
        let effect_id = catalogue[i % catalogue.len()];
        let mut params = ParamSet::new();
        if effect_id == "solid" {
            params.set("r", 0.85);
            params.set("g", 0.25);
            params.set("b", 0.5);
        }
        zs.grid.push(Cell::single(Layer::shader(effect_id, params)));
        zs.active = Some(0);
        zs.live.dimmer = 0.8; // lit, so the mini-previews are visible
    }
    show
}

/// Everything the shell owns for the life of the process.
struct AppState {
    /// The live output engine (S1). `None` once shut down.
    engine: Mutex<Option<EngineHandle>>,
    /// The show's zones in slot order (M8 step 3): identity for the zone row, and the
    /// slot each zone's live controls are addressed by.
    zones: Vec<ZoneMeta>,
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

/// Apply the current editor draft to the live engine (§7 / S9): recompile the patch,
/// reusing the GPU. The show "resumes" on the new layout. In M7 (app == editor) this is
/// an explicit action; in M8 the same recompile runs when the editor window closes.
#[tauri::command]
fn editor_apply_to_live(state: State<AppState>) -> Result<(), String> {
    // Clone the draft patch first so we do not hold the draft lock across the recompile.
    let patch = state.draft.lock().unwrap().patch().clone();
    // A FRESH preview WebSocket on a new port: binding port 0 never collides with the
    // still-running old engine's sink, and recompile drops the old one on teardown. The
    // viewer reconnects to the new port (re-queried on close).
    let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454);
    let (config, port) = preview_config(AdapterPreference::Software, target);
    let mut guard = state.engine.lock().unwrap();
    let engine = guard.take().ok_or("engine is not running")?;
    match engine.recompile(patch, config) {
        Ok(new) => {
            new.set_frozen(true); // editor still open — keep content frozen (§7)
            *guard = Some(new);
            if let Some(port) = port {
                *state.preview_port.lock().unwrap() = port;
            }
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

/// The slot (index in `Patch::zones`) of a zone id, or `None` if unknown.
fn zone_slot(zones: &[ZoneMeta], id: Uuid) -> Option<usize> {
    zones.iter().position(|z| z.id == id)
}

/// Every zone with its identity and current live controls, for the zone row. The
/// dimmer/solo/blackout are read live from the engine so the cards reflect state set
/// by any transport (or the loaded show), not just the UI's own writes.
#[tauri::command]
fn main_zones(state: State<AppState>) -> Vec<ZoneInfo> {
    let guard = state.engine.lock().unwrap();
    state
        .zones
        .iter()
        .enumerate()
        .map(|(slot, z)| {
            let live = guard
                .as_ref()
                .map(|e| e.zone_live(slot))
                .unwrap_or(ZoneLiveReadout {
                    dimmer: 0.0,
                    solo: false,
                    blackout: false,
                });
            ZoneInfo {
                id: z.id,
                name: z.name.clone(),
                color: z.color,
                dimmer: live.dimmer,
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
    let Some(slot) = zone_slot(&state.zones, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_dimmer(slot, value);
    }
}

/// Toggle a zone's solo (M8-SPEC §3.1).
#[tauri::command]
fn main_set_zone_solo(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state.zones, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_solo(slot, on);
    }
}

/// Toggle a zone's blackout/mute (M8-SPEC §3.1).
#[tauri::command]
fn main_set_zone_blackout(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state.zones, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_blackout(slot, on);
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
            main_set_zone_solo,
            main_set_zone_blackout,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // M8: the app boots into the main operation window running a real Show (the
            // wedding rig with a distinct effect per zone). This is where the engine
            // stops rendering IdleContent and runs per-zone content — the zone row's
            // live mini-previews light up. The editor draft below stays for the patch
            // editor (a secondary window in a later step).
            let show = build_show();
            let zones: Vec<ZoneMeta> = show
                .patch
                .zones
                .iter()
                .map(|z| ZoneMeta {
                    id: z.id,
                    name: z.name.clone(),
                    color: z.display_color,
                })
                .collect();

            // Loopback Art-Net (not broadcast); prefer the real GPU, fall back to
            // software so the window still opens without a configured Vulkan driver.
            // Each attempt binds its own preview WebSocket (a failed attempt frees its
            // port before the next binds).
            let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454);
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
                zones,
                draft: Mutex::new(PatchDraft::new(patch)),
                library: Mutex::new(library),
                library_path,
                preview_port: Mutex::new(preview_port.unwrap_or(0)),
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

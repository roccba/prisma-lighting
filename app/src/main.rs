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
use prisma_engine::{Engine, EngineConfig, EngineHandle};
use prisma_patch::{load_patch, save_patch, Diagnostic, Patch};
use prisma_render::AdapterPreference;
use serde::Serialize;
use tauri::{Manager, RunEvent, State};
use uuid::Uuid;

/// Everything the shell owns for the life of the process.
struct AppState {
    /// The live output engine (S1). `None` once shut down.
    engine: Mutex<Option<EngineHandle>>,
    /// The editor's working patch and its undo history (S2).
    draft: Mutex<PatchDraft>,
    /// The operator's reusable fixture library (S7, §3.2). Feeds the placement dialog
    /// and the library manager; persisted to [`library_path`](AppState::library_path)
    /// on every edit.
    library: Mutex<FixtureLibrary>,
    /// Where the library file lives — the per-user app-config dir, not the project or
    /// the `.prismap`.
    library_path: PathBuf,
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
fn editor_selection_aggregate(state: State<AppState>, ids: Vec<Uuid>) -> Option<SelectionAggregate> {
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
fn editor_library_add_type(state: State<AppState>, ty: FixtureType) -> Result<FixtureLibrary, String> {
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
fn editor_library_remove_type(state: State<AppState>, index: usize) -> Result<FixtureLibrary, String> {
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
fn editor_place_fixtures(state: State<AppState>, spec: PlacementSpec) -> Result<EditorState, String> {
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
            editor_remove_fixtures,
            editor_save_to,
            editor_open_from,
            editor_undo,
            editor_redo,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The editor opens on the sample wedding rig — a realistic layout
            // (four zones, varied tube lengths, two diagonal bar runs).
            let patch = prisma_editor::samples::wedding_rig();

            // Start the live engine on the same patch. Loopback Art-Net (not
            // broadcast); prefer the real GPU, fall back to software so the window
            // still opens without a configured Vulkan driver.
            let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454);
            let engine = match Engine::start(
                patch.clone(),
                EngineConfig::new(AdapterPreference::HighPerformance, target),
            ) {
                Ok(engine) => engine,
                Err(_) => Engine::start(
                    patch.clone(),
                    EngineConfig::new(AdapterPreference::Software, target),
                )?,
            };

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
                draft: Mutex::new(PatchDraft::new(patch)),
                library: Mutex::new(library),
                library_path,
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

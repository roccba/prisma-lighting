//! Prisma desktop shell.
//!
//! Hosts the engine (started once, shut down in order on exit — S1) and serves the
//! patch-editor UI. The editor works against a Rust-side [`PatchDraft`] (the single
//! source of truth, S2); the webview renders it and sends structural edits back as
//! commands. S5 adds the first edits: fixture placement (into the active zone) plus
//! delete / undo / redo, each an undoable [`EditOp`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use glam::Vec3;
use prisma_editor::{
    aggregate, builtin_fixture_types, next_free_address, plan, plan_preview, EditOp, FixtureType,
    PatchDraft, PlacementSpec, PreviewRun, SelectionAggregate,
};
use prisma_engine::{Engine, EngineConfig, EngineHandle};
use prisma_patch::Patch;
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
}

/// The editor snapshot the UI renders after any command: the patch plus the
/// history/dirty flags (toolbar state) and the current validation status. Validation
/// is surfaced, never enforced — the editor shows problems, it does not block edits
/// (§4.3/§4.4). Collecting *all* problems for the canvas overlay is S6; this is the
/// model's first-error string for now.
#[derive(Serialize)]
struct EditorState {
    patch: Patch,
    can_undo: bool,
    can_redo: bool,
    dirty: bool,
    validation: Option<String>,
}

fn state_of(draft: &PatchDraft) -> EditorState {
    EditorState {
        patch: draft.patch().clone(),
        can_undo: draft.can_undo(),
        can_redo: draft.can_redo(),
        dirty: draft.is_dirty(),
        validation: draft.validate().err().map(|e| e.to_string()),
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

/// The built-in fixture types offered by the placement dialog (§3.2 library later).
#[tauri::command]
fn editor_fixture_types() -> Vec<FixtureType> {
    builtin_fixture_types()
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
            editor_next_free_address,
            editor_preview_placement,
            editor_place_fixtures,
            editor_translate_fixtures,
            editor_remove_fixtures,
            editor_undo,
            editor_redo,
        ])
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

            app.manage(AppState {
                engine: Mutex::new(Some(engine)),
                draft: Mutex::new(PatchDraft::new(patch)),
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

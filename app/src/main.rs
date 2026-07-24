//! Prisma desktop shell.
//!
//! Hosts the engine (started once, shut down in order on exit — S1) and, from S3,
//! serves the patch-editor UI and the read/query commands it calls. The editor
//! works against a Rust-side [`PatchDraft`] (the single source of truth, S2); the
//! webview renders it and sends structural edits back as commands (edit commands
//! land with the placement/edit UI, S5). S3 is view + selection only.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use prisma_editor::{aggregate, PatchDraft, SelectionAggregate};
use prisma_engine::{Engine, EngineConfig, EngineHandle};
use prisma_patch::Patch;
use prisma_render::AdapterPreference;
use tauri::{Manager, RunEvent, State};
use uuid::Uuid;

/// Everything the shell owns for the life of the process.
struct AppState {
    /// The live output engine (S1). `None` once shut down.
    engine: Mutex<Option<EngineHandle>>,
    /// The editor's working patch and its undo history (S2).
    draft: Mutex<PatchDraft>,
}

// --- IPC command surface (S3: read + query) ---

/// The editor's working patch, as geometry for the canvas and lists.
#[tauri::command]
fn editor_get_patch(state: State<AppState>) -> Patch {
    state.draft.lock().unwrap().patch().clone()
}

/// Aggregate properties of a fixture selection (§5.4), or `null` if empty.
#[tauri::command]
fn editor_selection_aggregate(state: State<AppState>, ids: Vec<Uuid>) -> Option<SelectionAggregate> {
    let draft = state.draft.lock().unwrap();
    aggregate(draft.patch(), &ids)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            editor_get_patch,
            editor_selection_aggregate
        ])
        .setup(|app| {
            // The editor opens on the sample wedding rig — a realistic layout
            // (four zones, varied tube lengths, two diagonal bar runs) so the
            // canvas is worth judging.
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
                // App-side half of the S1 shutdown criterion: stop the engine in
                // order before the process exits.
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

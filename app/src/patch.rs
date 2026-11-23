//! The patch editor: its commands over the editable draft, the fixture library, applying the
//! edits to the running show, and the editor window.

use super::*;

/// The editor snapshot the UI renders after any command: the patch plus the
/// history/dirty flags (toolbar state) and the patch's physical DMX **wire faults**.
/// Faults are surfaced, never enforced — the editor marks them on the canvas and shows
/// a discreet notice, it does not block edits (§1, §4.3). `diagnostics` carries each
/// fault (an address collision or an out-of-range channel) annotated with the pixels it
/// points at, so a duplicated channel among thousands of pixels cannot hide. Out-of-
/// matrix positions and shared cells are valid choices and are not reported (§2.4/§4.4).
#[derive(Serialize)]
pub(crate) struct EditorState {
    pub(crate) patch: Patch,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    pub(crate) dirty: bool,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

pub(crate) fn state_of(draft: &PatchDraft) -> EditorState {
    EditorState {
        patch: draft.patch().clone(),
        can_undo: draft.can_undo(),
        can_redo: draft.can_redo(),
        dirty: draft.is_dirty(),
        diagnostics: draft.wire_faults(),
    }
}

#[tauri::command]
pub fn editor_get_state(state: State<AppState>) -> EditorState {
    state_of(&state.draft.lock().unwrap())
}

/// Aggregate properties of a fixture selection (§5.4), or `null` if empty.
#[tauri::command]
pub fn editor_selection_aggregate(
    state: State<AppState>,
    ids: Vec<Uuid>,
) -> Option<SelectionAggregate> {
    aggregate(state.draft.lock().unwrap().patch(), &ids)
}

/// The fixture types the placement dialog offers — the operator's library (§3.2).
#[tauri::command]
pub fn editor_fixture_types(state: State<AppState>) -> Vec<FixtureType> {
    state.library.lock().unwrap().types.clone()
}

/// The whole fixture library, for the manager UI (§3.2).
#[tauri::command]
pub fn editor_library_get(state: State<AppState>) -> FixtureLibrary {
    state.library.lock().unwrap().clone()
}

/// Add a fixture type to the library; persist and return the updated library.
#[tauri::command]
pub fn editor_library_add_type(
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
pub fn editor_library_update_type(
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
pub fn editor_library_remove_type(
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
pub fn editor_next_free_address(state: State<AppState>) -> [u16; 2] {
    let (u, c) = next_free_address(state.draft.lock().unwrap().patch());
    [u, c]
}

/// The next free fixture ID, to prefill "Iniciar ID de instalación".
#[tauri::command]
pub fn editor_next_fixture_id(state: State<AppState>) -> u32 {
    next_fixture_id(state.draft.lock().unwrap().patch())
}

/// A placement error in the operator's words (the dialog shows it verbatim).
pub(crate) fn placement_error_text(e: &PlacementError) -> String {
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
pub fn editor_place_fixtures(
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
pub fn editor_preview_placement(spec: PlacementSpec) -> Vec<PreviewRun> {
    plan_preview(&spec)
}

/// Translate fixtures by a cell delta — the canvas drag-move (§5.5). One undoable
/// op for the whole selection, so a drag is a single undo step.
#[tauri::command]
pub fn editor_translate_fixtures(
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
pub fn editor_set_zone_mapping_size(
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
pub(crate) const ZONE_COLORS: [[u8; 3]; 6] = [
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
pub fn editor_add_zone(state: State<AppState>, name: String) -> Result<EditorState, String> {
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
pub fn editor_rename_zone(
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
pub fn editor_remove_zone(state: State<AppState>, zone: Uuid) -> Result<EditorState, String> {
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
pub fn editor_highlight(state: State<AppState>, ids: Vec<Uuid>, blink: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_mut() {
        engine.highlight(&ids, blink);
    }
}

/// The TCP port the live-preview WebSocket is bound to (§M6/S10). The React preview
/// connects to `ws://127.0.0.1:{port}` and re-queries on reconnect (the port changes
/// when a recompile rebinds a fresh one).
#[tauri::command]
pub fn editor_preview_port(state: State<AppState>) -> u16 {
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
pub(crate) fn apply_editor_draft(app: &AppHandle, resume: bool) -> Result<(), String> {
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
pub fn editor_apply_to_live(app: AppHandle) -> Result<(), String> {
    apply_editor_draft(&app, false)
}

/// Delete the given fixtures (undoable).
#[tauri::command]
pub fn editor_remove_fixtures(
    state: State<AppState>,
    ids: Vec<Uuid>,
) -> Result<EditorState, String> {
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
pub fn editor_save_to(state: State<AppState>, path: String) -> Result<EditorState, String> {
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
pub fn editor_open_from(state: State<AppState>, path: String) -> Result<EditorState, String> {
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let patch = load_patch(&json).map_err(|e| e.to_string())?;
    let mut draft = state.draft.lock().unwrap();
    *draft = PatchDraft::new(patch);
    Ok(state_of(&draft))
}

/// The patch-editor window, opened to MATCH the main window: on its monitor, fullscreen
/// if it is fullscreen (maximized otherwise), and as its child so it stays above it and
/// travels with it. A plain maximized window left a fullscreen operator with an editor
/// that had a title bar and the desktop panel, possibly on the other monitor.
/// (On Wayland the compositor ignores client positions; the parent link is what keeps the
/// editor with the main window there.)
pub(crate) fn editor_window_builder(
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
pub async fn main_open_editor(app: AppHandle) -> Result<(), String> {
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

#[tauri::command]
pub fn editor_undo(state: State<AppState>) -> EditorState {
    let mut draft = state.draft.lock().unwrap();
    draft.undo();
    state_of(&draft)
}

#[tauri::command]
pub fn editor_redo(state: State<AppState>) -> EditorState {
    let mut draft = state.draft.lock().unwrap();
    draft.redo();
    state_of(&draft)
}

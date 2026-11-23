//! Control (LIVE-MODEL-SPEC §8.2): the selection in the tree and the keyboard shortcuts.

use super::*;

/// The selection, as the UI receives it.
#[derive(Serialize, Clone, Copy)]
pub(crate) struct SelectionInfo {
    pub(crate) track: Option<Uuid>,
    /// `None` = the track's running clip.
    pub(crate) clip: Option<usize>,
}

/// Select a track and one of its clips (`None` = its running clip) — a click. Goes
/// through the tree (`/select/track`, `/select/clip`) so the keyboard and a controller's
/// knobs share it. Never changes output.
#[tauri::command]
pub fn main_select(state: State<AppState>, track: Option<Uuid>, clip: Option<usize>) {
    let slot = track.and_then(|t| track_slot(&state, t));
    if let Some(engine) = state.engine.locked().as_ref() {
        engine.select(slot, slot.and(clip));
    }
}

/// The selection as the engine holds it.
#[tauri::command]
pub fn main_selection(state: State<AppState>) -> SelectionInfo {
    let (slot, clip) = state
        .engine
        .locked()
        .as_ref()
        .map_or((None, None), |e| e.selection());
    let track = slot.and_then(|s| state.show.locked().tracks.get(s).map(|t| t.id));
    SelectionInfo { track, clip }
}

/// A key press in the main window. The decision — is a shortcut bound to this combo,
/// and is the focus somewhere a shortcut may fire — is `prisma_engine::keys::dispatch`,
/// in one place; the UI only reports the combo and what kind of element has the focus.
/// A tree path is activated here; a window action (`ui:…`, e.g. hide the parameter
/// panel) is returned for the UI to perform. `None` = no shortcut ran.
#[tauri::command]
pub fn main_key(app: AppHandle, combo: KeyCombo, focus: FocusKind) -> Option<String> {
    let state = app.state::<AppState>();
    let keys = state.midi.keys();
    let path = prisma_engine::keys::dispatch(&keys, &combo, focus)?;
    if prisma_engine::keys::is_ui_action(path) {
        return Some(path.to_string());
    }
    let done = state
        .engine
        .locked()
        .as_ref()
        .is_some_and(|e| e.activate_path(path));
    if !done {
        return None;
    }
    // Same refresh the UI does after a controller moved something (blackout button).
    let _ = app.emit("midi-applied", ());
    Some(path.to_string())
}

/// The operator's keyboard shortcuts.
#[tauri::command]
pub fn control_keys(state: State<AppState>) -> Vec<KeyBinding> {
    state.midi.keys()
}

/// Bind `path` to `combo` (`None` = no key), moving the combo off any other action.
/// Refuses the menu's keys.
#[tauri::command]
pub fn control_set_key(
    app: AppHandle,
    path: String,
    combo: Option<KeyCombo>,
) -> Result<Vec<KeyBinding>, String> {
    let state = app.state::<AppState>();
    let profile = state
        .midi
        .rebind_key(&path, combo)
        .ok_or("esa tecla la usa el menú")?;
    persist_mappings(&app, &profile)?;
    Ok(profile.keys)
}

/// Back to the default shortcuts.
#[tauri::command]
pub fn control_reset_keys(app: AppHandle) -> Result<Vec<KeyBinding>, String> {
    let state = app.state::<AppState>();
    let profile = state.midi.reset_keys();
    persist_mappings(&app, &profile)?;
    Ok(profile.keys)
}

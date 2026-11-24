//! The Grupos tab (LIVE-MODEL-SPEC §8.5 step 4): every group master as a fader, and the
//! group presets as buttons — apply, store the current masters, rename, re-record, fade,
//! delete. Preset edits change the show and reload the engine's presets in place (no
//! recompile: nothing else restarts).

use super::*;

/// One group as the tab shows it.
#[derive(Serialize)]
pub(crate) struct GroupMasterInfo {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) color: [u8; 3],
    /// Normalized, as the fader shows it (live when the engine runs).
    pub(crate) level: f32,
}

/// One preset as the tab shows it.
#[derive(Serialize)]
pub(crate) struct GroupPresetInfo {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    /// Seconds (0 = cut).
    pub(crate) fade: f64,
}

/// The whole tab.
#[derive(Serialize)]
pub(crate) struct GroupsInfo {
    pub(crate) groups: Vec<GroupMasterInfo>,
    pub(crate) presets: Vec<GroupPresetInfo>,
}

/// The tab for `show`, with the masters' live levels from `levels` (`None` = the show's
/// stored levels). Pure: the UI test fixture is built from it.
pub(crate) fn groups_info(show: &Show, levels: Option<&[f32]>) -> GroupsInfo {
    let stored = |id: Uuid| {
        show.group_masters
            .iter()
            .find(|m| m.group == id)
            .map_or(1.0, |m| m.level)
    };
    GroupsInfo {
        groups: show
            .patch
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| GroupMasterInfo {
                id: g.id,
                name: g.name.clone(),
                color: g.display_color,
                level: levels
                    .and_then(|l| l.get(i).copied())
                    .unwrap_or(stored(g.id)),
            })
            .collect(),
        presets: show
            .group_presets
            .iter()
            .map(|p| GroupPresetInfo {
                id: p.id,
                name: p.name.clone(),
                fade: p.fade,
            })
            .collect(),
    }
}

#[tauri::command]
pub fn main_groups(state: State<AppState>) -> GroupsInfo {
    let show = state.show.locked();
    let levels = state.engine.locked().as_ref().map(|e| e.group_levels());
    groups_info(&show, levels.as_deref())
}

/// Move a group master (the tab's fader), normalized.
#[tauri::command]
pub fn main_set_group_level(state: State<AppState>, group: Uuid, level: f32) {
    if let Some(engine) = state.engine.locked().as_ref() {
        engine.set_group_level(group, level);
    }
}

/// Apply a preset (as its tree path does, so MIDI and the tab do the same thing).
#[tauri::command]
pub fn main_apply_group_preset(state: State<AppState>, preset: Uuid) -> Result<(), String> {
    let index = state
        .show
        .locked()
        .group_presets
        .iter()
        .position(|p| p.id == preset)
        .ok_or("no such preset")?;
    if let Some(engine) = state.engine.locked().as_ref() {
        engine.activate_path(&format!("/group-preset/{index}/apply"));
    }
    Ok(())
}

/// The current masters as preset levels: every group, at its live level.
fn current_levels(state: &AppState) -> Vec<prisma_engine::GroupMaster> {
    let show = state.show.locked();
    let levels = state.engine.locked().as_ref().map(|e| e.group_levels());
    groups_info(&show, levels.as_deref())
        .groups
        .into_iter()
        .map(|g| prisma_engine::GroupMaster {
            group: g.id,
            level: g.level,
        })
        .collect()
}

/// Change the show's presets with `edit`, then reload them into the running engine.
fn edit_presets(
    app: &AppHandle,
    edit: impl FnOnce(&mut Vec<prisma_engine::GroupPreset>) -> Result<(), String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (groups, presets) = {
        let mut show = state.show.locked();
        edit(&mut show.group_presets)?;
        (show.patch.groups.clone(), show.group_presets.clone())
    };
    if let Some(engine) = state.engine.locked().as_ref() {
        engine.set_group_presets(&groups, &presets);
    }
    let _ = app.emit("groups-changed", ());
    Ok(())
}

/// "Preset n" with the smallest free n.
pub(crate) fn next_preset_name(presets: &[prisma_engine::GroupPreset]) -> String {
    (1..=presets.len() + 1)
        .map(|n| format!("Preset {n}"))
        .find(|name| !presets.iter().any(|p| &p.name == name))
        .unwrap_or_else(|| format!("Preset {}", presets.len() + 1))
}

/// "+ Preset": store the current masters as a new preset (cut, no fade), appended.
#[tauri::command]
pub fn main_add_group_preset(app: AppHandle) -> Result<Uuid, String> {
    let levels = current_levels(&app.state::<AppState>());
    let id = Uuid::new_v4();
    edit_presets(&app, |presets| {
        let name = next_preset_name(presets);
        presets.push(prisma_engine::GroupPreset {
            id,
            name,
            levels,
            fade: 0.0,
        });
        Ok(())
    })?;
    Ok(id)
}

fn preset_mut(
    presets: &mut [prisma_engine::GroupPreset],
    id: Uuid,
) -> Result<&mut prisma_engine::GroupPreset, String> {
    presets
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| "no such preset".to_string())
}

#[tauri::command]
pub fn main_rename_group_preset(app: AppHandle, preset: Uuid, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("El nombre no puede estar vacío.".into());
    }
    edit_presets(&app, |presets| {
        preset_mut(presets, preset)?.name = name;
        Ok(())
    })
}

/// Re-record: the preset takes the current masters.
#[tauri::command]
pub fn main_rerecord_group_preset(app: AppHandle, preset: Uuid) -> Result<(), String> {
    let levels = current_levels(&app.state::<AppState>());
    edit_presets(&app, |presets| {
        preset_mut(presets, preset)?.levels = levels;
        Ok(())
    })
}

/// The preset's fade, in seconds (clamped to 0..=60).
#[tauri::command]
pub fn main_set_group_preset_fade(
    app: AppHandle,
    preset: Uuid,
    seconds: f64,
) -> Result<(), String> {
    edit_presets(&app, |presets| {
        preset_mut(presets, preset)?.fade = seconds.clamp(0.0, 60.0);
        Ok(())
    })
}

#[tauri::command]
pub fn main_delete_group_preset(app: AppHandle, preset: Uuid) -> Result<(), String> {
    edit_presets(&app, |presets| {
        let before = presets.len();
        presets.retain(|p| p.id != preset);
        if presets.len() == before {
            return Err("no such preset".into());
        }
        Ok(())
    })
}

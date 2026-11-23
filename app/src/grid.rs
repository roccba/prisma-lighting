//! The track grid (LIVE-MODEL-SPEC §7): tracks, clips (cells), scenes and the effect parameter
//! panel — their views and their commands.

use super::*;

/// One scene-grid cell of the selected zone (M8 step 4): empty (assignable) or an
/// assigned effect, and whether it is the zone's running (active) cell.
#[derive(Serialize)]
pub(crate) struct CellInfo {
    pub(crate) index: usize,
    pub(crate) effect_id: Option<String>,
    pub(crate) effect_name: Option<String>,
    pub(crate) active: bool,
    /// Armed for GO (LIVE-MODEL-SPEC §5).
    pub(crate) armed: bool,
    /// The clip's own fade in seconds (0 = the track's transition).
    pub(crate) fade: f64,
    /// The operator's label for the cell (`None` = unnamed).
    pub(crate) name: Option<String>,
    /// The cell's grid colour, sRGB 0–255 (`None` = default panel grey).
    pub(crate) color: Option<[u8; 3]>,
    /// The effect's stored parameter overrides — carried so the grid's copy/paste can
    /// duplicate a cell whole (effect + parameters + colour table + label).
    pub(crate) params: Option<ParamSet>,
    /// The effect's stored colour table (linear light), if it has its own.
    pub(crate) palette: Option<Vec<[f32; 3]>>,
    /// The effect's main colour (its first colour group, linear light) — what the grid
    /// shows on a cell without a label colour. `None` for an effect without one.
    pub(crate) effect_color: Option<[f32; 3]>,
}

/// One entry the effect picker offers, catalogue-driven (§5.3).
#[derive(Serialize)]
pub(crate) struct EffectListingInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) category: String,
}

/// One row of the effect parameter panel (M8 step 5, §3.5), built from the effect's
/// manifest — never a hardcoded per-effect control (§5.3). `value_norm` is the cell's
/// current normalized value (its stored override, else the manifest default); `min`/
/// `max` are the real range so the panel can show a value the operator reads.
#[derive(Serialize)]
pub(crate) struct ParamInfo {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) value_norm: f32,
    /// Presentation hint the panel switches on (§3.5): `"scalar"` (a slider), `"color"`
    /// (a channel of the `group` colour picker), `"direction"` (the rose), `"enum"`,
    /// `"toggle"`, `"bpm"` or `"direction_menu"`. A palette is not a parameter: it travels
    /// as `EffectPanelInfo::palette`.
    pub(crate) role: &'static str,
    /// A grouping key: for `role == "color"` the colour group id. `Some` only then.
    pub(crate) group: Option<String>,
    /// The colour channel (`"r"`/`"g"`/`"b"`), `Some` for `role == "color"`.
    pub(crate) channel: Option<&'static str>,
    /// Dropdown labels for `role == "enum"` (or `"Group|Label"` for `"direction_menu"`),
    /// in index order; `None` for every other role.
    pub(crate) choices: Option<Vec<String>>,
    /// Greyed out unless this holds (console-style dependent controls); `None` = always on.
    pub(crate) requires: Option<RequiresInfo>,
    /// Which device knob drives it when this clip is selected (`/select/param/{n}`, 0-based):
    /// the n-th continuous parameter (slider or tempo). `None` for colours, choices, switches.
    pub(crate) knob: Option<usize>,
}

/// [`prisma_render::Requires`] for the panel: enabled while `param` has the choice
/// labelled `choice`, or (when `choice` is `None`) while `param` is above zero.
#[derive(Serialize)]
pub(crate) struct RequiresInfo {
    pub(crate) param: &'static str,
    pub(crate) choice: Option<&'static str>,
}

/// The effect work panel's data for the selected zone (M8 step 5): the zone's active
/// cell effect and its manifest-driven parameters. `effect_id` is `None` when the zone
/// has no running cell (empty selection) — the panel then shows an empty state.
#[derive(Serialize)]
pub(crate) struct EffectPanelInfo {
    pub(crate) track_id: Uuid,
    /// The cell is armed for GO.
    pub(crate) armed: bool,
    pub(crate) cell_index: Option<usize>,
    /// Whether `cell_index` is the zone's running cell — its edits drive the output live;
    /// any other cell's edits are only stored, for when it is played.
    pub(crate) active: bool,
    /// The cell is EMPTY and this panel shows the UI's draft effect at its defaults (a
    /// read of the manifest only — the cell stays empty until the UI assigns it).
    pub(crate) draft: bool,
    pub(crate) effect_id: Option<String>,
    pub(crate) effect_name: Option<String>,
    pub(crate) params: Vec<ParamInfo>,
    /// The colour table (linear RGB) for an effect that reads one; `None` otherwise.
    pub(crate) palette: Option<Vec<[f32; 3]>>,
    /// Most colours a table may hold.
    pub(crate) palette_max: usize,
    /// The clip's own fade in seconds (0 = the track's transition).
    pub(crate) fade: f64,
}

/// A display label from a manifest parameter key: capitalise the first letter, leaving
/// the rest as-is. Generic on purpose — the panel stays manifest-driven, with no
/// per-effect label table (§5.3). Real i18n resolution of the manifest's `label_key`
/// is deferred (the UI has no locale resolver yet); this is a legible stand-in.
pub(crate) fn humanize(name: &str) -> String {
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
pub(crate) fn role_dto(role: &ParamRole) -> (&'static str, Option<String>, Option<&'static str>) {
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

/// The picker's display name for an `effect_id`, from the catalogue.
pub(crate) fn effect_display_name(id: &str) -> Option<String> {
    prisma_render::catalogue()
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.name.to_string())
}

/// An effect's main colour: its manifest's FIRST colour group, each channel the stored
/// value or the manifest default (linear light `0..1`). `None` if it declares none.
pub(crate) fn effect_main_color(effect_id: &str, params: &ParamSet) -> Option<[f32; 3]> {
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
pub(crate) fn grid_dto(zs: &Track, armed: Option<usize>) -> Vec<CellInfo> {
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

/// The effects the picker offers, grouped by category — the M5 GPU
/// effects. Catalogue-driven, so the picker is never hardcoded (§5.3).
#[tauri::command]
pub fn main_effect_catalogue() -> Vec<EffectListingInfo> {
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
pub(crate) fn write_cell(
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
pub fn main_assign_cell(
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
pub fn main_assign_cell_silent(
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
pub fn main_trigger_cell(state: State<AppState>, track: Uuid, index: usize) -> Vec<CellInfo> {
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
pub fn main_clear_cell(state: State<AppState>, track: Uuid, index: usize) -> Vec<CellInfo> {
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
pub fn main_rename_cell(
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
pub fn main_set_cell_color(
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

/// The cell the effect panel works on: the one the operator selected on the grid, else
/// the zone's running cell. Selecting a cell never plays it (selected ≠ active).
pub(crate) fn panel_cell(zs: &Track, cell: Option<usize>) -> Option<usize> {
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
pub fn main_effect_panel(
    state: State<AppState>,
    track: Uuid,
    cell: Option<usize>,
    draft: Option<String>,
) -> EffectPanelInfo {
    let show = state.show.lock().unwrap();
    effect_panel_info(&show, track, cell, draft, armed_of(&state, track))
}

/// [`main_effect_panel`] for `show`, with `track`'s armed clip given. Pure: the UI test
/// fixture is built from it.
pub(crate) fn effect_panel_info(
    show: &Show,
    track: Uuid,
    cell: Option<usize>,
    draft: Option<String>,
    armed_clip: Option<usize>,
) -> EffectPanelInfo {
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
    let mut knobs = 0usize;
    let params = prisma_render::effect_params(&effect_id)
        .into_iter()
        .map(|p| {
            let knob = matches!(p.role, ParamRole::Scalar | ParamRole::Bpm).then(|| {
                knobs += 1;
                knobs - 1
            });
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
                knob,
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
pub fn main_set_palette(
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
pub fn main_set_param(
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
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        if live {
            engine.set_track_param(track, &name, value);
        }
        // The controller's knobs show the selected clip's values: re-read them.
        engine.refresh_selection();
    }
}

/// One track column of the grid (LIVE-MODEL-SPEC §7).
#[derive(Serialize)]
pub(crate) struct TrackInfo {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    /// Destination zones, in patch order.
    pub(crate) zones: Vec<Uuid>,
    /// The destination zones' names, joined for the header.
    pub(crate) destination: String,
    /// The first destination zone's display colour (the pads, the cells).
    pub(crate) color: [u8; 3],
    /// Every destination zone's display colour, parallel to `zones` (the header strip,
    /// one segment per zone).
    pub(crate) colors: Vec<[u8; 3]>,
    pub(crate) blend_mode: BlendMode,
    /// `BlendMode::LumaKey`'s threshold and softness (linear luminance, 0..1).
    pub(crate) key_threshold: f32,
    pub(crate) key_softness: f32,
    pub(crate) opacity: f32,
    pub(crate) mute: bool,
    pub(crate) solo: bool,
    pub(crate) transition: TrackTransition,
    pub(crate) launch: LaunchMode,
    pub(crate) active: Option<usize>,
    pub(crate) armed: Option<usize>,
    pub(crate) clips: Vec<CellInfo>,
}

/// One grid row's label.
#[derive(Serialize)]
pub(crate) struct SceneInfo {
    pub(crate) index: usize,
    pub(crate) name: Option<String>,
    pub(crate) color: Option<[u8; 3]>,
}

/// The whole grid: tracks in compositing order, one scene per row.
#[derive(Serialize)]
pub(crate) struct GridInfo {
    pub(crate) tracks: Vec<TrackInfo>,
    pub(crate) scenes: Vec<SceneInfo>,
}

/// A track's armed clip, as the clip controller last reported it.
pub(crate) fn armed_of(state: &AppState, track: Uuid) -> Option<usize> {
    state.armed.lock().unwrap().get(&track).copied()
}

/// The engine slot of a track. The engine numbers tracks in `Show::tracks` order, and
/// every engine is assembled from this same show.
pub(crate) fn track_slot(state: &AppState, track: Uuid) -> Option<usize> {
    state
        .show
        .lock()
        .unwrap()
        .tracks
        .iter()
        .position(|t| t.id == track)
}

/// Run `f` on the engine with a track's slot, if both exist.
pub(crate) fn with_track_engine(
    state: &AppState,
    track: Uuid,
    f: impl FnOnce(&EngineHandle, usize),
) {
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
pub fn main_grid(state: State<AppState>) -> GridInfo {
    let show = state.show.lock().unwrap();
    let engine = state.engine.lock().unwrap();
    grid_info(
        &show,
        |slot| engine.as_ref().and_then(|e| e.track_live(slot)),
        |track| armed_of(&state, track),
    )
}

/// The grid for `show`: each track's live controls from `live` (`None` = the show's
/// stored values) and its armed clip from `armed`. Pure: the UI test fixture is built
/// from it.
pub(crate) fn grid_info(
    show: &Show,
    live: impl Fn(usize) -> Option<TrackLiveReadout>,
    armed: impl Fn(Uuid) -> Option<usize>,
) -> GridInfo {
    let tracks: Vec<TrackInfo> = show
        .tracks
        .iter()
        .enumerate()
        .map(|(slot, t)| {
            let zone = |id: &Uuid| show.patch.zones.iter().find(|z| z.id == *id);
            let live = live(slot);
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
                colors: t
                    .destination
                    .zones
                    .iter()
                    .map(|id| zone(id).map_or([128, 128, 128], |z| z.display_color))
                    .collect(),
                blend_mode: t.blend_mode,
                key_threshold: t.key_threshold,
                key_softness: t.key_softness,
                opacity: live.map_or(t.opacity, |l| l.opacity),
                mute: live.map_or(t.mute, |l| l.mute),
                solo: live.map_or(t.solo, |l| l.solo),
                transition: t.transition,
                launch: t.launch,
                active: t.active,
                armed: armed(t.id),
                clips: grid_dto(t, armed(t.id)),
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
pub fn main_arm_cell(state: State<AppState>, track: Uuid, index: usize) {
    with_track_engine(&state, track, |e, slot| {
        e.arm_clip(slot, index);
    });
}

/// Stop a track (fade out with its transition; a second stop during it cuts).
#[tauri::command]
pub fn main_stop_track(state: State<AppState>, track: Uuid) {
    with_track_engine(&state, track, |e, slot| {
        e.stop_track(slot);
    });
}

/// The scene label of grid row `row`, created (and the rows before it) if missing.
pub(crate) fn scene_label(show: &mut Show, row: usize) -> &mut prisma_engine::SceneLabel {
    if show.scenes.len() <= row {
        show.scenes
            .resize_with(row + 1, prisma_engine::SceneLabel::default);
    }
    &mut show.scenes[row]
}

/// Rename a scene (the scene column's right-click → Renombrar). An empty name goes back to
/// the row number. A label only: no recompile.
#[tauri::command]
pub fn main_rename_scene(state: State<AppState>, row: usize, name: String) {
    let name = name.trim();
    let mut show = state.show.lock().unwrap();
    scene_label(&mut show, row).name = (!name.is_empty()).then(|| name.to_string());
}

/// Launch grid row `row` on every track that has a clip there (`arm` = arm instead).
#[tauri::command]
pub fn main_launch_scene(state: State<AppState>, row: usize, arm: bool) {
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
pub fn main_arm_clear(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.clear_arm();
    }
}

/// Stop every track (each with its transition; a second press during the fades cuts).
#[tauri::command]
pub fn main_stop_all(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.stop_all();
    }
}

/// GO: play every armed clip in the same frame.
#[tauri::command]
pub fn main_arm_go(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.go();
    }
}

/// A track's opacity (`0..=1`), live, and stored in the show.
#[tauri::command]
pub fn main_set_track_opacity(state: State<AppState>, track: Uuid, value: f32) {
    let value = value.clamp(0.0, 1.0);
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.opacity = value;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_opacity(slot, value));
}

/// Mute a track, live, and store it in the show.
#[tauri::command]
pub fn main_set_track_mute(state: State<AppState>, track: Uuid, on: bool) {
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.mute = on;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_mute(slot, on));
}

/// Solo a track, live, and store it in the show.
#[tauri::command]
pub fn main_set_track_solo(state: State<AppState>, track: Uuid, on: bool) {
    if let Some(t) = state.show.lock().unwrap().track_mut(track) {
        t.solo = on;
    }
    with_track_engine(&state, track, |e, slot| e.set_track_solo(slot, on));
}

/// The track popover: its transition and launch mode. Read by the clip controller at
/// each launch, so it applies at once — no recompile.
#[tauri::command]
pub fn main_set_track_behaviour(
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
pub fn main_set_track_blend(
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
pub fn main_set_cell_fade(state: State<AppState>, track: Uuid, index: usize, seconds: f64) {
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
pub fn main_rename_track(state: State<AppState>, track: Uuid, name: String) {
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
pub fn main_add_track(app: AppHandle, zone: Uuid) -> Result<Uuid, String> {
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
pub(crate) fn next_track_name(show: &Show) -> String {
    (1..)
        .map(|n| format!("Track {n}"))
        .find(|name| !show.tracks.iter().any(|t| &t.name == name))
        .expect("an unused number exists")
}

/// Reorder: move `track` to sit just before `before` (or last when `None`). The order is
/// global: it is the compositing order in every zone two tracks share (later = on top),
/// and means nothing where they share none (LIVE-MODEL-SPEC §8.4). Live, no recompile
/// (§10.2): the engine only changes the fold order, so animations, armed clips and
/// running fades carry on. Falls back to a recompile if the engine cannot.
#[tauri::command]
pub fn main_move_track(app: AppHandle, track: Uuid, before: Option<Uuid>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let order: Vec<Uuid> = {
        let mut show = state.show.lock().unwrap();
        let old: Vec<Uuid> = show.tracks.iter().map(|t| t.id).collect();
        move_track(&mut show.tracks, track, before);
        let order: Vec<Uuid> = show.tracks.iter().map(|t| t.id).collect();
        if order == old {
            return Ok(());
        }
        order
    };
    let live = state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|e| e.reorder_tracks(&order));
    if !live {
        return recompile_running_show(&app, |_| {});
    }
    let _ = app.emit("cells-changed", ());
    Ok(())
}

/// Move `track` just before `before` (or to the end when `None`). An unknown id, or
/// `before == track`, leaves the order unchanged.
pub(crate) fn move_track(tracks: &mut Vec<Track>, track: Uuid, before: Option<Uuid>) {
    let Some(from) = tracks.iter().position(|t| t.id == track) else {
        return;
    };
    if let Some(b) = before {
        if b == track || !tracks.iter().any(|t| t.id == b) {
            return;
        }
    }
    let moved = tracks.remove(from);
    let at = match before {
        Some(b) => tracks
            .iter()
            .position(|t| t.id == b)
            .expect("checked above"),
        None => tracks.len(),
    };
    tracks.insert(at, moved);
}

/// "Aplicar destino" in the track dialog: the zones `track` reaches. Recompiles the
/// running show (LIVE-MODEL-SPEC §4.4); nothing happens when the set is unchanged.
#[tauri::command]
pub fn main_set_track_destination(
    app: AppHandle,
    track: Uuid,
    zones: Vec<Uuid>,
) -> Result<(), String> {
    let zones = {
        let state = app.state::<AppState>();
        let show = state.show.lock().unwrap();
        let zones = destination_zones(&show.patch, &zones)?;
        match show.track(track) {
            None => return Err("no such track".into()),
            Some(t) if t.destination.zones == zones => return Ok(()),
            Some(_) => zones,
        }
    };
    recompile_running_show(&app, |show| {
        if let Some(t) = show.track_mut(track) {
            t.destination.zones = zones;
        }
    })
}

/// A requested destination as the show stores it: the patch's zones among `asked`, in
/// patch order, each once. Unknown ids are dropped; none left is an error (a track must
/// reach at least one zone).
pub(crate) fn destination_zones(
    patch: &prisma_patch::Patch,
    asked: &[Uuid],
) -> Result<Vec<Uuid>, String> {
    let zones: Vec<Uuid> = patch
        .zones
        .iter()
        .map(|z| z.id)
        .filter(|id| asked.contains(id))
        .collect();
    if zones.is_empty() {
        return Err("a track needs at least one zone".into());
    }
    Ok(zones)
}

/// Delete a track and its clips. Recompiles the running show.
#[tauri::command]
pub fn main_remove_track(app: AppHandle, track: Uuid) -> Result<(), String> {
    recompile_running_show(&app, |show| show.tracks.retain(|t| t.id != track))
}

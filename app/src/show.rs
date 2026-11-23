//! The show document's life: a new show, open / save / the demo, recovery after an unsaved
//! session, running a show on the engine and recompiling it when its structure changes.

use super::*;

/// A fresh patch for a new show: ONE zone ("Zona 1") already mapped with a
/// `DEFAULT_RIG`×`DEFAULT_RIG` matrix of 1-pixel RGB fixtures, addressed from universe 1
/// channel 1 (170 per universe), and the zone's render matrix set to that same size so the
/// content is sharp from the first frame. Unrouted universes go to the virtual sink, so
/// this lights nothing on the network until a device is configured.
pub(crate) fn empty_patch() -> Patch {
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
            white: prisma_patch::WhiteProfile::default(),
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
pub(crate) fn pad_grids(show: &mut Show) {
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
pub(crate) fn empty_show() -> Show {
    let mut show = Show::new("Sin título", empty_patch());
    pad_grids(&mut show);
    // The default zone starts at full dimmer (like the reconcile path's new-zone branch),
    // so the first clip the operator plays lights the rig at once.
    for zs in &mut show.zones {
        zs.live.dimmer = 1.0;
    }
    show
}

/// Bring a loaded Show's network output up to date: one-time migration from the old
/// app-wide Art-Net target (`network.json`) for shows that predate the device manager,
/// then the ranges → one-universe-per-port migration. Idempotent.
///
/// The legacy migration keeps an old show behaving exactly as before: an enabled legacy
/// target becomes one node receiving the patch's universes; a disabled one leaves the
/// show on the virtual sink. A show already initialized (even deliberately without
/// nodes) is never re-migrated.
pub(crate) fn prepare_show_network(show: &mut Show, legacy: &NetworkConfig) {
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
pub(crate) fn patch_universes(patch: &Patch) -> Vec<u16> {
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
pub(crate) fn fold_live_into_show(engine: &EngineHandle, show: &mut Show) {
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
        zs.live.white_auto = z.white.auto.read_bool().unwrap_or(zs.live.white_auto);
        zs.live.white_kelvin = z.white.kelvin.read_real().unwrap_or(zs.live.white_kelvin);
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

/// The show as it would be saved right now: the document with the engine's live values
/// folded in, serialized like a `.prisma`. Never holds the show and engine locks
/// together (the show is cloned first).
pub(crate) fn show_snapshot(state: &AppState) -> Result<String, String> {
    let mut show = state.show.lock().unwrap().clone();
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        fold_live_into_show(engine, &mut show);
    }
    save_show(&show).map_err(|e| e.to_string())
}

/// The operator saved or opened `snapshot`: it is the clean state the autosave compares
/// against from now on.
pub(crate) fn mark_saved(state: &AppState, snapshot: String) {
    let mut g = state.autosave_state.lock().unwrap();
    let (baseline, last) = &mut *g;
    autosave::mark_clean(baseline, last, snapshot);
}

/// The session was recovered from an autosave at boot.
#[derive(Serialize, Clone)]
pub(crate) struct RecoveryInfo {
    /// The last saved show, if there is one to fall back to.
    pub(crate) last_saved: Option<String>,
}

/// Taken once by the main window at boot: `Some` if this session was recovered.
#[tauri::command]
pub fn main_recovery_info(state: State<AppState>) -> Option<RecoveryInfo> {
    state.recovered.lock().unwrap().take()
}

/// The last opened or saved show file, if any (Archivo → "Abrir el último guardado").
#[tauri::command]
pub fn main_last_saved(state: State<AppState>) -> Option<String> {
    read_last_show(&state.last_show_file).map(|p| p.display().to_string())
}

/// Save the current live show to a `.prisma` file (M8-SPEC §5): serialize the Show — with
/// its embedded Patch — through the versioned envelope. The frontend picks the path with
/// the native dialog.
#[tauri::command]
pub fn main_save_show(state: State<AppState>, path: String) -> Result<(), String> {
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
pub fn main_new_show(app: AppHandle) -> Result<(), String> {
    run_show(&app, empty_show(), None)
}

/// Open a `.prisma` show and run it: load the Show, recompile the engine onto its embedded
/// Patch, and adopt it as the live + editable state. Like [`apply_editor_draft`], the patch
/// is validated (compile) BEFORE the engine is torn down, so a bad file is rejected with the
/// running show intact rather than leaving the app with no engine. The main window re-fetches
/// on the `layout-changed` event.
#[tauri::command]
pub fn main_open_show(app: AppHandle, path: String) -> Result<(), String> {
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let show = load_show(&json).map_err(|e| e.to_string())?;
    run_show(&app, show, Some(Path::new(&path)))
}

/// "Archivo → Abrir show de demo": the built-in demo show (`prisma_engine::demo`), unsaved
/// like a new one — saving it asks for a file, and the last-show memory is cleared.
#[tauri::command]
pub fn main_open_demo_show(app: AppHandle) -> Result<(), String> {
    run_show(&app, prisma_engine::demo::demo_show(), None)
}

/// Run `show` in place of the current one: derive and validate its patch, recompile the
/// engine onto it (keeping the GPU), adopt it as the live + editable state, remember `path`
/// as the last show (`None` forgets it) and mark it saved. A show that fails to validate or
/// compile leaves the running one intact.
pub(crate) fn run_show(app: &AppHandle, mut show: Show, path: Option<&Path>) -> Result<(), String> {
    let state = app.state::<AppState>();
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
            // Adopt the show + its patch as the editable state (so the editor, if opened
            // next, shows this rig), then tell the main window to re-fetch.
            *state.draft.lock().unwrap() = PatchDraft::new(show.patch.clone());
            *state.show.lock().unwrap() = show;
            watch_show_nodes(&state);
            if let Some(engine) = state.engine.lock().unwrap().as_ref() {
                refresh_all_cells(engine);
            }
            write_last_show(&state.last_show_file, path);
            if let Ok(snap) = show_snapshot(&state) {
                mark_saved(&state, snap);
            }
            let _ = app.emit("layout-changed", ());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Edit the running show and recompile the engine onto it (reusing the GPU). Live values
/// are folded into the show FIRST — by the engine's current zone/track order, before the
/// edit can add or remove a track and shift the slots — so the recompile does not reset
/// faders; the tempo and freeze carry over; the preview WebSocket moves to a fresh port,
/// so the UI is nudged to re-fetch.
pub(crate) fn recompile_running_show(
    app: &AppHandle,
    edit: impl FnOnce(&mut Show),
) -> Result<(), String> {
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

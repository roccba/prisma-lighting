//! The zone row, the master section and the tempo: their views and live controls, and the clip
//! host the engine reports to.

use super::*;

/// A zone card's data for the zone row: identity + its current live controls.
#[derive(Serialize)]
pub(crate) struct ZoneInfo {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) color: [u8; 3],
    pub(crate) dimmer: f32,
    /// Velocity multiplier, real `-10..=10` (0 = static, negative = reverse). The
    /// zone-card speed fader seeds from this and writes it back live.
    pub(crate) speed: f32,
    pub(crate) solo: bool,
    pub(crate) blackout: bool,
    pub(crate) strobe_on: bool,
    /// Strobe speed, `0..=1` over 1–25 Hz.
    pub(crate) strobe_rate: f32,
    /// White temperature (fixtures with two whites): automatic, or `white_kelvin`.
    pub(crate) white_auto: bool,
    pub(crate) white_kelvin: f32,
    /// The zone has fixtures with two whites (RGBWW): only then does the card show
    /// the white-temperature control.
    pub(crate) has_dual_white: bool,
}

/// The slot (index in `Patch::zones`) of a zone id, or `None` if unknown. Derived from
/// the running show's embedded patch — the single source of truth for the zone set, so
/// after a hot patch edit (add/remove zone) the slots the engine's `LiveParams` use and
/// the app's addressing stay in lockstep with no separate list to keep in sync.
/// The app's side of clip playback: the engine's clip controller plays clips from the
/// Parameter Tree (pads, clicks — every transport the same way) and asks here what a
/// clip holds and how a track transitions; it reports back which clip each track is
/// running or has armed, which this folds into the app state and announces to the UI
/// (`cells-changed`, the track id).
///
/// Lock discipline: it only ever takes the `show` lock, never `engine`, so an engine
/// recompile (which holds `engine` while it stops the controller) cannot deadlock on it.
pub(crate) struct AppCellHost {
    pub(crate) app: AppHandle,
}

impl ClipHost for AppCellHost {
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect> {
        let state = self.app.try_state::<AppState>()?;
        let show = state.show.lock().unwrap();
        let cell = show.track(track)?.clips.get(clip)?;
        let layer = cell.layers.first()?;
        match &layer.source {
            Source::Shader {
                effect_id,
                params,
                palette,
            } => Some(ClipEffect {
                effect_id: effect_id.clone(),
                params: params.clone(),
                palette: palette.clone(),
                fade_time: cell.fade_time,
            }),
        }
    }

    fn track_behaviour(&self, track: Uuid) -> Option<TrackBehaviour> {
        let state = self.app.try_state::<AppState>()?;
        let show = state.show.lock().unwrap();
        let t = show.track(track)?;
        Some(TrackBehaviour {
            transition: t.transition,
            launch: t.launch,
        })
    }

    fn armed_changed(&self, track: Uuid, armed: Option<usize>) {
        if let Some(state) = self.app.try_state::<AppState>() {
            let mut map = state.armed.lock().unwrap();
            match armed {
                Some(clip) => map.insert(track, clip),
                None => map.remove(&track),
            };
        }
        let _ = self.app.emit("cells-changed", track);
    }

    fn active_changed(&self, track: Uuid, active: Option<usize>) {
        if let Some(state) = self.app.try_state::<AppState>() {
            if let Some(t) = state.show.lock().unwrap().track_mut(track) {
                t.active = active;
            }
        }
        let _ = self.app.emit("cells-changed", track);
    }

    /// The selection moved (a click echoing back, a controller's track select, the
    /// keyboard's arrows): the UI follows it.
    fn selection_changed(&self, track: Option<Uuid>, clip: Option<usize>) {
        let _ = self
            .app
            .emit("selection-changed", SelectionInfo { track, clip });
    }

    /// A knob moved a parameter of the selected clip: store it, as the panel does. The
    /// panel re-reads on the (coalesced) `midi-applied` the knob's write already raised.
    fn param_stored(&self, track: Uuid, clip: usize, name: &str, value_norm: f32) {
        let Some(state) = self.app.try_state::<AppState>() else {
            return;
        };
        let mut show = state.show.lock().unwrap();
        let layer = show
            .track_mut(track)
            .and_then(|t| t.clips.get_mut(clip))
            .and_then(|c| c.layers.first_mut());
        if let Some(layer) = layer {
            let Source::Shader { params, .. } = &mut layer.source;
            params.set(name, value_norm);
        }
    }

    /// A scene row fired (any transport): the scene column flashes it — a confirmation of
    /// the press, not a state.
    fn scene_fired(&self, row: usize) {
        let _ = self.app.emit("scene-fired", row);
    }
}

/// Re-publish every track's clip states — after the show document was swapped under a
/// running engine (open / new), whose controller started against the previous one.
pub(crate) fn refresh_all_cells(engine: &EngineHandle) {
    for slot in 0..engine.track_count() {
        engine.refresh_clips(slot);
    }
}

pub(crate) fn zone_slot(state: &AppState, id: Uuid) -> Option<usize> {
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
pub fn main_zones(state: State<AppState>) -> Vec<ZoneInfo> {
    // Identity comes from the show's embedded patch (the single source of truth, so a
    // hot patch edit is reflected here); the live controls come from the engine. Clone
    // the show first, then drop its lock before taking the engine lock.
    let show = state.show.lock().unwrap().clone();
    let guard = state.engine.lock().unwrap();
    zone_infos(&show, |slot| guard.as_ref().map(|e| e.zone_live(slot)))
}

/// The zone row for `show`, each zone's live controls from `live` (`None` = the show's
/// stored values). Pure: the UI test fixture is built from it.
pub(crate) fn zone_infos(
    show: &Show,
    live: impl Fn(usize) -> Option<ZoneLiveReadout>,
) -> Vec<ZoneInfo> {
    show.patch
        .zones
        .iter()
        .enumerate()
        .map(|(slot, z)| {
            let has_dual_white = show.patch.fixtures.iter().any(|f| {
                f.zone == z.id && f.profile.components == prisma_patch::ComponentSet::Rgbww
            });
            let stored = show.zone(z.id).map(|zs| ZoneLiveReadout {
                dimmer: zs.live.dimmer,
                speed: zs.live.speed_multiplier,
                solo: zs.live.solo,
                blackout: zs.live.blackout,
                strobe_on: false,
                strobe_rate: 0.4,
                white_auto: zs.live.white_auto,
                white_kelvin: zs.live.white_kelvin,
            });
            let live = live(slot).or(stored).unwrap_or(ZoneLiveReadout {
                dimmer: 0.0,
                speed: 1.0,
                solo: false,
                blackout: false,
                strobe_on: false,
                strobe_rate: 0.4,
                white_auto: true,
                white_kelvin: 4000.0,
            });
            ZoneInfo {
                id: z.id,
                name: z.name.clone(),
                color: z.display_color,
                dimmer: live.dimmer,
                speed: live.speed,
                solo: live.solo,
                blackout: live.blackout,
                strobe_on: live.strobe_on,
                strobe_rate: live.strobe_rate,
                white_auto: live.white_auto,
                white_kelvin: live.white_kelvin,
                has_dual_white,
            }
        })
        .collect()
}

/// Set a zone's dimmer (the zone-row fader) — a live control that changes the output,
/// not the UI's selection. `selected ≠ active`: this is the "active" side.
#[tauri::command]
pub fn main_set_zone_dimmer(state: State<AppState>, zone: Uuid, value: f32) {
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
pub fn main_set_zone_speed(state: State<AppState>, zone: Uuid, value: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_speed(slot, value);
    }
}

/// Toggle a zone's solo (M8-SPEC §3.1).
#[tauri::command]
pub fn main_set_zone_solo(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_solo(slot, on);
    }
}

/// Toggle a zone's blackout/mute (M8-SPEC §3.1).
#[tauri::command]
pub fn main_set_zone_blackout(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_blackout(slot, on);
    }
}

/// Engage/release a zone's strobe (M8-SPEC §3.6; a tree toggle since M9, so a pad can
/// drive it too). Gates the zone's non-overlapped pixels at the output stage.
#[tauri::command]
pub fn main_set_zone_strobe_on(state: State<AppState>, zone: Uuid, on: bool) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_strobe_on(slot, on);
    }
}

/// Set a zone's strobe speed in Hz (1–25), engaged or not.
#[tauri::command]
pub fn main_set_zone_strobe_rate(state: State<AppState>, zone: Uuid, rate: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_strobe_rate(slot, rate);
    }
}

/// Set a zone's white temperature: automatic (follow the colour), or `kelvin`.
#[tauri::command]
pub fn main_set_zone_white(state: State<AppState>, zone: Uuid, auto: bool, kelvin: f32) {
    let Some(slot) = zone_slot(&state, zone) else {
        return;
    };
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_zone_white(slot, auto, kelvin);
    }
}

/// The global bar's current master state, so the bar reflects what the engine holds
/// (a loaded show, or another transport) rather than only the UI's own writes.
#[derive(Serialize)]
pub(crate) struct MasterInfo {
    pub(crate) dimmer: f32,
    pub(crate) blackout: bool,
    pub(crate) strobe_on: bool,
    /// Strobe speed, `0..=1` over 1–25 Hz.
    pub(crate) strobe_rate: f32,
}

/// Read the master section (dimmer, blackout, global strobe) for the global bar.
#[tauri::command]
pub fn main_master(state: State<AppState>) -> MasterInfo {
    let guard = state.engine.lock().unwrap();
    let Some(engine) = guard.as_ref() else {
        return master_info(&state.show.lock().unwrap());
    };
    let m = engine.live().master();
    MasterInfo {
        dimmer: m.dimmer.read_real().unwrap_or(1.0),
        blackout: m.blackout.read_real().unwrap_or(0.0) > 0.5,
        strobe_on: m.strobe.on.read_bool().unwrap_or(false),
        strobe_rate: m.strobe.rate.read_norm().unwrap_or(0.4),
    }
}

/// The master section as `show` stores it (no engine). Pure: the UI test fixture is built
/// from it.
pub(crate) fn master_info(show: &Show) -> MasterInfo {
    MasterInfo {
        dimmer: show.master.dimmer,
        blackout: false,
        strobe_on: false,
        strobe_rate: 0.4,
    }
}

/// Set the master dimmer (the global-bar fader), real 0..1.
#[tauri::command]
pub fn main_set_master_dimmer(state: State<AppState>, value: f32) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_dimmer(value);
    }
}

/// Toggle the master blackout (kills all output while holding the fader value).
#[tauri::command]
pub fn main_set_master_blackout(state: State<AppState>, on: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_blackout(on);
    }
}

/// Engage/release the global strobe.
#[tauri::command]
pub fn main_set_master_strobe_on(state: State<AppState>, on: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_strobe_on(on);
    }
}

/// Set the global strobe speed in Hz (1–25), engaged or not.
#[tauri::command]
pub fn main_set_master_strobe_rate(state: State<AppState>, rate: f32) {
    if let Some(engine) = state.engine.lock().unwrap().as_ref() {
        engine.set_master_strobe_rate(rate);
    }
}

/// The tempo in the global bar.
#[derive(Serialize)]
pub(crate) struct ClockInfo {
    pub(crate) bpm: f64,
}

#[tauri::command]
pub fn main_clock(state: State<AppState>) -> ClockInfo {
    let bpm = state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .map_or(120.0, |e| e.bpm());
    ClockInfo { bpm }
}

#[tauri::command]
pub fn main_set_bpm(state: State<AppState>, bpm: f64) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_bpm(bpm);
    }
}

#[tauri::command]
pub fn main_tap(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.tap();
    }
}

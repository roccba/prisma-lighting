//! MIDI: the input and output ports, the device profile, MIDI Learn and the operator's controller
//! mappings profile.

use super::*;

/// (Re)open the configured MIDI input, closing the previous link first. A missing device
/// is not an error worth shouting about in auto mode; it is recorded for the device
/// manager to show.
///
/// The output (LEDs) is the port with the input's name — how ALSA, CoreMUSIC and WinMM
/// all name the two halves of one USB device — falling back to the keywords. A device
/// without an output simply gets no feedback. Its device profile (LED semantics) is
/// picked from the port name.
pub(crate) fn connect_midi(app: &AppHandle) {
    let state = app.state::<AppState>();
    open_midi(&state);
    let _ = app.emit("midi-connected", ());
    // The first connection of a device to an empty profile applies the device's
    // default mappings, once, with a notice (LIVE-MODEL-SPEC §8.2).
    if let Some((profile, added)) = state.midi.seed_device_defaults() {
        if let Err(e) = persist_mappings(app, &profile) {
            report_error(app, format!("No se pudieron guardar los mapeos MIDI: {e}"));
        }
        let _ = app.emit(
            "midi-seeded",
            SeededInfo {
                device: state.midi.device().display_name,
                added,
            },
        );
    }
}

/// What the "defaults applied" notice says.
#[derive(Serialize, Clone)]
pub(crate) struct SeededInfo {
    pub(crate) device: String,
    pub(crate) added: usize,
}

pub(crate) fn open_midi(state: &AppState) {
    let mut link = state.midi_link.locked();
    let mut out_link = state.midi_out_link.locked();
    state.midi.set_output(None);
    *link = None;
    *out_link = None;
    let port = state.midi_config.locked().input_port.clone();
    let opened = match &port {
        Some(name) => MidiInputLink::open(state.midi.clone(), name),
        None => MidiInputLink::open_by_keywords(state.midi.clone(), APC40_KEYWORDS),
    };
    *state.midi_error.locked() = match opened {
        Ok(l) => {
            let name = l.port_name().to_string();
            *link = Some(l);
            state.midi.set_device(DeviceProfile::for_port(&name));
            *out_link = MidiOutputLink::open(&name)
                .or_else(|_| MidiOutputLink::open_by_keywords(APC40_KEYWORDS))
                .ok();
            if let Some(out) = out_link.as_ref() {
                state.midi.set_output(Some(out.sink()));
            }
            None
        }
        Err(MidiIoError::NotFound) if port.is_none() => None,
        Err(e) => Some(e.to_string()),
    };
}

/// Forward runtime notices to the UI. Learn persists the profile and emits `midi-learned`
/// at once. `Applied` (a mapped control moved) arrives at controller rate, so it only
/// marks a flag; a notifier thread emits `midi-applied` at most every 50 ms and always
/// once more after the last move, so the UI's faders land on the final value.
///
/// A control that drives a zone also emits `midi-focus` (the zone slot) so the main
/// window can follow the operator's hands when "follow MIDI focus" is on. It fires on the
/// first message of a gesture — a different zone than last time, or the same zone after a
/// pause — not at controller rate: re-selecting the zone on every CC would fight a click
/// the operator makes on another zone mid-gesture.
pub(crate) fn install_midi_listener(app: &AppHandle, midi: &MidiRuntime, mappings_path: PathBuf) {
    let dirty = Arc::new(AtomicBool::new(false));
    let notifier = {
        let app = app.clone();
        let dirty = Arc::clone(&dirty);
        std::thread::Builder::new()
            .name("prisma-midi-ui".into())
            .spawn(move || loop {
                std::thread::park();
                while dirty.swap(false, Ordering::Relaxed) {
                    let _ = app.emit("midi-applied", ());
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
    };
    // Without the notifier the faders do not follow a controller live (they still refresh
    // on the next read); the rest of MIDI works. Never a reason to stop the app.
    let notifier = match notifier {
        Ok(thread) => Some(thread),
        Err(e) => {
            report_error(
                app,
                format!("MIDI: no se pudo iniciar el aviso a la ventana ({e})"),
            );
            None
        }
    };
    let app = app.clone();
    let last_focus: Mutex<Option<(u16, std::time::Instant)>> = Mutex::new(None);
    midi.set_listener(Some(Arc::new(move |notice| match notice {
        MidiNotice::Learned { mapping, profile } => {
            match save_mappings(&profile) {
                Ok(json) => {
                    if let Err(e) = persist_text(&mappings_path, &json) {
                        report_error(&app, format!("No se pudieron guardar los mapeos MIDI: {e}"));
                    }
                }
                Err(e) => {
                    report_error(&app, format!("No se pudieron guardar los mapeos MIDI: {e}"))
                }
            }
            let _ = app.emit("midi-learned", mapping);
        }
        MidiNotice::Bank(bank) => {
            let _ = app.emit("midi-bank", bank);
        }
        MidiNotice::Applied { zone, .. } => {
            if !dirty.swap(true, Ordering::Relaxed) {
                if let Some(n) = &notifier {
                    n.thread().unpark();
                }
            }
            if let Some(slot) = zone {
                let now = std::time::Instant::now();
                let mut last = last_focus.locked();
                let new_gesture = match *last {
                    Some((prev, at)) => prev != slot || now.duration_since(at) > FOCUS_GESTURE_GAP,
                    None => true,
                };
                if new_gesture {
                    let _ = app.emit("midi-focus", slot);
                }
                *last = Some((slot, now));
            }
        }
    })));
}

/// A pause longer than this between messages for the same zone starts a new gesture,
/// which re-focuses that zone (e.g. after the operator clicked elsewhere).
pub(crate) const FOCUS_GESTURE_GAP: std::time::Duration = std::time::Duration::from_millis(400);

/// Everything the MIDI tab and the Learn overlay need.
#[derive(Serialize)]
pub(crate) struct MidiStatus {
    /// Present input ports (empty if the MIDI system is unavailable).
    pub(crate) ports: Vec<String>,
    /// The configured port; `None` = auto-detect (APC40).
    pub(crate) selected: Option<String>,
    /// The port actually open right now.
    pub(crate) connected: Option<String>,
    /// The output open for LED feedback, if the device has one.
    pub(crate) output: Option<String>,
    /// The connected device's profile name (e.g. "Akai APC40 mkII", "Generic MIDI").
    pub(crate) device: String,
    pub(crate) error: Option<String>,
    /// The target waiting for a control, if Learn is armed.
    pub(crate) learning: Option<MapTarget>,
    pub(crate) mappings: Vec<MidiMapping>,
    /// The connected device's LED palette as `#rrggbb`, index = colour value; empty
    /// when the device has none (colour choices are then hidden).
    pub(crate) palette: Vec<String>,
    /// Where the controller's grid window sits (bank buttons move it).
    pub(crate) bank: prisma_engine::Bank,
    /// How many default mappings the connected device's profile ships.
    pub(crate) device_defaults: usize,
    /// The device's grid window (tracks × rows) when it has bank buttons: the UI draws it.
    pub(crate) grid_window: Option<(u16, u16)>,
}

#[tauri::command]
pub fn midi_status(state: State<AppState>) -> MidiStatus {
    let (ports, list_error) = match input_port_names() {
        Ok(p) => (p, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    MidiStatus {
        ports,
        selected: state.midi_config.locked().input_port.clone(),
        connected: state
            .midi_link
            .locked()
            .as_ref()
            .map(|l| l.port_name().to_string()),
        output: state
            .midi_out_link
            .locked()
            .as_ref()
            .map(|l| l.port_name().to_string()),
        device: state.midi.device().display_name,
        error: state.midi_error.locked().clone().or(list_error),
        learning: state.midi.learning(),
        mappings: state.midi.profile().mappings,
        palette: state
            .midi
            .device()
            .palette()
            .map(|p| p.iter().map(|c| format!("#{c:06x}")).collect())
            .unwrap_or_default(),
        bank: state.midi.bank(),
        device_defaults: state.midi.device().default_mappings.len(),
        grid_window: {
            let device = state.midi.device();
            device.surface.bank.as_ref().map(|_| device.window())
        },
    }
}

/// Choose the input (`None` = auto-detect), persist it, and reopen.
#[tauri::command]
pub fn midi_set_input(app: AppHandle, port: Option<String>) -> Result<MidiStatus, String> {
    let state = app.state::<AppState>();
    let config = MidiConfig { input_port: port };
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    persist_text(&state.midi_config_path, &json)?;
    *state.midi_config.locked() = config;
    connect_midi(&app);
    Ok(midi_status(state))
}

/// Re-scan and reopen the configured input (hot-plug: the device was connected late).
#[tauri::command]
pub fn midi_reconnect(app: AppHandle) -> MidiStatus {
    connect_midi(&app);
    midi_status(app.state::<AppState>())
}

/// "Cargar mapeos del perfil": add the connected device's default mappings for the
/// controls (and targets) that are not mapped yet — never overwriting — and persist.
#[tauri::command]
pub fn midi_load_device_defaults(app: AppHandle) -> Result<DefaultsLoaded, String> {
    let state = app.state::<AppState>();
    let (profile, added) = state.midi.add_device_defaults();
    persist_mappings(&app, &profile)?;
    Ok(DefaultsLoaded {
        added,
        mappings: profile.mappings,
    })
}

#[derive(Serialize)]
pub(crate) struct DefaultsLoaded {
    pub(crate) added: usize,
    pub(crate) mappings: Vec<MidiMapping>,
}

/// Arm MIDI Learn for a target: the next pad/fader touched on the hardware binds to it.
#[tauri::command]
pub fn midi_learn_arm(state: State<AppState>, target: MapTarget) {
    state.midi.arm_learn(target);
}

#[tauri::command]
pub fn midi_learn_cancel(state: State<AppState>) {
    state.midi.cancel_learn();
}

/// Remove the mapping for a target and persist the profile.
#[tauri::command]
pub fn midi_unmap(app: AppHandle, target: MapTarget) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    if let Some(profile) = state.midi.unmap(&target) {
        persist_mappings(&app, &profile)?;
    }
    Ok(state.midi.profile().mappings)
}

/// Change a mapped control's pad style and LED look (the Learn right-click menu, M9
/// Slice 3), persist the profile, and return the mappings.
#[tauri::command]
pub fn midi_configure(
    app: AppHandle,
    target: MapTarget,
    mode: ControlMode,
    led: Option<LedStyle>,
) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    let profile = state
        .midi
        .configure(&target, mode, led)
        .ok_or("that control is not mapped")?;
    persist_mappings(&app, &profile)?;
    Ok(profile.mappings)
}

/// Change how a mapped fader / knob takes over its value — soft takeover ("pickup",
/// the default) or "jump" — persist the profile, and return the mappings.
#[tauri::command]
pub fn midi_set_takeover(
    app: AppHandle,
    target: MapTarget,
    takeover: Takeover,
) -> Result<Vec<MidiMapping>, String> {
    let state = app.state::<AppState>();
    let profile = state
        .midi
        .set_takeover(&target, takeover)
        .ok_or("that control is not mapped")?;
    persist_mappings(&app, &profile)?;
    Ok(profile.mappings)
}

pub(crate) fn persist_mappings(
    app: &AppHandle,
    profile: &ControllerMappings,
) -> Result<(), String> {
    let json = save_mappings(profile).map_err(|e| e.to_string())?;
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join(MAPPINGS_FILE);
    persist_text(&path, &json)
}

/// The operator's controller profile, in the per-user app-config dir (CLAUDE.md §4.6 —
/// separate from any Show).
pub(crate) const MAPPINGS_FILE: &str = "controller-mappings.json";

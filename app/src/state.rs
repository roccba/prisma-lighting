//! Shared application state: the engine, the show, the editor draft, the MIDI and network
//! configuration, and the small helpers every part of the shell uses.

use super::*;

/// How many assignable clips each track has (8 columns × 5 rows — the common
/// pad-controller layout, shown as the zone's grid until the track grid lands in
/// phase 2). Shorter tracks are padded up to this on load.
pub(crate) const GRID_CELLS: usize = 40;

/// Side of the default rig (like professional consoles, a fresh start is a mapped square, not a blank).
pub(crate) const DEFAULT_RIG: u32 = 50;

/// Read the remembered last-show path, if any. Empty/missing file ⇒ `None` ⇒ boot empty.
pub(crate) fn read_last_show(file: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(file).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// Remember (or, with `None`, forget) the last-show path so the next launch reopens it.
pub(crate) fn write_last_show(file: &Path, path: Option<&Path>) {
    let content = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = std::fs::write(file, content);
}

/// Everything the shell owns for the life of the process.
pub(crate) struct AppState {
    /// The live output engine (S1). `None` once shut down.
    pub(crate) engine: Mutex<Option<EngineHandle>>,
    /// Autosave (see `autosave`): its files, and the clean baseline + last snapshot the
    /// autosave thread compares against.
    pub(crate) autosave: autosave::Autosave,
    pub(crate) autosave_state: Mutex<(autosave::Baseline, Option<String>)>,
    /// Set at boot when the session was recovered from an autosave; taken once by the UI
    /// to show "Sesión recuperada".
    pub(crate) recovered: Mutex<Option<RecoveryInfo>>,
    /// Each track's armed clip, as the clip controller reports it (live state, not
    /// saved with the show).
    pub(crate) armed: Mutex<std::collections::HashMap<Uuid, usize>>,
    /// The editable Show — the source of truth for each zone's scene grid (M8 step 4)
    /// AND for the zone set/identity (its embedded patch; §7 derives the zone row and
    /// slot mapping from it, so a hot patch edit needs no separate list kept in sync).
    /// The engine ran a copy at boot; grid edits mutate this and are pushed to the
    /// engine as effect triggers through the frame-boundary queue.
    pub(crate) show: Mutex<Show>,
    /// The editor's working patch and its undo history (S2).
    pub(crate) draft: Mutex<PatchDraft>,
    /// The operator's reusable fixture library (S7, §3.2). Feeds the placement dialog
    /// and the library manager; persisted to [`library_path`](AppState::library_path)
    /// on every edit.
    pub(crate) library: Mutex<FixtureLibrary>,
    /// Where the library file lives — the per-user app-config dir, not the project or
    /// the `.prismap`.
    pub(crate) library_path: PathBuf,
    /// TCP port the live-preview WebSocket is bound to (§M6/S10). The React preview
    /// queries it and connects to `ws://127.0.0.1:{port}`. A fresh (OS-assigned) port
    /// is bound per engine, so a recompile never fights the old bind for a fixed port;
    /// the viewer re-queries and reconnects.
    pub(crate) preview_port: Mutex<u16>,
    /// The LEGACY app-wide Art-Net target (`network.json`, pre device manager). Read-only:
    /// it only seeds shows that predate the per-show network ([`prepare_show_network`]).
    pub(crate) network: Mutex<NetworkConfig>,
    /// The Art-Net node monitor (status + Scan), the single owner of UDP 6454. `None`
    /// when another program holds the port; retried on the next Scan.
    pub(crate) artnet_monitor: Mutex<Option<Arc<NodeMonitor>>>,
    /// File holding the path of the last opened/saved show, so the next launch reopens it.
    /// Empty/missing ⇒ boot empty. Cleared by "Nueva configuración".
    pub(crate) last_show_file: PathBuf,
    /// MIDI (M9): a clone of the engine's runtime. It outlives every recompile (the engine
    /// carries it across), so the open input link below keeps feeding the live tree.
    pub(crate) midi: MidiRuntime,
    /// The open hardware input, if any. Dropping it closes the port.
    pub(crate) midi_link: Mutex<Option<MidiInputLink>>,
    /// The same device's output (LED feedback, M9 Slice 2), if it has one.
    pub(crate) midi_out_link: Mutex<Option<MidiOutputLink>>,
    /// Which input to open (persisted) and why the last open failed (transient).
    pub(crate) midi_config: Mutex<MidiConfig>,
    pub(crate) midi_config_path: PathBuf,
    pub(crate) midi_error: Mutex<Option<String>>,
}

/// Which MIDI input the app opens. `None` = auto-detect the reference controller (APC40)
/// by port-name keyword, so plugging it in just works on any OS.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct MidiConfig {
    pub(crate) input_port: Option<String>,
}

/// LEGACY app-wide Art-Net target (`network.json`), from before the device manager: one
/// IP for every universe. The output now follows each Show's own `network` (nodes); this
/// is only read to migrate shows that predate it ([`prepare_show_network`]) and is never
/// written any more.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct NetworkConfig {
    pub(crate) artnet_enabled: bool,
    pub(crate) artnet_ip: String,
    pub(crate) artnet_port: u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        // Disabled by default → the virtual sink, so a fresh install behaves exactly as
        // before. A subnet broadcast is the safe first target to try against real gear.
        Self {
            artnet_enabled: false,
            artnet_ip: "255.255.255.255".into(),
            artnet_port: 6454,
        }
    }
}

/// The loopback virtual sink: where DMX goes when the show's output is not routed
/// (Art-Net off or no nodes). Routed shows send each universe to its node instead —
/// the engine compiles that from `Show::network` (docs/DEVICES-SPEC.md §5).
pub(crate) fn virtual_sink() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454)
}

/// Bind a live-preview WebSocket on an OS-assigned port and package it as an engine
/// config (adapter ignored on a recompile — the GPU is reused). Returns the config and
/// the bound port, or `None` if the bind failed (the engine then runs without a
/// preview — never fatal).
pub(crate) fn preview_config(
    adapter: AdapterPreference,
    target: SocketAddr,
) -> (EngineConfig, Option<u16>) {
    let mut config = EngineConfig::new(adapter, target);
    config.output_rate_hz = 44;
    config.preview_rate_hz = 30;
    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    match WebSocketSink::bind(loopback) {
        Ok(ws) => {
            let port = ws.local_addr().port();
            config.preview_sink = Some(Box::new(ws));
            (config, Some(port))
        }
        Err(e) => {
            eprintln!("prisma: live preview unavailable (WebSocket bind failed: {e})");
            (config, None)
        }
    }
}

/// Write the library to its file, creating the config directory if needed.
pub(crate) fn persist_library(path: &Path, library: &FixtureLibrary) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = save_library(library).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Write a JSON-able config next to the others, creating the config directory if needed.
pub(crate) fn persist_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

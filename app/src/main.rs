//! Prisma desktop shell: hosts the engine (started once, shut down in order on exit) and
//! serves the UI over Tauri commands. This file boots the app and registers the commands;
//! the rest lives in modules, one per part of the app:
//!
//! - `state` — the shared [`AppState`] and small helpers (paths, the virtual sink).
//! - `show` — the show document's life: new, open, demo, save, recovery, recompile.
//! - `patch` — the patch editor (its draft, the fixture library, the editor window).
//! - `network` — network output: Art-Net nodes, interface, status, scan.
//! - `midi` — MIDI ports, device profile, MIDI Learn, the controller mappings profile.
//! - `control` — the selection in the tree and the keyboard shortcuts.
//! - `zones` — the zone row, the master, the tempo, and the engine's clip host.
//! - `grid` — tracks, clips, scenes and the effect parameter panel.
//! - `autosave` — the unsaved-session autosave and its thread.
//!
//! Every module does `use super::*`, and this file imports every module's items, so a name
//! resolves the same wherever it is used.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autosave;
mod control;
mod grid;
mod midi;
mod network;
mod patch;
mod show;
mod state;
#[cfg(test)]
mod tests;
mod zones;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glam::Vec3;
use prisma_editor::{
    aggregate, load_library, next_fixture_id, next_free_address, plan, plan_preview, save_library,
    EditOp, FixtureLibrary, FixtureType, PatchDraft, PlacementError, PlacementSpec, PreviewRun,
    SelectionAggregate,
};
use prisma_engine::{
    host_interfaces, load_mappings, load_show, save_mappings, save_show, ArtNetNode, Cell,
    ClipEffect, ClipHost, ControlMode, ControllerMappings, DiscoveredNode, Engine, EngineConfig,
    EngineHandle, HostInterface, InterfaceResolution, Layer, LedStyle, MapTarget, MidiMapping,
    MidiNotice, MidiRuntime, NodeMonitor, NodeOrigin, NodeStatus, ParamSet, ScanError, Show,
    ShowNetwork, Source, Takeover, Track, TrackBehaviour, TrackLiveReadout, ZoneLiveReadout,
    ZoneShow,
};
use prisma_engine::{FocusKind, KeyBinding, KeyCombo};
use prisma_engine::{LaunchMode, TrackTransition};
use prisma_midi::io::{input_port_names, MidiInputLink, MidiIoError, MidiOutputLink};
use prisma_midi::{DeviceProfile, APC40_KEYWORDS};
use prisma_patch::{load_patch, save_patch, BlendMode, Diagnostic, Patch, Zone};
use prisma_preview::sink::WebSocketSink;
use prisma_render::{AdapterPreference, ColorChannel, ParamRole, Requires};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use uuid::Uuid;

use autosave::autosave_tick;
use control::*;
use grid::*;
use midi::*;
use network::*;
use patch::*;
use show::*;
use state::*;
use zones::*;

/// Open the M7 patch editor as a **secondary window** (M8-SPEC §6), from the main
/// window's menu. Ownership is already B (this process owns the engine; the editor
/// window talks to it over the same command surface), so opening just brackets the
/// show: the content **freezes** while the editor is open (output stays alive so
/// Highlight works, §7), and on **close** the editor's edits are applied to the running
/// show and it resumes on the new layout ([`apply_editor_draft`]). A second open focuses
/// the existing window.
/// Boot handover, step 1: show the console BEHIND the splash. Called by the splash once
/// the engine and UI are ready and its minimum on-screen time has passed. The splash is
/// always-on-top, so the operator still sees only the splash while the window manager
/// brings the console to its final (maximized) size — on Linux a hidden window ignores
/// size, position and "maximized" until it is shown, so it cannot be sized beforehand.
/// The console keeps its content invisible until that size has settled, then emits
/// `prisma://shown` and the splash fades and calls [`close_splash`].
#[tauri::command]
fn show_main(app: AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        reveal_main(&main);
    }
    let _ = app.emit("prisma://show-main", ());
}

/// Boot handover, step 2: drop the splash and focus the console. Also reveals the console
/// itself if step 1 never happened (the splash's safety cap), so the app can never be
/// left with nothing on screen. Idempotent.
#[tauri::command]
fn close_splash(app: AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        reveal_main(&main);
        let _ = main.set_focus();
    }
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.close();
    }
}

/// Show and maximize the console. No-op once visible.
fn reveal_main(main: &tauri::WebviewWindow) {
    if main.is_visible().unwrap_or(false) {
        return;
    }
    let _ = main.show();
    let _ = main.maximize();
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            editor_get_state,
            editor_selection_aggregate,
            editor_fixture_types,
            editor_library_get,
            editor_library_add_type,
            editor_library_update_type,
            editor_library_remove_type,
            editor_next_free_address,
            editor_next_fixture_id,
            editor_preview_placement,
            editor_place_fixtures,
            editor_translate_fixtures,
            editor_set_zone_mapping_size,
            editor_add_zone,
            editor_rename_zone,
            editor_remove_zone,
            editor_highlight,
            editor_apply_to_live,
            editor_preview_port,
            editor_remove_fixtures,
            editor_save_to,
            editor_open_from,
            editor_undo,
            editor_redo,
            main_zones,
            main_set_zone_dimmer,
            main_set_zone_speed,
            main_network_interfaces,
            main_output_get,
            main_output_set,
            main_open_node_page,
            main_host_name,
            main_output_status,
            main_artnet_scan,
            main_node_statuses,
            main_set_zone_solo,
            main_set_zone_blackout,
            main_set_zone_strobe_on,
            main_set_zone_strobe_rate,
            main_master,
            main_set_master_dimmer,
            main_set_master_blackout,
            main_set_master_strobe_on,
            main_set_master_strobe_rate,
            main_open_editor,
            main_save_show,
            main_new_show,
            main_open_show,
            main_open_demo_show,
            main_effect_catalogue,
            main_assign_cell,
            main_assign_cell_silent,
            main_trigger_cell,
            main_recovery_info,
            main_set_track_blend,
            main_stop_all,
            main_move_track,
            main_set_track_destination,
            main_arm_clear,
            main_set_zone_white,
            main_grid,
            main_arm_cell,
            main_stop_track,
            main_launch_scene,
            main_rename_scene,
            main_arm_go,
            main_set_track_opacity,
            main_set_track_mute,
            main_set_track_solo,
            main_set_track_behaviour,
            main_set_cell_fade,
            main_rename_track,
            main_add_track,
            main_remove_track,
            main_clock,
            main_set_bpm,
            main_tap,
            main_clear_cell,
            main_rename_cell,
            main_set_cell_color,
            main_effect_panel,
            main_set_param,
            main_set_palette,
            midi_status,
            midi_set_input,
            midi_reconnect,
            midi_learn_arm,
            midi_learn_cancel,
            midi_unmap,
            midi_configure,
            midi_set_takeover,
            midi_load_device_defaults,
            main_select,
            main_selection,
            main_last_saved,
            main_key,
            control_keys,
            control_set_key,
            control_reset_keys,
            close_splash,
            show_main,
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // From here on a panic anywhere is reported on the status line, not just logged.
            install_panic_hook(app.handle());
            // The app boots EMPTY (no zones, no patch, nothing running) unless a last show
            // is remembered — then it reopens that `.prisma` (which carries its own embedded
            // patch). This is the "start from zero, or resume the last show" behaviour.
            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?;
            // Network output config (device manager): where DMX goes. Loaded from the
            // per-user app-config dir; a missing/invalid file → defaults (virtual sink),
            // so a fresh install behaves exactly as before.
            let network_path = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?
                .join("network.json");
            let network = std::fs::read_to_string(&network_path)
                .ok()
                .and_then(|json| serde_json::from_str::<NetworkConfig>(&json).ok())
                .unwrap_or_default();

            let last_show_file = config_dir.join("last-show.txt");
            let prepare = |json: String| {
                load_show(&json)
                    .ok()
                    .map(|mut s| {
                        s.patch.fill_zone_members_from_ownership();
                        s.patch.fit_matrix_size();
                        // An old save (empty or 16-cell grids) is padded to the full grid.
                        pad_grids(&mut s);
                        s
                    })
                    .filter(|s| s.patch.compile().is_ok())
            };
            // An autosave on disk means the last session ended with unsaved changes:
            // recover it (and keep a backup); otherwise reopen the last saved show.
            // `--demo` opens the demo show instead (manual tests, the UI end-to-end
            // tests): no recovery, the last-show memory untouched, and an unsaved session
            // still backed up first, since the demo's own autosave replaces it.
            let demo = std::env::args().any(|a| a == "--demo");
            let autosave = autosave::Autosave::new(&config_dir);
            let last_saved = read_last_show(&last_show_file);
            if demo && autosave.read().is_some() {
                autosave.rotate_backups();
            }
            let recovered_show = if demo {
                None
            } else {
                autosave.read().and_then(prepare)
            };
            let recovered = recovered_show.is_some().then(|| {
                autosave.rotate_backups();
                RecoveryInfo {
                    last_saved: last_saved
                        .as_ref()
                        .filter(|p| autosave::exists(p))
                        .map(|p| p.to_string_lossy().into_owned()),
                }
            });
            let demo_show = demo
                .then(|| save_show(&prisma_engine::demo::demo_show()).ok())
                .flatten()
                .and_then(prepare);
            let mut show = recovered_show
                .or(demo_show)
                .or_else(|| {
                    last_saved
                        .as_ref()
                        .and_then(|p| std::fs::read_to_string(p).ok())
                        .and_then(prepare)
                })
                .unwrap_or_else(empty_show);
            // A show from before the device manager inherits the old app-wide target.
            prepare_show_network(&mut show, &network);
            // Keep an editable copy as the grid's + zone-set source of truth; the engine
            // runs its own copy and follows our triggers through the queue.
            let show_state = show.clone();
            // The editor draft opens on the same patch (empty until the operator builds a
            // rig, or the loaded show's rig).
            let draft_patch = show.patch.clone();

            // Prefer the real GPU, fall back to software so the window still opens without
            // a configured Vulkan driver. Each attempt binds its own preview WebSocket (a
            // failed attempt frees its port before the next binds).
            let target = virtual_sink();
            let (config_hp, port_hp) = preview_config(AdapterPreference::HighPerformance, target);
            let (engine, preview_port) = match Engine::start_show(show.clone(), config_hp) {
                Ok(engine) => (engine, port_hp),
                Err(e_hp) => {
                    let (config_sw, port_sw) = preview_config(AdapterPreference::Software, target);
                    match Engine::start_show(show, config_sw) {
                        Ok(engine) => (engine, port_sw),
                        Err(e_sw) => {
                            // No GPU at all (a bare VM with neither a Vulkan driver nor a
                            // DX12/WARP fallback). Release builds have no console
                            // (`windows_subsystem = "windows"`), so record why the boot
                            // failed to a file the operator can read, then surface the error.
                            let msg = format!(
                                "Prisma could not start the render engine — no usable GPU.\n\
                                 HighPerformance adapter: {e_hp}\n\
                                 Software adapter: {e_sw}\n"
                            );
                            let _ = std::fs::write(config_dir.join("boot-error.txt"), &msg);
                            return Err(msg.into());
                        }
                    }
                }
            };

            // The show runs LIVE (not frozen) — the main window operates content. The
            // editor's freeze bracket returns with the editor window in a later step.

            // Load the operator's fixture library from the per-user app-config dir
            // (§3.2). First run: seed a few generic types and write the file, so the
            // placement dialog is never empty. A present-but-invalid file is left
            // untouched (so a hand-edit mistake is recoverable) and a seeded library is
            // used in memory for the session.
            let library_path = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("no app config dir: {e}"))?
                .join("fixture-library.json");
            let library = match std::fs::read_to_string(&library_path) {
                Ok(json) => load_library(&json).unwrap_or_else(|e| {
                    eprintln!(
                        "prisma: fixture library at {} is invalid ({e}); \
                         using a seeded library in memory (file left untouched)",
                        library_path.display()
                    );
                    FixtureLibrary::seed()
                }),
                Err(_) => {
                    let lib = FixtureLibrary::seed();
                    if let Err(e) = persist_library(&library_path, &lib) {
                        eprintln!("prisma: could not write seed fixture library: {e}");
                    }
                    lib
                }
            };

            // MIDI (M9): the operator's controller profile + which input to open, both
            // per-user. A present-but-invalid profile is left untouched on disk (so a
            // hand-edit mistake is recoverable) and an empty one is used for the session.
            let mappings_path = config_dir.join(MAPPINGS_FILE);
            let profile = match std::fs::read_to_string(&mappings_path) {
                Ok(json) => load_mappings(&json).unwrap_or_else(|e| {
                    eprintln!(
                        "prisma: controller mappings at {} are invalid ({e}); \
                         using an empty profile in memory (file left untouched)",
                        mappings_path.display()
                    );
                    ControllerMappings::new("default")
                }),
                Err(_) => ControllerMappings::new("default"),
            };
            let midi = engine.midi().clone();
            midi.set_profile(profile);
            install_midi_listener(app.handle(), &midi, mappings_path);
            let midi_config_path = config_dir.join("midi.json");
            let midi_config = std::fs::read_to_string(&midi_config_path)
                .ok()
                .and_then(|json| serde_json::from_str::<MidiConfig>(&json).ok())
                .unwrap_or_default();

            app.manage(AppState {
                engine: Mutex::new(Some(engine)),
                show: Mutex::new(show_state),
                draft: Mutex::new(PatchDraft::new(draft_patch)),
                library: Mutex::new(library),
                library_path,
                preview_port: Mutex::new(preview_port.unwrap_or(0)),
                network: Mutex::new(network),
                artnet_monitor: Mutex::new(None),
                last_show_file,
                midi,
                midi_link: Mutex::new(None),
                midi_out_link: Mutex::new(None),
                midi_config: Mutex::new(midi_config),
                midi_config_path,
                midi_error: Mutex::new(None),
                armed: Mutex::new(Default::default()),
                autosave,
                autosave_state: Mutex::new((
                    if recovered.is_some() {
                        autosave::Baseline::Recovered
                    } else {
                        autosave::Baseline::Pending
                    },
                    None,
                )),
                recovered: Mutex::new(recovered),
            });
            // Autosave: a snapshot every couple of seconds, off every hot path.
            let autosaver = app.handle().clone();
            let spawned = std::thread::Builder::new()
                .name("prisma-autosave".into())
                .spawn(move || loop {
                    std::thread::sleep(autosave::INTERVAL);
                    autosave_tick(&autosaver);
                });
            if let Err(e) = spawned {
                // The app runs without autosave; saving by hand still works.
                report_error(
                    app.handle(),
                    format!("El autoguardado no pudo iniciarse ({e}); guardá a mano"),
                );
            }
            // Track clips play from the Parameter Tree; the engine keeps this host
            // across every recompile. Set after `manage` — the host reads the state.
            if let Some(engine) = app.state::<AppState>().engine.locked().as_ref() {
                engine.set_clip_host(Arc::new(AppCellHost {
                    app: app.handle().clone(),
                }));
            }
            // Open the MIDI input off the boot path's critical section: a missing device
            // just leaves the link empty (the device manager can reconnect later).
            connect_midi(app.handle());
            // Art-Net node monitor (status dots + Scan). A held 6454 (another lighting program open on this
            // PC) just leaves it off; the Scan retries and explains.
            let _ = ensure_artnet_monitor(app.handle());

            // Boot splash safety net: the splash webview normally reveals the console (via
            // `close_splash`) once the UI signals `prisma://ready`, and it has its own hard
            // cap. This is the backstop for a splash whose JS never runs: after a generous
            // timeout, reveal the console the same way and drop the splash, so the app can
            // never boot to nothing on screen. No-op if the normal path got there first.
            let boot = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(20));
                if let Some(main) = boot.get_webview_window("main") {
                    reveal_main(&main);
                    let _ = main.set_focus();
                }
                if let Some(splash) = boot.get_webview_window("splash") {
                    let _ = splash.close();
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // The window could not be built (a boot error from `setup`, or the webview
            // itself): there is no status line to show it on. Release builds have no
            // console either, so it is written next to the boot-error file and the app
            // exits cleanly instead of panicking.
            let msg = format!("Prisma could not start: {e}\n");
            eprintln!("prisma: {msg}");
            if let Some(dir) = dirs_config() {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join("boot-error.txt"), &msg);
            }
            std::process::exit(1)
        })
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                // Last autosave step while the engine still holds the live values.
                autosave_tick(app_handle);
                if let Some(state) = app_handle.try_state::<AppState>() {
                    // Close the MIDI input before the engine, so no input thread is writing
                    // while it tears down; darken the LEDs, then close the output.
                    state.midi_link.locked().take();
                    state.midi.clear_leds();
                    state.midi.set_output(None);
                    state.midi_out_link.locked().take();
                    if let Some(engine) = state.engine.locked().take() {
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

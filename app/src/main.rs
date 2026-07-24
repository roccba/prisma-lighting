//! Prisma desktop shell.
//!
//! S1's job here is narrow and deliberate: **own the engine and shut it down
//! cleanly on exit.** The shell starts [`prisma_engine::Engine`] once, keeps the
//! [`EngineHandle`] in Tauri managed state for the life of the process, and calls
//! [`EngineHandle::shutdown`] on `RunEvent::ExitRequested` — before the process
//! goes away — so the loops stop in order and the GPU is released. The engine's
//! own `Drop` backstop covers any exit path that skips `ExitRequested` (a panic).
//!
//! The IPC command surface (patch load/validate/edit) and the editor window are
//! S2; the React canvas is S3+. This file is intentionally just the lifecycle.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use prisma_engine::{Engine, EngineConfig, EngineHandle};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    PixelRef, Zone,
};
use prisma_render::AdapterPreference;
use tauri::{Manager, RunEvent};
use uuid::Uuid;

/// Tauri managed state: the running engine, taken (`None`) once shut down so
/// shutdown runs exactly once.
struct EngineState(Mutex<Option<EngineHandle>>);

/// The patch the shell boots with until the editor loads a real one (S2). A single
/// one-pixel fixture in a "Default" zone: enough to bring the pipeline up so
/// Highlight and the preview have something to drive, nothing more.
fn default_patch() -> Patch {
    let fixture_id = Uuid::from_u128(0x01);
    let zone_id = Uuid::from_u128(0x0d);
    Patch {
        id: Uuid::new_v4(),
        name: "Untitled".into(),
        matrix_size: [1, 1, 1],
        fixtures: vec![Fixture {
            id: fixture_id,
            name: "Fixture 1".into(),
            profile: FixtureProfile {
                name: "RGB".into(),
                components: ComponentSet::Rgb,
                gamma: GammaCalibration::default(),
            },
            pixels: vec![Pixel {
                index: 0,
                position: glam::Vec3::ZERO,
                universe: 1,
                start_channel: 1,
                color_order: ColorOrder::Rgb,
            }],
            zone: zone_id,
        }],
        zones: vec![Zone {
            id: zone_id,
            name: "Default".into(),
            members: vec![PixelRef {
                fixture: fixture_id,
                pixel: 0,
            }],
            bounds: None,
            z_order: 0,
            blend_mode: BlendMode::Normal,
            mapping_size: [1, 1, 1],
            display_color: [230, 57, 70],
            world_transform: None,
        }],
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // Dev/default routing: Art-Net to loopback on the standard port.
            // Loopback (not broadcast) avoids needing SO_BROADCAST and never sprays
            // the LAN; real per-node routing arrives with the network-config
            // milestone. Nothing need be listening — unheard packets are harmless.
            let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6454);
            // Prefer the real GPU; fall back to the software adapter (lavapipe) so
            // the window still opens on a box without a configured Vulkan driver.
            let engine = match Engine::start(
                default_patch(),
                EngineConfig::new(AdapterPreference::HighPerformance, target),
            ) {
                Ok(engine) => engine,
                Err(_) => Engine::start(
                    default_patch(),
                    EngineConfig::new(AdapterPreference::Software, target),
                )?,
            };
            app.manage(EngineState(Mutex::new(Some(engine))));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Prisma app")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                // App-side half of the S1 shutdown criterion: stop the engine in
                // order before the process exits.
                if let Some(state) = app_handle.try_state::<EngineState>() {
                    if let Some(engine) = state.0.lock().unwrap().take() {
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

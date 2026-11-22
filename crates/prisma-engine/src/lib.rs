//! The engine assembly (CLAUDE.md §3.3, milestone M7/S1).
//!
//! This crate owns the one object that **is** the running product: [`Engine`]
//! brings up the GPU, the master clock, the Parameter Tree and the compiled patch,
//! then spawns the three long-lived loops — render, output, preview — and hands
//! back an [`EngineHandle`]. The handle stops them **in order** on
//! [`EngineHandle::shutdown`] (and, as a backstop, on drop).
//!
//! # Why a crate, not code inside `app/`
//!
//! The assembly depends on every runtime crate (core, patch, render, output,
//! preview), so it sits at the top of the crate DAG with only the Tauri `app/`
//! shell above it. Keeping it here — not inside the Tauri binary — buys two things
//! the design needs:
//!
//! - **Headless testability.** The boot + clean-shutdown path is verified on the
//!   software GPU (lavapipe) with no Tauri and no window, the same way M5/M6 tests
//!   drive the loops.
//! - **Reuse by M8.** The main window (M8) is a *second* consumer of the same
//!   `Engine`. If the assembly lived in the Tauri binary, M8 would have to rebuild
//!   it. This crate is the "definitive structure" the shell and the main window
//!   both sit on.
//!
//! # What S1 deliberately leaves for later
//!
//! - **Content is [`IdleContent`]** (black) — the "no show loaded" state. M8's
//!   per-zone compositor replaces it; the engine's shape does not change.
//! - **Output routing is a single `SocketAddr`.** Per-universe / per-NIC routing
//!   arrives with the network-config milestone (CLAUDE.md Deferred).
//! - **Highlight** (writing into the live output while the show is frozen, §7)
//!   is M7/S7; the seam is the Parameter Tree and the output stage, both already
//!   owned here.

pub mod clips;
pub mod config;
pub mod content;
pub mod engine;
pub mod zone_content;

pub use clips::{
    ClipEffect, ClipHost, GridParams, TrackBehaviour, CLIP_ARMED, CLIP_ASSIGNED, CLIP_EMPTY,
    CLIP_RUNNING,
};
pub use config::EngineConfig;
pub use content::IdleContent;
pub use engine::{
    Engine, EngineError, EngineHandle, OutputRouting, ShutdownReport, TrackLiveReadout,
    ZoneLiveReadout,
};
// Host interfaces for the Device Manager (docs/DEVICES-SPEC.md §6).
pub use prisma_output::netif::{host_interfaces, HostInterface, InterfaceResolution};
// Art-Net Scan (docs/DEVICES-SPEC.md §7).
pub use prisma_output::discovery::{scan as artnet_scan, DiscoveredNode, ScanError};
// The node monitor: single owner of UDP 6454 — status + Scan (§7–§8).
pub use prisma_output::monitor::{NodeMonitor, NodeStatus};

// The Show model the assembly consumes, re-exported so the app (which sits on this
// crate) can build a `Show` without depending on `prisma-core` directly.
pub use prisma_core::show::{
    load_show, save_show, ArtNetNode, Cell, Destination, InterfaceRef, LaunchMode, Layer,
    MasterState, NodeOrigin, ParamSet, SceneLabel, Show, Source, Track, TrackTransition,
    UniverseRange, ZoneLive, ZoneShow,
};
// The Show's network output (docs/DEVICES-SPEC.md), aliased so it never reads as the
// app's legacy app-wide `NetworkConfig` (`network.json`) it replaces.
pub use prisma_core::show::NetworkConfig as ShowNetwork;
// The MIDI transport (M9) and its document types, re-exported for the app shell.
pub use prisma_core::mappings::{
    load_mappings, save_mappings, ControlMode, ControllerMappings, LedAnimation, LedRate, LedStyle,
    MapTarget, MidiControl, MidiMapping, SlotIds, Takeover, ZoneSub,
};
pub use prisma_midi::{MidiNotice, MidiRuntime};
pub use zone_content::{
    build_track_effects, build_track_instances, destination_slots, seed_live_from_show,
    track_param_prefix, CompositorOptions, Fade, TrackContent, TrackLive, ZoneCommand,
    ZoneCommandSender, ZoneCompositor,
};

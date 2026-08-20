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

pub mod config;
pub mod content;
pub mod engine;
pub mod zone_content;

pub use config::EngineConfig;
pub use content::IdleContent;
pub use engine::{Engine, EngineError, EngineHandle, ShutdownReport, ZoneLiveReadout};

// The Show model the assembly consumes, re-exported so the app (which sits on this
// crate) can build a `Show` without depending on `prisma-core` directly.
pub use prisma_core::show::{
    load_show, save_show, Cell, Layer, MasterState, ParamSet, Show, Source, ZoneLive, ZoneShow,
};
pub use zone_content::{
    build_zone_effects, seed_live_from_show, ZoneCommand, ZoneCommandSender, ZoneCompositor,
};

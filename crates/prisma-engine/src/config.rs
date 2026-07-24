//! Configuration for bringing the engine up.

use std::net::SocketAddr;

use prisma_preview::PreviewSink;
use prisma_render::AdapterPreference;

/// Everything [`Engine::start`](crate::Engine::start) needs that is not the patch
/// itself. Fields are public; [`EngineConfig::new`] fills the common defaults.
pub struct EngineConfig {
    /// Which GPU adapter to bring up. `Software` (lavapipe) for headless/dev and
    /// CI determinism; `HighPerformance` for the real product.
    pub adapter: AdapterPreference,

    /// Where DMX goes. Every universe is sent here for now — dev points it at the
    /// virtual sink; a real rig points it at a node or the Art-Net broadcast
    /// address. Per-universe / per-NIC routing is deferred with the network-config
    /// milestone (CLAUDE.md Deferred); the field is a `SocketAddr` today, not a
    /// richer routing table, on purpose.
    pub output_target: SocketAddr,

    /// Desired refresh for every universe, in Hz. Art-Net's practical ceiling is
    /// ~44 (§1.4). The **render rate is derived from this**, not set independently:
    /// the render loop runs at the fastest active output rate (§1.4), computed from
    /// the output [`RatePlan`](prisma_output::rate::RatePlan). This closes the M5
    /// gap "render rate is not derived from the active output set".
    pub output_rate_hz: u32,

    /// Preview stream rate, in Hz. Off the §1.5 budget; normal priority (§M6).
    pub preview_rate_hz: u32,

    /// Where preview frames go. `None` runs the preview loop with no consumer
    /// (zero cost — it encodes nothing). The Tauri-IPC sink drops in here when the
    /// webview exists; a `WebSocketSink` or `ChannelSink` can be supplied now.
    pub preview_sink: Option<Box<dyn PreviewSink>>,

    /// Pin the render thread to this core (affinity only — the render thread is
    /// **not** RT-elevated; it must stay below the output thread, §3.3). Closes the
    /// M5 gap "no priority/affinity for the render thread" without inverting the
    /// priority order. `None` leaves the OS scheduler in charge.
    pub render_pin_core: Option<usize>,

    /// Pin the output thread to this core. The output loop also RT-elevates itself
    /// (M1); this is the affinity half. `None` leaves it unpinned.
    pub output_pin_core: Option<usize>,

    /// Broadcast an `ArtSync` after each output frame so multi-node rigs latch
    /// tear-free (§5.1). `None` = no sync (each `ArtDmx` latches on receipt).
    pub artsync_target: Option<SocketAddr>,
}

impl EngineConfig {
    /// A config with the common defaults: 44 Hz per universe, 30 Hz preview, no
    /// preview sink, no pinning, no ArtSync. Set the remaining fields directly.
    pub fn new(adapter: AdapterPreference, output_target: SocketAddr) -> Self {
        Self {
            adapter,
            output_target,
            output_rate_hz: 44,
            preview_rate_hz: 30,
            preview_sink: None,
            render_pin_core: None,
            output_pin_core: None,
            artsync_target: None,
        }
    }
}

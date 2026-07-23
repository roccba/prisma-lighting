//! `prisma-preview` — the virtual preview (CLAUDE.md M6, §6).
//!
//! A **second consumer of the master pixel buffer**, not a second renderer. The
//! readback the output path already performs produces a pixel-domain buffer
//! (thousands of values); the preview reuses it, applies the output stage's flat
//! parameters through the *same* shared function the wire encoder uses, encodes to
//! sRGB, and streams it to a consumer. No second GPU pass, no HD framebuffer over
//! IPC, no effect logic duplicated in the webview.
//!
//! Layering (M6 build steps):
//!   - [`plan`]       — master buffer → linear-light screen RGB (P0).
//!   - [`frame`]      — the preview handoff payload and its producer (P1).
//!   - [`encode`] / [`descriptor`] — sRGB + the binary wire format (P2).
//!   - [`preview_loop`] — the fixed-interval preview thread (P3).
//!   - `sink`         — in-process and WebSocket transports (P4).
//!
//! # It is never on the critical path
//!
//! The preview thread runs at normal priority or below and must never compete with
//! output. Its latency is **not** part of the §1.5 budget, which is about light on
//! the rig; under backpressure it drops frames rather than blocking anything.

pub mod descriptor;
pub mod encode;
pub mod frame;
pub mod plan;
pub mod preview_loop;

pub use descriptor::{DescriptorPixel, DescriptorZone, PreviewDescriptor};
pub use encode::{encode_frame, linear_to_srgb8, PreviewEncodeError, FORMAT_VERSION, MAGIC};
pub use frame::{preview_handoff, PreviewConsumer, PreviewFrame, PreviewProducer, ZoneActiveMask};
pub use plan::{PreviewPixel, PreviewPlan, NO_COMPONENT};
pub use preview_loop::{
    run_preview_loop, PreviewLoop, PreviewLoopConfig, PreviewOutcome, PreviewSink, PreviewStats,
};

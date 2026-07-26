//! `prisma-output` — output pipeline and virtual sink (CLAUDE.md M1, §5.1).
//!
//! Layering (see the M1 build slice):
//!   - [`packet`]  — pure protocol codec (build + parse), no I/O, no hot-path alloc.
//!   - `frame`     — latest-frame handoff (double-buffer), added at step 3.
//!   - `router` / `universe` — master buffer → per-universe DMX, step 3.
//!   - `transport` — UDP socket, interface binding, link-state (step 4).
//!   - `scheduler` / `rate` — fixed-interval output thread(s), step 4.
//!   - `timing`    — jitter / latency probe, step 6.
//!   - `sink`      — virtual Art-Net/sACN receiver, step 7.
//!
//! Art-Net is the first vertical slice; sACN and DDP reuse this same structure.

pub mod affinity;
pub mod frame;
pub mod highlight;
pub mod packet;
pub mod plan;
pub mod priority;
pub mod rate;
pub mod router;
pub mod scheduler;
pub mod sink;
pub mod timing;
pub mod transport;
pub mod universe;

//! Asynchronous GPU→CPU readback of the master buffer (CLAUDE.md §5.2 step 5).
//!
//! # The render thread cannot block here — structurally, not by discipline
//!
//! This type exposes **no blocking wait at all**. Its only poll is
//! [`wgpu::PollType::Poll`], which returns immediately; there is no method that
//! waits on a map. The blocking helper `Sampler::read_master_f16` exists for
//! tests and setup and is deliberately not used by the render loop. So "never
//! block the render thread" (§3.3) is not a rule someone must remember — there is
//! nothing here to call that could.
//!
//! # What happens when frame N's readback is not ready as frame N+1 begins
//!
//! A slot is claimed **before** any work for the new frame is recorded. If every
//! slot is still in flight, the frame is **skipped entirely**: nothing is
//! rendered, nothing is submitted, and [`ReadbackRing::stalls`] counts it.
//!
//! That rule, rather than the alternatives, because:
//!
//! - **Waiting** is forbidden (§3.3) and would convert a GPU hiccup into audible
//!   output jitter.
//! - **Reusing an in-flight slot** is unsound: a `map_async` cannot be cancelled,
//!   so its callback would land on a buffer already being rewritten.
//! - **Submitting anyway and discarding the result** adds GPU work at exactly the
//!   moment the GPU is already more than `slots` frames behind, deepening the very
//!   backlog that caused the stall.
//!
//! Skipping applies back-pressure at the only safe point. The show does not
//! freeze: the clock still advances, and the output thread re-sends its last frame
//! (timing accuracy over freshness, §3.3), so a stall reads as one held frame
//! rather than a glitch. A non-zero stall count means the GPU cannot keep up with
//! the render rate, which is a capacity problem to be seen in the probe, not
//! papered over here.

use prisma_core::OverlapSnapshot;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

const FREE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const READY: u8 = 2;
const FAILED: u8 = 3;

/// Default number of readback slots. Deep enough to cover the one-to-two frames a
/// GPU pipeline typically runs behind without unbounded memory; shallow enough
/// that genuine overload shows up as stalls instead of ever-growing latency.
pub const DEFAULT_SLOTS: usize = 3;

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// When the frame using this slot was submitted — the start of the readback
    /// latency measurement (§1.5).
    submit_at: Instant,
    /// When the render frame began (`MasterClock::tick`), for the tick→wire chain.
    tick_at: Instant,
    frame: u64,
    /// The classification THIS frame was rendered under. It must travel with the
    /// slot: a readback completing now belongs to a frame from several ticks ago,
    /// and publishing it with the *current* snapshot would break the "exactly one
    /// stage" invariant (§5.2.1) precisely when the mask has just changed.
    overlap: Option<Arc<OverlapSnapshot>>,
}

/// Timing metadata for a completed readback.
#[derive(Debug, Clone)]
pub struct ReadyMeta {
    pub frame: u64,
    /// When the copy was submitted — start of the readback latency measurement.
    pub submit_at: Instant,
    /// When the render frame began (`MasterClock::tick`) — start of tick→wire.
    pub tick_at: Instant,
    /// When the render loop first observed this readback as complete.
    pub ready_at: Instant,
    /// The snapshot this frame was rendered under; publish it alongside the pixels.
    pub overlap: Option<Arc<OverlapSnapshot>>,
}

impl ReadyMeta {
    /// Submit → observed complete. This is the latency the pipeline actually
    /// experiences, so it is the number that feeds the §1.5 budget; note it is
    /// bounded below by the loop's poll cadence.
    #[inline]
    pub fn readback_latency(&self) -> std::time::Duration {
        self.ready_at.saturating_duration_since(self.submit_at)
    }
}

/// A ring of readback staging buffers, cycled without ever blocking.
pub struct ReadbackRing {
    slots: Vec<Slot>,
    component_count: usize,
    stalls: u64,
    failures: u64,
}

impl ReadbackRing {
    /// Allocate `slots` staging buffers, each sized for `component_count` `f32`s.
    pub fn new(gpu: &crate::Gpu, component_count: usize, slots: usize) -> ReadbackRing {
        let size = (component_count * std::mem::size_of::<f32>()).max(4) as u64;
        let now = Instant::now();
        let slots = (0..slots.max(1))
            .map(|i| Slot {
                buffer: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("readback-slot-{i}")),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                submit_at: now,
                tick_at: now,
                frame: 0,
                overlap: None,
            })
            .collect::<Vec<_>>();
        ReadbackRing {
            slots,
            component_count,
            stalls: 0,
            failures: 0,
        }
    }

    /// Frames skipped because every slot was still in flight — i.e. the GPU is
    /// more than `slots` frames behind. See the module docs for why skipping is
    /// the chosen response.
    #[inline]
    pub fn stalls(&self) -> u64 {
        self.stalls
    }

    /// Readback maps that failed (a device error). Counted separately from stalls
    /// because they mean something is wrong, not merely slow.
    #[inline]
    pub fn failures(&self) -> u64 {
        self.failures
    }

    #[inline]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Claim a free slot for this frame, or `None` if all are in flight — in which
    /// case the caller must **skip the frame entirely** and record the stall.
    /// Call this before recording any GPU work.
    pub fn try_claim(&mut self) -> Option<usize> {
        let free = self
            .slots
            .iter()
            .position(|s| s.state.load(Ordering::Acquire) == FREE);
        match free {
            Some(i) => {
                self.slots[i].state.store(IN_FLIGHT, Ordering::Release);
                Some(i)
            }
            None => {
                self.stalls += 1;
                None
            }
        }
    }

    /// The staging buffer for a claimed slot; the gather copies the master buffer
    /// into it.
    #[inline]
    pub fn buffer(&self, slot: usize) -> &wgpu::Buffer {
        &self.slots[slot].buffer
    }

    /// Register the async map for a claimed slot whose copy has been submitted.
    /// Returns immediately; the callback only flips an atomic.
    pub fn begin_map(
        &mut self,
        slot: usize,
        tick_at: Instant,
        frame: u64,
        overlap: Option<Arc<OverlapSnapshot>>,
    ) {
        let s = &mut self.slots[slot];
        s.submit_at = Instant::now();
        s.tick_at = tick_at;
        s.frame = frame;
        s.overlap = overlap;
        let state = Arc::clone(&s.state);
        s.buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            state.store(if r.is_ok() { READY } else { FAILED }, Ordering::Release);
        });
    }

    /// Let wgpu run any completed map callbacks. **Non-blocking**: this is
    /// `PollType::Poll`, never a wait.
    pub fn poll(&self, gpu: &crate::Gpu) {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
    }

    /// Consume the oldest completed readback, if any, handing its components to
    /// `f`. The slot is unmapped and freed as soon as `f` returns — release is
    /// structural, not the caller's responsibility.
    ///
    /// Returns `None` when nothing has completed yet, which is the normal case for
    /// the first frames after start-up.
    pub fn consume_ready<R>(
        &mut self,
        ready_at: Instant,
        f: impl FnOnce(&ReadyMeta, &[f32]) -> R,
    ) -> Option<R> {
        let mut candidate: Option<usize> = None;
        for i in 0..self.slots.len() {
            match self.slots[i].state.load(Ordering::Acquire) {
                READY => {
                    let older = candidate.is_none_or(|c| self.slots[i].frame < self.slots[c].frame);
                    if older {
                        candidate = Some(i);
                    }
                }
                FAILED => {
                    // Surface it and recycle: a broken map must not wedge the ring.
                    self.failures += 1;
                    self.slots[i].state.store(FREE, Ordering::Release);
                }
                _ => {}
            }
        }
        let slot = candidate?;
        let meta = ReadyMeta {
            frame: self.slots[slot].frame,
            submit_at: self.slots[slot].submit_at,
            tick_at: self.slots[slot].tick_at,
            ready_at,
            overlap: self.slots[slot].overlap.clone(),
        };
        let result = {
            let view = self.slots[slot]
                .buffer
                .slice(..)
                .get_mapped_range()
                .expect("a slot reported READY, so its range is mapped");
            let data: &[f32] = bytemuck::cast_slice(&view);
            f(&meta, &data[..self.component_count])
        };
        self.slots[slot].buffer.unmap();
        self.slots[slot].overlap = None;
        self.slots[slot].state.store(FREE, Ordering::Release);
        Some(result)
    }
}

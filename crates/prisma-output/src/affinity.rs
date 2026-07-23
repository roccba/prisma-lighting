//! Output-thread core affinity — an **opt-in power-user knob, NOT the default**.
//!
//! # Read this before enabling core pinning
//!
//! Pinning the output thread to a core is **not** the way Prisma achieves stable
//! timing. Real-time scheduling is ([`crate::priority`]). Core affinity is here
//! only for operators building dedicated, OS-tuned show machines. **Enabled
//! naively it makes jitter worse, not better.** Measured on the M1 `probe`
//! (16 logical cores = 8 physical × 2 SMT, full CPU oversubscription, 20 s):
//!
//! - **Naive pin** (output → one logical core, sleeping between ticks):
//!   σ 2.7–4.6 ms, max **68–148 ms** — *worse than not pinning at all*
//!   (σ 1.01 ms floating). Causes: C-state wakeup latency on an otherwise-idle
//!   reserved core, and SMT-sibling contention (a "free" logical core shares
//!   execution units with its busy sibling on the same physical core).
//! - **Done right** (reserve the *whole physical core* = both SMT siblings,
//!   busy-poll to keep it hot, pin your own other threads away): the common
//!   case gets excellent (p99 4.5 µs) but a **~25 ms tail remains** from
//!   per-CPU kernel work (IRQs, timer softirqs, RCU kthreads). Removing that
//!   needs `isolcpus` / `nohz_full` **boot parameters** (root + reboot) — more
//!   invasive than requesting real-time scheduling, and still worse than it
//!   (SCHED_FIFO under the same load: σ 1.6 µs, max 30.3 µs).
//!
//! **Therefore:** affinity is only worth enabling when the operator has *also*
//! isolated the core at boot (`isolcpus`), pinned every other hot thread away
//! from it, and is keeping it hot. On a normal machine, leave it off and rely
//! on [`crate::priority`]. The scheduler defaults `pin_to_core` to `None`.

/// Pin the current thread to `core`, returning the core id if it took effect,
/// or `None` if affinity could not be set.
///
/// See the module docs: this alone is counterproductive under load. It belongs
/// only in a fully core-isolated setup.
pub fn pin_current_thread(core: usize) -> Option<usize> {
    if core_affinity::set_for_current(core_affinity::CoreId { id: core }) {
        Some(core)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinning_returns_the_core_or_none() {
        // Core 0 exists everywhere; pinning to it should take effect.
        let pinned = pin_current_thread(0);
        assert!(pinned == Some(0) || pinned.is_none());
    }
}

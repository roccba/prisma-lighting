//! Cross-platform real-time scheduling elevation for the output thread.
//!
//! # Decision record — why priority elevation, and why NOT core affinity
//!
//! "Jitter is the enemy" (CLAUDE.md §1.3). The output thread must hit its
//! fixed interval even when the machine is under a real show's load (UI + video
//! decode + effects). Measured on the M1 `probe`, 4 universes @ 44 Hz, on a
//! 16-logical-core box (8 physical × 2 SMT), 20 s per run:
//!
//! | Strategy (full CPU oversubscription)              | jitter σ | max spike | late/20s |
//! |---------------------------------------------------|---------:|----------:|---------:|
//! | Normal scheduler (no elevation)                   |  1.01 ms |    6.9 ms |       78 |
//! | App core affinity, naive (1 logical core)         | 2.7–4.6 ms | 68–148 ms |    29–76 |
//! | App core affinity, "done right" (reserved phys.   |          |           |          |
//! |   core + busy-poll + own threads pinned away)     |   1.3 ms |   25.7 ms |        4 |
//! | **SCHED_FIFO (real-time class)**                  | **1.6 µs** | **30.3 µs** |  **0** |
//!
//! For reference, Normal at *idle* is σ 2.5 µs / max 45.9 µs — i.e. FIFO under
//! full saturation beats Normal with the machine doing nothing.
//!
//! **Chosen strategy: request the platform's real-time scheduling class.** It
//! addresses the actual cause (preemption by other threads) directly, needs no
//! whole reserved core, and on the primary market (Windows) and macOS needs
//! **no administrator privileges** (MMCSS / Mach time-constraint). This keeps
//! the Phase-2 installer simple.
//!
//! **Rejected: application-level core affinity as the default.** The data above
//! shows it is fragile and insufficient. Naive pinning is *worse* than doing
//! nothing (C-state wakeup latency on an idle reserved core + SMT-sibling
//! contention). Even "done right" it leaves a ~25 ms tail from per-CPU kernel
//! work (IRQs / softirqs / RCU kthreads) that only `isolcpus`/`nohz_full` boot
//! parameters remove — root + reboot, *more* invasive than RT, not less. See
//! [`crate::affinity`], where affinity survives only as an opt-in power-user
//! knob with that warning attached. If you are revisiting this decision: rerun
//! `probe` under load in each mode before changing it.
//!
//! # Per-platform mechanism (primary → fallback)
//!
//! - **Windows** (main market): MMCSS "Pro Audio" thread class → `TIME_CRITICAL`.
//! - **macOS**: Mach `THREAD_TIME_CONSTRAINT_POLICY` (real-time thread).
//! - **Linux**: RtKit (`SCHED_FIFO` without root, as PipeWire/JACK use) → plain
//!   `SCHED_FIFO` (needs CAP_SYS_NICE or an `rtprio` limit set at install).
//!
//! The mode **actually achieved** is returned and reported by the probe (and,
//! later, shown in the UI) so an operator can tell real-time from best-effort
//! without reading logs.

use std::time::Duration;

/// The scheduling class actually obtained on this machine. Named per mechanism
/// (not a generic "elevated/normal") so the probe/UI can show exactly what the
/// output thread got: MMCSS, QoS, RtKit, plain FIFO, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingMode {
    /// Windows MMCSS "Pro Audio" task class — near-real-time, no admin.
    MmcssProAudio,
    /// Windows `THREAD_PRIORITY_TIME_CRITICAL` (MMCSS unavailable).
    WindowsTimeCritical,
    /// macOS Mach time-constraint (real-time) thread policy — no privileges.
    MacTimeConstraint,
    /// Linux `SCHED_FIFO` granted through RtKit — no root.
    RtkitFifo,
    /// Linux `SCHED_FIFO` obtained directly (CAP_SYS_NICE / `rtprio` limit).
    PlainFifo,
    /// No real-time class obtained — best-effort fallback.
    Normal,
}

impl SchedulingMode {
    /// Whether a real-time class was obtained (any non-`Normal` mode).
    pub fn is_realtime(self) -> bool {
        !matches!(self, SchedulingMode::Normal)
    }

    /// Short human label for logs / UI.
    pub fn label(self) -> &'static str {
        match self {
            SchedulingMode::MmcssProAudio => "Windows MMCSS (Pro Audio)",
            SchedulingMode::WindowsTimeCritical => "Windows TIME_CRITICAL",
            SchedulingMode::MacTimeConstraint => "macOS time-constraint",
            SchedulingMode::RtkitFifo => "Linux SCHED_FIFO (RtKit)",
            SchedulingMode::PlainFifo => "Linux SCHED_FIFO (direct)",
            SchedulingMode::Normal => "Normal (best-effort, no RT)",
        }
    }
}

/// Elevate the current thread to the best real-time class the platform grants,
/// returning the mode actually achieved. `tick_period` is the output tick
/// interval; some platforms (macOS time-constraint) size their guarantees from
/// it. Never fails — an ungranted elevation returns [`SchedulingMode::Normal`].
pub fn elevate_current_thread(tick_period: Duration) -> SchedulingMode {
    #[cfg(target_os = "linux")]
    {
        let _ = tick_period;
        linux::elevate()
    }
    #[cfg(target_os = "windows")]
    {
        let _ = tick_period;
        windows_impl::elevate()
    }
    #[cfg(target_os = "macos")]
    {
        macos::elevate(tick_period)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = tick_period;
        SchedulingMode::Normal
    }
}

// ---------------------------------------------------------------------------
// Linux — VERIFIED on this dev machine (plain FIFO path).
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod linux {
    use super::SchedulingMode;

    pub fn elevate() -> SchedulingMode {
        // Primary (Phase 2): RtKit grants SCHED_FIFO without root. Gated behind
        // the `rtkit` feature so the default build stays free of the D-Bus
        // dependency; when enabled it is tried first.
        #[cfg(feature = "rtkit")]
        if let Some(mode) = rtkit::try_make_realtime() {
            return mode;
        }

        // Fallback: plain SCHED_FIFO. Works when CAP_SYS_NICE is present or an
        // `rtprio` limit is configured (e.g. /etc/security/limits.d at install).
        if try_plain_fifo() {
            return SchedulingMode::PlainFifo;
        }

        // Note: we do NOT try to merely raise niceness as a middle tier —
        // lowering nice also needs privileges and is not a real-time class, so
        // the honest report is `Normal`.
        SchedulingMode::Normal
    }

    fn try_plain_fifo() -> bool {
        use thread_priority::*;
        let native = thread_native_id();
        // High, but not the maximum, real-time priority — polite to the kernel
        // and to hardirq threads above us.
        match ThreadPriorityValue::try_from(80u8) {
            Ok(prio) => set_thread_priority_and_policy(
                native,
                ThreadPriority::Crossplatform(prio),
                ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
            )
            .is_ok(),
            Err(_) => false,
        }
    }

    #[cfg(feature = "rtkit")]
    mod rtkit {
        use super::SchedulingMode;

        /// Ask RtKit (`org.freedesktop.RealtimeKit1`) to move this thread to
        /// SCHED_FIFO without root. Returns `Some(RtkitFifo)` on success.
        pub fn try_make_realtime() -> Option<SchedulingMode> {
            // RtKit requires RLIMIT_RTTIME to be set before the request.
            set_rttime_limit()?;

            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
            let conn = zbus::blocking::Connection::system().ok()?;
            let proxy = zbus::blocking::Proxy::new(
                &conn,
                "org.freedesktop.RealtimeKit1",
                "/org/freedesktop/RealtimeKit1",
                "org.freedesktop.RealtimeKit1",
            )
            .ok()?;

            // Query RtKit's max allowed priority and request just under it.
            let max_prio: i32 = proxy
                .get_property::<i64>("MaxRealtimePriority")
                .map(|v| v as i32)
                .unwrap_or(20);
            let priority = max_prio.clamp(1, 20) as u32;

            proxy
                .call::<_, _, ()>("MakeThreadRealtime", &(tid, priority))
                .ok()?;
            Some(SchedulingMode::RtkitFifo)
        }

        fn set_rttime_limit() -> Option<()> {
            let limit = libc::rlimit {
                rlim_cur: 200_000, // 200 ms of CPU before SIGXCPU — generous
                rlim_max: 200_000,
            };
            let rc = unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) };
            (rc == 0).then_some(())
        }
    }
}

// ---------------------------------------------------------------------------
// Windows — UNVERIFIED ON-TARGET. Written against documented Win32 APIs; must
// be validated by CI on Windows before release. Compiled only on Windows.
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod windows_impl {
    use super::SchedulingMode;
    use std::ffi::c_void;

    // MMCSS lives in avrt.dll and must be linked explicitly.
    #[link(name = "avrt")]
    extern "system" {
        fn AvSetMmThreadCharacteristicsW(
            task_name: *const u16,
            task_index: *mut u32,
        ) -> *mut c_void;
    }
    // Thread priority lives in kernel32 (implicitly linked).
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }

    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

    pub fn elevate() -> SchedulingMode {
        // 1) MMCSS "Pro Audio" — near-RT scheduling boost without admin, the
        //    same mechanism DAWs use for glitch-free audio. Our glitch-free DMX
        //    output is the analogous case.
        let task: Vec<u16> = "Pro Audio"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut index: u32 = 0;
        // SAFETY: `task` is a valid null-terminated UTF-16 buffer; `index` is a
        // valid out-param. The returned handle is intentionally leaked: the
        // output thread lives for the whole session, and the MMCSS association
        // ends when the thread exits. A Drop guard calling
        // AvRevertMmThreadCharacteristics is the tidy version once validated.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
        if !handle.is_null() {
            return SchedulingMode::MmcssProAudio;
        }

        // 2) Fallback: TIME_CRITICAL within the normal scheduler.
        // SAFETY: GetCurrentThread returns a pseudo-handle valid for this call.
        let ok = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
        if ok != 0 {
            return SchedulingMode::WindowsTimeCritical;
        }

        SchedulingMode::Normal
    }
}

// ---------------------------------------------------------------------------
// macOS — UNVERIFIED ON-TARGET. Written against the Mach thread-policy API;
// must be validated by CI on macOS before release. Compiled only on macOS.
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod macos {
    use super::SchedulingMode;
    use std::time::Duration;

    type MachPort = u32;
    type KernReturn = i32;

    const THREAD_TIME_CONSTRAINT_POLICY: i32 = 2;
    // The policy struct is four 32-bit integers.
    const THREAD_TIME_CONSTRAINT_POLICY_COUNT: u32 = 4;
    const KERN_SUCCESS: KernReturn = 0;

    #[repr(C)]
    struct ThreadTimeConstraintPolicy {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: u32, // boolean_t
    }

    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        fn mach_thread_self() -> MachPort;
        fn thread_policy_set(
            thread: MachPort,
            flavor: i32,
            policy_info: *mut u32,
            count: u32,
        ) -> KernReturn;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> KernReturn;
    }

    fn nanos_to_abs(ns: u64) -> u32 {
        let mut tb = MachTimebaseInfo { numer: 1, denom: 1 };
        // SAFETY: valid out-param.
        unsafe { mach_timebase_info(&mut tb) };
        // abs = ns * denom / numer
        let abs = ns as u128 * tb.denom as u128 / tb.numer as u128;
        abs.min(u32::MAX as u128) as u32
    }

    pub fn elevate(tick_period: Duration) -> SchedulingMode {
        let period_ns = tick_period.as_nanos().min(u64::MAX as u128) as u64;
        // Guarantee a slice of CPU each period: budget ~20% computation, allow
        // completion within ~50% of the period. Non-preemptible while computing.
        let mut policy = ThreadTimeConstraintPolicy {
            period: nanos_to_abs(period_ns),
            computation: nanos_to_abs(period_ns / 5),
            constraint: nanos_to_abs(period_ns / 2),
            preemptible: 0,
        };
        // SAFETY: mach_thread_self returns this thread's port; policy is a valid
        // 4-word buffer matching THREAD_TIME_CONSTRAINT_POLICY_COUNT.
        let kr = unsafe {
            thread_policy_set(
                mach_thread_self(),
                THREAD_TIME_CONSTRAINT_POLICY,
                &mut policy as *mut _ as *mut u32,
                THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        if kr == KERN_SUCCESS {
            SchedulingMode::MacTimeConstraint
        } else {
            SchedulingMode::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevate_returns_a_mode_and_never_panics() {
        // Without privileges this is `Normal`; with CAP_SYS_NICE / rtprio it is
        // a real-time mode. Either way it must return, not panic.
        let mode = elevate_current_thread(Duration::from_millis(5));
        // On Linux, the only outcomes are the RT FIFO modes or Normal.
        #[cfg(target_os = "linux")]
        assert!(matches!(
            mode,
            SchedulingMode::PlainFifo | SchedulingMode::RtkitFifo | SchedulingMode::Normal
        ));
        let _ = mode.is_realtime();
        assert!(!mode.label().is_empty());
    }
}

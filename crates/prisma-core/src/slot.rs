//! The value cell (`Slot`) and the hot-path handle (`ParamHandle`).
//!
//! # Why a handle, not an index
//!
//! Transports resolve a path to a [`ParamHandle`] **once** (at mapping load /
//! MIDI-learn) and then read/write by handle every frame — the string path never
//! touches the hot path (CLAUDE.md §4.5). A handle is an `Arc<Slot>`: holding one
//! keeps the slot's storage alive at a stable address, so reads are a plain
//! atomic load with **no arena indexing and no lock** — wait-free, as §3.3
//! demands of the render/output threads.
//!
//! # Appear/disappear
//!
//! A zone (and its parameters) can be created or deleted at runtime. Deletion
//! flips [`Slot::live`] to `false`; any outstanding handle (e.g. a MIDI mapping
//! to a now-deleted zone) then reads `None` and writes are no-ops — no panic, no
//! stale address. The storage frees when the last handle drops. This mirrors the
//! `LayoutGeneration` guard M2 uses for the patch index.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::arbitration::ArbState;
use crate::descriptor::{ParamDescriptor, ParamKind};

/// Process-unique identity for a parameter. Purely an in-process label used to
/// deduplicate notifications and key subscriber filters — it is **not**
/// serialized (persistent references use the path, §4.6).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ParamId(pub u64);

/// One parameter's storage. Reached only through a [`ParamHandle`].
pub(crate) struct Slot {
    pub(crate) id: ParamId,
    pub(crate) path: Arc<str>,
    pub(crate) kind: ParamKind,
    pub(crate) desc: Arc<ParamDescriptor>,
    /// For [`ParamKind::Level`]/[`ParamKind::Toggle`]: the normalized `f32`
    /// value, bit-cast to `u32`. For [`ParamKind::Trigger`]: a monotonic fire
    /// count. One `AtomicU32` serves both — no branch, no extra storage.
    pub(crate) value: AtomicU32,
    /// `false` once the parameter is unregistered; reads/writes become no-ops.
    pub(crate) live: AtomicBool,
    /// Coalescing flag for the writer→notifier queue: a slot is enqueued once
    /// until the notifier drains it (see `notify.rs`).
    pub(crate) dirty: AtomicBool,
    /// Arbitration state. Touched only by writes (transport threads), never by
    /// the wait-free read path.
    pub(crate) arb: Mutex<ArbState>,
}

/// A cheap, cloneable handle to one parameter. This is the hot-path object:
/// reads are wait-free. Writes go through [`ParameterTree`](crate::tree::ParameterTree)
/// so they run arbitration and notification.
#[derive(Clone)]
pub struct ParamHandle(pub(crate) Arc<Slot>);

impl ParamHandle {
    pub fn id(&self) -> ParamId {
        self.0.id
    }

    pub fn path(&self) -> &Arc<str> {
        &self.0.path
    }

    pub fn kind(&self) -> ParamKind {
        self.0.kind
    }

    pub fn descriptor(&self) -> &ParamDescriptor {
        &self.0.desc
    }

    /// Whether the parameter is still registered. A `false` here is why a stale
    /// mapping to a deleted zone reads `None` instead of garbage.
    pub fn is_live(&self) -> bool {
        self.0.live.load(Ordering::Relaxed)
    }

    /// Wait-free read of the normalized `0..=1` value (hot path). `None` if the
    /// parameter is dead or is a [`ParamKind::Trigger`] (use
    /// [`fire_count`](Self::fire_count) for those).
    pub fn read_norm(&self) -> Option<f32> {
        if !self.is_live() || self.0.kind == ParamKind::Trigger {
            return None;
        }
        Some(f32::from_bits(self.0.value.load(Ordering::Relaxed)))
    }

    /// The value mapped to its real range through the descriptor's curve.
    pub fn read_real(&self) -> Option<f32> {
        self.read_norm().map(|n| self.0.desc.to_real(n))
    }

    /// A [`ParamKind::Toggle`] read as a bool (`>= 0.5` is on). `None` for other
    /// kinds or dead parameters.
    pub fn read_bool(&self) -> Option<bool> {
        if self.0.kind != ParamKind::Toggle {
            return None;
        }
        self.read_norm().map(|n| n >= 0.5)
    }

    /// The monotonic fire count of a [`ParamKind::Trigger`]. Feed it to a
    /// [`TriggerCursor`](crate::trigger::TriggerCursor) to get edge counts.
    /// `None` for other kinds or dead parameters.
    pub fn fire_count(&self) -> Option<u32> {
        if !self.is_live() || self.0.kind != ParamKind::Trigger {
            return None;
        }
        Some(self.0.value.load(Ordering::Relaxed))
    }
}

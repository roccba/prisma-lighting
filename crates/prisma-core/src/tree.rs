//! The Parameter Tree: addressable, normalized, observable, thread-safe
//! (CLAUDE.md §4.5, milestone M3).
//!
//! # Two layers
//!
//! - **Value arena (hot path).** Values live in [`Slot`](crate::slot)s reached
//!   through [`ParamHandle`]s. Reads are wait-free atomic loads; the registry
//!   below is never touched on a read.
//! - **Path registry (bind + introspection).** A `path → handle` map, taken only
//!   when resolving a path (once, at mapping load) or enumerating the tree (UI /
//!   MIDI-learn). Structural change — a zone created or deleted — mutates this
//!   under a write lock that never sits on the hot path.
//!
//! # Concurrency contract
//!
//! - `read_*` on a handle: wait-free, any thread, including render/output.
//! - [`write`](ParameterTree::write): from transport threads. Runs arbitration
//!   (a short per-slot mutex) and enqueues a notification (non-blocking). Never
//!   invokes a subscriber callback inline.
//! - [`register`](ParameterTree::register) / [`unregister`](ParameterTree::unregister):
//!   structural, rare, under a registry write lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::{Mutex, RwLock};

use crate::arbitration::ArbState;
use crate::descriptor::{ParamDescriptor, ParamKind};
use crate::notify::{
    notifier_loop, Filter, Mailbox, Notification, SubEntry, SubId, SubscriberRegistry, Subscription,
};
use crate::slot::{ParamHandle, ParamId, Slot};
use crate::source::SourceId;
use crate::time::{SystemClock, Tick, TimeSource};

/// The outcome of a [`ParameterTree::write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The write was applied.
    Accepted,
    /// Rejected by arbitration: a higher-priority source owns the parameter and
    /// is not yet idle. Carries the owning source for feedback.
    Rejected { owner: SourceId },
    /// The parameter has been unregistered; the write was a no-op. This is the
    /// safe outcome for a stale handle to a deleted zone.
    Dead,
}

/// One row of tree introspection, for building UI / MIDI-learn menus by walking
/// the tree rather than a hardcoded list (§4.5).
#[derive(Clone)]
pub struct NodeInfo {
    pub id: ParamId,
    pub path: Arc<str>,
    pub kind: ParamKind,
    pub descriptor: Arc<ParamDescriptor>,
}

/// Configuration for a tree.
pub struct TreeConfig {
    /// The clock arbitration idle-timeouts are measured against. Inject a
    /// [`ManualClock`](crate::time::ManualClock) in tests.
    pub clock: Arc<dyn TimeSource>,
    /// How long an owner must be idle before a lower-priority source may take
    /// over, in [`Tick`]s (nanoseconds for [`SystemClock`]).
    pub takeover_timeout: Tick,
}

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock::new()),
            // 1 second of owner idle before a lower-priority source can take over.
            takeover_timeout: 1_000_000_000,
        }
    }
}

struct TreeInner {
    clock: Arc<dyn TimeSource>,
    takeover_timeout: Tick,
    next_param_id: AtomicU64,
    next_sub_id: AtomicU64,
    /// `path → handle`. Source of truth for resolution and enumeration.
    paths: RwLock<HashMap<Arc<str>, ParamHandle>>,
    subs: Arc<SubscriberRegistry>,
    /// Sender to the notifier thread. Behind a mutex+option so `Drop` can send
    /// `Shutdown` and then drop it, unblocking the notifier.
    tx: Mutex<Option<Sender<Notification>>>,
}

/// The Parameter Tree. Owns the notifier thread; joins it on drop.
pub struct ParameterTree {
    inner: Arc<TreeInner>,
    notifier: Option<JoinHandle<()>>,
}

impl ParameterTree {
    pub fn new(config: TreeConfig) -> Self {
        let subs = Arc::new(SubscriberRegistry::new());
        let (tx, rx) = mpsc::channel::<Notification>();
        let notifier = {
            let subs = subs.clone();
            std::thread::Builder::new()
                .name("param-notifier".into())
                .spawn(move || notifier_loop(rx, subs))
                .expect("spawn notifier thread")
        };
        Self {
            inner: Arc::new(TreeInner {
                clock: config.clock,
                takeover_timeout: config.takeover_timeout,
                next_param_id: AtomicU64::new(1),
                next_sub_id: AtomicU64::new(1),
                paths: RwLock::new(HashMap::new()),
                subs,
                tx: Mutex::new(Some(tx)),
            }),
            notifier: Some(notifier),
        }
    }

    /// Convenience constructor with the default (system) clock and timeout.
    pub fn with_defaults() -> Self {
        Self::new(TreeConfig::default())
    }

    // ---- structural: register / unregister --------------------------------

    /// Register a parameter at `path`. Returns a handle for hot-path access. If
    /// `path` already exists, the old parameter is marked dead and replaced
    /// (e.g. a zone recreated with the same id).
    pub fn register(&self, path: &str, desc: ParamDescriptor) -> ParamHandle {
        let id = ParamId(self.inner.next_param_id.fetch_add(1, Ordering::Relaxed));
        let path: Arc<str> = Arc::from(path);
        let init_bits = match desc.kind {
            ParamKind::Trigger => 0u32,
            _ => desc.default_norm.clamp(0.0, 1.0).to_bits(),
        };
        let slot = Arc::new(Slot {
            id,
            path: path.clone(),
            kind: desc.kind,
            desc: Arc::new(desc),
            value: AtomicU32::new(init_bits),
            live: AtomicBool::new(true),
            dirty: AtomicBool::new(false),
            arb: Mutex::new(ArbState::new()),
        });
        let handle = ParamHandle(slot);
        if let Some(old) = self.inner.paths.write().insert(path, handle.clone()) {
            old.0.live.store(false, Ordering::Relaxed);
        }
        handle
    }

    /// Unregister the parameter at `path`. Outstanding handles go dead (reads
    /// return `None`, writes are no-ops). Returns whether a parameter was there.
    pub fn unregister(&self, path: &str) -> bool {
        if let Some(h) = self.inner.paths.write().remove(path) {
            h.0.live.store(false, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Unregister every parameter whose path starts with `prefix` — the way a
    /// deleted zone drops its whole `/zone/{id}/…` subtree in one call. Returns
    /// the count removed.
    pub fn unregister_prefix(&self, prefix: &str) -> usize {
        let mut map = self.inner.paths.write();
        let victims: Vec<Arc<str>> = map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        for k in &victims {
            if let Some(h) = map.remove(k) {
                h.0.live.store(false, Ordering::Relaxed);
            }
        }
        victims.len()
    }

    // ---- resolution & introspection ---------------------------------------

    /// Resolve a path to a handle (once, at mapping load — not per frame).
    pub fn resolve(&self, path: &str) -> Option<ParamHandle> {
        self.inner.paths.read().get(path).cloned()
    }

    /// Enumerate every live parameter whose path starts with `prefix`, sorted by
    /// path. The UI builds MIDI-learn menus from this — nothing is hardcoded.
    /// Pass `""` to walk the whole tree.
    pub fn walk(&self, prefix: &str) -> Vec<NodeInfo> {
        let map = self.inner.paths.read();
        let mut out: Vec<NodeInfo> = map
            .iter()
            .filter(|(p, h)| p.starts_with(prefix) && h.is_live())
            .map(|(p, h)| NodeInfo {
                id: h.id(),
                path: p.clone(),
                kind: h.kind(),
                descriptor: h.0.desc.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// The descriptor for a path, if it exists.
    pub fn describe(&self, path: &str) -> Option<Arc<ParamDescriptor>> {
        self.inner.paths.read().get(path).map(|h| h.0.desc.clone())
    }

    // ---- writes ------------------------------------------------------------

    /// Write a normalized `0..=1` value to a parameter through arbitration. For a
    /// [`ParamKind::Trigger`] the value is ignored and the fire counter is
    /// incremented (see [`fire`](Self::fire)). Non-blocking with respect to
    /// notification.
    pub fn write(&self, handle: &ParamHandle, value_norm: f32, src: SourceId) -> WriteOutcome {
        let slot = &handle.0;
        if !slot.live.load(Ordering::Relaxed) {
            return WriteOutcome::Dead;
        }
        let now = self.inner.clock.now();

        // Arbitration: short per-slot mutex, only on the write path.
        {
            let mut arb = slot.arb.lock();
            if !arb.decide(src, now, self.inner.takeover_timeout) {
                let owner = arb.owner().expect("rejection implies an owner");
                return WriteOutcome::Rejected { owner };
            }
            arb.commit(src, now);
        }

        // Apply the value.
        match slot.kind {
            ParamKind::Trigger => {
                slot.value.fetch_add(1, Ordering::Release);
            }
            _ => {
                slot.value
                    .store(value_norm.clamp(0.0, 1.0).to_bits(), Ordering::Release);
            }
        }

        // Enqueue for notification, coalescing on the clean→dirty transition.
        if !slot.dirty.swap(true, Ordering::SeqCst) {
            if let Some(tx) = self.inner.tx.lock().as_ref() {
                let _ = tx.send(Notification::Changed(handle.clone()));
            }
        }

        WriteOutcome::Accepted
    }

    /// Write by path (resolves first). `None` if the path does not exist. Prefer
    /// [`write`](Self::write) with a cached handle on the hot path.
    pub fn write_path(&self, path: &str, value_norm: f32, src: SourceId) -> Option<WriteOutcome> {
        let h = self.resolve(path)?;
        Some(self.write(&h, value_norm, src))
    }

    /// Write a real-range value; converts to normalized via the descriptor curve.
    pub fn write_real(&self, handle: &ParamHandle, real: f32, src: SourceId) -> WriteOutcome {
        let norm = handle.0.desc.to_norm(real);
        self.write(handle, norm, src)
    }

    /// Fire a [`ParamKind::Trigger`] (one pulse). A no-op that returns `Dead` on
    /// other kinds is avoided — callers should only fire triggers.
    pub fn fire(&self, handle: &ParamHandle, src: SourceId) -> WriteOutcome {
        self.write(handle, 0.0, src)
    }

    // ---- subscriptions -----------------------------------------------------

    /// Subscribe to parameter changes matching `filter`. Drain the returned
    /// [`Subscription`] on your own thread; changes arrive off the writing
    /// thread (§4.5). Dropping the subscription deregisters it.
    pub fn subscribe(&self, filter: Filter) -> Subscription {
        let id = SubId(self.inner.next_sub_id.fetch_add(1, Ordering::Relaxed));
        let mailbox = Arc::new(Mailbox::new());
        self.inner.subs.entries.lock().push(SubEntry {
            id,
            filter,
            mailbox: mailbox.clone(),
        });
        Subscription::new(id, mailbox, Arc::downgrade(&self.inner.subs))
    }
}

impl Drop for ParameterTree {
    fn drop(&mut self) {
        // Signal the notifier to stop, then join. Taking `tx` drops the last
        // sender after the Shutdown message is queued.
        if let Some(tx) = self.inner.tx.lock().take() {
            let _ = tx.send(Notification::Shutdown);
        }
        if let Some(j) = self.notifier.take() {
            let _ = j.join();
        }
    }
}

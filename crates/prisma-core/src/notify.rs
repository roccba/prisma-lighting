//! Asynchronous, queued observer fan-out (CLAUDE.md §3.3, §4.5).
//!
//! A parameter write **enqueues and returns**; it never calls a subscriber
//! callback inline, and it never blocks on notification. A dedicated notifier
//! thread drains the queue and fans changes out to subscribers. No subscriber
//! callback ever runs on the render or output thread — subscribers drain their
//! own [`Subscription`] on their own thread.
//!
//! # Two stages, both coalescing, both non-blocking
//!
//! 1. **Writer → notifier.** Each write sets the slot's `dirty` flag and, only on
//!    the clean→dirty transition, sends the handle down an unbounded channel. A
//!    fader firing thousands of writes/s therefore enqueues its slot **once** per
//!    drain cycle: coalescing bounds the in-flight set to the number of distinct
//!    dirty parameters, so "unbounded" never actually grows without bound. The
//!    writer's `send` is non-blocking.
//!
//! 2. **Notifier → subscriber.** Each subscriber owns a [`Mailbox`]: a bounded,
//!    per-parameter-deduplicated queue of **handles**. The notifier pushes a
//!    handle at most once per subscriber (dedup by [`ParamId`]); the subscriber
//!    reads the **live value off the handle** when it drains. Delivering handles
//!    plus reading live is what makes both stages coalesce safely: a slow
//!    subscriber that wakes once per second sees each changed parameter once, at
//!    its latest value — never a stale intermediate, never a lost final state.
//!    For triggers the same live-read feeds a
//!    [`TriggerCursor`](crate::trigger::TriggerCursor), so even coalesced fires
//!    are counted correctly.
//!
//! # Slow / hung subscribers are contained
//!
//! A subscriber's dedup set caps its mailbox at the number of parameters it is
//! subscribed to. A subscriber that stops draining entirely only fills its own
//! (bounded) mailbox; the notifier's push is non-blocking, and neither the writer
//! nor any other subscriber is affected. A truly hung consumer is a leak of at
//! most O(its parameters), never a stall of the pipeline.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::slot::{ParamHandle, ParamId};

/// What the notifier thread receives from writers.
pub(crate) enum Notification {
    /// A parameter changed; the handle carries the live value.
    Changed(ParamHandle),
    /// Tree is dropping — drain and exit.
    Shutdown,
}

/// Selects which parameters a subscriber wants. Matched on the notifier thread,
/// off the hot path.
pub enum Filter {
    /// Every parameter (e.g. the UI/preview bridge).
    All,
    /// Parameters whose path starts with this prefix (e.g. one zone's subtree).
    Prefix(Arc<str>),
    /// A specific set of parameters (e.g. exactly the paths a MIDI profile maps,
    /// for feedback LEDs).
    Ids(HashSet<ParamId>),
}

impl Filter {
    pub(crate) fn matches(&self, h: &ParamHandle) -> bool {
        match self {
            Filter::All => true,
            Filter::Prefix(p) => h.path().starts_with(p.as_ref()),
            Filter::Ids(set) => set.contains(&h.id()),
        }
    }
}

/// One subscriber's inbox: a per-parameter-deduplicated queue of handles.
pub(crate) struct Mailbox {
    inner: Mutex<MailboxInner>,
    cvar: Condvar,
}

struct MailboxInner {
    queue: VecDeque<ParamHandle>,
    /// Parameters currently sitting in `queue`. Dedup key: a parameter already
    /// pending is not pushed again — the consumer will read its latest value
    /// when it drains, so a second entry would be redundant. This is what bounds
    /// the mailbox and coalesces at the subscriber stage.
    pending: HashSet<ParamId>,
    closed: bool,
}

impl Mailbox {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(MailboxInner {
                queue: VecDeque::new(),
                pending: HashSet::new(),
                closed: false,
            }),
            cvar: Condvar::new(),
        }
    }

    /// Non-blocking push from the notifier. Deduplicates by [`ParamId`].
    pub(crate) fn push(&self, h: ParamHandle) {
        let mut g = self.inner.lock();
        if g.pending.insert(h.id()) {
            g.queue.push_back(h);
            self.cvar.notify_one();
        }
    }

    /// Wake any blocked consumer and mark the mailbox closed (tree shutting down).
    pub(crate) fn close(&self) {
        let mut g = self.inner.lock();
        g.closed = true;
        self.cvar.notify_all();
    }

    /// Block until at least one change is pending (or the mailbox closes), then
    /// drain everything. Returns empty only when closed.
    fn drain_blocking(&self) -> Vec<ParamHandle> {
        let mut g = self.inner.lock();
        loop {
            if !g.queue.is_empty() {
                g.pending.clear();
                return g.queue.drain(..).collect();
            }
            if g.closed {
                return Vec::new();
            }
            self.cvar.wait(&mut g);
        }
    }

    /// Drain whatever is pending right now, without blocking.
    fn drain_now(&self) -> Vec<ParamHandle> {
        let mut g = self.inner.lock();
        g.pending.clear();
        g.queue.drain(..).collect()
    }
}

/// The registry of live subscribers, shared between the tree (to add/remove) and
/// the notifier thread (to fan out).
pub(crate) struct SubscriberRegistry {
    pub(crate) entries: Mutex<Vec<SubEntry>>,
}

pub(crate) struct SubEntry {
    pub(crate) id: SubId,
    pub(crate) filter: Filter,
    pub(crate) mailbox: Arc<Mailbox>,
}

impl SubscriberRegistry {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

/// Process-unique subscriber id, used to deregister on drop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SubId(pub(crate) u64);

/// A subscription to parameter changes. Drain it on your own thread; dropping it
/// deregisters. Callbacks are your business — the tree only hands you the changed
/// handles, off the writing thread.
pub struct Subscription {
    id: SubId,
    mailbox: Arc<Mailbox>,
    registry: std::sync::Weak<SubscriberRegistry>,
}

impl Subscription {
    pub(crate) fn new(
        id: SubId,
        mailbox: Arc<Mailbox>,
        registry: std::sync::Weak<SubscriberRegistry>,
    ) -> Self {
        Self {
            id,
            mailbox,
            registry,
        }
    }

    /// Block until at least one parameter has changed, then return the changed
    /// handles (each at its latest value). Returns empty only when the tree is
    /// shutting down.
    pub fn recv(&self) -> Vec<ParamHandle> {
        self.mailbox.drain_blocking()
    }

    /// Return whatever has changed since the last drain, without blocking.
    pub fn try_recv(&self) -> Vec<ParamHandle> {
        self.mailbox.drain_now()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(reg) = self.registry.upgrade() {
            reg.entries.lock().retain(|e| e.id != self.id);
        }
    }
}

/// The notifier thread body: drain writer notifications, fan out to matching
/// subscribers. Clearing `dirty` **before** reading is deliberate — a write that
/// races the clear re-enqueues, so a change is never lost (it may be delivered
/// twice, which is harmless because delivery reads the live value).
pub(crate) fn notifier_loop(
    rx: std::sync::mpsc::Receiver<Notification>,
    subs: Arc<SubscriberRegistry>,
) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Notification::Shutdown => break,
            Notification::Changed(h) => {
                h.0.dirty.store(false, std::sync::atomic::Ordering::SeqCst);
                let entries = subs.entries.lock();
                for e in entries.iter() {
                    if e.filter.matches(&h) {
                        e.mailbox.push(h.clone());
                    }
                }
            }
        }
    }
    // Shutting down: wake every blocked consumer so their threads can exit.
    for e in subs.entries.lock().iter() {
        e.mailbox.close();
    }
}

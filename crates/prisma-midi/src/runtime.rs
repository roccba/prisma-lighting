//! The MIDI runtime: active profile + binding + MIDI Learn (M9-SPEC §4, §7).
//!
//! [`MidiRuntime`] is the hardware-independent heart of the MIDI transport. Bytes
//! enter through [`feed`](MidiRuntime::feed) — the injectable seam (§5, §13): the
//! real `midir` adapter calls it from its input callback, tests call it directly.
//!
//! # Lifecycle across recompiles
//!
//! The engine rebuilds its Parameter Tree on every patch/show recompile, so the
//! runtime is **not** tied to one tree. It holds the operator's profile for the life
//! of the process and is re-[`bind`](MidiRuntime::bind)-ed to each new tree; while
//! unbound (mid-recompile) input is dropped, never queued against a dead tree. Open
//! ports therefore survive a recompile untouched.
//!
//! # Threading
//!
//! `feed` runs on the MIDI input thread (a transport thread, §3.3). It takes the
//! runtime's short state lock and writes the tree, whose write path never blocks on
//! notification (§4.5). The render and output threads never see this lock. The
//! [`MidiListener`] is called **after** the lock is released, so a listener that
//! persists a file or emits a UI event cannot stall the next message's resolution
//! behind it.

use std::sync::Arc;

use parking_lot::Mutex;
use prisma_core::{
    ControlMode, ControllerMappings, MapTarget, MidiControl, MidiMapping, ParameterTree,
    ResponseCurve,
};
use uuid::Uuid;

use crate::message::{EventKind, MidiEvent};
use crate::resolver::Binding;

/// Something the runtime reports to its host (the app), off the lock.
#[derive(Debug, Clone)]
pub enum MidiNotice {
    /// Learn captured a control. `profile` is the whole updated profile, ready to
    /// persist; `mapping` is the binding just created.
    Learned {
        mapping: MidiMapping,
        profile: ControllerMappings,
    },
    /// A mapped control wrote the tree (the UI refreshes what it shows). Emitted for
    /// accepted writes only; the host coalesces. `zone` is the zone slot the control
    /// drives (`None` for globals like the master), so the UI can follow the operator's
    /// hands to that zone.
    Applied {
        control: MidiControl,
        zone: Option<u16>,
    },
}

/// Host callback for [`MidiNotice`]s.
pub type MidiListener = Arc<dyn Fn(MidiNotice) + Send + Sync>;

struct State {
    profile: ControllerMappings,
    /// The running tree and zone order, kept so a profile change can recompile the
    /// binding without the host re-supplying them.
    tree: Option<(Arc<ParameterTree>, Vec<Uuid>)>,
    binding: Option<Binding>,
    learn: Option<MapTarget>,
}

impl State {
    fn rebuild(&mut self) {
        self.binding = self
            .tree
            .as_ref()
            .map(|(tree, zones)| Binding::compile(Arc::clone(tree), zones, &self.profile));
    }
}

/// Cheaply cloneable handle to the shared runtime.
#[derive(Clone)]
pub struct MidiRuntime {
    state: Arc<Mutex<State>>,
    listener: Arc<Mutex<Option<MidiListener>>>,
}

impl MidiRuntime {
    pub fn new(profile: ControllerMappings) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                profile,
                tree: None,
                binding: None,
                learn: None,
            })),
            listener: Arc::new(Mutex::new(None)),
        }
    }

    /// Install (or replace) the host listener.
    pub fn set_listener(&self, listener: Option<MidiListener>) {
        *self.listener.lock() = listener;
    }

    /// Bind to a running tree, with its zones in slot order. Recompiles every
    /// mapping's target against it (slot → uuid → handle, once).
    pub fn bind(&self, tree: Arc<ParameterTree>, zone_ids: Vec<Uuid>) {
        let mut s = self.state.lock();
        s.tree = Some((tree, zone_ids));
        s.rebuild();
    }

    /// Drop the tree reference. Input is ignored until the next [`bind`](Self::bind).
    /// The engine calls this before tearing a tree down, so the runtime never keeps
    /// an old tree (and its notifier thread) alive.
    pub fn unbind(&self) {
        let mut s = self.state.lock();
        s.tree = None;
        s.binding = None;
    }

    pub fn is_bound(&self) -> bool {
        self.state.lock().binding.is_some()
    }

    /// Mappings that resolved against the bound tree (0 when unbound).
    pub fn resolved_count(&self) -> usize {
        self.state
            .lock()
            .binding
            .as_ref()
            .map_or(0, Binding::resolved_count)
    }

    /// A copy of the active profile.
    pub fn profile(&self) -> ControllerMappings {
        self.state.lock().profile.clone()
    }

    /// Replace the active profile (e.g. loaded from disk) and rebind.
    pub fn set_profile(&self, profile: ControllerMappings) {
        let mut s = self.state.lock();
        s.profile = profile;
        s.rebuild();
    }

    /// Remove the mapping for `target`. Returns the updated profile if one was
    /// removed, for the host to persist.
    pub fn unmap(&self, target: &MapTarget) -> Option<ControllerMappings> {
        let mut s = self.state.lock();
        if !s.profile.unbind_target(target) {
            return None;
        }
        s.rebuild();
        Some(s.profile.clone())
    }

    /// Arm Learn for `target`: the next note-on or CC captured becomes its control.
    /// Re-arming replaces the previous armed target.
    pub fn arm_learn(&self, target: MapTarget) {
        self.state.lock().learn = Some(target);
    }

    pub fn cancel_learn(&self) {
        self.state.lock().learn = None;
    }

    /// The target currently waiting for a control, if any.
    pub fn learning(&self) -> Option<MapTarget> {
        self.state.lock().learn.clone()
    }

    /// The input seam: one complete MIDI message from any source.
    pub fn feed(&self, bytes: &[u8]) {
        let Some(event) = MidiEvent::parse(bytes) else {
            return;
        };
        let notice = {
            let mut s = self.state.lock();
            match learn_capture(&mut s, &event) {
                Some(notice) => Some(notice),
                None => s.binding.as_ref().and_then(|b| {
                    let accepted = b.apply(&event)? == prisma_core::WriteOutcome::Accepted;
                    let control = event.control().filter(|_| accepted)?;
                    Some(MidiNotice::Applied {
                        control,
                        zone: b.zone_of(control),
                    })
                }),
            }
        };
        if let Some(notice) = notice {
            let listener = self.listener.lock().clone();
            if let Some(listener) = listener {
                listener(notice);
            }
        }
    }
}

/// If Learn is armed and `event` is a capturable gesture, bind it, rebuild, apply
/// the event (so the fader that was just touched takes effect at once) and return
/// the notice. Note-offs are never captured: releasing the pad that armed nothing
/// must not bind, and a note's key is already taken on its note-on.
fn learn_capture(s: &mut State, event: &MidiEvent) -> Option<MidiNotice> {
    let capturable = matches!(event.kind, EventKind::NoteOn { .. } | EventKind::Cc { .. });
    if s.learn.is_none() || !capturable {
        return None;
    }
    let target = s.learn.take()?;
    let mapping = MidiMapping {
        control: event.control()?,
        target,
        mode: ControlMode::Absolute,
        curve: ResponseCurve::Linear,
    };
    s.profile.bind(mapping.clone());
    s.rebuild();
    if let Some(b) = s.binding.as_ref() {
        b.apply(event);
    }
    Some(MidiNotice::Learned {
        mapping,
        profile: s.profile.clone(),
    })
}

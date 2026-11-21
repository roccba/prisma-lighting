//! Track clips as Parameter Tree targets, and the controller that plays them
//! (M9 Slice 3 pads, reshaped by docs/LIVE-MODEL-SPEC.md §5).
//!
//! Every clip exposes tree parameters and **all** transports go through them (§3.1
//! principle 2):
//!
//! ```text
//! /track/{id}/clip/{i}/launch    trigger  latch: the clip becomes the track's base
//! /track/{id}/clip/{i}/trigger   trigger  latch + restart the effect from its start
//! /track/{id}/clip/{i}/hold      toggle   1 = run temporarily, 0 = return to before
//! /track/{id}/clip/{i}/state     level    0 empty · 1 assigned · 2 running (read-only)
//! /track/{id}/clips/refresh      trigger  re-read which clips are assigned
//! ```
//!
//! The [`ClipController`] thread subscribes to those, runs each track's
//! [`ZoneDeck`] (the latch/trigger/hold decisions, including where a temporary clip
//! returns to), swaps effects through the frame-boundary queue, and publishes each
//! clip's `state` — which is what pad LEDs show.
//!
//! The Show document stays with its owner (the app): the controller asks a
//! [`ClipHost`] what a clip holds and tells it when a track's running clip changed.
//! The controller holds no lock while it waits for changes, and never calls the host
//! with one held.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use prisma_core::show::{ParamSet, Show};
use prisma_core::source::{priority, SourceId};
use prisma_core::tree::ParameterTree;
use prisma_core::{
    Curve, DeckChange, Filter, ParamDescriptor, ParamHandle, ParamId, RealRange,
    SubscriptionCloser, TriggerCursor, ZoneDeck,
};
use prisma_patch::Patch;
use prisma_render::Gpu;
use uuid::Uuid;

use crate::zone_content::{build_track_instances, destination_slots, ZoneCommandSender};

/// `state` value of an empty clip.
pub const CLIP_EMPTY: f32 = 0.0;
/// `state` value of a clip with an effect that is not running.
pub const CLIP_ASSIGNED: f32 = 1.0;
/// `state` value of the track's running clip.
pub const CLIP_RUNNING: f32 = 2.0;

/// What a clip plays: its effect, the effect's stored parameter values and, for an
/// effect with a palette, the clip's colour table (`None` = the factory colours).
#[derive(Debug, Clone, PartialEq)]
pub struct ClipEffect {
    pub effect_id: String,
    pub params: ParamSet,
    pub palette: Option<Vec<[f32; 3]>>,
}

/// The owner of the Show document, as the clip controller sees it.
pub trait ClipHost: Send + Sync {
    /// The effect in `track`'s clip `clip`, or `None` if it is empty.
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect>;
    /// `track`'s running clip changed (a pad, a UI click, a release returning).
    fn active_changed(&self, track: Uuid, active: Option<usize>);
}

/// A clip's stored effect parameters, written when it starts.
fn stored_params_source() -> SourceId {
    SourceId::new(priority::UI, 0)
}

/// The engine writes clip state as the highest tier: it is derived, nobody else's.
fn engine_source() -> SourceId {
    SourceId::new(u8::MAX, 0)
}

/// One track's clip parameters, resolved once.
#[derive(Clone)]
pub struct TrackClips {
    pub track: Uuid,
    /// How many zones the track reaches = effect instances per launch.
    pub zones: usize,
    pub launch: Vec<ParamHandle>,
    pub trigger: Vec<ParamHandle>,
    pub hold: Vec<ParamHandle>,
    pub state: Vec<ParamHandle>,
    pub refresh: ParamHandle,
}

/// Register every track's clip parameters, sized by its clips. Indexed by track
/// slot (`Show::tracks` order).
pub fn register_clips(tree: &ParameterTree, patch: &Patch, show: &Show) -> Vec<TrackClips> {
    show.tracks
        .iter()
        .map(|track| {
            let clips = track.clips.len();
            let base = format!("/track/{}/clip", track.id);
            let each = |leaf: &str, desc: &dyn Fn() -> ParamDescriptor| -> Vec<ParamHandle> {
                (0..clips)
                    .map(|i| tree.register(&format!("{base}/{i}/{leaf}"), desc()))
                    .collect()
            };
            TrackClips {
                track: track.id,
                zones: destination_slots(patch, track).len(),
                launch: each("launch", &|| ParamDescriptor::trigger("clip.launch")),
                trigger: each("trigger", &|| ParamDescriptor::trigger("clip.trigger")),
                hold: each("hold", &|| ParamDescriptor::toggle("clip.hold", false)),
                state: each("state", &|| {
                    ParamDescriptor::level(
                        "clip.state",
                        RealRange {
                            min: CLIP_EMPTY,
                            max: CLIP_RUNNING,
                        },
                        Curve::Linear,
                        0.0,
                    )
                }),
                refresh: tree.register(
                    &format!("/track/{}/clips/refresh", track.id),
                    ParamDescriptor::trigger("clips.refresh"),
                ),
            }
        })
        .collect()
}

/// Build `effect_id` for every zone the track reaches on the caller's thread (a
/// pipeline compile, never on the render thread), write its stored params, and queue
/// the swap for the next frame boundary. `false` if the effect is not in the
/// catalogue or the render loop is gone.
#[allow(clippy::too_many_arguments)] // each is a distinct piece of the engine it needs
pub fn fire_track_effect(
    gpu: &Gpu,
    tree: &ParameterTree,
    sender: &ZoneCommandSender,
    slot: usize,
    clips: &TrackClips,
    effect_id: &str,
    params: &ParamSet,
    palette: Option<&[[f32; 3]]>,
    src: SourceId,
) -> bool {
    match build_track_instances(
        gpu,
        tree,
        clips.track,
        clips.zones,
        effect_id,
        params,
        palette,
        src,
    ) {
        Some(effects) => sender.set_track_effects(slot, effects),
        None => false,
    }
}

#[derive(Clone, Copy)]
enum Action {
    Launch,
    Trigger,
    Hold,
    Refresh,
}

/// The running clip controller. Dropping it stops the thread and waits for it.
pub struct ClipController {
    closer: SubscriptionCloser,
    thread: Option<JoinHandle<()>>,
}

impl ClipController {
    /// Start playing `tracks` (by slot) from their show `active` clips. Publishes every
    /// clip's state once, then follows the pads.
    pub fn start(
        tree: Arc<ParameterTree>,
        gpu: Arc<Gpu>,
        sender: ZoneCommandSender,
        tracks: Vec<TrackClips>,
        active: Vec<Option<usize>>,
        host: Arc<dyn ClipHost>,
    ) -> ClipController {
        let mut routes: HashMap<ParamId, (usize, usize, Action)> = HashMap::new();
        let mut cursors: HashMap<ParamId, TriggerCursor> = HashMap::new();
        for (slot, z) in tracks.iter().enumerate() {
            for (cell, h) in z.launch.iter().enumerate() {
                routes.insert(h.id(), (slot, cell, Action::Launch));
            }
            for (cell, h) in z.trigger.iter().enumerate() {
                routes.insert(h.id(), (slot, cell, Action::Trigger));
            }
            for (cell, h) in z.hold.iter().enumerate() {
                routes.insert(h.id(), (slot, cell, Action::Hold));
            }
            routes.insert(z.refresh.id(), (slot, 0, Action::Refresh));
        }
        // Subscribe, then prime every trigger cursor at its current count, so nothing
        // fired before start is replayed and nothing fired after is missed.
        let sub = tree.subscribe(Filter::Ids(routes.keys().copied().collect()));
        for z in &tracks {
            for h in z.launch.iter().chain(&z.trigger).chain([&z.refresh]) {
                let mut c = TriggerCursor::new();
                c.poll(h.fire_count().unwrap_or(0));
                cursors.insert(h.id(), c);
            }
        }
        let closer = sub.closer();
        let mut ctl = Controller {
            tree,
            gpu,
            sender,
            decks: tracks
                .iter()
                .enumerate()
                .map(|(slot, _)| ZoneDeck::new(active.get(slot).copied().flatten()))
                .collect(),
            tracks,
            host,
        };
        let thread = std::thread::Builder::new()
            .name("prisma-clips".into())
            .spawn(move || {
                for slot in 0..ctl.tracks.len() {
                    ctl.publish_states(slot);
                }
                loop {
                    let changed = sub.recv();
                    if changed.is_empty() {
                        break;
                    }
                    for handle in changed {
                        let Some(&(slot, cell, action)) = routes.get(&handle.id()) else {
                            continue;
                        };
                        let fired = |cursors: &mut HashMap<ParamId, TriggerCursor>| {
                            let count = handle.fire_count().unwrap_or(0);
                            cursors
                                .get_mut(&handle.id())
                                .is_some_and(|c| c.poll_edge(count))
                        };
                        match action {
                            Action::Launch if fired(&mut cursors) => ctl.latch(slot, cell),
                            Action::Trigger if fired(&mut cursors) => ctl.trigger(slot, cell),
                            Action::Refresh if fired(&mut cursors) => ctl.publish_states(slot),
                            Action::Hold => ctl.hold(slot, cell, handle.read_bool() == Some(true)),
                            _ => {}
                        }
                    }
                }
            })
            .expect("spawn clip controller thread");
        ClipController {
            closer,
            thread: Some(thread),
        }
    }
}

impl Drop for ClipController {
    fn drop(&mut self) {
        self.closer.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Controller {
    tree: Arc<ParameterTree>,
    gpu: Arc<Gpu>,
    sender: ZoneCommandSender,
    tracks: Vec<TrackClips>,
    decks: Vec<ZoneDeck>,
    host: Arc<dyn ClipHost>,
}

impl Controller {
    fn latch(&mut self, slot: usize, cell: usize) {
        let held = self.held_cells(slot);
        let change = self.decks[slot].latch(cell);
        self.release_hold_params(slot, held);
        self.apply(slot, change);
    }

    fn trigger(&mut self, slot: usize, cell: usize) {
        let held = self.held_cells(slot);
        let change = self.decks[slot].trigger(cell);
        self.release_hold_params(slot, held);
        self.apply(slot, change);
    }

    fn hold(&mut self, slot: usize, cell: usize, on: bool) {
        let deck = &mut self.decks[slot];
        let change = match (on, deck.is_held(cell)) {
            (true, false) => deck.press(cell),
            (false, true) => deck.release(cell),
            _ => DeckChange::None, // already in that state (e.g. our own reset below)
        };
        self.apply(slot, change);
    }

    fn held_cells(&self, slot: usize) -> Vec<usize> {
        (0..self.tracks[slot].hold.len())
            .filter(|&c| self.decks[slot].is_held(c))
            .collect()
    }

    /// A latch/trigger forgot these temporary clips: put their `hold` back to 0 so a
    /// toggle pad's next tap presses again instead of releasing a hold that is gone.
    fn release_hold_params(&self, slot: usize, cells: Vec<usize>) {
        for cell in cells {
            self.tree
                .write_real(&self.tracks[slot].hold[cell], 0.0, engine_source());
        }
    }

    fn apply(&mut self, slot: usize, change: DeckChange) {
        let DeckChange::Play { cell, restart } = change else {
            return;
        };
        let track = self.tracks[slot].track;
        let effect = cell.and_then(|c| self.host.clip_effect(track, c));
        match effect {
            Some(ClipEffect {
                effect_id,
                params,
                palette,
            }) => {
                fire_track_effect(
                    &self.gpu,
                    &self.tree,
                    &self.sender,
                    slot,
                    &self.tracks[slot],
                    &effect_id,
                    &params,
                    palette.as_deref(),
                    // The clip's stored values are a load, not a performance: written at
                    // UI tier so the panel (or a controller) takes them over at once. At
                    // the engine's tier they would lock every control out for the
                    // takeover timeout after each effect change.
                    stored_params_source(),
                );
            }
            None => {
                self.sender.clear_track(slot);
            }
        }
        if restart {
            self.sender.restart_track(slot);
        }
        self.host.active_changed(track, self.decks[slot].active());
        self.publish_states(slot);
    }

    fn publish_states(&self, slot: usize) {
        let z = &self.tracks[slot];
        let active = self.decks[slot].active();
        for (clip, handle) in z.state.iter().enumerate() {
            let value = if active == Some(clip) {
                CLIP_RUNNING
            } else if self.host.clip_effect(z.track, clip).is_some() {
                CLIP_ASSIGNED
            } else {
                CLIP_EMPTY
            };
            self.tree.write_real(handle, value, engine_source());
        }
    }
}

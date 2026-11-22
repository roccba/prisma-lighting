//! Track clips, scenes and GO as Parameter Tree targets, and the controller that
//! plays them (M9 Slice 3 pads, reshaped by docs/LIVE-MODEL-SPEC.md §5).
//!
//! Every control exposes tree parameters and **all** transports go through them
//! (§3.1 principle 2):
//!
//! ```text
//! /track/{id}/clip/{i}/launch    trigger  latch (or arm, on an Arm-mode track)
//! /track/{id}/clip/{i}/trigger   trigger  latch + restart the effect from its start
//! /track/{id}/clip/{i}/hold      toggle   1 = run temporarily, 0 = return to before
//! /track/{id}/clip/{i}/arm       trigger  arm (or disarm) the clip for GO
//! /track/{id}/clip/{i}/state     level    0 empty · 1 assigned · 2 running · 3 armed
//! /track/{id}/opacity            level    read by the render loop every frame
//! /track/{id}/mute | solo        toggle   read by the render loop every frame
//! /track/{id}/stop               trigger  fade to black (a stop during it cuts)
//! /track/{id}/clips/refresh      trigger  re-read which clips are assigned
//! /scene/{i}/launch              trigger  launch row i on every track that has a clip there
//! /scene/{i}/arm                 trigger  arm row i on every track that has a clip there
//! /arm/go                        trigger  play every armed clip, all in the same frame
//! /tracks/stop                   trigger  stop every track (each with its transition)
//! ```
//!
//! The [`ClipController`] thread subscribes to those, runs each track's
//! [`ZoneDeck`] (the latch/trigger/hold/arm decisions), builds effects and sends the
//! swaps through the frame-boundary queue — one [`ZoneCommand::Batch`] for a scene or
//! GO, so every track changes in the same frame — and publishes each clip's `state`,
//! which is what pad LEDs show.
//!
//! The Show document stays with its owner (the app): the controller asks a
//! [`ClipHost`] what a clip holds and how a track transitions, and tells it when a
//! track's running or armed clip changed. The controller holds no lock while it waits
//! for changes, and never calls the host with one held.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use prisma_core::show::{LaunchMode, ParamSet, Show, TrackTransition};
use prisma_core::source::{priority, SourceId};
use prisma_core::tree::ParameterTree;
use prisma_core::{
    Curve, DeckChange, Filter, ParamDescriptor, ParamHandle, ParamId, RealRange,
    SubscriptionCloser, TriggerCursor, ZoneDeck,
};
use prisma_patch::Patch;
use prisma_render::Gpu;
use uuid::Uuid;

use crate::zone_content::{
    build_track_instances, destination_slots, Fade, TrackLive, ZoneCommand, ZoneCommandSender,
};

/// `state` value of an empty clip.
pub const CLIP_EMPTY: f32 = 0.0;
/// `state` value of a clip with an effect that is not running.
pub const CLIP_ASSIGNED: f32 = 1.0;
/// `state` value of the track's running clip.
pub const CLIP_RUNNING: f32 = 2.0;
/// `state` value of the track's armed clip (waiting for GO).
pub const CLIP_ARMED: f32 = 3.0;

/// What a clip plays: its effect, the effect's stored parameter values, for an effect
/// with a palette the clip's colour table (`None` = the factory colours), and the
/// clip's own fade (zero = the track's transition).
#[derive(Debug, Clone, PartialEq)]
pub struct ClipEffect {
    pub effect_id: String,
    pub params: ParamSet,
    pub palette: Option<Vec<[f32; 3]>>,
    pub fade_time: Duration,
}

/// How a track behaves when played, as the host stores it (editable live).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackBehaviour {
    pub transition: TrackTransition,
    pub launch: LaunchMode,
}

/// The owner of the Show document, as the clip controller sees it.
pub trait ClipHost: Send + Sync {
    /// The effect in `track`'s clip `clip`, or `None` if it is empty.
    fn clip_effect(&self, track: Uuid, clip: usize) -> Option<ClipEffect>;
    /// `track`'s transition and launch mode. `None` = a hard cut, direct launch.
    fn track_behaviour(&self, _track: Uuid) -> Option<TrackBehaviour> {
        None
    }
    /// `track`'s running clip changed (a pad, a UI click, a release returning, GO).
    fn active_changed(&self, track: Uuid, active: Option<usize>);
    /// `track`'s armed clip changed.
    fn armed_changed(&self, _track: Uuid, _armed: Option<usize>) {}
}

/// A clip's stored effect parameters, written when it starts.
fn stored_params_source() -> SourceId {
    SourceId::new(priority::UI, 0)
}

/// The engine writes clip state as the highest tier: it is derived, nobody else's.
fn engine_source() -> SourceId {
    SourceId::new(u8::MAX, 0)
}

/// One track's parameters, resolved once.
#[derive(Clone)]
pub struct TrackClips {
    pub track: Uuid,
    /// How many zones the track reaches = effect instances per launch.
    pub zones: usize,
    pub launch: Vec<ParamHandle>,
    pub trigger: Vec<ParamHandle>,
    pub hold: Vec<ParamHandle>,
    pub arm: Vec<ParamHandle>,
    pub state: Vec<ParamHandle>,
    pub refresh: ParamHandle,
    pub stop: ParamHandle,
    pub opacity: ParamHandle,
    pub mute: ParamHandle,
    pub solo: ParamHandle,
}

impl TrackClips {
    /// The handles the render loop reads every frame.
    pub fn live(&self) -> TrackLive {
        TrackLive {
            opacity: self.opacity.clone(),
            mute: self.mute.clone(),
            solo: self.solo.clone(),
        }
    }
}

/// Every grid parameter of a running show.
#[derive(Clone, Default)]
pub struct GridParams {
    /// By track slot (`Show::tracks` order).
    pub tracks: Vec<TrackClips>,
    /// `/scene/{i}/launch`, one per grid row (the longest track's clip count).
    pub scene_launch: Vec<ParamHandle>,
    /// `/scene/{i}/arm`.
    pub scene_arm: Vec<ParamHandle>,
    /// `/arm/go`; `None` before a show registered it.
    pub go: Option<ParamHandle>,
    /// `/tracks/stop`; `None` before a show registered it.
    pub stop_all: Option<ParamHandle>,
}

/// Register every track's, scene's and GO's parameters. A track's opacity / mute /
/// solo start at the values the show saved.
pub fn register_clips(tree: &ParameterTree, patch: &Patch, show: &Show) -> GridParams {
    let tracks: Vec<TrackClips> = show
        .tracks
        .iter()
        .map(|track| {
            let clips = track.clips.len();
            let base = format!("/track/{}", track.id);
            let each = |leaf: &str, desc: &dyn Fn() -> ParamDescriptor| -> Vec<ParamHandle> {
                (0..clips)
                    .map(|i| tree.register(&format!("{base}/clip/{i}/{leaf}"), desc()))
                    .collect()
            };
            TrackClips {
                track: track.id,
                zones: destination_slots(patch, track).len(),
                launch: each("launch", &|| ParamDescriptor::trigger("clip.launch")),
                trigger: each("trigger", &|| ParamDescriptor::trigger("clip.trigger")),
                hold: each("hold", &|| ParamDescriptor::toggle("clip.hold", false)),
                arm: each("arm", &|| ParamDescriptor::trigger("clip.arm")),
                state: each("state", &|| {
                    ParamDescriptor::level(
                        "clip.state",
                        RealRange {
                            min: CLIP_EMPTY,
                            max: CLIP_ARMED,
                        },
                        Curve::Linear,
                        0.0,
                    )
                }),
                refresh: tree.register(
                    &format!("{base}/clips/refresh"),
                    ParamDescriptor::trigger("clips.refresh"),
                ),
                stop: tree.register(
                    &format!("{base}/stop"),
                    ParamDescriptor::trigger("track.stop"),
                ),
                opacity: tree.register(
                    &format!("{base}/opacity"),
                    ParamDescriptor::unit_level("track.opacity", track.opacity),
                ),
                mute: tree.register(
                    &format!("{base}/mute"),
                    ParamDescriptor::toggle("track.mute", track.mute),
                ),
                solo: tree.register(
                    &format!("{base}/solo"),
                    ParamDescriptor::toggle("track.solo", track.solo),
                ),
            }
        })
        .collect();
    let rows = show.tracks.iter().map(|t| t.clips.len()).max().unwrap_or(0);
    let scene = |leaf: &str| -> Vec<ParamHandle> {
        (0..rows)
            .map(|i| {
                tree.register(
                    &format!("/scene/{i}/{leaf}"),
                    ParamDescriptor::trigger(format!("scene.{leaf}")),
                )
            })
            .collect()
    };
    GridParams {
        tracks,
        scene_launch: scene("launch"),
        scene_arm: scene("arm"),
        go: Some(tree.register("/arm/go", ParamDescriptor::trigger("arm.go"))),
        stop_all: Some(tree.register("/tracks/stop", ParamDescriptor::trigger("tracks.stop"))),
    }
}

/// Build `effect_id` for every zone the track reaches on the caller's thread (a
/// pipeline compile, never on the render thread), write its stored params, and queue
/// a hard-cut swap for the next frame boundary. `false` if the effect is not in the
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
    Launch(usize, usize),
    Trigger(usize, usize),
    Hold(usize, usize),
    Arm(usize, usize),
    Refresh(usize),
    Stop(usize),
    SceneLaunch(usize),
    SceneArm(usize),
    Go,
    StopAll,
}

impl Action {
    fn is_trigger(self) -> bool {
        !matches!(self, Action::Hold(..))
    }
}

/// The running clip controller. Dropping it stops the thread and waits for it.
pub struct ClipController {
    closer: SubscriptionCloser,
    thread: Option<JoinHandle<()>>,
}

impl ClipController {
    /// Start playing the show's tracks (by slot) from their `active` clips. Publishes
    /// every clip's state once, then follows the tree.
    pub fn start(
        tree: Arc<ParameterTree>,
        gpu: Arc<Gpu>,
        sender: ZoneCommandSender,
        grid: GridParams,
        active: Vec<Option<usize>>,
        host: Arc<dyn ClipHost>,
    ) -> ClipController {
        let mut routes: HashMap<ParamId, (ParamHandle, Action)> = HashMap::new();
        let mut route = |h: &ParamHandle, a: Action| {
            routes.insert(h.id(), (h.clone(), a));
        };
        for (slot, t) in grid.tracks.iter().enumerate() {
            for (clip, h) in t.launch.iter().enumerate() {
                route(h, Action::Launch(slot, clip));
            }
            for (clip, h) in t.trigger.iter().enumerate() {
                route(h, Action::Trigger(slot, clip));
            }
            for (clip, h) in t.hold.iter().enumerate() {
                route(h, Action::Hold(slot, clip));
            }
            for (clip, h) in t.arm.iter().enumerate() {
                route(h, Action::Arm(slot, clip));
            }
            route(&t.refresh, Action::Refresh(slot));
            route(&t.stop, Action::Stop(slot));
        }
        for (row, h) in grid.scene_launch.iter().enumerate() {
            route(h, Action::SceneLaunch(row));
        }
        for (row, h) in grid.scene_arm.iter().enumerate() {
            route(h, Action::SceneArm(row));
        }
        if let Some(go) = &grid.go {
            route(go, Action::Go);
        }
        if let Some(stop_all) = &grid.stop_all {
            route(stop_all, Action::StopAll);
        }
        // Subscribe, then prime every trigger cursor at its current count, so nothing
        // fired before start is replayed and nothing fired after is missed.
        let sub = tree.subscribe(Filter::Ids(routes.keys().copied().collect()));
        let mut cursors: HashMap<ParamId, TriggerCursor> = HashMap::new();
        for (id, (h, action)) in &routes {
            if action.is_trigger() {
                let mut c = TriggerCursor::new();
                c.poll(h.fire_count().unwrap_or(0));
                cursors.insert(*id, c);
            }
        }
        let closer = sub.closer();
        let mut ctl = Controller {
            tree,
            gpu,
            sender,
            decks: (0..grid.tracks.len())
                .map(|slot| ZoneDeck::new(active.get(slot).copied().flatten()))
                .collect(),
            tracks: grid.tracks,
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
                        let Some(&(_, action)) = routes.get(&handle.id()) else {
                            continue;
                        };
                        if action.is_trigger() {
                            let count = handle.fire_count().unwrap_or(0);
                            let fired = cursors
                                .get_mut(&handle.id())
                                .is_some_and(|c| c.poll_edge(count));
                            if !fired {
                                continue;
                            }
                        }
                        match action {
                            Action::Launch(slot, clip) => ctl.launch(slot, clip),
                            Action::Trigger(slot, clip) => ctl.trigger(slot, clip),
                            Action::Hold(slot, clip) => {
                                ctl.hold(slot, clip, handle.read_bool() == Some(true))
                            }
                            Action::Arm(slot, clip) => ctl.arm(slot, clip),
                            Action::Refresh(slot) => ctl.publish_states(slot),
                            Action::Stop(slot) => ctl.stop(slot),
                            Action::SceneLaunch(row) => ctl.scene(row, false),
                            Action::SceneArm(row) => ctl.scene(row, true),
                            Action::Go => ctl.go(),
                            Action::StopAll => ctl.stop_all(),
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
    fn behaviour(&self, slot: usize) -> TrackBehaviour {
        self.host
            .track_behaviour(self.tracks[slot].track)
            .unwrap_or(TrackBehaviour {
                transition: TrackTransition::default(),
                launch: LaunchMode::Direct,
            })
    }

    fn launch(&mut self, slot: usize, clip: usize) {
        if self.behaviour(slot).launch == LaunchMode::Arm {
            self.arm(slot, clip);
            return;
        }
        let held = self.held_clips(slot);
        let change = self.decks[slot].latch(clip);
        self.release_hold_params(slot, held);
        self.send_one(slot, change);
    }

    fn trigger(&mut self, slot: usize, clip: usize) {
        let held = self.held_clips(slot);
        let change = self.decks[slot].trigger(clip);
        self.release_hold_params(slot, held);
        self.send_one(slot, change);
    }

    fn hold(&mut self, slot: usize, clip: usize, on: bool) {
        let deck = &mut self.decks[slot];
        let change = match (on, deck.is_held(clip)) {
            (true, false) => deck.press(clip),
            (false, true) => deck.release(clip),
            _ => DeckChange::None, // already in that state (e.g. our own reset below)
        };
        self.send_one(slot, change);
    }

    fn arm(&mut self, slot: usize, clip: usize) {
        let armed = self.decks[slot].arm(clip);
        self.host.armed_changed(self.tracks[slot].track, armed);
        self.publish_states(slot);
    }

    /// Stop: always sent, even when the deck already has nothing running — the render
    /// side turns a stop during a fade-out into a cut.
    fn stop(&mut self, slot: usize) {
        let command = self.stop_command(slot);
        self.send_batch(vec![command]);
    }

    /// Stop every track, each with its own transition, in the same frame (a second Stop
    /// All during the fades cuts them, track by track, like a second stop).
    fn stop_all(&mut self) {
        let batch = (0..self.tracks.len())
            .map(|slot| self.stop_command(slot))
            .collect();
        self.send_batch(batch);
    }

    /// Update a track's deck for a stop and build its render command.
    fn stop_command(&mut self, slot: usize) -> ZoneCommand {
        let held = self.held_clips(slot);
        self.decks[slot].stop();
        self.release_hold_params(slot, held);
        let fade = Fade::of_track(&self.behaviour(slot).transition);
        self.host
            .active_changed(self.tracks[slot].track, self.decks[slot].active());
        self.publish_states(slot);
        ZoneCommand::StopTrack { track: slot, fade }
    }

    /// Launch (or arm) row `row` on every track that has a clip there, in one frame.
    fn scene(&mut self, row: usize, arm: bool) {
        let mut batch = Vec::new();
        for slot in 0..self.tracks.len() {
            let track = self.tracks[slot].track;
            if row >= self.tracks[slot].launch.len() || self.host.clip_effect(track, row).is_none()
            {
                continue;
            }
            if arm || self.behaviour(slot).launch == LaunchMode::Arm {
                self.arm(slot, row);
                continue;
            }
            let held = self.held_clips(slot);
            let change = self.decks[slot].latch(row);
            self.release_hold_params(slot, held);
            batch.extend(self.commands_for(slot, change));
        }
        self.send_batch(batch);
    }

    /// GO: every armed clip plays, in one frame.
    fn go(&mut self) {
        let mut batch = Vec::new();
        for slot in 0..self.tracks.len() {
            if self.decks[slot].armed().is_none() {
                continue;
            }
            let held = self.held_clips(slot);
            let change = self.decks[slot].take_armed();
            self.release_hold_params(slot, held);
            self.host.armed_changed(self.tracks[slot].track, None);
            batch.extend(self.commands_for(slot, change));
            self.publish_states(slot);
        }
        self.send_batch(batch);
    }

    fn held_clips(&self, slot: usize) -> Vec<usize> {
        (0..self.tracks[slot].hold.len())
            .filter(|&c| self.decks[slot].is_held(c))
            .collect()
    }

    /// A latch/trigger forgot these temporary clips: put their `hold` back to 0 so a
    /// toggle pad's next tap presses again instead of releasing a hold that is gone.
    fn release_hold_params(&self, slot: usize, clips: Vec<usize>) {
        for clip in clips {
            self.tree
                .write_real(&self.tracks[slot].hold[clip], 0.0, engine_source());
        }
    }

    fn send_one(&mut self, slot: usize, change: DeckChange) {
        let commands = self.commands_for(slot, change);
        self.send_batch(commands);
    }

    fn send_batch(&self, mut commands: Vec<ZoneCommand>) {
        match commands.len() {
            0 => {}
            1 => {
                self.sender.send(commands.pop().expect("one command"));
            }
            _ => {
                self.sender.send(ZoneCommand::Batch(commands));
            }
        }
    }

    /// The render commands a deck change needs (built here, off the render thread),
    /// after telling the host and republishing the track's clip states.
    fn commands_for(&mut self, slot: usize, change: DeckChange) -> Vec<ZoneCommand> {
        let DeckChange::Play { cell, restart } = change else {
            return Vec::new();
        };
        let track = self.tracks[slot].track;
        let transition = self.behaviour(slot).transition;
        let effect = cell.and_then(|c| self.host.clip_effect(track, c));
        let mut commands = Vec::with_capacity(2);
        match effect {
            Some(ClipEffect {
                effect_id,
                params,
                palette,
                fade_time,
            }) => {
                let built = build_track_instances(
                    &self.gpu,
                    &self.tree,
                    track,
                    self.tracks[slot].zones,
                    &effect_id,
                    &params,
                    palette.as_deref(),
                    // The clip's stored values are a load, not a performance: written at
                    // UI tier so the panel (or a controller) takes them over at once. At
                    // the engine's tier they would lock every control out for the
                    // takeover timeout after each effect change.
                    stored_params_source(),
                );
                if let Some(effects) = built {
                    commands.push(ZoneCommand::SetTrackEffects {
                        track: slot,
                        effects,
                        fade: Fade::into_clip(&transition, fade_time),
                    });
                }
            }
            None => commands.push(ZoneCommand::StopTrack {
                track: slot,
                fade: Fade::of_track(&transition),
            }),
        }
        if restart {
            commands.push(ZoneCommand::Restart { track: slot });
        }
        self.host.active_changed(track, self.decks[slot].active());
        self.publish_states(slot);
        commands
    }

    fn publish_states(&self, slot: usize) {
        let t = &self.tracks[slot];
        let deck = &self.decks[slot];
        for (clip, handle) in t.state.iter().enumerate() {
            let value = if deck.active() == Some(clip) {
                CLIP_RUNNING
            } else if deck.armed() == Some(clip) {
                CLIP_ARMED
            } else if self.host.clip_effect(t.track, clip).is_some() {
                CLIP_ASSIGNED
            } else {
                CLIP_EMPTY
            };
            self.tree.write_real(handle, value, engine_source());
        }
    }
}

//! Scene-grid cells as Parameter Tree targets, and the controller that plays them
//! (M9 Slice 3 — pads on the grid; CLAUDE.md §4.5 `/zone/{id}/cell/{i}/…`).
//!
//! Firing a cell used to be a UI command that edited the show and swapped the effect
//! directly — nothing a MIDI pad (or OSC, later) could reach. Now every cell exposes
//! tree parameters and **all** transports go through them (§3.1 principle 2):
//!
//! ```text
//! /zone/{id}/cell/{i}/launch    trigger  latch: the cell becomes the zone's base
//! /zone/{id}/cell/{i}/trigger   trigger  latch + restart the effect from its start
//! /zone/{id}/cell/{i}/hold      toggle   1 = run temporarily, 0 = return to before
//! /zone/{id}/cell/{i}/state     level    0 empty · 1 assigned · 2 running (read-only)
//! /zone/{id}/cells/refresh      trigger  re-read which cells are assigned
//! ```
//!
//! The [`CellController`] thread subscribes to those, runs each zone's
//! [`ZoneDeck`] (the latch/trigger/hold decisions, including where a temporary cell
//! returns to), swaps effects through the frame-boundary queue, and publishes each
//! cell's `state` — which is what pad LEDs show.
//!
//! The Show document stays with its owner (the app): the controller asks a
//! [`CellHost`] what a cell holds and tells it when a zone's running cell changed.
//! The controller holds no lock while it waits for changes, and never calls the host
//! with one held.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use prisma_core::show::{ParamSet, Show};
use prisma_core::source::SourceId;
use prisma_core::tree::ParameterTree;
use prisma_core::{
    Curve, DeckChange, Filter, ParamDescriptor, ParamHandle, ParamId, RealRange,
    SubscriptionCloser, TriggerCursor, ZoneDeck,
};
use prisma_patch::Patch;
use prisma_render::effect::EffectInstance;
use prisma_render::{manifest_by_id, Gpu};
use uuid::Uuid;

use crate::zone_content::ZoneCommandSender;

/// `state` value of an empty cell.
pub const CELL_EMPTY: f32 = 0.0;
/// `state` value of a cell with an effect that is not running.
pub const CELL_ASSIGNED: f32 = 1.0;
/// `state` value of the zone's running cell.
pub const CELL_RUNNING: f32 = 2.0;

/// The owner of the Show document, as the cell controller sees it.
pub trait CellHost: Send + Sync {
    /// The effect in `zone`'s cell `cell`, or `None` if it is empty.
    fn cell_effect(&self, zone: Uuid, cell: usize) -> Option<(String, ParamSet)>;
    /// `zone`'s running cell changed (a pad, a UI click, a release returning).
    fn active_changed(&self, zone: Uuid, active: Option<usize>);
}

/// The engine writes cell state as the highest tier: it is derived, nobody else's.
fn engine_source() -> SourceId {
    SourceId::new(u8::MAX, 0)
}

/// One zone's cell parameters, resolved once.
#[derive(Clone)]
pub struct ZoneCells {
    pub zone: Uuid,
    pub launch: Vec<ParamHandle>,
    pub trigger: Vec<ParamHandle>,
    pub hold: Vec<ParamHandle>,
    pub state: Vec<ParamHandle>,
    pub refresh: ParamHandle,
}

/// Register every zone's cell parameters, sized by the show's grids (zones the show
/// does not describe get none). Indexed by zone slot.
pub fn register_cells(tree: &ParameterTree, patch: &Patch, show: &Show) -> Vec<ZoneCells> {
    patch
        .zones
        .iter()
        .map(|zone| {
            let cells = show.zone(zone.id).map_or(0, |z| z.grid.len());
            let base = format!("/zone/{}/cell", zone.id);
            let each = |leaf: &str, desc: &dyn Fn() -> ParamDescriptor| -> Vec<ParamHandle> {
                (0..cells)
                    .map(|i| tree.register(&format!("{base}/{i}/{leaf}"), desc()))
                    .collect()
            };
            ZoneCells {
                zone: zone.id,
                launch: each("launch", &|| ParamDescriptor::trigger("cell.launch")),
                trigger: each("trigger", &|| ParamDescriptor::trigger("cell.trigger")),
                hold: each("hold", &|| ParamDescriptor::toggle("cell.hold", false)),
                state: each("state", &|| {
                    ParamDescriptor::level(
                        "cell.state",
                        RealRange {
                            min: CELL_EMPTY,
                            max: CELL_RUNNING,
                        },
                        Curve::Linear,
                        0.0,
                    )
                }),
                refresh: tree.register(
                    &format!("/zone/{}/cells/refresh", zone.id),
                    ParamDescriptor::trigger("cells.refresh"),
                ),
            }
        })
        .collect()
}

/// Build `effect_id` on the caller's thread (a pipeline compile, never on the render
/// thread), write its stored params, and queue the swap for the next frame boundary.
/// `false` if the effect is not in the catalogue or the render loop is gone.
#[allow(clippy::too_many_arguments)] // each is a distinct piece of the engine it needs
pub fn fire_zone_effect(
    gpu: &Gpu,
    tree: &ParameterTree,
    sender: &ZoneCommandSender,
    slot: usize,
    zone: Uuid,
    effect_id: &str,
    params: &ParamSet,
    src: SourceId,
) -> bool {
    let Some(manifest) = manifest_by_id(effect_id) else {
        return false;
    };
    let prefix = format!("/zone/{zone}/layer/0");
    let effect = EffectInstance::new(gpu, tree, &prefix, &manifest);
    for (name, value) in params.iter() {
        tree.write_path(&format!("{prefix}/param/{name}"), value, src);
    }
    sender.set_zone_effect(slot, Some(effect))
}

#[derive(Clone, Copy)]
enum Action {
    Launch,
    Trigger,
    Hold,
    Refresh,
}

/// The running cell controller. Dropping it stops the thread and waits for it.
pub struct CellController {
    closer: SubscriptionCloser,
    thread: Option<JoinHandle<()>>,
}

impl CellController {
    /// Start playing `zones` (by slot) from their show `active` cells. Publishes every
    /// cell's state once, then follows the pads.
    pub fn start(
        tree: Arc<ParameterTree>,
        gpu: Arc<Gpu>,
        sender: ZoneCommandSender,
        zones: Vec<ZoneCells>,
        active: Vec<Option<usize>>,
        host: Arc<dyn CellHost>,
    ) -> CellController {
        let mut routes: HashMap<ParamId, (usize, usize, Action)> = HashMap::new();
        let mut cursors: HashMap<ParamId, TriggerCursor> = HashMap::new();
        for (slot, z) in zones.iter().enumerate() {
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
        for z in &zones {
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
            decks: zones
                .iter()
                .enumerate()
                .map(|(slot, _)| ZoneDeck::new(active.get(slot).copied().flatten()))
                .collect(),
            zones,
            host,
        };
        let thread = std::thread::Builder::new()
            .name("prisma-cells".into())
            .spawn(move || {
                for slot in 0..ctl.zones.len() {
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
            .expect("spawn cell controller thread");
        CellController {
            closer,
            thread: Some(thread),
        }
    }
}

impl Drop for CellController {
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
    zones: Vec<ZoneCells>,
    decks: Vec<ZoneDeck>,
    host: Arc<dyn CellHost>,
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
        (0..self.zones[slot].hold.len())
            .filter(|&c| self.decks[slot].is_held(c))
            .collect()
    }

    /// A latch/trigger forgot these temporary cells: put their `hold` back to 0 so a
    /// toggle pad's next tap presses again instead of releasing a hold that is gone.
    fn release_hold_params(&self, slot: usize, cells: Vec<usize>) {
        for cell in cells {
            self.tree
                .write_real(&self.zones[slot].hold[cell], 0.0, engine_source());
        }
    }

    fn apply(&mut self, slot: usize, change: DeckChange) {
        let DeckChange::Play { cell, restart } = change else {
            return;
        };
        let zone = self.zones[slot].zone;
        let effect = cell.and_then(|c| self.host.cell_effect(zone, c));
        match effect {
            Some((effect_id, params)) => {
                fire_zone_effect(
                    &self.gpu,
                    &self.tree,
                    &self.sender,
                    slot,
                    zone,
                    &effect_id,
                    &params,
                    engine_source(),
                );
            }
            None => {
                self.sender.set_zone_effect(slot, None);
            }
        }
        if restart {
            self.sender.restart_zone(slot);
        }
        self.host.active_changed(zone, self.decks[slot].active());
        self.publish_states(slot);
    }

    fn publish_states(&self, slot: usize) {
        let z = &self.zones[slot];
        let active = self.decks[slot].active();
        for (cell, handle) in z.state.iter().enumerate() {
            let value = if active == Some(cell) {
                CELL_RUNNING
            } else if self.host.cell_effect(z.zone, cell).is_some() {
                CELL_ASSIGNED
            } else {
                CELL_EMPTY
            };
            self.tree.write_real(handle, value, engine_source());
        }
    }
}

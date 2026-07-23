//! Binding of master and per-zone live state to the Parameter Tree
//! (CLAUDE.md §3.1 principle 2, §4.5).
//!
//! This is what makes zone state *addressable*: without it, dimmers and colour
//! filters are values passed by hand, MIDI/OSC/UI have nothing to write to, and
//! the consumers end up sharing state through a lock — on the output thread, in
//! the hot path. Registering them here fixes all three at once.
//!
//! # Paths
//!
//! ```text
//! /master/dimmer                /zone/{uuid}/dimmer
//! /master/color/{r,g,b}         /zone/{uuid}/color/{r,g,b}
//!                               /zone/{uuid}/speed
//!                               /zone/{uuid}/solo
//!                               /zone/{uuid}/blackout
//! ```
//!
//! §4.5 writes colour as a single `/zone/{id}/color`, but a tree parameter holds
//! one scalar, so colour decomposes into three unit-range leaves. The UI presents
//! them as one swatch; MIDI can map them individually.
//!
//! # Wait-free by construction
//!
//! Paths are resolved to [`ParamHandle`]s **once**, at registration. Both readers
//! below then do plain atomic loads per frame — no path lookup, no lock, nothing
//! that can block the render or output thread (§3.3). The same
//! [`LiveParams`] object serves both: it implements the render side's
//! [`ZoneStateSource`] and the output side's [`FlatParamSource`], so the two
//! stages are guaranteed to read the same parameters rather than two copies that
//! could drift.
//!
//! Dimmers use an exponential response curve (§4.5: "dimmers should not be
//! linear"). Transports write the **normalized** fader position; the engine
//! consumes `read_real()`, which applies the curve.

use prisma_core::descriptor::{Curve, ParamDescriptor, RealRange};
use prisma_core::zone::ZoneStates;
use prisma_core::{ParamHandle, ParameterTree};
use prisma_output::plan::FlatParams;
use prisma_output::scheduler::FlatParamSource;
use prisma_patch::Patch;

use crate::render_loop::ZoneStateSource;

/// Perceptual dimmer curve. `gamma > 1` pushes resolution to the low end, which is
/// where an operator does the delicate work.
const DIMMER_GAMMA: f32 = 2.2;

/// Handles for one zone's live parameters.
#[derive(Clone)]
pub struct ZoneHandles {
    pub dimmer: ParamHandle,
    pub color: [ParamHandle; 3],
    pub speed: ParamHandle,
    pub solo: ParamHandle,
    pub blackout: ParamHandle,
}

/// Handles for the master section.
#[derive(Clone)]
pub struct MasterHandles {
    pub dimmer: ParamHandle,
    pub color: [ParamHandle; 3],
}

/// Master + per-zone parameters, resolved once and read wait-free every frame.
/// Cheap to clone (handles are refcounted), which is how one binding feeds both
/// the render loop and the output stage.
#[derive(Clone)]
pub struct LiveParams {
    master: MasterHandles,
    /// Indexed by zone **slot** — the zone's position in `Patch::zones`, the same
    /// coordinate the patch index and the overlap counter use.
    zones: Vec<ZoneHandles>,
}

fn dimmer_descriptor(label: &str, default_norm: f32) -> ParamDescriptor {
    ParamDescriptor::level(
        label,
        RealRange { min: 0.0, max: 1.0 },
        Curve::Exponential {
            gamma: DIMMER_GAMMA,
        },
        default_norm,
    )
}

impl LiveParams {
    /// Register every master and zone parameter for `patch` and resolve handles.
    ///
    /// Zones start with their dimmer down, so loading a patch never lights the rig
    /// by itself; master starts open.
    pub fn register(tree: &ParameterTree, patch: &Patch) -> LiveParams {
        let master = MasterHandles {
            dimmer: tree.register("/master/dimmer", dimmer_descriptor("master.dimmer", 1.0)),
            color: [
                tree.register(
                    "/master/color/r",
                    ParamDescriptor::unit_level("master.color.r", 1.0),
                ),
                tree.register(
                    "/master/color/g",
                    ParamDescriptor::unit_level("master.color.g", 1.0),
                ),
                tree.register(
                    "/master/color/b",
                    ParamDescriptor::unit_level("master.color.b", 1.0),
                ),
            ],
        };

        let zones = patch
            .zones
            .iter()
            .map(|zone| {
                let base = format!("/zone/{}", zone.id);
                ZoneHandles {
                    dimmer: tree.register(
                        &format!("{base}/dimmer"),
                        dimmer_descriptor("zone.dimmer", 0.0),
                    ),
                    color: [
                        tree.register(
                            &format!("{base}/color/r"),
                            ParamDescriptor::unit_level("zone.color.r", 1.0),
                        ),
                        tree.register(
                            &format!("{base}/color/g"),
                            ParamDescriptor::unit_level("zone.color.g", 1.0),
                        ),
                        tree.register(
                            &format!("{base}/color/b"),
                            ParamDescriptor::unit_level("zone.color.b", 1.0),
                        ),
                    ],
                    speed: tree.register(
                        &format!("{base}/speed"),
                        ParamDescriptor::level(
                            "zone.speed",
                            RealRange { min: 0.0, max: 4.0 },
                            Curve::Linear,
                            0.25, // real 1.0 — unity speed
                        ),
                    ),
                    solo: tree.register(
                        &format!("{base}/solo"),
                        ParamDescriptor::toggle("zone.solo", false),
                    ),
                    blackout: tree.register(
                        &format!("{base}/blackout"),
                        ParamDescriptor::toggle("zone.blackout", false),
                    ),
                }
            })
            .collect();

        LiveParams { master, zones }
    }

    #[inline]
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    /// Handles for one zone, so a caller can write through the tree (which is how
    /// every transport, including the UI, must do it).
    #[inline]
    pub fn zone(&self, slot: usize) -> &ZoneHandles {
        &self.zones[slot]
    }

    #[inline]
    pub fn master(&self) -> &MasterHandles {
        &self.master
    }

    #[inline]
    fn zone_color(&self, slot: usize) -> [f32; 3] {
        let c = &self.zones[slot].color;
        [
            c[0].read_real().unwrap_or(1.0),
            c[1].read_real().unwrap_or(1.0),
            c[2].read_real().unwrap_or(1.0),
        ]
    }
}

/// Render side: refresh the loop's `ZoneStates` at each frame boundary.
///
/// `into` is the loop's persistent state, so writing the dimmer through
/// `set_dimmer` keeps the hysteretic `dimmer_up` latch evolving across frames —
/// which is what stops a noisy fader from flapping the overlap counter (§4.3).
impl ZoneStateSource for LiveParams {
    fn read_into(&self, into: &mut ZoneStates) {
        for slot in 0..self.zones.len().min(into.len()) {
            let z = &self.zones[slot];
            into.set_dimmer(slot, z.dimmer.read_real().unwrap_or(0.0));
            into.set_color_filter(slot, self.zone_color(slot));
            into.set_speed_multiplier(slot, z.speed.read_real().unwrap_or(1.0));
            into.set_solo(slot, z.solo.read_bool().unwrap_or(false));
            into.set_blackout(slot, z.blackout.read_bool().unwrap_or(false));
        }
    }
}

/// Output side: refresh the frame's flat parameters. Called once per output frame
/// — the values are live, while the frame's overlap snapshot decides which pixels
/// they apply to (§5.2 step 6).
impl FlatParamSource for LiveParams {
    fn read_into(&self, into: &mut FlatParams) {
        let m = &self.master;
        into.set_master(
            m.dimmer.read_real().unwrap_or(1.0),
            [
                m.color[0].read_real().unwrap_or(1.0),
                m.color[1].read_real().unwrap_or(1.0),
                m.color[2].read_real().unwrap_or(1.0),
            ],
        );
        for slot in 0..self.zones.len() {
            into.set_zone(
                slot,
                self.zones[slot].dimmer.read_real().unwrap_or(0.0),
                self.zone_color(slot),
            );
        }
    }
}

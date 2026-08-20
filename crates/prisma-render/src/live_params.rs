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

use std::time::Instant;

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

/// Handles for a strobe: a free-running (Hz) on/off gate. `rate == 0` means off
/// (the gate is a constant 1.0). `duty` is the fraction of each cycle the gate is on.
/// Applied at the output stage, so master strobe gates every pixel and a zone strobe
/// gates that zone's non-overlapped pixels (§5.2 step 6). No BPM/beat sync in M8 — the
/// phase is wall-clock, matching the "global effects apply from above" model.
#[derive(Clone)]
pub struct StrobeHandles {
    pub rate: ParamHandle,
    pub duty: ParamHandle,
}

/// Handles for one zone's live parameters.
#[derive(Clone)]
pub struct ZoneHandles {
    pub dimmer: ParamHandle,
    pub color: [ParamHandle; 3],
    pub speed: ParamHandle,
    pub solo: ParamHandle,
    pub blackout: ParamHandle,
    pub strobe: StrobeHandles,
}

/// Handles for the master section.
#[derive(Clone)]
pub struct MasterHandles {
    pub dimmer: ParamHandle,
    pub color: [ParamHandle; 3],
    pub blackout: ParamHandle,
    pub strobe: StrobeHandles,
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
    /// Wall-clock origin for the free-running strobe phase (§ StrobeHandles). Shared
    /// by whatever reads the flat params (output + preview); each reads `elapsed()`
    /// on its own thread, so their phases may differ by a sub-frame — fine, a strobe
    /// is a picture, and with `rate == 0` (the default) the gate is a constant 1.0 so
    /// they agree exactly (the M6 preview==wire equivalence holds).
    start: Instant,
}

/// Free-running strobe gate. `1.0` (full on) when `rate <= 0` (off); otherwise a hard
/// on/off gate at `rate` Hz, on for the first `duty` fraction of each cycle. Depth is
/// 100% in M8 (off = black). Phase is computed in `f64` so it stays exact over long
/// runs (an `f32` product would quantise the phase after minutes).
fn strobe_gate(elapsed_s: f64, rate: f32, duty: f32) -> f32 {
    if rate <= 0.0 {
        return 1.0;
    }
    let phase = (elapsed_s * rate as f64).fract();
    if phase < duty.clamp(0.0, 1.0) as f64 {
        1.0
    } else {
        0.0
    }
}

/// Descriptor for a strobe rate (0–30 Hz, linear, default 0 = off).
fn strobe_rate_descriptor(label: &str) -> ParamDescriptor {
    ParamDescriptor::level(
        label,
        RealRange {
            min: 0.0,
            max: 30.0,
        },
        Curve::Linear,
        0.0,
    )
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
            blackout: tree.register(
                "/master/blackout",
                ParamDescriptor::toggle("master.blackout", false),
            ),
            strobe: StrobeHandles {
                rate: tree.register(
                    "/master/strobe/rate",
                    strobe_rate_descriptor("master.strobe.rate"),
                ),
                duty: tree.register(
                    "/master/strobe/duty",
                    ParamDescriptor::unit_level("master.strobe.duty", 0.5),
                ),
            },
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
                            // Bipolar velocity (professional consoles-style): 0 = static, sign = play
                            // direction (negative runs the zone's effects in reverse).
                            // The reverse is realised by the render's per-zone phase
                            // accumulator ([`zone_content`]), not by scaling absolute time.
                            RealRange {
                                min: -10.0,
                                max: 10.0,
                            },
                            Curve::Linear,
                            // Centre (norm 0.5) is 0 = static; unity forward is real 1.0.
                            0.55, // (1.0 - (-10)) / 20 = real 1.0 — unity speed
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
                    strobe: StrobeHandles {
                        rate: tree.register(
                            &format!("{base}/strobe/rate"),
                            strobe_rate_descriptor("zone.strobe.rate"),
                        ),
                        duty: tree.register(
                            &format!("{base}/strobe/duty"),
                            ParamDescriptor::unit_level("zone.strobe.duty", 0.5),
                        ),
                    },
                }
            })
            .collect();

        LiveParams {
            master,
            zones,
            start: Instant::now(),
        }
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
        // One wall-clock read per output frame drives every strobe's phase; the gate
        // folds into the dimmer each scale is built from, so `scale_component` (the
        // shared hot path) is untouched. Master strobe/blackout gate every pixel; a
        // zone's strobe gates that zone's non-overlapped pixels (overlapped pixels are
        // dimmed in render, which does not yet apply a zone strobe — the documented
        // follow-up). `rate == 0` ⇒ gate 1.0, so an idle strobe costs nothing.
        let t = self.start.elapsed().as_secs_f64();

        let m = &self.master;
        let master_dimmer = if m.blackout.read_bool().unwrap_or(false) {
            0.0
        } else {
            let gate = strobe_gate(
                t,
                m.strobe.rate.read_real().unwrap_or(0.0),
                m.strobe.duty.read_real().unwrap_or(0.5),
            );
            m.dimmer.read_real().unwrap_or(1.0) * gate
        };
        into.set_master(
            master_dimmer,
            [
                m.color[0].read_real().unwrap_or(1.0),
                m.color[1].read_real().unwrap_or(1.0),
                m.color[2].read_real().unwrap_or(1.0),
            ],
        );

        for slot in 0..self.zones.len() {
            let z = &self.zones[slot];
            let gate = strobe_gate(
                t,
                z.strobe.rate.read_real().unwrap_or(0.0),
                z.strobe.duty.read_real().unwrap_or(0.5),
            );
            into.set_zone(
                slot,
                z.dimmer.read_real().unwrap_or(0.0) * gate,
                self.zone_color(slot),
            );
        }
    }
}

//! The Show document model (CLAUDE.md §4.3, M8-SPEC §5).
//!
//! The Show is the **performable document**: per-zone grids of assignable cells,
//! each zone's always-live state, and the master state, over an embedded Patch.
//! It is what the operator saves as "the show file" (`.prisma`); the Patch is the
//! reusable wiring it drives.
//!
//! # Per-zone grids, not a cross-zone matrix (locked decision, M8-SPEC §3.3)
//!
//! Each [`ZoneShow`] owns its **own** [`grid`](ZoneShow::grid) of [`Cell`]s and
//! its own [`live`](ZoneShow::live) state. There is no cross-zone `Scene` and no
//! whole-column firing: you select a zone, edit/trigger *its* grid, and the other
//! zones keep running untouched. *Selected* (a UI state) is not modelled here at
//! all — it never changes what a zone outputs (§3.2).
//!
//! # Embedded patch (M8-SPEC §5)
//!
//! The Patch is **embedded**, not referenced by id, so a `.prisma` opens
//! standalone — consistent with how the patch already serialises. The embedded
//! `Patch::id` is the mapping's identity; there is no separate `patch_id` field.
//!
//! # Document vs. runtime state
//!
//! [`ZoneLive`] here is the *persisted* form of a zone's live state; the runtime
//! counterpart [`crate::zone::ZoneLiveState`] adds the hysteretic `dimmer_up`
//! latch, which is **derived** from `dimmer` when a show loads and never stored.
//! Effect parameters ([`ParamSet`]) are stored normalized (`0.0..=1.0`), the same
//! external representation the Parameter Tree uses; the engine seeds the tree from
//! them on load.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::Duration;

use prisma_patch::{BlendMode, Patch};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Normalized effect parameter values, keyed by the manifest parameter name
/// (`EffectManifest::params[..].name`). Values are the external `0.0..=1.0`
/// representation the Parameter Tree uses; the per-parameter curve to a real range
/// lives in the effect manifest, not here.
///
/// Backed by a `BTreeMap` so serialization order is deterministic and diffs stay
/// small (the same reason the patch uses `Vec`s over `HashMap`s).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamSet(pub BTreeMap<String, f32>);

impl ParamSet {
    /// An empty parameter set (effect defaults apply until overridden).
    pub fn new() -> ParamSet {
        ParamSet(BTreeMap::new())
    }

    /// The normalized value for `name`, if set.
    pub fn get(&self, name: &str) -> Option<f32> {
        self.0.get(name).copied()
    }

    /// Set `name` to a normalized value, clamped to `0.0..=1.0`.
    pub fn set(&mut self, name: impl Into<String>, value: f32) {
        self.0.insert(name.into(), value.clamp(0.0, 1.0));
    }

    /// Iterate `(name, normalized value)` in deterministic (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f32)> {
        self.0.iter().map(|(k, &v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// A layer's content source (CLAUDE.md §4.2).
///
/// **M8 ships `Shader` only** — it wires the M5 GPU effect library (Color,
/// Gradient, Plasma, Wave, Noise, …) into assignable scenes. `Media` (M10) and
/// `Script` (M11) are **additive variants** behind a `schema_version` bump: adding
/// a variant leaves every M8 `.prisma` readable, and an M8 reader meeting a newer
/// file is refused by the newer-than-reader guard rather than silently dropping a
/// source it does not understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A procedural GPU effect from the built-in library. `effect_id` matches an
    /// `EffectManifest::effect_id` (e.g. `"plasma"`); `params` are its normalized
    /// values.
    Shader { effect_id: String, params: ParamSet },
}

/// One layer of a cell's stack (CLAUDE.md §4.2).
///
/// **M8 fills exactly one layer per cell** (one effect — M8-SPEC §3.3). The
/// `Vec<Layer>` shape is kept so a multi-layer stack is additive later, not a model
/// change. The §4.2 `fx_filters` (FX post-passes) and `mapping` (source→geometry
/// [`MappingMode`]) fields are deliberately **not** on the M8 layer: no filter ships
/// in M8 and mapping modes land with media (M10). Both are `#[serde(default)]`
/// additions when built, so their absence in an M8 file stays backward compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub source: Source,
    /// How this layer blends onto the layers below it. Reuses the patch's
    /// canonical [`BlendMode`] — the same enum the compositor and zone stacking use.
    pub blend_mode: BlendMode,
    /// `0.0..=1.0`.
    pub opacity: f32,
}

impl Layer {
    /// A shader layer at full opacity, `Normal` blend — the common case an effect
    /// picker produces.
    pub fn shader(effect_id: impl Into<String>, params: ParamSet) -> Layer {
        Layer {
            source: Source::Shader {
                effect_id: effect_id.into(),
                params,
            },
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
        }
    }
}

/// One assignable cell of a zone's grid (CLAUDE.md §4.3) — a zone's "scene".
///
/// An **empty** cell has no layers ([`Cell::is_empty`]); triggering it makes the
/// zone output nothing. A populated M8 cell holds exactly one [`Layer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The layer stack, stored inline (not by reference — CLAUDE.md §4.3 "cell
    /// storage decision"). M8 fills exactly one; empty = unassigned.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Transition into this cell, driven by the master clock.
    #[serde(default)]
    pub fade_time: Duration,
}

impl Cell {
    /// An unassigned cell: no content, no fade.
    pub fn empty() -> Cell {
        Cell {
            layers: Vec::new(),
            fade_time: Duration::ZERO,
        }
    }

    /// A single-effect cell (the M8 shape).
    pub fn single(layer: Layer) -> Cell {
        Cell {
            layers: vec![layer],
            fade_time: Duration::ZERO,
        }
    }

    /// Whether this cell has no content (an unassigned grid slot).
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// Persisted per-zone always-live state (CLAUDE.md §4.3 `ZoneLiveState`).
///
/// The runtime counterpart [`crate::zone::ZoneLiveState`] adds the hysteretic
/// `dimmer_up` latch, recomputed from `dimmer` when a show loads — the latch is
/// derived, never stored. Defaults mirror the runtime default so a zone created in
/// a show and a zone at engine-default agree: dimmer **down** (not contributing
/// until driven), unit colour filter, unit speed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneLive {
    /// `0.0..=1.0`, applied at the output stage.
    pub dimmer: f32,
    /// Linear-light colour filter, applied at the output stage.
    pub color_filter: [f32; 3],
    /// Scales this zone's effect animation timing; applied in render.
    pub speed_multiplier: f32,
    pub solo: bool,
    pub blackout: bool,
}

impl Default for ZoneLive {
    fn default() -> Self {
        ZoneLive {
            dimmer: 0.0,
            color_filter: [1.0, 1.0, 1.0],
            speed_multiplier: 1.0,
            solo: false,
            blackout: false,
        }
    }
}

/// One zone's performable state: its own grid, its running cell, its live state
/// (CLAUDE.md §4.3). Addressed to a patch zone by [`zone_id`](ZoneShow::zone_id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneShow {
    /// The patch [`Zone::id`](prisma_patch::Zone) this drives.
    pub zone_id: Uuid,
    /// THIS zone's own grid of assignable cells (its scenes). May be empty.
    #[serde(default)]
    pub grid: Vec<Cell>,
    /// Index of the running cell in [`grid`](ZoneShow::grid), or `None` (nothing
    /// playing). Validated in range by [`Show::validate`].
    #[serde(default)]
    pub active: Option<usize>,
    /// Always-live dimmer/colour/speed/solo/blackout.
    #[serde(default)]
    pub live: ZoneLive,
}

impl ZoneShow {
    /// A zone show with an empty grid and default live state.
    pub fn new(zone_id: Uuid) -> ZoneShow {
        ZoneShow {
            zone_id,
            grid: Vec::new(),
            active: None,
            live: ZoneLive::default(),
        }
    }
}

/// Master state applied **above** all zones (CLAUDE.md §4.3): master dimmer,
/// colour and strobe, applied at the output stage for every pixel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MasterState {
    /// `0.0..=1.0`. Defaults to **full** so a loaded show is not globally dark.
    pub dimmer: f32,
    /// Linear-light master colour filter.
    pub color: [f32; 3],
    /// `0.0..=1.0`; `0.0` = off.
    pub strobe: f32,
}

impl Default for MasterState {
    fn default() -> Self {
        MasterState {
            dimmer: 1.0,
            color: [1.0, 1.0, 1.0],
            strobe: 0.0,
        }
    }
}

/// The Show's DMX network output (docs/DEVICES-SPEC.md): which Art-Net nodes receive
/// which universes, through which network interface.
///
/// Lives in the Show — not the machine config — because the nodes and the lighting
/// network belong to the rig the show drives (Bauti, 2026-11-16). Filled in additively
/// over the M8 placeholder (`"network": {}`), so old files load unchanged: every field
/// has a serde default, and [`initialized`](Self::initialized) tells a never-configured
/// show (which the app migrates from the old app-wide Art-Net target) apart from one
/// whose operator deliberately left it without nodes.
///
/// Universe numbers here are the raw Art-Net 15-bit **port address** (`Net:Sub-Net:
/// Universe`, 0-based) — the wire truth. The UI shows them 1-based, like professional consoles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// `false` until the device manager (or the one-time migration) has written this
    /// config. Old shows deserialize `false`.
    #[serde(default)]
    pub initialized: bool,
    /// Master switch ("Habilitar"). Off → output goes to the loopback virtual sink.
    #[serde(default)]
    pub artnet_enabled: bool,
    /// Send one ArtSync per frame to every enabled node (tear-free multi-node output).
    #[serde(default = "default_true")]
    pub artsync: bool,
    /// The network interface to send from, by network (see [`InterfaceRef`]). `None` =
    /// let the OS route.
    #[serde(default)]
    pub interface: Option<InterfaceRef>,
    /// The Art-Net nodes, in the operator's order.
    #[serde(default)]
    pub nodes: Vec<ArtNetNode>,
}

fn default_true() -> bool {
    true
}

/// The network interface a show sends from, stored by its **network** so the show
/// still resolves on another machine, where the same NIC has another name ("eth0" vs
/// "Ethernet 2"). `name_hint` is only for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceRef {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    #[serde(default)]
    pub name_hint: String,
}

impl InterfaceRef {
    /// Whether `ip` is on this interface's network.
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = prefix_mask(self.prefix_len);
        u32::from(ip) & mask == u32::from(self.address) & mask
    }

    /// The network's directed broadcast address (e.g. `2.255.255.255` for 2.x/8).
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.address) | !prefix_mask(self.prefix_len))
    }
}

fn prefix_mask(prefix_len: u8) -> u32 {
    match prefix_len {
        0 => 0,
        n if n >= 32 => u32::MAX,
        n => u32::MAX << (32 - n),
    }
}

/// One Art-Net node (a controller board: H802RA, ESP32/WLED, …) and the universes it
/// receives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtNetNode {
    pub id: Uuid,
    pub name: String,
    pub ip: Ipv4Addr,
    /// Inclusive port-address ranges this node receives.
    #[serde(default)]
    pub universes: Vec<UniverseRange>,
    /// Quick mute of one node without deleting it.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `true` = unicast to [`ip`](Self::ip); `false` = the interface's subnet broadcast.
    #[serde(default = "default_true")]
    pub unicast: bool,
    /// Pad every ArtDmx to 512 channels, for nodes that need full frames.
    #[serde(default)]
    pub full_frames: bool,
    /// Where the node came from (manual entry or a Scan), for display.
    #[serde(default)]
    pub origin: NodeOrigin,
}

impl ArtNetNode {
    /// Whether this node receives `port_address`.
    pub fn receives(&self, port_address: u16) -> bool {
        self.universes.iter().any(|r| r.contains(port_address))
    }
}

/// An inclusive range of Art-Net port addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseRange {
    pub first: u16,
    pub last: u16,
}

impl UniverseRange {
    /// The highest valid Art-Net port address (15 bits).
    pub const MAX: u16 = 0x7FFF;

    pub fn contains(&self, port_address: u16) -> bool {
        (self.first..=self.last).contains(&port_address)
    }
}

/// How a node entered the list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeOrigin {
    #[default]
    Manual,
    /// Found by Scan (ArtPollReply): the node's own long name and MAC, for display.
    Scanned { long_name: String, mac: String },
}

impl NetworkConfig {
    /// Structural checks the device manager runs before applying: every range ordered
    /// and within 15 bits. (Unreachable IPs or uncovered universes are warnings in the
    /// UI, not errors — the show must still run.)
    pub fn validate(&self) -> Result<(), crate::show::ShowError> {
        for node in &self.nodes {
            for r in &node.universes {
                if r.first > r.last || r.last > UniverseRange::MAX {
                    return Err(crate::show::ShowError::InvalidNetwork(format!(
                        "node \"{}\": universe range {}..={} is invalid",
                        node.name, r.first, r.last
                    )));
                }
            }
        }
        Ok(())
    }
}

/// The Show document (CLAUDE.md §4.3, M8-SPEC §5). Serializes to `.prisma`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    pub id: Uuid,
    pub name: String,
    /// The embedded Patch this Show drives (M8-SPEC §5 — embedded, standalone).
    pub patch: Patch,
    /// One [`ZoneShow`] per patch zone. [`Show::validate`] enforces the bijection.
    pub zones: Vec<ZoneShow>,
    #[serde(default)]
    pub master: MasterState,
    /// Reserved for the device manager (M8-SPEC §5).
    #[serde(default)]
    pub network: NetworkConfig,
    /// Optional embedded controller mappings for turnkey/locked deliveries
    /// (CLAUDE.md §4.6). Populated in **M9**; reserved as opaque JSON until the
    /// `ControllerMappings` type exists, so the field is stable now and typed
    /// later without a schema break. `None` by default — normal mappings live in a
    /// separate profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_mappings: Option<serde_json::Value>,
}

impl Show {
    /// A new show over `patch`, with one empty-grid [`ZoneShow`] per patch zone and
    /// a fresh id. This is the "engine loads a show" starting point (§9 step 1).
    pub fn new(name: impl Into<String>, patch: Patch) -> Show {
        let zones = patch.zones.iter().map(|z| ZoneShow::new(z.id)).collect();
        Show {
            id: Uuid::new_v4(),
            name: name.into(),
            patch,
            zones,
            master: MasterState::default(),
            network: NetworkConfig::default(),
            embedded_mappings: None,
        }
    }

    /// The [`ZoneShow`] driving a given patch zone, if present.
    pub fn zone(&self, zone_id: Uuid) -> Option<&ZoneShow> {
        self.zones.iter().find(|z| z.zone_id == zone_id)
    }

    /// Structural validation, the Show analogue of [`Patch::validate`]:
    ///
    /// 1. the embedded patch validates;
    /// 2. the zone shows are a **bijection** onto the patch zones — exactly one per
    ///    zone, no unknown `zone_id`, no duplicates;
    /// 3. every `active` index is in range of its own grid.
    ///
    /// Loading does **not** validate (work-in-progress saves are allowed); call
    /// this before running a show, as the patch pipeline does.
    pub fn validate(&self) -> Result<(), crate::show::ShowError> {
        use crate::show::ShowError;

        self.patch.validate().map_err(ShowError::Patch)?;

        // Every zone show targets a real patch zone, with no duplicates.
        let mut seen = std::collections::HashSet::with_capacity(self.zones.len());
        for zs in &self.zones {
            if !self.patch.zones.iter().any(|z| z.id == zs.zone_id) {
                return Err(ShowError::UnknownZone { zone: zs.zone_id });
            }
            if !seen.insert(zs.zone_id) {
                return Err(ShowError::DuplicateZone { zone: zs.zone_id });
            }
            if let Some(active) = zs.active {
                if active >= zs.grid.len() {
                    return Err(ShowError::ActiveOutOfRange {
                        zone: zs.zone_id,
                        active,
                        grid_len: zs.grid.len(),
                    });
                }
            }
        }

        // Every patch zone is covered (bijection, not just injection).
        if seen.len() != self.patch.zones.len() {
            return Err(ShowError::MissingZones {
                expected: self.patch.zones.len(),
                found: seen.len(),
            });
        }

        Ok(())
    }
}

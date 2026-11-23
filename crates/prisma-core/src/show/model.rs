//! The Show document model (CLAUDE.md §4.3, docs/LIVE-MODEL-SPEC.md §3).
//!
//! The Show is the **performable document**: tracks of clips, each zone's
//! always-live state, and the master state, over an embedded Patch. It is what the
//! operator saves as "the show file" (`.prisma`); the Patch is the reusable wiring
//! it drives.
//!
//! # Tracks, not per-zone grids (LIVE-MODEL-SPEC §2–§3)
//!
//! A [`Track`] is a column of clips ([`Cell`]s) that plays at most one at a time
//! into a [`Destination`] (zones). Zones stay independent output regions: a
//! [`ZoneShow`] keeps only the zone's live state. Several tracks may reach one zone;
//! the renderer folds them in track order with each track's blend mode and opacity.
//! *Selected* (a UI state) is not modelled here at all — it never changes output.
//!
//! A new show gets one track per zone ([`Show::new`]); [`Show::home_track`] finds a
//! zone's own track. The grid UI and MIDI address tracks directly (by id, and by slot in
//! the controller's bank window).
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

use crate::transition::{Easing, TimeBase};
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
    Shader {
        effect_id: String,
        params: ParamSet,
        /// The cell's colour TABLE for effects that read a palette (the classic "Tabla de
        /// color"), linear-light RGB in order. A list, not parameters, so it can be long
        /// (the renderer caps it at 255). `None` = the effect's factory colours. Absent
        /// in older files, which keeps them loading unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        palette: Option<Vec<[f32; 3]>>,
    },
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
                palette: None,
            },
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
        }
    }
}

/// One clip of a [`Track`] (LIVE-MODEL-SPEC §2) — what a grid cell holds.
///
/// An **empty** cell has no layers ([`Cell::is_empty`]); launching it makes the
/// track output nothing. A populated cell holds exactly one [`Layer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The layer stack, stored inline (not by reference — CLAUDE.md §4.3 "cell
    /// storage decision"). M8 fills exactly one; empty = unassigned.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Transition into this clip, driven by the master clock. Non-zero overrides the
    /// track's [`TrackTransition`] (LIVE-MODEL-SPEC §3; honoured from phase 2).
    #[serde(default)]
    pub fade_time: Duration,
    /// The operator's label for this cell, shown on the grid. `None` = unnamed (the
    /// grid falls back to the effect's name). Older files without it load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_name: Option<String>,
    /// The cell's own colour on the grid, sRGB 0–255 (what the operator sees — a UI
    /// label, never light on the rig). `None` = the default panel grey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_color: Option<[u8; 3]>,
}

impl Cell {
    /// An unassigned cell: no content, no fade.
    pub fn empty() -> Cell {
        Cell {
            layers: Vec::new(),
            fade_time: Duration::ZERO,
            cell_name: None,
            cell_color: None,
        }
    }

    /// A single-effect cell (the M8 shape).
    pub fn single(layer: Layer) -> Cell {
        Cell {
            layers: vec![layer],
            fade_time: Duration::ZERO,
            cell_name: None,
            cell_color: None,
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
    /// "Temperatura de blanco" (fixtures with two whites): follow the colour asked for…
    #[serde(default = "default_true")]
    pub white_auto: bool,
    /// …or this fixed temperature, in kelvin.
    #[serde(default = "default_white_kelvin")]
    pub white_kelvin: f32,
}

fn default_white_kelvin() -> f32 {
    4000.0
}

impl Default for ZoneLive {
    fn default() -> Self {
        ZoneLive {
            dimmer: 0.0,
            color_filter: [1.0, 1.0, 1.0],
            speed_multiplier: 1.0,
            solo: false,
            blackout: false,
            white_auto: true,
            white_kelvin: default_white_kelvin(),
        }
    }
}

/// One zone's performable state: its always-live state (CLAUDE.md §4.3).
/// Addressed to a patch zone by [`zone_id`](ZoneShow::zone_id). Content lives in
/// [`Track`]s, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneShow {
    /// The patch [`Zone::id`](prisma_patch::Zone) this drives.
    pub zone_id: Uuid,
    /// Always-live dimmer/colour/speed/solo/blackout.
    #[serde(default)]
    pub live: ZoneLive,
}

impl ZoneShow {
    /// A zone show with default live state.
    pub fn new(zone_id: Uuid) -> ZoneShow {
        ZoneShow {
            zone_id,
            live: ZoneLive::default(),
        }
    }
}

/// Where a track's clips render (LIVE-MODEL-SPEC §3, §4.1): whole zones and/or groups.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    /// Patch zone ids. A track that reaches several zones gets one effect instance
    /// per zone, each at that zone's own `mapping_size`.
    #[serde(default)]
    pub zones: Vec<Uuid>,
    /// Patch group ids (phase 5). A group reaches every zone that owns one of its
    /// fixtures; there the track renders as for the whole zone and is masked to the
    /// group's cells. Absent in older files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Uuid>,
}

/// How a track reaches one zone ([`Destination::reach`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneReach {
    /// The whole zone (it is in the destination's zones).
    Whole,
    /// Only the cells of these groups' fixtures in the zone, in destination order.
    Groups(Vec<Uuid>),
}

impl Destination {
    /// A destination of exactly one zone.
    pub fn zone(zone: Uuid) -> Destination {
        Destination {
            zones: vec![zone],
            groups: Vec::new(),
        }
    }

    /// Whether this destination names `zone` directly (not through a group).
    pub fn reaches(&self, zone: Uuid) -> bool {
        self.zones.contains(&zone)
    }

    /// Whether it names nothing (no zone, no group).
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty() && self.groups.is_empty()
    }

    /// Every zone slot (`Patch::zones` position) this destination reaches, each once,
    /// and how: the named zones first, in destination order, then the zones reached
    /// only through groups, in patch order. A zone named directly is reached
    /// [`Whole`](ZoneReach::Whole) even when a group of it is named too (the zone wins,
    /// LIVE-MODEL-SPEC §8.5). Unknown zones and groups, and groups with no fixture in a
    /// zone, reach nothing there. The compositor and the clip controller both size a
    /// track's instances from this, so they agree.
    pub fn reach(&self, patch: &Patch) -> Vec<(usize, ZoneReach)> {
        let mut out: Vec<(usize, ZoneReach)> = Vec::new();
        for id in &self.zones {
            if let Some(slot) = patch.zones.iter().position(|z| z.id == *id) {
                if !out.iter().any(|(s, _)| *s == slot) {
                    out.push((slot, ZoneReach::Whole));
                }
            }
        }
        for (slot, zone) in patch.zones.iter().enumerate() {
            if out.iter().any(|(s, _)| *s == slot) {
                continue;
            }
            let groups: Vec<Uuid> = self
                .groups
                .iter()
                .filter(|g| {
                    patch.group(**g).is_some_and(|g| {
                        g.members
                            .iter()
                            .any(|m| patch.fixture(*m).is_some_and(|f| f.zone == zone.id))
                    })
                })
                .copied()
                .collect();
            if !groups.is_empty() {
                out.push((slot, ZoneReach::Groups(groups)));
            }
        }
        out
    }
}

/// The default transition a track uses between its clips (LIVE-MODEL-SPEC §3,
/// honoured from phase 2). `time == 0` is a hard cut.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackTransition {
    pub time: f64,
    pub base: TimeBase,
    pub easing: Easing,
}

impl Default for TrackTransition {
    fn default() -> Self {
        TrackTransition {
            time: 0.0,
            base: TimeBase::Seconds,
            easing: Easing::Linear,
        }
    }
}

/// How a launch on a track behaves (LIVE-MODEL-SPEC §5, honoured from phase 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    /// A launch plays the clip at once.
    #[default]
    Direct,
    /// A launch arms the clip; `/arm/go` plays every armed clip together.
    Arm,
}

/// A column of the grid (LIVE-MODEL-SPEC §2–§3): a list of clips playing at most
/// one at a time into its [`Destination`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    pub name: String,
    pub destination: Destination,
    /// How this track blends onto the tracks before it, in each zone it reaches.
    pub blend_mode: BlendMode,
    /// `0.0..=1.0`.
    pub opacity: f32,
    #[serde(default)]
    pub transition: TrackTransition,
    #[serde(default)]
    pub launch: LaunchMode,
    /// The track's clips. May be empty.
    #[serde(default)]
    pub clips: Vec<Cell>,
    /// Index of the running clip, or `None` (nothing playing). Validated in range by
    /// [`Show::validate`].
    #[serde(default)]
    pub active: Option<usize>,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// `BlendMode::LumaKey`: the luminance (linear) where the source starts to show…
    #[serde(default = "default_key_threshold")]
    pub key_threshold: f32,
    /// …and how far above it it reaches full opacity.
    #[serde(default = "default_key_softness")]
    pub key_softness: f32,
}

fn default_key_threshold() -> f32 {
    0.05
}

fn default_key_softness() -> f32 {
    0.1
}

impl Track {
    /// A track into `destination` with no clips: `Normal`, full opacity, hard cut,
    /// direct launch.
    pub fn new(name: impl Into<String>, destination: Destination) -> Track {
        Track {
            id: Uuid::new_v4(),
            name: name.into(),
            destination,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            transition: TrackTransition::default(),
            launch: LaunchMode::Direct,
            clips: Vec::new(),
            active: None,
            mute: false,
            solo: false,
            key_threshold: default_key_threshold(),
            key_softness: default_key_softness(),
        }
    }

    /// The running clip, if any (an `active` past the end counts as none).
    pub fn active_clip(&self) -> Option<&Cell> {
        self.clips.get(self.active?)
    }
}

/// A grid row's optional label (LIVE-MODEL-SPEC §3): launching scene `n` launches
/// clip `n` on every track that has one. UI-only — the engine never reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneLabel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// sRGB 0–255, like a cell's colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
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

/// One Art-Net device row, as professional consoles list it: a node (or one `BindIndex` group of a
/// node with more than 4 ports — the H802RA answers as 18 of them) and the universe of
/// each of its output ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtNetNode {
    pub id: Uuid,
    /// Short name ("H802RA"; classic "Nombre del dispositivo").
    pub name: String,
    /// Long name ("Oem ArtNet-Node").
    #[serde(default)]
    pub long_name: String,
    /// Unicast destination. `0.0.0.0` = not set yet (a manually added device before its
    /// IP is configured): a unicast node without an IP receives nothing.
    pub ip: Ipv4Addr,
    /// The universe (raw Art-Net port address) of each output port, in port order.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// Pre-ports format (inclusive ranges), read only to migrate; see
    /// [`NetworkConfig::normalize`]. Never written.
    #[serde(default, skip_serializing)]
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
        self.ports.contains(&port_address)
    }

    /// Whether this node has somewhere to send to: broadcast always does; unicast needs
    /// a configured IP.
    pub fn has_destination(&self) -> bool {
        !self.unicast || !self.ip.is_unspecified()
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
    /// Found by Scan (ArtPollReply): what the node reported about itself, for display.
    Scanned {
        #[serde(default)]
        mac: String,
        /// 1-based `BindIndex` of this row's group ("BindIdx1"…).
        #[serde(default = "default_bind")]
        bind_index: u8,
        /// ESTA manufacturer code.
        #[serde(default)]
        esta: u16,
        /// Art-Net OEM code.
        #[serde(default)]
        oem: u16,
        /// Firmware revision (`VersInfo`).
        #[serde(default)]
        firmware: u16,
    },
}

fn default_bind() -> u8 {
    1
}

impl NetworkConfig {
    /// Structural checks the device manager runs before applying: every range ordered
    /// and within 15 bits. (Unreachable IPs or uncovered universes are warnings in the
    /// UI, not errors — the show must still run.)
    pub fn validate(&self) -> Result<(), crate::show::ShowError> {
        for node in &self.nodes {
            if let Some(&u) = node.ports.iter().find(|&&u| u > UniverseRange::MAX) {
                return Err(crate::show::ShowError::InvalidNetwork(format!(
                    "node \"{}\": universe {u} is beyond Art-Net's 15-bit range",
                    node.name
                )));
            }
        }
        Ok(())
    }

    /// Migrate nodes saved in the pre-ports format (universe ranges) to one universe per
    /// port. A range the size of a real board becomes its ports; the legacy "every
    /// universe" target (`0..=32767`, from the old single-IP config) becomes exactly the
    /// patch's universes, `patch_universes`. Idempotent.
    pub fn normalize(&mut self, patch_universes: &[u16]) {
        const REAL_BOARD_MAX: usize = 4096;
        for node in &mut self.nodes {
            if node.universes.is_empty() {
                continue;
            }
            let total: usize = node
                .universes
                .iter()
                .map(|r| (r.last.saturating_sub(r.first)) as usize + 1)
                .sum();
            if node.ports.is_empty() {
                node.ports = if total <= REAL_BOARD_MAX {
                    node.universes
                        .iter()
                        .flat_map(|r| r.first..=r.last)
                        .collect()
                } else {
                    patch_universes.to_vec()
                };
            }
            node.universes.clear();
        }
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
    /// The grid's columns, in compositing order (first = bottom).
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Optional labels per grid row (index = clip row). May be shorter than the rows.
    #[serde(default)]
    pub scenes: Vec<SceneLabel>,
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
    /// A new show over `patch`, with one [`ZoneShow`] per patch zone, **one track
    /// per zone** (named after it, destination = that zone, no clips) and a fresh
    /// id (LIVE-MODEL-SPEC §8 phase 1).
    pub fn new(name: impl Into<String>, patch: Patch) -> Show {
        let zones = patch.zones.iter().map(|z| ZoneShow::new(z.id)).collect();
        let tracks = patch
            .zones
            .iter()
            .map(|z| Track::new(z.name.clone(), Destination::zone(z.id)))
            .collect();
        Show {
            id: Uuid::new_v4(),
            name: name.into(),
            patch,
            zones,
            tracks,
            scenes: Vec::new(),
            master: MasterState::default(),
            network: NetworkConfig::default(),
            embedded_mappings: None,
        }
    }

    /// The [`ZoneShow`] driving a given patch zone, if present.
    pub fn zone(&self, zone_id: Uuid) -> Option<&ZoneShow> {
        self.zones.iter().find(|z| z.zone_id == zone_id)
    }

    /// The track with this id, if present.
    pub fn track(&self, track_id: Uuid) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == track_id)
    }

    /// Mutable [`track`](Self::track).
    pub fn track_mut(&mut self, track_id: Uuid) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == track_id)
    }

    /// The zone's own track — the first whose destination is exactly that one zone
    /// (the one [`Show::new`] created for it). `None` if no track targets the zone alone.
    pub fn home_track(&self, zone_id: Uuid) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| t.destination.zones == [zone_id])
    }

    /// Mutable [`home_track`](Self::home_track).
    pub fn home_track_mut(&mut self, zone_id: Uuid) -> Option<&mut Track> {
        self.tracks
            .iter_mut()
            .find(|t| t.destination.zones == [zone_id])
    }

    /// Index in [`tracks`](Self::tracks) of the zone's [`home_track`](Self::home_track).
    pub fn home_track_index(&self, zone_id: Uuid) -> Option<usize> {
        self.tracks
            .iter()
            .position(|t| t.destination.zones == [zone_id])
    }

    /// Structural validation, the Show analogue of [`Patch::validate`]:
    ///
    /// 1. the embedded patch validates;
    /// 2. the zone shows are a **bijection** onto the patch zones — exactly one per
    ///    zone, no unknown `zone_id`, no duplicates;
    /// 3. track ids are unique, every destination zone is a patch zone, and every
    ///    `active` index is in range of its track's clips.
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
        }

        let mut track_ids = std::collections::HashSet::with_capacity(self.tracks.len());
        for t in &self.tracks {
            if !track_ids.insert(t.id) {
                return Err(ShowError::DuplicateTrack { track: t.id });
            }
            if let Some(&zone) = t
                .destination
                .zones
                .iter()
                .find(|&&zone| !self.patch.zones.iter().any(|z| z.id == zone))
            {
                return Err(ShowError::UnknownDestinationZone { track: t.id, zone });
            }
            if let Some(&group) = t
                .destination
                .groups
                .iter()
                .find(|&&g| self.patch.group(g).is_none())
            {
                return Err(ShowError::UnknownDestinationGroup { track: t.id, group });
            }
            if let Some(active) = t.active {
                if active >= t.clips.len() {
                    return Err(ShowError::ActiveOutOfRange {
                        track: t.id,
                        active,
                        clips: t.clips.len(),
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

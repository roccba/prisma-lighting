//! The demo show ("Archivo → Abrir show de demo", or `prisma-app --demo`): a rig and a
//! grid that walk through **every feature of the live model**, built in code so it keeps
//! loading when the format changes (`tests/demo_show.rs` loads and validates it). It is
//! also the show the UI end-to-end tests run on, so its ids are fixed.
//!
//! **Patch** (4 zones, all on the virtual sink — no Art-Net nodes):
//! - "Techo": 8 horizontal tubes of 60 RGB pixels (a 60 × 8 zone).
//! - "Cortina Norte", "Cortina Este", "Cortina Oeste": curtains of 16 × 24 pixels, each 16
//!   vertical strands of 24. Cortina Este is **RGBW** (whites extracted at the output) with
//!   a fixed warm white temperature (3000 K); Cortina Oeste runs at VEL 0.6.
//!
//! **Tracks**, in compositing order (9: one more than an APC40's bank window):
//!
//! | Track | Zones | Blend | Transition | What it shows |
//! |---|---|---|---|---|
//! | Techo · Fondo | Techo | Normal | 3 s, ease in-out | the field (Plasma pair, Fuego with its own 6 s fade) |
//! | Techo · Golpes | Techo | Add, 90 % | cut | figures, transparent background |
//! | Techo · Máscara | Techo | Mask | 2 s | the figure's alpha cuts everything below |
//! | Cortinas · Fondo | 3 curtains | Normal | 4 s, ease in-out | one track, shared params (Gotas pair) |
//! | Norte · Figuras | Norte | Normal (alpha over) | 1 beat, ease out | shapes over the field |
//! | Este · Key | Este | Luma key | 2 s | only the bright parts of the source |
//! | Oeste · Multiplicar | Oeste | Multiply | 2 beats | darkens / tints the field |
//! | Cortinas · Acentos | 3 curtains | Max, 80 % | 0.5 s | launch mode **Armar**: waits for GO |
//! | Strobe | every zone | Add, 85 % | cut | the strobe row |
//!
//! **Scenes** (rows): 1 Tranquilo, 2 Medio, 3 Fuerte, 4 Strobe, 5 Blancos; row 6 holds extra
//! clips to try by hand. Rows 1–3 and 5 have a clip on **every** track, so a scene defines
//! the whole look: where a track has nothing to add, its clip is **neutral** for its blend
//! (black on Add / Max / Luma key, white on Mask / Multiply, transparent on Normal) — a
//! scene launches only the tracks with a clip in its row, so an empty cell would leave the
//! previous clip running. Row 4 has only the strobe: it plays over whatever look runs and
//! the next scene turns it off. On the Armar track a scene **arms** its clip: GO plays it.
//! Pairs of the same effect with different values crossfade on their track's transition:
//! Plasma (Techo · Fondo, rows 1 → 2) and Gotas (Cortinas · Fondo, rows 2 → 3).
//! "Tranquilo" is running when the show opens. Every effect of the catalogue appears.
//!
//! Parameter values are written here in each effect's **real** units (BPM, seconds, …) and
//! stored normalized through the effect's own manifest, the way the panel stores them.

use std::time::Duration;

use glam::Vec3;
use prisma_core::show::{
    Cell, Destination, LaunchMode, Layer, NetworkConfig, ParamSet, SceneLabel, Show, Source, Track,
    TrackTransition,
};
use prisma_core::{Easing, TimeBase};
use prisma_patch::{
    BlendMode, ColorOrder, ComponentSet, Fixture, FixtureProfile, GammaCalibration, Patch, Pixel,
    WhiteProfile, Zone,
};
use prisma_render::manifest_by_id;
use uuid::Uuid;

/// The demo show's name.
pub const DEMO_NAME: &str = "Show de demo";

/// The zones' ids, in patch order: Techo, Cortina Norte, Cortina Este, Cortina Oeste.
pub const DEMO_ZONES: [Uuid; 4] = [
    Uuid::from_u128(0xDE30_0001),
    Uuid::from_u128(0xDE30_0002),
    Uuid::from_u128(0xDE30_0003),
    Uuid::from_u128(0xDE30_0004),
];

/// The tracks' ids, in grid order (see the table above).
pub const DEMO_TRACKS: [Uuid; 9] = [
    Uuid::from_u128(0xDE30_0101),
    Uuid::from_u128(0xDE30_0102),
    Uuid::from_u128(0xDE30_0103),
    Uuid::from_u128(0xDE30_0104),
    Uuid::from_u128(0xDE30_0105),
    Uuid::from_u128(0xDE30_0106),
    Uuid::from_u128(0xDE30_0107),
    Uuid::from_u128(0xDE30_0108),
    Uuid::from_u128(0xDE30_0109),
];

/// The named scene rows, in order.
pub const DEMO_SCENES: [&str; 5] = ["Tranquilo", "Medio", "Fuerte", "Strobe", "Blancos"];

/// The row of the strobe scene (only the Strobe track has a clip there).
pub const STROBE_ROW: usize = 3;

const TUBES: u32 = 8;
const TUBE_PX: u32 = 60;
const CURTAIN: [u32; 2] = [16, 24];
/// World rows: the ceiling at y 0–7, the curtains from y 10.
const CURTAIN_Y: u32 = 10;
/// World x of each curtain (a 6-cell gap between them).
const CURTAIN_X: [u32; 3] = [0, 22, 44];

/// The demo show. Pure: the same show (same ids) every call.
pub fn demo_show() -> Show {
    let patch = demo_patch();
    let mut show = Show::new(DEMO_NAME, patch);
    show.id = Uuid::from_u128(0xDE30_0000);
    for z in &mut show.zones {
        z.live.dimmer = 1.0;
    }
    // Cortina Este (RGBW): a fixed warm white instead of following the colour.
    show.zones[2].live.white_auto = false;
    show.zones[2].live.white_kelvin = 3000.0;
    // Cortina Oeste: its own speed, so the shared Cortinas · Fondo runs slower there.
    show.zones[3].live.speed_multiplier = 0.6;
    // Everything to the virtual sink. `initialized` keeps the app from migrating the
    // machine's legacy network config (real nodes) into this show.
    show.network = NetworkConfig {
        initialized: true,
        artnet_enabled: false,
        artsync: true,
        interface: None,
        nodes: Vec::new(),
    };
    show.tracks = demo_tracks();
    show.scenes = DEMO_SCENES
        .iter()
        .zip([
            [70, 120, 200],
            [60, 170, 110],
            [220, 120, 40],
            [235, 235, 235],
            [240, 200, 140],
        ])
        .map(|(name, color)| SceneLabel {
            name: Some((*name).to_string()),
            color: Some(color),
        })
        .collect();
    show
}

// --- Patch ------------------------------------------------------------------------------

/// Hands out DMX addresses: a pixel never straddles two universes, and each zone starts
/// on a fresh universe so the sink reads zone by zone.
struct Addr {
    universe: u16,
    channel: u16,
}

impl Addr {
    fn next(&mut self, width: u16) -> (u16, u16) {
        if self.channel + width - 1 > 512 {
            self.universe += 1;
            self.channel = 1;
        }
        let at = (self.universe, self.channel);
        self.channel += width;
        at
    }

    fn next_universe(&mut self) {
        if self.channel > 1 {
            self.universe += 1;
            self.channel = 1;
        }
    }
}

fn profile(components: ComponentSet) -> FixtureProfile {
    let name = match components {
        ComponentSet::Rgb => "Píxel RGB",
        ComponentSet::Rgbw => "Píxel RGBW",
        ComponentSet::Rgbww => "Píxel RGBWW",
    };
    FixtureProfile {
        name: name.into(),
        components,
        gamma: GammaCalibration::default(),
        white: WhiteProfile::default(),
    }
}

/// One fixture: `count` pixels from `origin` stepping by `step`.
#[allow(clippy::too_many_arguments)] // each is a distinct fact about the strand
fn strand(
    id: u128,
    name: String,
    zone: Uuid,
    components: ComponentSet,
    origin: [u32; 2],
    step: [u32; 2],
    count: u32,
    addr: &mut Addr,
) -> Fixture {
    let width = match components {
        ComponentSet::Rgb => 3,
        ComponentSet::Rgbw => 4,
        ComponentSet::Rgbww => 5,
    };
    let pixels = (0..count)
        .map(|i| {
            let (universe, start_channel) = addr.next(width);
            Pixel {
                index: i,
                position: Vec3::new(
                    (origin[0] + step[0] * i) as f32,
                    (origin[1] + step[1] * i) as f32,
                    0.0,
                ),
                universe,
                start_channel,
                color_order: ColorOrder::Rgb,
            }
        })
        .collect();
    Fixture {
        id: Uuid::from_u128(id),
        name,
        profile: profile(components),
        pixels,
        zone,
    }
}

fn zone(slot: usize, name: &str, color: [u8; 3], mapping: [u32; 2]) -> Zone {
    Zone {
        id: DEMO_ZONES[slot],
        name: name.into(),
        members: Vec::new(),
        bounds: None,
        z_order: slot as i32,
        blend_mode: BlendMode::Normal,
        mapping_size: [mapping[0], mapping[1], 1],
        display_color: color,
        world_transform: None,
    }
}

fn demo_patch() -> Patch {
    let mut addr = Addr {
        universe: 1,
        channel: 1,
    };
    let mut fixtures = Vec::new();
    for t in 0..TUBES {
        fixtures.push(strand(
            0xDE30_1000 + t as u128,
            format!("Tubo {}", t + 1),
            DEMO_ZONES[0],
            ComponentSet::Rgb,
            [0, t],
            [1, 0],
            TUBE_PX,
            &mut addr,
        ));
    }
    let curtains = [
        ("Norte", ComponentSet::Rgb),
        ("Este", ComponentSet::Rgbw),
        ("Oeste", ComponentSet::Rgb),
    ];
    for (k, (name, components)) in curtains.into_iter().enumerate() {
        addr.next_universe();
        for s in 0..CURTAIN[0] {
            fixtures.push(strand(
                0xDE30_2000 + (k as u128) * 0x100 + s as u128,
                format!("Cortina {name} · tira {}", s + 1),
                DEMO_ZONES[k + 1],
                components,
                [CURTAIN_X[k] + s, CURTAIN_Y],
                [0, 1],
                CURTAIN[1],
                &mut addr,
            ));
        }
    }
    let mut patch = Patch {
        id: Uuid::from_u128(0xDE30_0010),
        name: "Demo · techo y cortinas".into(),
        matrix_size: [1, 1, 1],
        fixtures,
        zones: vec![
            zone(0, "Techo", [70, 130, 220], [TUBE_PX, TUBES]),
            zone(1, "Cortina Norte", [220, 90, 80], CURTAIN),
            zone(2, "Cortina Este", [230, 180, 60], CURTAIN),
            zone(3, "Cortina Oeste", [110, 190, 100], CURTAIN),
        ],
    };
    patch.fill_zone_members_from_ownership();
    patch.fit_matrix_size();
    patch
}

// --- Tracks and clips -------------------------------------------------------------------

/// A clip of `effect` with `params` in the effect's real units (unlisted ones keep their
/// defaults), stored normalized through the manifest. Panics on an unknown effect or
/// parameter — the demo test builds the show, so a renamed parameter fails there.
fn clip(label: &str, effect: &str, params: &[(&str, f32)]) -> Cell {
    let manifest = manifest_by_id(effect).unwrap_or_else(|| panic!("demo: no effect {effect}"));
    let mut set = ParamSet::new();
    for (name, real) in params {
        let spec = manifest
            .params
            .iter()
            .find(|p| p.name == *name)
            .unwrap_or_else(|| panic!("demo: {effect} has no parameter {name}"));
        set.set(*name, spec.descriptor.to_norm(*real));
    }
    let mut cell = Cell::single(Layer::shader(effect, set));
    cell.cell_name = Some(label.into());
    cell
}

/// The same, with its own colour table (linear RGB).
fn clip_palette(label: &str, effect: &str, params: &[(&str, f32)], colors: &[[f32; 3]]) -> Cell {
    let mut cell = clip(label, effect, params);
    let Source::Shader { palette, .. } = &mut cell.layers[0].source;
    *palette = Some(colors.to_vec());
    cell
}

/// The grid colour of the neutral clips: they are there to define the scene, not to be
/// played by hand.
const NEUTRAL_COLOR: [u8; 3] = [64, 64, 64];

fn neutral(mut cell: Cell) -> Cell {
    cell.cell_color = Some(NEUTRAL_COLOR);
    cell
}

/// Neutral on Add, Max and Luma key: opaque black adds nothing, never wins a max, and keys
/// itself out.
fn black(label: &str) -> Cell {
    neutral(clip(label, "solid", &[("r", 0.0), ("g", 0.0), ("b", 0.0)]))
}

/// Neutral on Mask and Multiply: opaque white lets everything through unchanged.
fn white(label: &str) -> Cell {
    neutral(clip(label, "solid", &[("r", 1.0), ("g", 1.0), ("b", 1.0)]))
}

/// Neutral on Normal: a fully transparent picture (a black gradient on a transparent
/// background has alpha 0 everywhere).
fn clear(label: &str) -> Cell {
    neutral(clip(
        label,
        "gradient",
        &[("start", 0.0), ("end", 0.0), ("background", TRANSPARENT)],
    ))
}

/// `background` real values.
const TRANSPARENT: f32 = 0.0;
const BLACK_BG: f32 = 1.0;

#[allow(clippy::too_many_arguments)] // each is one setting of the track
fn track(
    slot: usize,
    name: &str,
    zones: &[usize],
    blend: BlendMode,
    opacity: f32,
    time: f64,
    base: TimeBase,
    easing: Easing,
) -> Track {
    let mut t = Track::new(name, Destination::zone(DEMO_ZONES[zones[0]]));
    t.id = DEMO_TRACKS[slot];
    t.destination.zones = zones.iter().map(|&z| DEMO_ZONES[z]).collect();
    t.blend_mode = blend;
    t.opacity = opacity;
    t.transition = TrackTransition { time, base, easing };
    t.active = Some(0);
    t
}

const S: TimeBase = TimeBase::Seconds;
const BEATS: TimeBase = TimeBase::Beats;
const CURTAINS: [usize; 3] = [1, 2, 3];
const ALL: [usize; 4] = [0, 1, 2, 3];

fn demo_tracks() -> Vec<Track> {
    use BlendMode::*;
    use Easing::*;

    // Rows: 0 Tranquilo · 1 Medio · 2 Fuerte · 3 Strobe · 4 Blancos · 5 extras.

    let mut fondo = track(0, "Techo · Fondo", &[0], Normal, 1.0, 3.0, S, EaseInOut);
    let mut fuego = clip("Fuego", "fire", &[("intensity", 1.4), ("speed", 2.0)]);
    fuego.fade_time = Duration::from_secs(6); // its own fade wins over the track's 3 s
    fondo.clips = vec![
        clip("Plasma lento", "plasma", &[("scale", 4.0), ("speed", 0.3)]),
        clip(
            "Plasma rápido",
            "plasma",
            &[("scale", 12.0), ("speed", 1.8)],
        ),
        fuego,
        Cell::empty(),
        clip(
            "Blanco cálido",
            "solid",
            &[("r", 1.0), ("g", 0.62), ("b", 0.3)],
        ),
        clip(
            "Ruido",
            "noise",
            &[("scale", 8.0), ("speed", 0.6), ("background", BLACK_BG)],
        ),
    ];

    let mut golpes = track(1, "Techo · Golpes", &[0], Add, 0.9, 0.0, S, Linear);
    golpes.clips = vec![
        clip(
            "Chispas suaves",
            "sparkle",
            &[
                ("density", 4.0),
                ("speed", 0.6),
                ("background", TRANSPARENT),
            ],
        ),
        clip(
            "Metaballs",
            "metaballs",
            &[
                ("count", 3.0),
                ("size", 0.35),
                ("speed", 0.8),
                ("r", 1.0),
                ("g", 0.45),
                ("b", 0.1),
                ("background", TRANSPARENT),
            ],
        ),
        clip(
            "Explosiones",
            "explosions",
            &[
                ("rate", 2.5),
                ("size", 0.6),
                ("r", 1.0),
                ("g", 0.9),
                ("b", 0.6),
                ("background", TRANSPARENT),
            ],
        ),
        Cell::empty(),
        black("Sin golpes"),
        clip(
            "Chispas densas",
            "sparkle",
            &[
                ("density", 30.0),
                ("speed", 2.5),
                ("background", TRANSPARENT),
            ],
        ),
    ];

    // Mask: the figure's alpha gates the Techo stack below (fondo + golpes).
    let mut mascara = track(2, "Techo · Máscara", &[0], Mask, 1.0, 2.0, S, Linear);
    mascara.clips = vec![
        white("Sin máscara"),
        clip(
            "Ondas (máscara)",
            "wave",
            &[
                ("frequency", 3.0),
                ("speed", 1.0),
                ("background", TRANSPARENT),
            ],
        ),
        clip_palette(
            "Barras (máscara)",
            "bounce",
            &[
                ("speed", 140.0),
                ("count", 5.0),
                ("size", 0.25),
                ("direction", 0.0),
                ("background", TRANSPARENT),
            ],
            &[[1.0, 1.0, 1.0]],
        ),
        Cell::empty(),
        white("Sin máscara"),
        clip(
            "Círculo (máscara)",
            "simple_shape",
            &[
                ("shape", 0.0),
                ("size", 0.9),
                ("border", 0.4),
                ("background", TRANSPARENT),
            ],
        ),
    ];

    // One track on three curtains: shared params, each curtain at its own speed.
    let mut cortinas = track(
        3,
        "Cortinas · Fondo",
        &CURTAINS,
        Normal,
        1.0,
        4.0,
        S,
        EaseInOut,
    );
    cortinas.clips = vec![
        clip_palette(
            "Scroll lento",
            "color_scroll",
            // mode 5 = "Lineal · Inferior": down the curtains.
            &[("bpm", 60.0), ("mode", 5.0)],
            &[[0.0, 0.05, 0.5], [0.25, 0.0, 0.45], [0.0, 0.35, 0.45]],
        ),
        clip(
            "Gotas suaves",
            "drops",
            &[
                ("speed", 0.8),
                ("density", 6.0),
                ("length", 0.5),
                ("r", 0.2),
                ("g", 0.6),
                ("b", 1.0),
                ("background", BLACK_BG),
            ],
        ),
        clip(
            "Gotas tormenta",
            "drops",
            &[
                ("speed", 3.0),
                ("density", 28.0),
                ("length", 0.25),
                ("r", 1.0),
                ("g", 1.0),
                ("b", 1.0),
                ("background", BLACK_BG),
            ],
        ),
        Cell::empty(),
        // Pure white: on the RGBW curtain it goes to the white channel (Extract).
        clip("Blanco", "solid", &[("r", 1.0), ("g", 1.0), ("b", 1.0)]),
        clip("Ondas", "wave", &[("frequency", 2.0), ("speed", 1.0)]),
    ];

    // Shapes alpha-over the field, on Norte only.
    let mut norte = track(4, "Norte · Figuras", &[1], Normal, 1.0, 1.0, BEATS, EaseOut);
    norte.clips = vec![
        clip(
            "Círculo",
            "simple_shape",
            &[
                ("shape", 0.0),
                ("size", 0.45),
                ("border", 0.3),
                ("r", 1.0),
                ("g", 0.7),
                ("b", 0.2),
                ("background", TRANSPARENT),
            ],
        ),
        clip_palette(
            "Rebote",
            "bounce",
            &[("speed", 90.0), ("count", 4.0), ("background", TRANSPARENT)],
            &[[1.0, 0.2, 0.0], [1.0, 1.0, 1.0]],
        ),
        clip(
            "Estrella",
            "simple_shape",
            &[
                ("shape", 5.0),
                ("size", 0.8),
                ("r", 1.0),
                ("g", 1.0),
                ("b", 0.3),
                ("background", TRANSPARENT),
            ],
        ),
        Cell::empty(),
        clear("Sin figuras"),
        clip(
            "Corazón",
            "simple_shape",
            &[
                ("shape", 6.0),
                ("size", 0.7),
                ("r", 1.0),
                ("g", 0.1),
                ("b", 0.3),
                ("background", TRANSPARENT),
            ],
        ),
    ];

    // Luma key on the RGBW curtain: only the bright parts of the source show.
    let mut este = track(5, "Este · Key", &[2], LumaKey, 1.0, 2.0, S, Linear);
    este.key_threshold = 0.3;
    este.key_softness = 0.15;
    este.clips = vec![
        clip(
            "Ruido (key)",
            "noise",
            &[("scale", 6.0), ("speed", 0.3), ("background", BLACK_BG)],
        ),
        clip(
            "Fuego (key)",
            "fire",
            &[("intensity", 1.2), ("speed", 1.5), ("background", BLACK_BG)],
        ),
        clip(
            "Onda radial (key)",
            "radial",
            &[("frequency", 4.0), ("speed", 2.0), ("background", BLACK_BG)],
        ),
        Cell::empty(),
        black("Sin key"),
        clip(
            "Plasma (key)",
            "plasma",
            &[("scale", 8.0), ("speed", 1.0), ("background", BLACK_BG)],
        ),
    ];

    // Multiply on Oeste: darkens and tints the field.
    let mut oeste = track(
        6,
        "Oeste · Multiplicar",
        &[3],
        Multiply,
        1.0,
        2.0,
        BEATS,
        Linear,
    );
    oeste.clips = vec![
        clip(
            "Degradado",
            "gradient",
            &[("start", 0.15), ("end", 1.0), ("background", BLACK_BG)],
        ),
        clip(
            "Ondas oscuras",
            "wave",
            &[("frequency", 3.0), ("speed", 0.8), ("background", BLACK_BG)],
        ),
        clip_palette(
            "Teñir",
            "color_change",
            &[("speed", 60.0), ("fade", 1.0)],
            &[[1.0, 0.3, 0.1], [0.3, 1.0, 0.4], [0.3, 0.4, 1.0]],
        ),
        Cell::empty(),
        white("Sin multiplicar"),
        clip(
            "Metaballs (multiplicar)",
            "metaballs",
            &[
                ("count", 5.0),
                ("size", 0.5),
                ("speed", 0.5),
                ("background", BLACK_BG),
            ],
        ),
    ];

    // Launch mode Armar: a scene (or a click) arms the clip, GO plays it.
    let mut acentos = track(7, "Cortinas · Acentos", &CURTAINS, Max, 0.8, 0.5, S, Linear);
    acentos.launch = LaunchMode::Arm;
    acentos.clips = vec![
        black("Sin acentos"),
        clip(
            "Chispas",
            "sparkle",
            &[("density", 12.0), ("speed", 2.0), ("background", BLACK_BG)],
        ),
        clip_palette(
            "Barrido",
            "color_scroll",
            &[("bpm", 480.0), ("mode", 3.0)],
            &[[1.0, 0.0, 0.2], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
        ),
        Cell::empty(),
        black("Sin acentos"),
        clip(
            "Explosiones",
            "explosions",
            &[("rate", 1.5), ("particles", 30.0), ("background", BLACK_BG)],
        ),
    ];

    // Every zone. Rows 1–3 and 5: no strobe (black adds nothing), so a scene turns it off.
    let mut strobe = track(8, "Strobe", &ALL, Add, 0.85, 0.0, S, Linear);
    strobe.clips = vec![
        black("Sin strobe"),
        black("Sin strobe"),
        black("Sin strobe"),
        clip_palette(
            "Strobe blanco",
            "pulse",
            &[("speed", 600.0), ("ratio", 20.0), ("fade", 0.0)],
            &[[1.0, 1.0, 1.0]],
        ),
        black("Sin strobe"),
        clip_palette(
            "Flash lento",
            "pulse",
            &[("speed", 120.0), ("ratio", 30.0), ("fade", 1.0)],
            &[[1.0, 1.0, 1.0]],
        ),
    ];

    vec![
        fondo, golpes, mascara, cortinas, norte, este, oeste, acentos, strobe,
    ]
}

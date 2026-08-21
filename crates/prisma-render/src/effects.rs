//! Built-in effect library (CLAUDE.md §5.3): procedural WGSL effects, each a
//! manifest + a shader. Effects are **data**, not special-cased engine code —
//! adding one is a manifest and a `.wgsl`, never a compositor change (§7).
//!
//! S1 ships one effect (a gradient) to establish the tree→uniform→pixel seam;
//! S2+ add plasma, wave/noise, etc. behind the same [`EffectManifest`] contract.

use crate::effect::{ColorChannel, EffectManifest, ParamRole, ParamSpec};
use prisma_core::{Curve, ParamDescriptor, RealRange};

/// WGSL source for the horizontal grayscale gradient, from the shared shader
/// library (§7 `shaders/`).
const GRADIENT_WGSL: &str = include_str!("../../../shaders/gradient.wgsl");
const SOLID_WGSL: &str = include_str!("../../../shaders/solid.wgsl");
const PLASMA_WGSL: &str = include_str!("../../../shaders/plasma.wgsl");
const WAVE_WGSL: &str = include_str!("../../../shaders/wave.wgsl");
const NOISE_WGSL: &str = include_str!("../../../shaders/noise.wgsl");
const FIRE_WGSL: &str = include_str!("../../../shaders/fire.wgsl");
const RADIAL_WGSL: &str = include_str!("../../../shaders/radial.wgsl");
const SPARKLE_WGSL: &str = include_str!("../../../shaders/sparkle.wgsl");
const COLOR_SCROLL_WGSL: &str = include_str!("../../../shaders/color_scroll.wgsl");

/// The fixed number of palette slots a palette effect carries. Slots beyond the live
/// `count` are packed but unread by the shader. Raising this means more uniform slots
/// and more shader lines — deliberately capped for a first cut (8 is generous for a
/// scroll). Kept in lockstep with `color_scroll.wgsl`'s `pal` array and `PALETTE_SLOTS`.
const PALETTE_SLOTS: usize = 8;

/// Static per-slot channel names (`c{i}r/g/b`) — the trailing path segments and the
/// uniform fields, in the order `color_scroll.wgsl` reads them. `&'static` so a
/// [`ParamSpec`] can borrow them without leaking.
const PALETTE_CHANNELS: [[&str; 3]; PALETTE_SLOTS] = [
    ["c0r", "c0g", "c0b"],
    ["c1r", "c1g", "c1b"],
    ["c2r", "c2g", "c2b"],
    ["c3r", "c3g", "c3b"],
    ["c4r", "c4g", "c4b"],
    ["c5r", "c5g", "c5b"],
    ["c6r", "c6g", "c6b"],
    ["c7r", "c7g", "c7b"],
];

/// The default colour-scroll palette: the saturated rainbow. Values are
/// LINEAR-light `0..1` — for fully-saturated sRGB primaries the sRGB curve maps 0→0
/// and 255→1 unchanged, so these read the same in both spaces. The trailing two slots
/// are inactive at the default `count` (6) but seeded so raising the count yields a
/// usable colour rather than black.
const PALETTE_DEFAULTS: [[f32; 3]; PALETTE_SLOTS] = [
    [1.0, 0.0, 0.0], // red
    [1.0, 1.0, 0.0], // yellow
    [0.0, 1.0, 0.0], // green
    [0.0, 1.0, 1.0], // cyan
    [0.0, 0.0, 1.0], // blue
    [1.0, 0.0, 1.0], // magenta
    [1.0, 1.0, 1.0], // white (inactive at default count)
    [1.0, 0.0, 0.0], // red   (inactive at default count)
];

/// The 24 palette colour-channel params (8 slots × r/g/b), tagged with their slot index
/// and channel so the panel collects them into one palette editor. Seeded with the
/// Rainbow defaults; the value model is plain linear-light scalars (§2.3), so a
/// palette persists and MIDI-maps like any other parameter.
fn palette_color_params() -> Vec<ParamSpec> {
    let mut out = Vec::with_capacity(PALETTE_SLOTS * 3);
    for slot in 0..PALETTE_SLOTS {
        let channels = [ColorChannel::R, ColorChannel::G, ColorChannel::B];
        for (ch_idx, channel) in channels.into_iter().enumerate() {
            let name = PALETTE_CHANNELS[slot][ch_idx];
            let default = PALETTE_DEFAULTS[slot][ch_idx];
            out.push(ParamSpec::palette(
                name,
                ParamDescriptor::unit_level(name, default),
                slot as u8,
                channel,
            ));
        }
    }
    out
}

/// Scrolling bands of a user palette (§5.3 "Scroll de color") — the
/// first effect to declare a **palette** and a **direction**, so it lights up the
/// palette editor and the 8-way rose. `speed` is scroll rate, `width` the number of
/// palette repetitions across the scroll axis, `offset` a static phase, `direction`
/// the 8-way axis, and `count`/`c0..c7` the palette itself. Param order here **is**
/// the uniform layout the shader reads.
pub fn color_scroll() -> EffectManifest {
    let mut params = vec![
        ParamSpec::scalar(
            "speed",
            ParamDescriptor::level(
                "effect.color_scroll.speed",
                RealRange { min: 0.0, max: 4.0 },
                Curve::Linear,
                // Mid-low so a freshly-assigned scroll drifts out of the box.
                0.3,
            ),
        ),
        ParamSpec::scalar(
            "width",
            ParamDescriptor::level(
                "effect.color_scroll.width",
                RealRange { min: 0.5, max: 8.0 },
                Curve::Linear,
                // ~2 repetitions across the axis: bands wide enough to read.
                0.2,
            ),
        ),
        ParamSpec::scalar(
            "offset",
            ParamDescriptor::unit_level("effect.color_scroll.offset", 0.0),
        ),
        ParamSpec::direction(
            "direction",
            // Default 0.0 → rose index 0 → Right.
            ParamDescriptor::unit_level("effect.color_scroll.direction", 0.0),
        ),
        ParamSpec::palette_count(
            "count",
            ParamDescriptor::level(
                "effect.color_scroll.count",
                RealRange {
                    min: 1.0,
                    max: PALETTE_SLOTS as f32,
                },
                Curve::Linear,
                // Default 6 live colours (the rainbow): (6-1)/(8-1).
                5.0 / 7.0,
            ),
        ),
    ];
    params.extend(palette_color_params());
    EffectManifest {
        effect_id: "color_scroll",
        params,
        shader_source: COLOR_SCROLL_WGSL,
    }
}

/// Sum-of-sines plasma. `scale` sets the spatial frequency of the field and
/// `speed` how fast it evolves against show time. Param order here **is** the
/// uniform layout the shader reads.
pub fn plasma() -> EffectManifest {
    EffectManifest {
        effect_id: "plasma",
        params: vec![
            ParamSpec::scalar(
                "scale",
                ParamDescriptor::level(
                    "effect.plasma.scale",
                    RealRange {
                        min: 1.0,
                        max: 20.0,
                    },
                    Curve::Linear,
                    0.25,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.plasma.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    0.25,
                ),
            ),
        ],
        shader_source: PLASMA_WGSL,
    }
}

/// A travelling sine along X. `frequency` is cycles across the matrix width,
/// `speed` cycles per second against show time.
pub fn wave() -> EffectManifest {
    EffectManifest {
        effect_id: "wave",
        params: vec![
            ParamSpec::scalar(
                "frequency",
                ParamDescriptor::level(
                    "effect.wave.frequency",
                    RealRange {
                        min: 0.0,
                        max: 10.0,
                    },
                    Curve::Linear,
                    0.1,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.wave.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    // Default to mid-slider so a freshly-assigned wave animates out of the
                    // box (a 0 default looked broken — the effect sat frozen).
                    0.5,
                ),
            ),
        ],
        shader_source: WAVE_WGSL,
    }
}

/// Animated fractal value noise (§5.3 "Noise", the Perlin/Simplex entry). `scale`
/// sets the base spatial frequency of the field and `speed` how fast it evolves
/// against show time. Order here **is** the uniform layout the shader reads.
pub fn noise() -> EffectManifest {
    EffectManifest {
        effect_id: "noise",
        params: vec![
            ParamSpec::scalar(
                "scale",
                ParamDescriptor::level(
                    "effect.noise.scale",
                    RealRange {
                        min: 1.0,
                        max: 20.0,
                    },
                    Curve::Linear,
                    0.25,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.noise.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    0.25,
                ),
            ),
        ],
        shader_source: NOISE_WGSL,
    }
}

/// Rising flames with a built-in fire colour ramp (§5.3 "Fire"). `intensity` scales
/// the overall heat and `speed` the rise rate. The ramp is intrinsic to the effect,
/// so unlike the palette generators fire needs no external colours.
pub fn fire() -> EffectManifest {
    EffectManifest {
        effect_id: "fire",
        params: vec![
            ParamSpec::scalar(
                "intensity",
                ParamDescriptor::level(
                    "effect.fire.intensity",
                    RealRange { min: 0.0, max: 2.0 },
                    Curve::Linear,
                    // Default to ~1.0 so a freshly-assigned fire actually burns.
                    0.5,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.fire.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    // ~1.0 unit/s so the flames move out of the box.
                    0.25,
                ),
            ),
        ],
        shader_source: FIRE_WGSL,
    }
}

/// Concentric sine rings expanding from the matrix centre (§5.3 "Onda / radial").
/// `frequency` is rings across the half-width, `speed` how fast they travel outward.
pub fn radial() -> EffectManifest {
    EffectManifest {
        effect_id: "radial",
        params: vec![
            ParamSpec::scalar(
                "frequency",
                ParamDescriptor::level(
                    "effect.radial.frequency",
                    RealRange {
                        min: 0.0,
                        max: 10.0,
                    },
                    Curve::Linear,
                    0.3,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.radial.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    // Mid-slider so the rings animate immediately.
                    0.5,
                ),
            ),
        ],
        shader_source: RADIAL_WGSL,
    }
}

/// Scattered twinkling points (§5.3 "Sparkle / twinkle"). `density` controls how
/// many sparkles (lattice resolution + active fraction) and `speed` the twinkle rate.
pub fn sparkle() -> EffectManifest {
    EffectManifest {
        effect_id: "sparkle",
        params: vec![
            ParamSpec::scalar(
                "density",
                ParamDescriptor::level(
                    "effect.sparkle.density",
                    RealRange {
                        min: 1.0,
                        max: 40.0,
                    },
                    Curve::Linear,
                    0.25,
                ),
            ),
            ParamSpec::scalar(
                "speed",
                ParamDescriptor::level(
                    "effect.sparkle.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    0.5,
                ),
            ),
        ],
        shader_source: SPARKLE_WGSL,
    }
}

/// A flat linear-light color from three unit-range parameters `r`, `g`, `b`. The
/// simplest effect (§5.3 "solid color") and a clean, known building block for the
/// compositor tests. Param order here is the uniform layout the shader reads.
pub fn solid() -> EffectManifest {
    EffectManifest {
        effect_id: "solid",
        // The three channels share one explicit colour group, so the panel presents
        // them as a single 255-space picker instead of three raw sliders (§3.5). The
        // values stay linear-light `0..1` in the tree/uniform — only the UI converts.
        params: vec![
            ParamSpec::color(
                "r",
                ParamDescriptor::unit_level("effect.solid.r", 0.0),
                "color",
                ColorChannel::R,
            ),
            ParamSpec::color(
                "g",
                ParamDescriptor::unit_level("effect.solid.g", 0.0),
                "color",
                ColorChannel::G,
            ),
            ParamSpec::color(
                "b",
                ParamDescriptor::unit_level("effect.solid.b", 0.0),
                "color",
                ColorChannel::B,
            ),
        ],
        shader_source: SOLID_WGSL,
    }
}

/// A horizontal grayscale linear gradient. Two unit-range parameters, `start` and
/// `end`, set the grayscale value at the left and right edges; the shader
/// interpolates between them per column. Their order here **is** the uniform
/// layout the shader reads (see [`crate::effect`]).
pub fn gradient() -> EffectManifest {
    EffectManifest {
        effect_id: "gradient",
        // Grayscale edges (scalars), not colours — plain sliders. (When a colour
        // gradient effect arrives, its two ends become two distinct colour groups.)
        params: vec![
            ParamSpec::scalar(
                "start",
                ParamDescriptor::unit_level("effect.gradient.start", 0.0),
            ),
            ParamSpec::scalar(
                "end",
                ParamDescriptor::unit_level("effect.gradient.end", 1.0),
            ),
        ],
        shader_source: GRADIENT_WGSL,
    }
}

/// One entry in the effect catalogue: an effect the picker can offer, with a
/// human-readable name and a category to group it under. The UI
/// builds the picker from this list, never a hardcoded one (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectListing {
    /// The `effect_id` a [`Source::Shader`](prisma_core::show::Source) stores.
    pub id: &'static str,
    /// Display name for the picker.
    pub name: &'static str,
    /// Category to group under (M8 ships generators only).
    pub category: &'static str,
}

/// The built-in effects available to assign to a scene cell (M8 step 4). These are
/// the M5 GPU effects; audio-reactive (S2L/M2L) and scripting (MAS) categories come
/// in later phases (§5.3). **Noise is a known M5 gap** (its shader is unwritten) —
/// it appears here the day its `fn` and `manifest_by_id` arm exist, no UI change.
pub fn catalogue() -> Vec<EffectListing> {
    vec![
        EffectListing {
            id: "solid",
            name: "Color",
            category: "Generadores",
        },
        EffectListing {
            id: "gradient",
            name: "Gradient",
            category: "Generadores",
        },
        EffectListing {
            id: "plasma",
            name: "Plasma",
            category: "Generadores",
        },
        EffectListing {
            id: "wave",
            name: "Wave",
            category: "Generadores",
        },
        EffectListing {
            id: "radial",
            name: "Onda radial",
            category: "Generadores",
        },
        EffectListing {
            id: "noise",
            name: "Ruido",
            category: "Generadores",
        },
        EffectListing {
            id: "fire",
            name: "Fuego",
            category: "Generadores",
        },
        EffectListing {
            id: "sparkle",
            name: "Chispas",
            category: "Generadores",
        },
        EffectListing {
            id: "color_scroll",
            name: "Scroll de color",
            category: "Generadores",
        },
    ]
}

/// One parameter of an effect, flattened for the UI's manifest-driven parameter
/// panel (M8 step 5, §3.5): its key, real range, and normalized default. The panel
/// builds itself from this list — never a per-effect hardcoded UI (§5.3). `name` is
/// the trailing path segment the value is addressed by (`/zone/{id}/layer/0/param/
/// {name}`); the real range lets the panel show a value the operator can read.
#[derive(Debug, Clone)]
pub struct EffectParam {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default_norm: f32,
    /// Presentation hint (scalar / colour channel / direction) — which control the
    /// panel draws for this param (§3.5). The value model is unchanged regardless.
    pub role: ParamRole,
}

/// The parameters an effect exposes, in manifest (= uniform) order, for `effect_id`;
/// an empty vec if the effect is unknown. This is the manifest surface the parameter
/// panel walks so it never hardcodes an effect's controls (§5.3).
pub fn effect_params(id: &str) -> Vec<EffectParam> {
    manifest_by_id(id)
        .map(|m| {
            m.params
                .into_iter()
                .map(|p| EffectParam {
                    name: p.name.to_string(),
                    min: p.descriptor.range.min,
                    max: p.descriptor.range.max,
                    default_norm: p.descriptor.default_norm,
                    role: p.role,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The manifest for a built-in effect by its `effect_id`, or `None` if unknown.
///
/// This is the catalogue lookup the show model addresses effects through: a
/// [`Source::Shader`](prisma_core::show::Source) stores an `effect_id` string, and
/// the engine resolves it here when it builds a zone's [`crate::EffectInstance`].
/// Adding an effect means adding its `fn` above and one arm here — no compositor or
/// UI change (§5.3 / §7: effects are data).
pub fn manifest_by_id(id: &str) -> Option<EffectManifest> {
    match id {
        "solid" => Some(solid()),
        "gradient" => Some(gradient()),
        "plasma" => Some(plasma()),
        "wave" => Some(wave()),
        "noise" => Some(noise()),
        "fire" => Some(fire()),
        "radial" => Some(radial()),
        "sparkle" => Some(sparkle()),
        "color_scroll" => Some(color_scroll()),
        _ => None,
    }
}

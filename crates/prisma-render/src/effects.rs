//! Built-in effect library (CLAUDE.md §5.3): procedural WGSL effects, each a
//! manifest + a shader. Effects are **data**, not special-cased engine code —
//! adding one is a manifest and a `.wgsl`, never a compositor change (§7).
//!
//! S1 ships one effect (a gradient) to establish the tree→uniform→pixel seam;
//! S2+ add plasma, wave/noise, etc. behind the same [`EffectManifest`] contract.

use crate::effect::{EffectManifest, ParamSpec};
use prisma_core::{Curve, ParamDescriptor, RealRange};

/// WGSL source for the horizontal grayscale gradient, from the shared shader
/// library (§7 `shaders/`).
const GRADIENT_WGSL: &str = include_str!("../../../shaders/gradient.wgsl");
const SOLID_WGSL: &str = include_str!("../../../shaders/solid.wgsl");
const PLASMA_WGSL: &str = include_str!("../../../shaders/plasma.wgsl");
const WAVE_WGSL: &str = include_str!("../../../shaders/wave.wgsl");

/// Sum-of-sines plasma. `scale` sets the spatial frequency of the field and
/// `speed` how fast it evolves against show time. Param order here **is** the
/// uniform layout the shader reads.
pub fn plasma() -> EffectManifest {
    EffectManifest {
        effect_id: "plasma",
        params: vec![
            ParamSpec {
                name: "scale",
                descriptor: ParamDescriptor::level(
                    "effect.plasma.scale",
                    RealRange {
                        min: 1.0,
                        max: 20.0,
                    },
                    Curve::Linear,
                    0.25,
                ),
            },
            ParamSpec {
                name: "speed",
                descriptor: ParamDescriptor::level(
                    "effect.plasma.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    0.25,
                ),
            },
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
            ParamSpec {
                name: "frequency",
                descriptor: ParamDescriptor::level(
                    "effect.wave.frequency",
                    RealRange {
                        min: 0.0,
                        max: 10.0,
                    },
                    Curve::Linear,
                    0.1,
                ),
            },
            ParamSpec {
                name: "speed",
                descriptor: ParamDescriptor::level(
                    "effect.wave.speed",
                    RealRange { min: 0.0, max: 4.0 },
                    Curve::Linear,
                    0.0,
                ),
            },
        ],
        shader_source: WAVE_WGSL,
    }
}

/// A flat linear-light color from three unit-range parameters `r`, `g`, `b`. The
/// simplest effect (§5.3 "solid color") and a clean, known building block for the
/// compositor tests. Param order here is the uniform layout the shader reads.
pub fn solid() -> EffectManifest {
    EffectManifest {
        effect_id: "solid",
        params: vec![
            ParamSpec {
                name: "r",
                descriptor: ParamDescriptor::unit_level("effect.solid.r", 0.0),
            },
            ParamSpec {
                name: "g",
                descriptor: ParamDescriptor::unit_level("effect.solid.g", 0.0),
            },
            ParamSpec {
                name: "b",
                descriptor: ParamDescriptor::unit_level("effect.solid.b", 0.0),
            },
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
        params: vec![
            ParamSpec {
                name: "start",
                descriptor: ParamDescriptor::unit_level("effect.gradient.start", 0.0),
            },
            ParamSpec {
                name: "end",
                descriptor: ParamDescriptor::unit_level("effect.gradient.end", 1.0),
            },
        ],
        shader_source: GRADIENT_WGSL,
    }
}

/// One entry in the effect catalogue: an effect the picker can offer, with a
/// human-readable name and a category to group it under (screenshot 14). The UI
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
        _ => None,
    }
}

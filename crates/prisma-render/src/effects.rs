//! Built-in effect library (CLAUDE.md §5.3): procedural WGSL effects, each a
//! manifest + a shader. Effects are **data**, not special-cased engine code —
//! adding one is a manifest and a `.wgsl`, never a compositor change (§7).
//!
//! S1 ships one effect (a gradient) to establish the tree→uniform→pixel seam;
//! S2+ add plasma, wave/noise, etc. behind the same [`EffectManifest`] contract.

use crate::effect::{ColorChannel, EffectManifest, ParamRole, ParamSpec, Requires};
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
const SIMPLE_SHAPE_WGSL: &str = include_str!("../../../shaders/simple_shape.wgsl");
const METABALLS_WGSL: &str = include_str!("../../../shaders/metaballs.wgsl");
const DROPS_WGSL: &str = include_str!("../../../shaders/drops.wgsl");
const EXPLOSIONS_WGSL: &str = include_str!("../../../shaders/explosions.wgsl");

/// The factory colour table of "Scroll de color" (the classic defaults: red, yellow,
/// green, cyan, blue, magenta). LINEAR light — for fully saturated sRGB primaries the
/// curve maps 0→0 and 255→1 unchanged, so these read the same in both spaces.
pub const SCROLL_PALETTE: &[[f32; 3]] = &[
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 1.0, 1.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
];

/// The movement modes of "Scroll de color", in INDEX order (the shader switches on the
/// index — keep `color_scroll.wgsl`'s `mode_*` tables in lockstep). `"Group|Label"`, the
/// professional consoles names and menu groups. "Forma 3D" is deliberately not offered yet.
pub const SCROLL_MODES: &[&str] = &[
    // 0..=25 — Lineal: (x, y, z) direction of travel; z = toward the viewer (Frontal).
    "Lineal|Frontal",
    "Lineal|Posterior",
    "Lineal|Izquierda",
    "Lineal|Derecha",
    "Lineal|Superior",
    "Lineal|Inferior",
    "Lineal|Superior izquierda",
    "Lineal|Superior derecha",
    "Lineal|Inferior izquierda",
    "Lineal|Inferior derecha",
    "Lineal|Frontal izquierda",
    "Lineal|Frontal derecha",
    "Lineal|Posterior izquierda",
    "Lineal|Posterior derecha",
    "Lineal|Frontal superior",
    "Lineal|Frontal inferior",
    "Lineal|Posterior superior",
    "Lineal|Posterior inferior",
    "Lineal|Frontal superior izquierda",
    "Lineal|Frontal superior derecha",
    "Lineal|Frontal inferior izquierda",
    "Lineal|Frontal inferior derecha",
    "Lineal|Posterior superior izquierda",
    "Lineal|Posterior superior derecha",
    "Lineal|Posterior inferior izquierda",
    "Lineal|Posterior inferior derecha",
    // 26..=31 — Lineal a 2 vías.
    "Lineal a 2 vías|Implosionar eje X",
    "Lineal a 2 vías|Explosionar eje X",
    "Lineal a 2 vías|Implosionar eje Y",
    "Lineal a 2 vías|Explosionar eje Y",
    "Lineal a 2 vías|Implosionar eje Z",
    "Lineal a 2 vías|Explosionar eje Z",
    // 32..=41 — Forma 2D.
    "Forma 2D|Implosionar rectángulo",
    "Forma 2D|Explosionar rectángulo",
    "Forma 2D|Implosionar cuadrado",
    "Forma 2D|Explosionar cuadrado",
    "Forma 2D|Implosionar elipse",
    "Forma 2D|Explosionar elipse",
    "Forma 2D|Implosionar círculo",
    "Forma 2D|Explosionar círculo",
    "Forma 2D|Implosionar diamante",
    "Forma 2D|Explosionar diamante",
    // 42 — Cross mode: opposing scrolls (the rose's centre ✕).
    "|Cruz",
];
/// Default mode: "Lineal|Derecha".
const SCROLL_MODE_DEFAULT: usize = 3;

const DISPLACEMENT_DISTRIBUTIONS: &[&str] = &[
    "Uniforme",
    "Lineal creciente",
    "Lineal decreciente",
    "Triángulo",
];
const CROSS_AXES: &[&str] = &["Ejes 1 y 2", "Eje 1", "Eje 2"];

/// Normalized default for a real value `v` in `min..=max` (linear).
fn norm_of(v: f32, min: f32, max: f32) -> f32 {
    (v - min) / (max - min)
}

/// "Scroll de color" — the classic colour-scroll effect (§5.3): bands of a user palette moving
/// across the matrix. Parameters and defaults follow the professional consoles 5 help:
/// `bpm` (speed, 0–9999, 600; 60 BPM = one step per second), `step` (how far each step
/// moves, % of the matrix, 0.01), `color_width` (width of one colour, %, 10),
/// `cross_width` (only in Cross mode, %, 50), `displacement` (distortion amount 0–100)
/// with its `disp_speed` and `disp_dist`ribution, `cross_axes` (Cross mode), `mode` (the
/// direction / movement, see [`SCROLL_MODES`]) and `fade` ("Atenuar": blend instead of hard cuts). Param order here **is** the uniform
/// layout the shader reads. The colours are the cell's colour TABLE (a list, up to
/// [`crate::effect::PALETTE_MAX`]), not parameters — see [`EffectManifest::palette`].
pub fn color_scroll() -> EffectManifest {
    let cruz = Requires::Choice {
        param: "mode",
        choice: "|Cruz",
    };
    let displaced = Requires::NonZero {
        param: "displacement",
    };
    let mut params = vec![
        ParamSpec::bpm(
            "bpm",
            ParamDescriptor::level(
                "effect.color_scroll.bpm",
                RealRange {
                    min: 0.0,
                    max: 9999.0,
                },
                Curve::Linear,
                norm_of(600.0, 0.0, 9999.0),
            ),
        ),
        ParamSpec::scalar(
            "step",
            ParamDescriptor::level(
                "effect.color_scroll.step",
                RealRange {
                    min: 0.01,
                    max: 100.0,
                },
                Curve::Linear,
                0.0,
            ),
        ),
        ParamSpec::scalar(
            "color_width",
            ParamDescriptor::level(
                "effect.color_scroll.color_width",
                RealRange {
                    min: 0.01,
                    max: 100.0,
                },
                Curve::Linear,
                norm_of(10.0, 0.01, 100.0),
            ),
        ),
        ParamSpec::scalar(
            "cross_width",
            ParamDescriptor::level(
                "effect.color_scroll.cross_width",
                RealRange {
                    min: 0.01,
                    max: 100.0,
                },
                Curve::Linear,
                norm_of(50.0, 0.01, 100.0),
            ),
        )
        .requires(cruz),
        ParamSpec::scalar(
            "displacement",
            ParamDescriptor::level(
                "effect.color_scroll.displacement",
                RealRange {
                    min: 0.0,
                    max: 100.0,
                },
                Curve::Linear,
                0.0,
            ),
        ),
        ParamSpec::scalar(
            "disp_speed",
            ParamDescriptor::level(
                "effect.color_scroll.disp_speed",
                RealRange {
                    min: 0.0,
                    max: 100.0,
                },
                Curve::Linear,
                norm_of(10.0, 0.0, 100.0),
            ),
        )
        .requires(displaced),
        ParamSpec::enum_choice(
            "disp_dist",
            ParamDescriptor::level(
                "effect.color_scroll.disp_dist",
                RealRange {
                    min: 0.0,
                    max: (DISPLACEMENT_DISTRIBUTIONS.len() - 1) as f32,
                },
                Curve::Linear,
                0.0,
            ),
            DISPLACEMENT_DISTRIBUTIONS,
        )
        .requires(displaced),
        ParamSpec::enum_choice(
            "cross_axes",
            ParamDescriptor::level(
                "effect.color_scroll.cross_axes",
                RealRange {
                    min: 0.0,
                    max: (CROSS_AXES.len() - 1) as f32,
                },
                Curve::Linear,
                0.0,
            ),
            CROSS_AXES,
        )
        .requires(cruz),
        ParamSpec::direction_menu(
            "mode",
            ParamDescriptor::level(
                "effect.color_scroll.mode",
                RealRange {
                    min: 0.0,
                    max: (SCROLL_MODES.len() - 1) as f32,
                },
                Curve::Linear,
                SCROLL_MODE_DEFAULT as f32 / (SCROLL_MODES.len() - 1) as f32,
            ),
            SCROLL_MODES,
        ),
    ];
    // "Atenuar": OFF = hard cuts between colours (the classic default), ON = each colour
    // fades into the next.
    params.push(ParamSpec::toggle(
        "fade",
        ParamDescriptor::unit_level("effect.color_scroll.fade", 0.0),
    ));
    EffectManifest {
        effect_id: "color_scroll",
        params,
        palette: Some(SCROLL_PALETTE),
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
        palette: None,
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
        palette: None,
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
        palette: None,
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
        palette: None,
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
        palette: None,
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
        palette: None,
        shader_source: SPARKLE_WGSL,
    }
}

/// The three `r`/`g`/`b` params of a single "color" group, defaulting to white. Effects
/// that carry one tint colour share this instead of spelling out the triple each time;
/// the panel collapses the group into one 255-space picker (§3.5). `keys` are the three
/// `label_key`s in r/g/b order.
fn color_group(keys: [&'static str; 3]) -> Vec<ParamSpec> {
    let channels = [ColorChannel::R, ColorChannel::G, ColorChannel::B];
    let names = ["r", "g", "b"];
    names
        .into_iter()
        .zip(channels)
        .zip(keys)
        .map(|((name, channel), key)| {
            ParamSpec::color(
                name,
                ParamDescriptor::unit_level(key, 1.0),
                "color",
                channel,
            )
        })
        .collect()
}

/// The Simple Shape dropdown labels, in index order. The shader's `switch` arms and
/// the enum's real range (`0 ..= len-1`) are kept in lockstep with this array; the
/// shader receives the selected index directly.
const SHAPES: [&str; 7] = [
    "Círculo",
    "Cuadrado",
    "Triángulo",
    "Diamante",
    "Cruz",
    "Estrella",
    "Corazón",
];

/// One centred shape drawn from a signed distance function (§5.3, NEW-EFFECTS §3.2) —
/// the showcase for the `enum` role. `shape` picks the SDF (a real dropdown), `size` its
/// radius, `pos_x`/`pos_y` its centre, `border` the edge softness, and the `color` group
/// tints the fill. Param order here **is** the uniform layout the shader reads.
pub fn simple_shape() -> EffectManifest {
    let mut params = vec![
        ParamSpec::enum_choice(
            "shape",
            ParamDescriptor::level(
                "effect.simple_shape.shape",
                RealRange {
                    min: 0.0,
                    max: (SHAPES.len() - 1) as f32,
                },
                Curve::Linear,
                // Default to the circle (index 0).
                0.0,
            ),
            &SHAPES,
        ),
        ParamSpec::scalar(
            "size",
            ParamDescriptor::unit_level("effect.simple_shape.size", 0.4),
        ),
        ParamSpec::scalar(
            "pos_x",
            ParamDescriptor::unit_level("effect.simple_shape.pos_x", 0.5),
        ),
        ParamSpec::scalar(
            "pos_y",
            ParamDescriptor::unit_level("effect.simple_shape.pos_y", 0.5),
        ),
        ParamSpec::scalar(
            "border",
            ParamDescriptor::unit_level("effect.simple_shape.border", 0.12),
        ),
    ];
    params.extend(color_group([
        "effect.simple_shape.r",
        "effect.simple_shape.g",
        "effect.simple_shape.b",
    ]));
    EffectManifest {
        effect_id: "simple_shape",
        params,
        palette: None,
        shader_source: SIMPLE_SHAPE_WGSL,
    }
}

/// Organic blobs that orbit and merge (§5.3, NEW-EFFECTS §3.3) — the inverse-square-sum
/// scalar field. `count` is the number of live blobs (1–8), `size` their radius, `speed`
/// their motion, `threshold` the iso-level where the surface forms, and the `color` group
/// tints the fill. Param order here **is** the uniform layout the shader reads.
pub fn metaballs() -> EffectManifest {
    let mut params = vec![
        ParamSpec::scalar(
            "count",
            ParamDescriptor::level(
                "effect.metaballs.count",
                RealRange { min: 1.0, max: 8.0 },
                Curve::Linear,
                // ~4 blobs by default: (4-1)/(8-1).
                3.0 / 7.0,
            ),
        ),
        ParamSpec::scalar(
            "size",
            ParamDescriptor::unit_level("effect.metaballs.size", 0.4),
        ),
        ParamSpec::scalar(
            "speed",
            ParamDescriptor::level(
                "effect.metaballs.speed",
                RealRange { min: 0.0, max: 4.0 },
                Curve::Linear,
                // Mid-low so the blobs drift out of the box.
                0.25,
            ),
        ),
        ParamSpec::scalar(
            "threshold",
            ParamDescriptor::unit_level("effect.metaballs.threshold", 0.5),
        ),
    ];
    params.extend(color_group([
        "effect.metaballs.r",
        "effect.metaballs.g",
        "effect.metaballs.b",
    ]));
    EffectManifest {
        effect_id: "metaballs",
        params,
        palette: None,
        shader_source: METABALLS_WGSL,
    }
}

/// Falling drops with trailing tails (§5.3, NEW-EFFECTS §3.1). `speed` is the fall rate,
/// `density` the number of columns, `length` the tail length, `fade` its falloff sharpness,
/// `direction` the 8-way travel axis, and the `color` group the tint. Param order here
/// **is** the uniform layout the shader reads.
pub fn drops() -> EffectManifest {
    let mut params = vec![
        ParamSpec::scalar(
            "speed",
            ParamDescriptor::level(
                "effect.drops.speed",
                RealRange { min: 0.0, max: 4.0 },
                Curve::Linear,
                // Mid-slider so drops fall out of the box.
                0.4,
            ),
        ),
        ParamSpec::scalar(
            "density",
            ParamDescriptor::level(
                "effect.drops.density",
                RealRange {
                    min: 1.0,
                    max: 40.0,
                },
                Curve::Linear,
                // ~12 columns: (12-1)/(40-1).
                0.28,
            ),
        ),
        ParamSpec::scalar(
            "length",
            ParamDescriptor::unit_level("effect.drops.length", 0.35),
        ),
        ParamSpec::scalar(
            "fade",
            ParamDescriptor::unit_level("effect.drops.fade", 0.5),
        ),
        ParamSpec::direction(
            "direction",
            // Default rose index 2 → Down (2/7), the natural fall.
            ParamDescriptor::unit_level("effect.drops.direction", 2.0 / 7.0),
        ),
    ];
    params.extend(color_group([
        "effect.drops.r",
        "effect.drops.g",
        "effect.drops.b",
    ]));
    EffectManifest {
        effect_id: "drops",
        params,
        palette: None,
        shader_source: DROPS_WGSL,
    }
}

/// Periodic radial particle bursts with gravity (§5.3, NEW-EFFECTS §3.4). `rate` is
/// bursts per second, `size` the burst reach, `particles` how many per burst, `gravity`
/// the downward pull, `fade` the decay sharpness, and the `color` group the tint. Param
/// order here **is** the uniform layout the shader reads.
pub fn explosions() -> EffectManifest {
    let mut params = vec![
        ParamSpec::scalar(
            "rate",
            ParamDescriptor::level(
                "effect.explosions.rate",
                RealRange { min: 0.0, max: 4.0 },
                Curve::Linear,
                // ~1 burst/s.
                0.25,
            ),
        ),
        ParamSpec::scalar(
            "size",
            ParamDescriptor::unit_level("effect.explosions.size", 0.5),
        ),
        ParamSpec::scalar(
            "particles",
            ParamDescriptor::level(
                "effect.explosions.particles",
                RealRange {
                    min: 1.0,
                    max: 40.0,
                },
                Curve::Linear,
                // ~20 particles: (20-1)/(40-1).
                0.49,
            ),
        ),
        ParamSpec::scalar(
            "gravity",
            ParamDescriptor::unit_level("effect.explosions.gravity", 0.4),
        ),
        ParamSpec::scalar(
            "fade",
            ParamDescriptor::unit_level("effect.explosions.fade", 0.5),
        ),
    ];
    params.extend(color_group([
        "effect.explosions.r",
        "effect.explosions.g",
        "effect.explosions.b",
    ]));
    EffectManifest {
        effect_id: "explosions",
        params,
        palette: None,
        shader_source: EXPLOSIONS_WGSL,
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
        palette: None,
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
        palette: None,
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
/// the M5 GPU effects; audio-reactive and scripting categories come
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
        EffectListing {
            id: "simple_shape",
            name: "Forma sencilla",
            category: "Generadores",
        },
        EffectListing {
            id: "metaballs",
            name: "Metaballs",
            category: "Generadores",
        },
        EffectListing {
            id: "drops",
            name: "Gotas",
            category: "Generadores",
        },
        EffectListing {
            id: "explosions",
            name: "Explosiones",
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
    /// Greyed out in the panel unless this holds (console-style dependent controls).
    pub requires: Option<crate::effect::Requires>,
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
                    requires: p.requires,
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
        "simple_shape" => Some(simple_shape()),
        "metaballs" => Some(metaballs()),
        "drops" => Some(drops()),
        "explosions" => Some(explosions()),
        _ => None,
    }
}

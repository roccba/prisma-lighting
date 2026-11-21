// "Pulse / Estroboscope" (CLAUDE.md §5.3). The whole target pulses in brightness, one
// pulse per beat at `speed` BPM. `ratio` (0–100 %) balances the two phases of a cycle:
// the rise takes (100 - ratio) % of the period and the fall the rest — 0 = rise only
// (saw up), 50 = symmetric, 100 = fall only (saw down). `fade` ON smooths the envelope
// (smoothstep); OFF turns it into a hard strobe (full on while the envelope is in its
// upper half, off otherwise).
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, ratio, fade, r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    ratio: f32,
    fade: f32,
    r: f32,
    g: f32,
    b: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    // Phase within the current beat, 0..1.
    let t = fract(u.time * u.speed / 60.0);

    // Fraction of the cycle spent rising; the rest is the fall.
    let rise = clamp(1.0 - u.ratio / 100.0, 0.0, 1.0);
    var env = 0.0;
    if (rise >= 1.0) {
        env = t;                                   // rise only
    } else if (rise <= 0.0) {
        env = 1.0 - t;                             // fall only
    } else if (t < rise) {
        env = t / rise;                            // rising
    } else {
        env = 1.0 - (t - rise) / (1.0 - rise);     // falling
    }

    var intensity = 0.0;
    if (u.fade > 0.5) {
        intensity = smoothstep(0.0, 1.0, env);
    } else {
        intensity = select(0.0, 1.0, env >= 0.5);
    }

    return vec4<f32>(vec3<f32>(u.r, u.g, u.b) * intensity, 1.0);
}

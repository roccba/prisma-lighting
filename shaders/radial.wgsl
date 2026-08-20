// Radial wave (CLAUDE.md §5.3 — the "Onda / radial" entry). Concentric sine rings
// expanding from the matrix centre: the wave effect's radial sibling, for ripples
// and pulses instead of sweeps.
//
// Output is LINEAR-light grayscale; gamma is an output-stage concern (§5.2).
// Animated: rings travel outward as `time` advances; per-zone speed scales the rate.
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here frequency, speed. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    frequency: f32,
    speed: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

const TAU: f32 = 6.283185307179586;

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
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    // Centre the coordinates on the matrix and normalize by half-width so the
    // pattern is aspect-independent: radius ~1 reaches the nearest edge.
    let uv = (frag.xy - 0.5 * res) / (0.5 * res.x);
    let r = length(uv);
    // Rings travel outward: subtract time so crests move away from the centre.
    let n = 0.5 + 0.5 * sin((r * u.frequency - u.time * u.speed) * TAU);
    return vec4<f32>(n, n, n, 1.0);
}

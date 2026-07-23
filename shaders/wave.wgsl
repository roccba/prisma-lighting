// Wave / sine (CLAUDE.md §5.3 effect library). A travelling sine along X — the
// building block of chases and sweeps, and the simplest animated effect there is.
//
// Output is LINEAR-light grayscale; gamma is an output-stage concern (§5.2).
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
    // Normalize so the left column is x = 0 and the right column x = 1, matching
    // the gradient effect's convention (fragment centres sit at integer + 0.5).
    let span = max(u.resolution.x - 1.0, 1.0);
    let x = (frag.x - 0.5) / span;
    let n = 0.5 + 0.5 * sin((x * u.frequency + u.time * u.speed) * TAU);
    return vec4<f32>(n, n, n, 1.0);
}

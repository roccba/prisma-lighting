// Plasma (CLAUDE.md §5.3 effect library). The classic sum-of-sines field: four
// sine terms at different rates and orientations, summed and remapped to 0..1.
//
// Output is LINEAR-light grayscale; gamma is an output-stage concern (§5.2).
// Animated: `time` is show time in seconds, so a paused clock freezes the pattern
// and per-zone speed scales it (once §5.2 step 3 wires speed_multiplier).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here scale, speed. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    scale: f32,
    speed: f32,
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
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let p = (frag.xy / res) * u.scale;
    let t = u.time * u.speed;

    // Four terms: two axis-aligned, one diagonal, one radial. Their differing
    // rates are what keeps the field from repeating on a simple period.
    var v = sin(p.x + t);
    v = v + sin(p.y * 0.8 + t * 1.3);
    v = v + sin((p.x + p.y) * 0.7 + t * 0.7);
    v = v + sin(sqrt(p.x * p.x + p.y * p.y) * 1.1 + t * 1.7);

    // Four sines span [-4, 4]; 0.125 * v + 0.5 maps that onto [0, 1].
    let n = clamp(v * 0.125 + 0.5, 0.0, 1.0);
    return vec4<f32>(n, n, n, 1.0);
}

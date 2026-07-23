// Solid color (CLAUDE.md §5.3 effect library). Emits one constant LINEAR-light
// color across the whole target; the three components come from parameters, so a
// transport/UI can drive the color through the tree like any other effect.
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
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
    return vec4<f32>(u.r, u.g, u.b, 1.0);
}

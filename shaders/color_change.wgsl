// "Color Change" (CLAUDE.md §5.3). The whole target cross-fades through four colours
// in a cycle, 1 → 2 → 3 → 4 → 1, one colour per beat at `speed` BPM. Global, not
// spatial: every pixel takes the same colour.
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, then colours a..d as r, g, b each. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    a_r: f32,
    a_g: f32,
    a_b: f32,
    b_r: f32,
    b_g: f32,
    b_b: f32,
    c_r: f32,
    c_g: f32,
    c_b: f32,
    d_r: f32,
    d_g: f32,
    d_b: f32,
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

fn color_at(i: i32) -> vec3<f32> {
    switch i {
        case 0: { return vec3<f32>(u.a_r, u.a_g, u.a_b); }
        case 1: { return vec3<f32>(u.b_r, u.b_g, u.b_b); }
        case 2: { return vec3<f32>(u.c_r, u.c_g, u.c_b); }
        default: { return vec3<f32>(u.d_r, u.d_g, u.d_b); }
    }
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    // Position in the 4-colour cycle, 0..4 (one colour per beat).
    let beats = u.time * u.speed / 60.0;
    let t = beats - 4.0 * floor(beats / 4.0);
    let i0 = clamp(i32(floor(t)), 0, 3);
    let i1 = (i0 + 1) % 4;
    let color = mix(color_at(i0), color_at(i1), fract(t));
    return vec4<f32>(color, 1.0);
}

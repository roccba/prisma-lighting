// Fire (CLAUDE.md §5.3 effect library). Flames rising from the bottom edge:
// fractal value noise scrolled upward, shaped by a vertical falloff (hot at the
// base, cooling toward the top), then mapped through a built-in fire colour ramp.
//
// Unlike the grayscale generators, fire emits COLOUR directly — its ramp
// (black → red → orange → yellow → white) is intrinsic to the effect, so it needs
// no external palette. Output is LINEAR-light; gamma is an output-stage concern.
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here intensity, speed. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    intensity: f32,
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

fn hash13(p_in: vec3<f32>) -> f32 {
    var p3 = fract(p_in * 0.1031);
    p3 = p3 + dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

fn vnoise(x: vec3<f32>) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let w = f * f * (3.0 - 2.0 * f);
    let c000 = hash13(i + vec3<f32>(0.0, 0.0, 0.0));
    let c100 = hash13(i + vec3<f32>(1.0, 0.0, 0.0));
    let c010 = hash13(i + vec3<f32>(0.0, 1.0, 0.0));
    let c110 = hash13(i + vec3<f32>(1.0, 1.0, 0.0));
    let c001 = hash13(i + vec3<f32>(0.0, 0.0, 1.0));
    let c101 = hash13(i + vec3<f32>(1.0, 0.0, 1.0));
    let c011 = hash13(i + vec3<f32>(0.0, 1.0, 1.0));
    let c111 = hash13(i + vec3<f32>(1.0, 1.0, 1.0));
    let x00 = mix(c000, c100, w.x);
    let x10 = mix(c010, c110, w.x);
    let x01 = mix(c001, c101, w.x);
    let x11 = mix(c011, c111, w.x);
    let y0 = mix(x00, x10, w.y);
    let y1 = mix(x01, x11, w.y);
    return mix(y0, y1, w.z);
}

// Three fBm octaves of the upward-scrolling field.
fn flames(uv: vec2<f32>, t: f32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var freq = 3.0;
    for (var o = 0; o < 3; o = o + 1) {
        // Scroll upward: as t grows the field moves toward smaller screen-y (up).
        sum = sum + amp * vnoise(vec3<f32>(uv.x * freq, uv.y * freq + t, t * 0.3));
        freq = freq * 2.0;
        amp = amp * 0.5;
    }
    return sum;
}

// Fire colour ramp in LINEAR light: black → deep red → orange → yellow → white.
fn fire_ramp(h: f32) -> vec3<f32> {
    let black = vec3<f32>(0.0, 0.0, 0.0);
    let red = vec3<f32>(0.6, 0.02, 0.0);
    let orange = vec3<f32>(0.9, 0.18, 0.0);
    let yellow = vec3<f32>(1.0, 0.7, 0.05);
    let white = vec3<f32>(1.0, 1.0, 0.9);
    let t = clamp(h, 0.0, 1.0);
    if (t < 0.25) {
        return mix(black, red, t / 0.25);
    } else if (t < 0.5) {
        return mix(red, orange, (t - 0.25) / 0.25);
    } else if (t < 0.75) {
        return mix(orange, yellow, (t - 0.5) / 0.25);
    } else {
        return mix(yellow, white, (t - 0.75) / 0.25);
    }
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;
    // Screen y grows downward, so uv.y ~1 is the base (hottest) and ~0 the top.
    let base = uv.y;
    let t = u.time * max(u.speed, 0.0);

    // Flames modulated by a vertical falloff: strong at the base, starved at the top.
    let f = flames(uv, t) * u.intensity;
    let heat = clamp(f * base * 2.0 - (1.0 - base) * 0.3, 0.0, 1.0);

    return vec4<f32>(fire_ramp(heat), 1.0);
}

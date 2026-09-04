// Drops (CLAUDE.md §5.3, docs/NEW-EFFECTS.md §3.1). Bright heads travelling along one
// of eight directions, each trailing an exponential tail — rain, comets, falling
// pixels. The matrix is diced into `density` columns across the travel axis; each
// column owns a hash-seeded phase and rate so the drops fall out of step.
//
// No CPU particle state — each fragment computes its distance behind the moving head
// (§2.2). Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, density, length, fade, direction, r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    density: f32,
    length: f32,
    fade: f32,
    direction: f32,
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

// Travel axis for a direction index, matching the editor's eight signal directions
// (Rose.tsx DIRS order, shared with color_scroll.wgsl). Screen y grows downward.
fn dir_vec(idx: i32) -> vec2<f32> {
    let inv = 0.70710678;
    switch idx {
        case 0: { return vec2<f32>( 1.0,  0.0); }   // Right
        case 1: { return vec2<f32>(-1.0,  0.0); }   // Left
        case 2: { return vec2<f32>( 0.0,  1.0); }   // Down
        case 3: { return vec2<f32>( 0.0, -1.0); }   // Up
        case 4: { return vec2<f32>( inv,  inv); }   // DownRight
        case 5: { return vec2<f32>(-inv,  inv); }   // DownLeft
        case 6: { return vec2<f32>( inv, -inv); }   // UpRight
        default: { return vec2<f32>(-inv, -inv); }  // UpLeft (idx 7)
    }
}

fn hash21(n: f32) -> vec2<f32> {
    let p = fract(vec2<f32>(n * 0.1031, n * 0.1030));
    let q = p + dot(p, p.yx + 33.33);
    return fract((q.xx + q.yx) * q.yx);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;

    let idx = clamp(i32(round(u.direction * 7.0)), 0, 7);
    let d = dir_vec(idx);
    let perp = vec2<f32>(-d.y, d.x);
    // Coordinate along the travel axis (tc, 0 at the start edge → 1 at the far edge)
    // and across it (columns).
    let tc = 0.5 + dot(uv - vec2<f32>(0.5), d);
    let across = 0.5 + dot(uv - vec2<f32>(0.5), perp);

    let cols = max(u.density, 1.0);
    let col = floor(across * cols);
    let h = hash21(col + 1.0);
    let rate = 0.6 + h.x * 0.9;
    // The head cycles along the axis; the current point is `delta` behind it.
    let head = fract(u.time * u.speed * rate + h.y);
    let delta = head - tc;

    let len = max(u.length * 0.6, 0.02);
    var bright = 0.0;
    if (delta >= 0.0 && delta <= len) {
        // Head brightest at delta 0, tail falling off; `fade` sharpens the falloff.
        bright = pow(1.0 - delta / len, 1.0 + u.fade * 5.0);
    }

    let color = vec3<f32>(u.r, u.g, u.b) * bright;
    return vec4<f32>(color, 1.0);
}

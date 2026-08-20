// Colour scroll (CLAUDE.md §5.3 — professional consoles's "Scroll de color", screenshot 15). Bands
// of a user palette scrolling across the matrix along one of eight directions. This
// is the first effect to declare a PALETTE and a DIRECTION, lighting up the palette
// editor and the 8-way rose that were wired but dormant.
//
// The palette is a fixed block of 8 colour slots plus a `count` of how many are live;
// the operator edits them through the palette editor, but on the wire they are just
// scalar params packed flat like any other (effect.rs is unchanged). The shader
// reassembles them into a local array and blends cyclically for a smooth scroll.
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, width, offset, direction, count, then c0r..c7b.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    width: f32,
    offset: f32,
    direction: f32,
    count: f32,
    c0r: f32, c0g: f32, c0b: f32,
    c1r: f32, c1g: f32, c1b: f32,
    c2r: f32, c2g: f32, c2b: f32,
    c3r: f32, c3g: f32, c3b: f32,
    c4r: f32, c4g: f32, c4b: f32,
    c5r: f32, c5g: f32, c5b: f32,
    c6r: f32, c6g: f32, c6b: f32,
    c7r: f32, c7g: f32, c7b: f32,
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

// Scroll axis for a direction index, matching the editor's eight signal directions
// (Rose.tsx DIRS order). Screen y grows downward, so "Down" is +y.
fn dir_vec(idx: i32) -> vec2<f32> {
    let inv = 0.70710678; // 1/sqrt(2), so diagonals span the same range as the axes
    switch idx {
        case 0: { return vec2<f32>( 1.0,  0.0); }             // Right
        case 1: { return vec2<f32>(-1.0,  0.0); }             // Left
        case 2: { return vec2<f32>( 0.0,  1.0); }             // Down
        case 3: { return vec2<f32>( 0.0, -1.0); }             // Up
        case 4: { return vec2<f32>( inv,  inv); }             // DownRight
        case 5: { return vec2<f32>(-inv,  inv); }             // DownLeft
        case 6: { return vec2<f32>( inv, -inv); }             // UpRight
        default: { return vec2<f32>(-inv, -inv); }            // UpLeft (idx 7)
    }
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;

    // Reassemble the palette into an indexable local array (uniform struct fields
    // cannot be indexed dynamically; a local array can).
    var pal = array<vec3<f32>, 8>(
        vec3<f32>(u.c0r, u.c0g, u.c0b),
        vec3<f32>(u.c1r, u.c1g, u.c1b),
        vec3<f32>(u.c2r, u.c2g, u.c2b),
        vec3<f32>(u.c3r, u.c3g, u.c3b),
        vec3<f32>(u.c4r, u.c4g, u.c4b),
        vec3<f32>(u.c5r, u.c5g, u.c5b),
        vec3<f32>(u.c6r, u.c6g, u.c6b),
        vec3<f32>(u.c7r, u.c7g, u.c7b),
    );
    let count = clamp(i32(round(u.count)), 1, 8);

    // Position along the scroll axis, in palette cycles, advanced by time and offset.
    let idx = clamp(i32(round(u.direction * 7.0)), 0, 7);
    let proj = dot(uv, dir_vec(idx));
    let phase = fract(proj * max(u.width, 0.0) - u.time * u.speed + u.offset);

    // Map phase 0..1 across the live palette, blending cyclically between neighbours.
    let f = phase * f32(count);
    let i0 = i32(floor(f)) % count;
    let i1 = (i0 + 1) % count;
    let frac = fract(f);
    let color = mix(pal[i0], pal[i1], frac);
    return vec4<f32>(color, 1.0);
}

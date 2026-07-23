// Horizontal grayscale linear gradient (CLAUDE.md §5.3 effect library).
//
// Output is LINEAR-light (the canonical color space, §5.2): gamma is applied
// once at the output stage, never here.
//
// # Uniform convention (shared by every effect)
//
// The `Uniforms` block starts with the standard globals — `time`, a pad, and
// `resolution` — and is then followed by one `f32` per manifest parameter, IN
// MANIFEST ORDER. The Rust side (`effect.rs`) packs the buffer to exactly this
// layout: `[time, _pad0, resolution.x, resolution.y, <params…>]`. An effect's
// WGSL struct and its manifest are the two halves of one contract; they must
// agree on the parameter order.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    // -- manifest params, in order --
    start: f32,   // grayscale value at the left edge
    end: f32,     // grayscale value at the right edge
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle covering the whole viewport.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // Fragment centers are at integer+0.5; normalize pixel i to t = i/(W-1) so
    // the left column is exactly `start` and the right column exactly `end`.
    var t: f32 = 0.0;
    if (u.resolution.x > 1.0) {
        t = (frag.x - 0.5) / (u.resolution.x - 1.0);
    }
    let v = mix(u.start, u.end, t);
    return vec4<f32>(v, v, v, 1.0);
}

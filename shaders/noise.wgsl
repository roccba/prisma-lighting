// Noise (CLAUDE.md §5.3 effect library — the Perlin/Simplex entry, an M5 gap now
// closed as pure content). Animated fractal value noise (fBm): several octaves of
// gradient-lattice noise summed at halving amplitude, evolving over show time.
//
// Output is LINEAR-light grayscale; gamma is an output-stage concern (§5.2).
// Animated: the noise is sampled in 3D with the third axis driven by `time`, so a
// paused clock freezes the field and per-zone speed scales its evolution.
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

// Integer-lattice hash → [0,1), after Dave Hoskins' hash13. Deterministic, so the
// pattern is stable frame-to-frame at a given time (no per-frame reseeding twinkle).
fn hash13(p_in: vec3<f32>) -> f32 {
    var p3 = fract(p_in * 0.1031);
    p3 = p3 + dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

// 3D value noise: trilinear blend of the eight lattice-corner hashes with a
// smoothstep fade, so the field is continuous (no lattice-aligned creases).
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

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    // Normalize to the unit square so `scale` reads as "cells across the matrix"
    // independent of pixel count.
    let uv = frag.xy / res;
    let z = u.time * u.speed;

    // Four fBm octaves: each doubles frequency and halves amplitude.
    var sum = 0.0;
    var amp = 0.5;
    var freq = u.scale;
    for (var o = 0; o < 4; o = o + 1) {
        sum = sum + amp * vnoise(vec3<f32>(uv * freq, z));
        freq = freq * 2.0;
        amp = amp * 0.5;
    }

    let n = clamp(sum, 0.0, 1.0);
    return vec4<f32>(n, n, n, 1.0);
}

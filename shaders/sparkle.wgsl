// Sparkle / twinkle (CLAUDE.md §5.3 effect library). Scattered points that fade up
// and down out of phase with each other — the classic "stars" / glitter look.
//
// The matrix is diced into a lattice; each cell owns a stable random phase and a
// random activation, so only a fraction of cells sparkle and each on its own clock.
// The pattern is hash-derived (deterministic), so a given time always yields the
// same field — the twinkle comes from the animated phase, not per-frame reseeding.
//
// Output is LINEAR-light grayscale; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here density, speed. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    density: f32,
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

// 2D → 2D hash: two independent [0,1) values per lattice cell.
fn hash22(p: vec2<f32>) -> vec2<f32> {
    var q = vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973);
    q = fract(q);
    q = q + dot(q, q.yzx + 33.33);
    return fract((q.xx + q.yz) * q.zy);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    // Density maps to lattice resolution: denser → more, smaller cells.
    let cells = max(u.density, 1.0);
    let g = (frag.xy / res) * cells;
    let cell = floor(g);

    let h = hash22(cell);
    // Only cells above the activation cut sparkle; the rest stay dark. Higher
    // density also raises the active fraction so the field fills in.
    let active_cut = clamp(1.0 - u.density * 0.05, 0.3, 0.95);
    if (h.x < active_cut) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Per-cell phase and rate give each sparkle its own twinkle clock.
    let phase = h.y * TAU;
    let rate = 0.5 + h.x;
    let s = 0.5 + 0.5 * sin(u.time * u.speed * rate + phase);
    // Sharpen the pulse so sparkles read as brief flashes, not slow breathing.
    let v = pow(s, 6.0);
    return vec4<f32>(v, v, v, 1.0);
}

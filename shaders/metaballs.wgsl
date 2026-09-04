// Metaballs (CLAUDE.md §5.3, docs/NEW-EFFECTS.md §3.3). Organic blobs that orbit and
// merge — the classic inverse-square-sum scalar field. Up to 8 blobs move on
// time-driven Lissajous paths seeded by a per-blob hash; `count` gates how many are
// live, `size` their radius, `speed` their motion, `threshold` the iso-level where the
// surface forms, and the `color` group tints the fill.
//
// No CPU particle state — the field is evaluated procedurally per fragment (§2.2).
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here count, size, speed, threshold, r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    count: f32,
    size: f32,
    speed: f32,
    threshold: f32,
    r: f32,
    g: f32,
    b: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

const TAU: f32 = 6.283185307179586;
const MAX_BLOBS: i32 = 8;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

// 1D → 2D hash: two independent [0,1) values from a scalar seed.
fn hash21(n: f32) -> vec2<f32> {
    let p = fract(vec2<f32>(n * 0.1031, n * 0.1030));
    let q = p + dot(p, p.yx + 33.33);
    return fract((q.xx + q.yx) * q.yx);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;
    let aspect = res.x / res.y;
    // Aspect-corrected space centred on the matrix.
    let p = vec2<f32>((uv.x - 0.5) * aspect, uv.y - 0.5);

    let n = clamp(i32(round(u.count)), 1, MAX_BLOBS);
    // Blob radius (in the corrected space) → its field weight.
    let rad = 0.06 + max(u.size, 0.0) * 0.30;
    let rr = rad * rad;

    var field = 0.0;
    for (var i = 0; i < MAX_BLOBS; i = i + 1) {
        if (i >= n) { break; }
        let h1 = hash21(f32(i) * 1.7 + 1.0);
        let h2 = hash21(f32(i) * 3.3 + 5.0);
        // Each blob orbits on its own Lissajous, its extent bounded so it stays on-matrix.
        let cx = (aspect * 0.5 - rad) * sin(u.time * u.speed * (0.4 + h1.x) + h1.y * TAU) * 0.85;
        let cy = (0.5 - rad) * cos(u.time * u.speed * (0.4 + h2.x) + h2.y * TAU) * 0.85;
        let d = p - vec2<f32>(cx, cy);
        field += rr / (dot(d, d) + 1e-4);
    }

    // The iso-surface sits where field ≈ 1; threshold slides that level. A soft
    // shoulder below the level antialiases the merging boundary.
    let thr = mix(0.4, 3.0, clamp(u.threshold, 0.0, 1.0));
    let a = smoothstep(thr * 0.75, thr, field);
    let color = vec3<f32>(u.r, u.g, u.b) * a;
    return vec4<f32>(color, 1.0);
}

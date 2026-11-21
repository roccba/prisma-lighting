// "Color Change" (CLAUDE.md §5.3). The whole target cross-fades through the cell's
// colour table, one colour per beat at `speed` BPM, in the order `color_mode` picks:
// Secuencial (1 → 2 → … → n → 1), Ping-pong (1 → … → n → … → 1) or Aleatorio (a
// stateless hash of the beat). `fade` ON blends each colour into the next across the
// beat; OFF holds each colour and cuts on the beat. Global, not spatial: every pixel
// takes the same colour.
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, color_mode, fade. The colours come from the `palette`
// uniform (binding 1). See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    color_mode: f32,
    fade: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// The cell's colour table (a list, not parameters): `len` live colours, up to 255.
// Same layout as color_scroll.wgsl.
struct Palette {
    len: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
    colors: array<vec4<f32>, 255>,
};
@group(0) @binding(1) var<uniform> palette: Palette;

fn pal_len() -> i32 {
    return clamp(i32(round(palette.len)), 1, 255);
}

// Integer hash (lowbias32) — stateless randomness for the RND mode.
fn hash_u(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// Index into the colour table for step `k` (a beat, a wall hit…) under `color_mode`
// (PALETTE_MODES in effects.rs): 0 Secuencial 0,1,…,n-1,0,…; 1 Ping-pong 0,1,…,n-1,
// n-2,…,1,0,…; 2 Aleatorio — a hash of the step (and `seed`), so it is stable for the
// whole step and needs no state. `k` may be negative (reverse zone speed).
fn pal_index(mode: f32, k: i32, seed: u32, n: i32) -> i32 {
    if (n <= 1) { return 0; }
    let m = clamp(i32(round(mode)), 0, 2);
    if (m == 1) {
        let period = 2 * (n - 1);
        let p = ((k % period) + period) % period;
        return select(period - p, p, p < n);
    }
    if (m == 2) {
        return i32(hash_u(bitcast<u32>(k) ^ hash_u(seed + 0x9e3779b9u)) % u32(n));
    }
    return ((k % n) + n) % n;
}

fn pal_color(i: i32) -> vec3<f32> {
    return palette.colors[i].rgb;
}

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
    let beats = u.time * u.speed / 60.0;
    let k = i32(floor(beats));
    let n = pal_len();
    let c0 = pal_color(pal_index(u.color_mode, k, 0u, n));
    let c1 = pal_color(pal_index(u.color_mode, k + 1, 0u, n));
    let f = select(0.0, fract(beats), u.fade > 0.5);
    return vec4<f32>(mix(c0, c1, f), 1.0);
}

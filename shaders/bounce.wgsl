// "Bounce" (CLAUDE.md §5.3). `count` round objects bouncing off the canvas edges. Each
// position is a triangle wave of show time — `abs(fract(x) * 2 - 1)` — so an object
// makes one round trip per beat at `speed` BPM, phase-shifted by its index so they
// spread out. `direction`: Horizontal (move in X, rows spread in Y), Vertical (the
// transpose) or Ambos (X and Y bounce at different rates, so objects roam the canvas).
// `size` is the radius as a fraction of the canvas' shorter side; `fade_out` softens
// the disc from its centre outward (0 = hard edge, 1 = falls off from the centre).
// Colour: every time an object hits a wall (along its main bounce) it takes the next
// colour of the cell's colour table per `color_mode`; objects start staggered.
//
// No CPU particle state (§2.2). Output is LINEAR-light; gamma is an output-stage
// concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, count, size, fade_out, direction, color_mode. The
// colours come from the `palette` uniform (binding 1). See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    count: f32,
    size: f32,
    fade_out: f32,
    direction: f32,
    color_mode: f32,
    // LIVE-MODEL-SPEC 2b: 0 = transparent background, 1 = black.
    background: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// Background (LIVE-MODEL-SPEC 2b): `background` 0 = transparent — the picture gets an
// alpha (a shape's coverage, a field's brightest channel) — 1 = black, opaque. Colour
// is premultiplied either way, so over black it is the same picture.
fn out_alpha(rgb: vec3<f32>, coverage: f32) -> vec4<f32> {
    if (u.background < 0.5) {
        return vec4<f32>(rgb, clamp(coverage, 0.0, 1.0));
    }
    return vec4<f32>(rgb, 1.0);
}


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

const MAX_OBJECTS: i32 = 20;

// 0..1..0 triangle wave of `x`.
fn tri(x: f32) -> f32 {
    return abs(fract(x) * 2.0 - 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let unit = min(res.x, res.y);
    let radius = max(u.size, 0.001) * unit;
    // Inner radius where the falloff begins: fade_out 0 → hard disc, 1 → from the centre.
    let inner = radius * (1.0 - clamp(u.fade_out, 0.0, 1.0));

    let n = clamp(i32(round(u.count)), 1, MAX_OBJECTS);
    let nf = f32(n);
    let axis = clamp(i32(round(u.direction)), 0, 2);
    let beats = u.time * u.speed / 60.0;
    let np = pal_len();

    var bright = 0.0;
    var color = vec3<f32>(0.0);
    for (var i = 0; i < MAX_OBJECTS; i = i + 1) {
        if (i >= n) { break; }
        let fi = f32(i);
        let phase = fi / nf;
        let x = beats + phase;
        let bx = tri(x);
        // Fixed lane across the travel axis, so the objects spread instead of stacking.
        let lane = (fi + 0.5) / nf;
        var pos = vec2<f32>(bx, lane);
        if (axis == 1) {
            pos = vec2<f32>(lane, bx);
        } else if (axis == 2) {
            // Y at a different (irrational-ish) rate and offset so paths cover the canvas.
            pos = vec2<f32>(bx, tri(beats * 0.7071 + phase * 0.618 + 0.25));
        }
        // Keep the whole disc inside the canvas: the centre bounces between the edges
        // minus the radius, as a real ball off a wall.
        let margin = min(vec2<f32>(radius), res * 0.5);
        let centre = margin + pos * (res - 2.0 * margin);
        let d = distance(frag.xy, centre);
        let b = 1.0 - smoothstep(inner, radius + 0.5, d);
        if (b > bright) {
            bright = b;
            // Wall hits so far: the triangle wave turns twice per beat.
            let hits = i32(floor(x * 2.0));
            color = pal_color(pal_index(u.color_mode, hits + i, u32(i), np));
        }
    }

    return out_alpha(color * bright, bright);
}

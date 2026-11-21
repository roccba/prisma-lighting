// "Pulse / Estroboscope" (CLAUDE.md §5.3). The whole target pulses in brightness, one
// pulse per beat at `speed` BPM. `ratio` (0–100 %) balances the two phases of a cycle:
// the rise takes (100 - ratio) % of the period and the fall the rest — 0 = rise only
// (saw up), 50 = symmetric, 100 = fall only (saw down). `fade` ON smooths the envelope
// (smoothstep); OFF turns it into a hard strobe (full on while the envelope is in its
// upper half, off otherwise). Each pulse takes one colour of the cell's colour table,
// advancing per `color_mode` (Secuencial / Ping-pong / Aleatorio).
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, ratio, fade, color_mode. The colours come from the
// `palette` uniform (binding 1). See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    ratio: f32,
    fade: f32,
    color_mode: f32,
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
    // Phase within the current beat, 0..1, and which pulse this is.
    let t = fract(beats);
    let k = i32(floor(beats));

    // Fraction of the cycle spent rising; the rest is the fall.
    let rise = clamp(1.0 - u.ratio / 100.0, 0.0, 1.0);
    var env = 0.0;
    if (rise >= 1.0) {
        env = t;                                   // rise only
    } else if (rise <= 0.0) {
        env = 1.0 - t;                             // fall only
    } else if (t < rise) {
        env = t / rise;                            // rising
    } else {
        env = 1.0 - (t - rise) / (1.0 - rise);     // falling
    }

    var intensity = 0.0;
    if (u.fade > 0.5) {
        intensity = smoothstep(0.0, 1.0, env);
    } else {
        intensity = select(0.0, 1.0, env >= 0.5);
    }

    let color = pal_color(pal_index(u.color_mode, k, 0u, pal_len()));
    return vec4<f32>(color * intensity, 1.0);
}

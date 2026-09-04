// Explosions (CLAUDE.md §5.3, docs/NEW-EFFECTS.md §3.4). Periodic radial bursts of
// particles thrown outward from a random centre and pulled down by gravity, each
// burst fading as it ages. Time is bucketed into bursts at `rate`; the current and
// previous bucket are both drawn so a burst lives out its full cycle. Per-particle
// angles and each burst's centre are hash-seeded, so it is deterministic and needs no
// CPU particle state (§2.2).
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here rate, size, particles, gravity, fade, r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    rate: f32,
    size: f32,
    particles: f32,
    gravity: f32,
    fade: f32,
    r: f32,
    g: f32,
    b: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

const TAU: f32 = 6.283185307179586;
const MAX_PARTICLES: i32 = 40;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

fn hash21(n: f32) -> vec2<f32> {
    let p = fract(vec2<f32>(n * 0.1031, n * 0.1030));
    let q = p + dot(p, p.yx + 33.33);
    return fract((q.xx + q.yx) * q.yx);
}

// Accumulated glow from one burst bucket at the given `age` (0..1 through its life).
fn burst_glow(burst: f32, age: f32, p: vec2<f32>, aspect: f32, n: i32) -> f32 {
    let c = hash21(burst * 57.0 + 11.0);
    // Burst centre, kept off the edges, in the aspect-corrected centred space.
    let center = vec2<f32>((c.x - 0.5) * aspect * 0.7, (c.y - 0.5) * 0.7);
    let reach = 0.1 + max(u.size, 0.0) * 0.6;
    let dot_r = 0.008 + max(u.size, 0.0) * 0.02;

    var glow = 0.0;
    for (var i = 0; i < MAX_PARTICLES; i = i + 1) {
        if (i >= n) { break; }
        let h = hash21(burst * 131.0 + f32(i) * 7.3);
        let ang = h.x * TAU;
        let spd = 0.5 + h.y;
        let dir = vec2<f32>(cos(ang), sin(ang));
        // Outward travel plus gravity pulling down (+y on screen) with age².
        let pos = center + dir * (age * reach * spd) + vec2<f32>(0.0, u.gravity * age * age * 0.5);
        let dist = length(p - pos);
        glow += smoothstep(dot_r, 0.0, dist);
    }
    // Whole burst fades out as it ages; `fade` sharpens the decay.
    return glow * pow(clamp(1.0 - age, 0.0, 1.0), 1.0 + u.fade * 4.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;
    let aspect = res.x / res.y;
    let p = vec2<f32>((uv.x - 0.5) * aspect, uv.y - 0.5);

    let n = clamp(i32(round(u.particles)), 1, MAX_PARTICLES);
    let t = u.time * max(u.rate, 0.0);
    let burst = floor(t);
    let age = fract(t);

    // One burst per cycle: it flashes at the centre (age 0), expands and falls, and
    // fades out (age → 1) just as the next burst is seeded.
    let glow = burst_glow(burst, age, p, aspect, n);

    let color = vec3<f32>(u.r, u.g, u.b) * clamp(glow, 0.0, 1.5);
    return vec4<f32>(color, 1.0);
}

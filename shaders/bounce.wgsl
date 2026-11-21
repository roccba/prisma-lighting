// "Bounce" (CLAUDE.md §5.3). `count` round objects bouncing off the canvas edges. Each
// position is a triangle wave of show time — `abs(fract(x) * 2 - 1)` — so an object
// makes one round trip per beat at `speed` BPM, phase-shifted by its index so they
// spread out. `direction`: Horizontal (move in X, rows spread in Y), Vertical (the
// transpose) or Ambos (X and Y bounce at different rates, so objects roam the canvas).
// `size` is the radius as a fraction of the canvas' shorter side; `fade_out` softens
// the disc from its centre outward (0 = hard edge, 1 = falls off from the centre).
//
// No CPU particle state (§2.2). Output is LINEAR-light; gamma is an output-stage
// concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here speed, count, size, fade_out, r, g, b, direction. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    speed: f32,
    count: f32,
    size: f32,
    fade_out: f32,
    r: f32,
    g: f32,
    b: f32,
    direction: f32,
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

    var bright = 0.0;
    for (var i = 0; i < MAX_OBJECTS; i = i + 1) {
        if (i >= n) { break; }
        let fi = f32(i);
        let phase = fi / nf;
        let bx = tri(beats + phase);
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
        bright = max(bright, b);
    }

    return vec4<f32>(vec3<f32>(u.r, u.g, u.b) * bright, 1.0);
}

// Simple Shape (CLAUDE.md §5.3, docs/NEW-EFFECTS.md §3.2). One centred shape drawn
// from a signed distance function, chosen from a dropdown. This is the showcase for
// the `enum` parameter role: `u.shape` arrives as the selected index (0..6).
//
// The SDFs are the standard 2D distance functions (iquilezles.org/articles/
// distfunctions2d); `d < 0` is inside. A soft edge of width `border` antialiases the
// boundary, and the whole fill is tinted by the `color` group.
//
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — here shape, size, pos_x, pos_y, border, r, g, b. See effect.rs.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    shape: f32,
    size: f32,
    pos_x: f32,
    pos_y: f32,
    border: f32,
    r: f32,
    g: f32,
    b: f32,
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

fn dot2(v: vec2<f32>) -> f32 { return dot(v, v); }

// --- Signed distance functions (d < 0 inside). ---

fn sd_circle(p: vec2<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn sd_box(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let q = abs(p) - b;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
}

// L1 ball — a diamond (square rotated 45°).
fn sd_diamond(p: vec2<f32>, r: f32) -> f32 {
    return abs(p.x) + abs(p.y) - r;
}

fn sd_triangle(p_in: vec2<f32>, r: f32) -> f32 {
    let k = sqrt(3.0);
    var p = p_in;
    p.x = abs(p.x) - r;
    p.y = p.y + r / k;
    if (p.x + k * p.y > 0.0) {
        p = vec2<f32>(p.x - k * p.y, -k * p.x - p.y) / 2.0;
    }
    p.x -= clamp(p.x, -2.0 * r, 0.0);
    return -length(p) * sign(p.y);
}

// A plus sign built from two crossed boxes.
fn sd_cross(p: vec2<f32>, r: f32) -> f32 {
    let arm = r * 0.34;
    let a = sd_box(p, vec2<f32>(r, arm));
    let b = sd_box(p, vec2<f32>(arm, r));
    return min(a, b);
}

fn sd_star5(p_in: vec2<f32>, r: f32, rf: f32) -> f32 {
    let k1 = vec2<f32>(0.809016994375, -0.587785252292);
    let k2 = vec2<f32>(-k1.x, k1.y);
    var p = p_in;
    p.x = abs(p.x);
    p -= 2.0 * max(dot(k1, p), 0.0) * k1;
    p -= 2.0 * max(dot(k2, p), 0.0) * k2;
    p.x = abs(p.x);
    p.y -= r;
    let ba = rf * vec2<f32>(-k1.y, k1.x) - vec2<f32>(0.0, 1.0);
    let h = clamp(dot(p, ba) / dot(ba, ba), 0.0, r);
    return length(p - ba * h) * sign(p.y * ba.x - p.x * ba.y);
}

// iq's heart, in its own unit space (point down, lobes up in maths-y-up coords).
fn sd_heart(p: vec2<f32>) -> f32 {
    var q = p;
    q.x = abs(q.x);
    if (q.y + q.x > 1.0) {
        return sqrt(dot2(q - vec2<f32>(0.25, 0.75))) - sqrt(2.0) / 4.0;
    }
    return sqrt(min(dot2(q - vec2<f32>(0.0, 1.0)),
                    dot2(q - 0.5 * max(q.x + q.y, 0.0)))) * sign(q.x - q.y);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let uv = frag.xy / res;
    // Aspect-correct so a circle is round on a non-square matrix. Screen y grows
    // downward; flip it so shapes read the conventional way up.
    let aspect = res.x / res.y;
    var p = vec2<f32>((uv.x - u.pos_x) * aspect, -(uv.y - u.pos_y));

    let r = max(u.size, 0.0) * 0.7;
    let s = i32(round(u.shape));

    var d: f32;
    switch s {
        case 0: { d = sd_circle(p, r); }                       // Círculo
        case 1: { d = sd_box(p, vec2<f32>(r)); }               // Cuadrado
        case 2: { d = sd_triangle(p, r); }                     // Triángulo
        case 3: { d = sd_diamond(p, r); }                      // Diamante
        case 4: { d = sd_cross(p, r); }                        // Cruz
        case 5: { d = sd_star5(p, r, 0.45); }                  // Estrella
        default: {                                             // Corazón (idx 6)
            let scale = max(r, 1e-4);
            let hp = vec2<f32>(p.x, -(p.y - r * 0.25)) / scale;
            d = sd_heart(hp) * scale;
        }
    }

    // Soft edge: fully inside at d < -edge, fully outside at d > edge.
    let edge = max(u.border * 0.5, 0.002);
    let a = 1.0 - smoothstep(-edge, edge, d);
    let color = vec3<f32>(u.r, u.g, u.b) * a;
    return vec4<f32>(color, 1.0);
}

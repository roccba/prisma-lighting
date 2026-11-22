// Layer / zone blending (CLAUDE.md §4.2 blend modes, §5.2 step 3; LIVE-MODEL-SPEC 2b).
//
// One source `b` blended onto the accumulator below it `a`, both LINEAR-light and
// PREMULTIPLIED alpha (rgb already multiplied by alpha). All math is in linear space;
// gamma is a per-fixture output-stage concern (§5.2). The same pass blends tracks
// inside a zone and, with `use_mask`, zone over zone (§4.1: "the same machinery").
// The master buffer reads only rgb, which for premultiplied colour IS the picture
// over black — so output, preview and the latency shortcut are unaffected by alpha.
//
// Opacity scales the source first (all four channels): b *= opacity. Then, with
// ca / cb the un-premultiplied colours and αa / αb the alphas:
//   Normal    : out = b + a·(1 − αb)                     (alpha-over)
//   separable : out.rgb = (1−αb)·a.rgb + (1−αa)·b.rgb + αa·αb·M(ca, cb)
//               out.a   = αa + αb − αa·αb
//     with M = Add: min(ca + cb, 1) · Subtract: max(ca − cb, 0) · Multiply: ca·cb
//              Screen: 1 − (1−ca)(1−cb) · Max: max(ca, cb) · Min: min(ca, cb)
//   Mask      : out = a · k,  k = 1 − opacity·(1 − source αb)   (gates by alpha)
//   LumaKey   : b *= smoothstep(t, t + s, luma(cb)), then Normal (a key for sources
//               without alpha; t = threshold, s = softness)
//   Crossfade : out = mix(a, b, opacity) on all four channels — the engine's clip
//               transitions. NOT alpha-over: an incoming clip with transparent areas
//               must still fade the outgoing one out there.
// With everything opaque (αa = αb = 1) the separable form reduces to the plain mode
// M(a, b), and opacity o to the familiar mix(a, M, o).
//
// `mode` codes MUST match `blend::blend_code`.

// When compositing ZONES (not intra-zone layers), `use_mask` is 1 and this pass
// also applies the zone's own dimmer/colour to its contribution — but ONLY on the
// framebuffer cells the frame's snapshot marks overlapped (§5.2.1). There the dimmer
// acts as an OPACITY (scales colour AND alpha), so a lowered upper zone reveals the
// zone below instead of black; the colour filter scales colour only. Everywhere else
// the dimmer is deferred to the output stage, which is the latency shortcut.
//
// The mask is per CELL, not per pixel: this pass works in framebuffer cells, and
// with a coarse matrix several pixels share one cell. See §5.2.1.
struct BlendParams {
    mode: u32,
    opacity: f32,
    // 1 = zone composite (consult the mask and apply zone_scale), 0 = layer blend.
    use_mask: u32,
    _pad0: u32,
    // rgb = zone dimmer x colour filter, a = zone dimmer (its opacity on overlapped
    // cells). Unused when use_mask == 0.
    zone_scale: vec4<f32>,
    // LumaKey: x = threshold, y = softness (linear luminance). Unused otherwise.
    key: vec4<f32>,
};

@group(0) @binding(0) var dst_tex: texture_2d<f32>; // accumulator below
@group(0) @binding(1) var src_tex: texture_2d<f32>; // layer on top
@group(0) @binding(2) var<uniform> bp: BlendParams;
@group(0) @binding(3) var<storage, read> cell_overlapped: array<u32>;
// Which cells this zone actually covers, from its patch geometry (§5.2 step 4).
// Outside its coverage a zone must leave the accumulator untouched — this is what
// makes zones independent, the product's whole thesis. Only read when use_mask==1.
@group(0) @binding(4) var<storage, read> zone_coverage: array<u32>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Un-premultiply (a fully transparent texel has no colour: 0).
fn unpremul(c: vec4<f32>) -> vec3<f32> {
    if (c.a <= 0.0) {
        return vec3<f32>(0.0);
    }
    return c.rgb / c.a;
}

fn separable(mode: u32, ca: vec3<f32>, cb: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return min(ca + cb, vec3<f32>(1.0)); }
        case 2u: { return ca * cb; }
        case 3u: { return vec3<f32>(1.0) - (vec3<f32>(1.0) - ca) * (vec3<f32>(1.0) - cb); }
        case 5u: { return max(ca, cb); }
        case 6u: { return max(ca - cb, vec3<f32>(0.0)); }
        case 7u: { return min(ca, cb); }
        default: { return cb; }
    }
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // 1:1 target ↔ inputs; integer load avoids any sampler filtering.
    let p = vec2<i32>(i32(frag.x), i32(frag.y));
    let a = textureLoad(dst_tex, p, 0);
    var b = textureLoad(src_tex, p, 0);

    if (bp.use_mask == 1u) {
        let width = i32(textureDimensions(dst_tex).x);
        let cell = u32(p.y * width + p.x);
        // Outside this zone's geometry it contributes nothing at all.
        if (zone_coverage[cell] == 0u) {
            return a;
        }
        // On overlapped cells the zone dimmer is an opacity, the colour filter a tint.
        if (cell_overlapped[cell] != 0u) {
            b = vec4<f32>(b.rgb * bp.zone_scale.rgb, b.a * bp.zone_scale.a);
        }
    }

    // Crossfade: a linear mix of both premultiplied pictures (clip transitions).
    if (bp.mode == 9u) {
        return mix(a, b, bp.opacity);
    }
    // Mask: the source's alpha gates what is below.
    if (bp.mode == 4u) {
        return a * (1.0 - bp.opacity * (1.0 - b.a));
    }

    b = b * bp.opacity;
    // LumaKey: alpha from the source's own brightness, then alpha-over.
    if (bp.mode == 8u) {
        let t = bp.key.x;
        let k = smoothstep(t, t + max(bp.key.y, 1e-4), luma(unpremul(b)));
        b = b * k;
        return b + a * (1.0 - b.a);
    }
    if (bp.mode == 0u) {
        return b + a * (1.0 - b.a);
    }
    let m = separable(bp.mode, unpremul(a), unpremul(b));
    let rgb = (1.0 - b.a) * a.rgb + (1.0 - a.a) * b.rgb + a.a * b.a * m;
    return vec4<f32>(rgb, a.a + b.a - a.a * b.a);
}

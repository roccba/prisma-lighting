// Layer / zone blending (CLAUDE.md §4.2 blend modes, §5.2 step 3).
//
// One source layer `b` blended onto the accumulator below it `a`, both LINEAR-
// light. All math is in linear space — never perceptual/gamma space; gamma is a
// per-fixture output-stage concern (§5.2). This same pass blends intra-zone
// layers (S2) and, from S4, zone-over-zone (§4.1: "the same blending machinery").
//
// Blend definitions, per channel unless noted (a = below, b = above):
//   Normal   = b
//   Add      = clamp(a + b, 0, 1)
//   Multiply = a * b
//   Screen   = 1 - (1 - a)(1 - b)
//   Mask     = a * luma(b)            // luminance key: dst gated by src brightness
//   Max      = max(a, b)
// `luma` is Rec.709 LINEAR luminance: dot(b, (0.2126, 0.7152, 0.0722)).
//
// PROVISIONAL — Mask semantics (revisit at M10). The BlendMode enum did not define
// Mask, so it is a luminance key here. When media sources with a real alpha channel
// arrive (M10), an operator will expect Mask to gate by the source's ALPHA, not its
// luma. Switch this case (and document it) then; it is isolated to this branch and
// `blend::blend_code`.
//
// Layer opacity mixes the blended result back toward the accumulator:
//   out = mix(a, blended, opacity)
// so opacity = 1 is the pure mode and opacity = 0 leaves `a` untouched. Normal at
// opacity o is therefore the familiar `mix(a, b, o)` alpha-over.
//
// `mode` codes MUST match `blend::blend_code` (Normal=0 … Max=5).

// When compositing ZONES (not intra-zone layers), `use_mask` is 1 and this pass
// also applies the zone's own dimmer/colour to its contribution — but ONLY on the
// framebuffer cells the frame's snapshot marks overlapped (§5.2.1). On those
// cells the zone dimmer cannot be factored out later, because Add/Max will have
// combined the zones; everywhere else the dimmer is deferred to the output stage,
// which is the latency shortcut.
//
// The mask is per CELL, not per pixel: this pass works in framebuffer cells, and
// with a coarse matrix several pixels share one cell. See §5.2.1.
struct BlendParams {
    mode: u32,
    opacity: f32,
    // 1 = zone composite (consult the mask and apply zone_scale), 0 = layer blend.
    use_mask: u32,
    _pad0: u32,
    // rgb = zone dimmer x colour filter. Unused when use_mask == 0.
    zone_scale: vec4<f32>,
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

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // 1:1 target ↔ inputs; integer load avoids any sampler filtering.
    let p = vec2<i32>(i32(frag.x), i32(frag.y));
    let a = textureLoad(dst_tex, p, 0).rgb;
    var b = textureLoad(src_tex, p, 0).rgb;

    if (bp.use_mask == 1u) {
        let width = i32(textureDimensions(dst_tex).x);
        let cell = u32(p.y * width + p.x);
        // Outside this zone's geometry it contributes nothing at all.
        if (zone_coverage[cell] == 0u) {
            return vec4<f32>(a, 1.0);
        }
        // Pre-scale this zone's contribution on overlapped cells only.
        if (cell_overlapped[cell] != 0u) {
            b = b * bp.zone_scale.rgb;
        }
    }

    var blended: vec3<f32>;
    switch bp.mode {
        case 0u: { blended = b; }
        case 1u: { blended = clamp(a + b, vec3<f32>(0.0), vec3<f32>(1.0)); }
        case 2u: { blended = a * b; }
        case 3u: { blended = vec3<f32>(1.0) - (vec3<f32>(1.0) - a) * (vec3<f32>(1.0) - b); }
        case 4u: { blended = a * luma(b); }
        case 5u: { blended = max(a, b); }
        default: { blended = b; }
    }

    let out = mix(a, blended, bp.opacity);
    return vec4<f32>(out, 1.0);
}

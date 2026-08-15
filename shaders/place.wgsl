// Zone placement pass (CLAUDE.md M8, docs/M8-PARTA.md §2.3).
//
// M8's per-zone render splits what M5 did in one step. In M5 a zone's effect
// rendered directly at the world (matrix_size) resolution. In M8 each zone renders
// its effect at its OWN `mapping_size` into a small texture, and THIS pass PLACES
// that texture into the world-sized framebuffer at the zone's footprint. The result
// is a world-sized texture that the UNCHANGED M5 compositor (`composite_zones`)
// consumes exactly as before — the compositor never learns placement happened.
//
// Resample = NEAREST, by integer fetch (locked decision 1, M8-PARTA §2.4): the same
// filtering philosophy as the gather's `cell_of` ("the cell IS the effect
// resolution", §3.1.5). `textureLoad` with integer coords does no sampler
// filtering, so the identity case (mapping_size == footprint, identity transform)
// is a byte-exact copy — the no-regression gate against M5.
//
// The footprint is given in WORLD cells: `foot_origin` is its lower corner and
// `foot_size` its extent. Outside the footprint the pass writes black; the zone's
// coverage mask (still applied later in `composite_zones`) does the fine cut, so
// this pass only needs to be a SUPERSET of coverage (M8-PARTA §2.3).

struct PlaceParams {
    // Zone footprint in world cells: lower corner and size.
    foot_origin: vec2<f32>,
    foot_size: vec2<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>; // the zone's mapping_size texture
@group(0) @binding(1) var<uniform> pp: PlaceParams;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle, identical convention to blend.wgsl / the effects.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // The world cell this fragment covers. `frag.x` is the cell centre (wx + 0.5),
    // so truncating recovers the integer cell index wx.
    let wx = frag.x;
    let wy = frag.y;

    // Continuous position inside the footprint, in [0,1) when inside.
    let u = (wx - pp.foot_origin.x) / pp.foot_size.x;
    let v = (wy - pp.foot_origin.y) / pp.foot_size.y;

    // Outside the footprint: black. Coverage (applied in composite_zones) confines
    // the zone precisely; this only needs to cover the footprint superset.
    if (u < 0.0 || u >= 1.0 || v < 0.0 || v >= 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Nearest source texel. With mapping_size == foot_size and identity origin,
    // floor(u * mapping.x) == wx exactly, so this is a 1:1 copy (the gate).
    let mapping = vec2<f32>(textureDimensions(src));
    let sx = clamp(i32(floor(u * mapping.x)), 0, i32(mapping.x) - 1);
    let sy = clamp(i32(floor(v * mapping.y)), 0, i32(mapping.y) - 1);

    return textureLoad(src, vec2<i32>(sx, sy), 0);
}

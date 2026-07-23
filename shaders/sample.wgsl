// Geometric gather: virtual-matrix framebuffer → canonical master pixel buffer
// (CLAUDE.md §5.2 step 5). One invocation per global pixel.
//
// This is where "physical layout is decoupled from logical grouping" (§3.1
// principle 5) becomes concrete: the framebuffer is the *effect's* 2D space, and
// each patched pixel reads the single cell its position maps to.
//
// # Nearest cell, no filtering
//
// `cell` was resolved on the CPU by `prisma_patch::cell_of` — the one definition
// of the rule, shared with patch validation. `textureLoad` reads exactly that
// texel: no sampler, no interpolation. LEDs are discrete emitters, so a chase or
// strobe boundary must switch cleanly between cells rather than smear across them.
//
// Several pixels landing in the same cell therefore show the SAME colour. That is
// intentional — `matrix_size` is the effect's resolution, deliberately independent
// of the physical pixel count. The remedy for wanting neighbouring LEDs to differ
// is a higher-resolution matrix, never a change to this gather.
//
// # Why the output is f32, not packed f16
//
// The canonical master buffer is f16, but core WGSL has no f16 storage type
// without the `shader-f16` extension, and packing two f16 per u32 would make
// adjacent pixels race on a shared word wherever a pixel has an odd component
// count (an RGB pixel at components 0..2 and the next at 3..5 share the word
// holding components 2 and 3). So the gather writes tight f32 and the CPU converts
// to f16 while copying into the frame — a copy it has to do anyway, so the
// conversion is essentially free.

struct PixelSample {
    // Cell to read. A NEGATIVE cell_x marks an out-of-range pixel (its position
    // fell outside matrix_size): it reads nothing and is written black. Patch
    // validation rejects these at load; this is the defensive runtime half.
    cell_x: i32,
    cell_y: i32,
    // Component offset into the master buffer, and how many components this pixel
    // has (3 = RGB, 4 = RGBW). Component k takes texel channel k.
    offset: u32,
    components: u32,
};

@group(0) @binding(0) var framebuffer: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> samples: array<PixelSample>;
@group(0) @binding(2) var<storage, read_write> master: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&samples)) {
        return;
    }
    let s = samples[i];

    var c = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if (s.cell_x >= 0) {
        c = textureLoad(framebuffer, vec2<i32>(s.cell_x, s.cell_y), 0);
    }

    // Each pixel owns a disjoint component range, so these writes never race.
    if (s.components > 0u) { master[s.offset + 0u] = c.r; }
    if (s.components > 1u) { master[s.offset + 1u] = c.g; }
    if (s.components > 2u) { master[s.offset + 2u] = c.b; }
    if (s.components > 3u) { master[s.offset + 3u] = c.a; }
}

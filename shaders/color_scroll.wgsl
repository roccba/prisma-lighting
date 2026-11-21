// "Scroll de color" — the classic colour-scroll effect. Bands of a user palette move across
// the matrix in one of the movement modes of `SCROLL_MODES` (effects.rs): 26 linear
// directions (the 3-D ones act on X/Y; pure front/back has no X/Y extent, so the whole
// matrix cycles through the palette), the 2-way implode/explode axes, the 2-D shapes,
// and Cross mode (opposing scrolls).
//
// Everything is measured in PIXELS of the zone's render matrix, so widths given in % of
// the matrix (professional consoles) mean the same on any size:
//   · colour width  = color_width % of the extent L along the mode's axis;
//   · each beat moves step % of L, but at least one pixel; BPM beats per minute
//     (60 BPM = one step per second). Movement is continuous between beats.
// Output is LINEAR-light; gamma is an output-stage concern (§5.2).
//
// Uniform convention: globals (time, _pad0, resolution) then one f32 per manifest
// param in order — bpm, step, color_width, cross_width, displacement, disp_speed,
// disp_dist, cross_axes, mode, fade. The colours come from the `palette` uniform.
struct Uniforms {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    bpm: f32,
    step: f32,
    color_width: f32,
    cross_width: f32,
    displacement: f32,
    disp_speed: f32,
    disp_dist: f32,
    cross_axes: f32,
    mode: f32,
    fade: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// The cell's colour table (a list, not parameters): `len` live colours, up to 255.
struct Palette {
    len: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
    colors: array<vec4<f32>, 255>,
};
@group(0) @binding(1) var<uniform> palette: Palette;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

const TAU: f32 = 6.2831853;

// Direction of travel for linear modes 0..=25, as (x, y, z): x right, y DOWN (screen),
// z toward the viewer (Frontal). Order = SCROLL_MODES.
fn linear_dir(m: i32) -> vec3<f32> {
    switch m {
        case 0: { return vec3<f32>( 0.0,  0.0,  1.0); }  // Frontal
        case 1: { return vec3<f32>( 0.0,  0.0, -1.0); }  // Posterior
        case 2: { return vec3<f32>(-1.0,  0.0,  0.0); }  // Izquierda
        case 3: { return vec3<f32>( 1.0,  0.0,  0.0); }  // Derecha
        case 4: { return vec3<f32>( 0.0, -1.0,  0.0); }  // Superior
        case 5: { return vec3<f32>( 0.0,  1.0,  0.0); }  // Inferior
        case 6: { return vec3<f32>(-1.0, -1.0,  0.0); }  // Superior izquierda
        case 7: { return vec3<f32>( 1.0, -1.0,  0.0); }  // Superior derecha
        case 8: { return vec3<f32>(-1.0,  1.0,  0.0); }  // Inferior izquierda
        case 9: { return vec3<f32>( 1.0,  1.0,  0.0); }  // Inferior derecha
        case 10: { return vec3<f32>(-1.0,  0.0,  1.0); } // Frontal izquierda
        case 11: { return vec3<f32>( 1.0,  0.0,  1.0); } // Frontal derecha
        case 12: { return vec3<f32>(-1.0,  0.0, -1.0); } // Posterior izquierda
        case 13: { return vec3<f32>( 1.0,  0.0, -1.0); } // Posterior derecha
        case 14: { return vec3<f32>( 0.0, -1.0,  1.0); } // Frontal superior
        case 15: { return vec3<f32>( 0.0,  1.0,  1.0); } // Frontal inferior
        case 16: { return vec3<f32>( 0.0, -1.0, -1.0); } // Posterior superior
        case 17: { return vec3<f32>( 0.0,  1.0, -1.0); } // Posterior inferior
        case 18: { return vec3<f32>(-1.0, -1.0,  1.0); } // Frontal superior izquierda
        case 19: { return vec3<f32>( 1.0, -1.0,  1.0); } // Frontal superior derecha
        case 20: { return vec3<f32>(-1.0,  1.0,  1.0); } // Frontal inferior izquierda
        case 21: { return vec3<f32>( 1.0,  1.0,  1.0); } // Frontal inferior derecha
        case 22: { return vec3<f32>(-1.0, -1.0, -1.0); } // Posterior superior izquierda
        case 23: { return vec3<f32>( 1.0, -1.0, -1.0); } // Posterior superior derecha
        case 24: { return vec3<f32>(-1.0,  1.0, -1.0); } // Posterior inferior izquierda
        default: { return vec3<f32>( 1.0,  1.0, -1.0); } // Posterior inferior derecha
    }
}

// Displacement distribution along the main axis, `t` in 0..1.
fn distribution(kind: i32, t: f32) -> f32 {
    switch kind {
        case 1: { return t; }                          // Lineal creciente
        case 2: { return 1.0 - t; }                    // Lineal decreciente
        case 3: { return 1.0 - abs(2.0 * t - 1.0); }   // Triángulo
        default: { return 1.0; }                       // Uniforme
    }
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = max(u.resolution, vec2<f32>(1.0, 1.0));
    let p = frag.xy;                 // pixel centre, y down
    let h = res * 0.5;               // half extents
    let q = p - h;                   // from the centre
    let a = abs(q);

    let mode = clamp(i32(round(u.mode)), 0, 42);

    // For the chosen mode: `pos` = position along the travel (pixels), `L` = the extent
    // the % widths refer to, `dir` = +1 bands move toward growing `pos`, -1 the other way,
    // `perp` = a coordinate across the travel (for the displacement wave).
    var pos = 0.0;
    var L = max(res.x, res.y);
    var dir = 1.0;
    var perp = 0.0;

    if (mode <= 25) {
        let d3 = linear_dir(mode);
        let d = d3.xy;
        let dl = length(d);
        if (dl > 0.0) {
            let n = d / dl;
            pos = dot(q, n);
            L = abs(n.x) * res.x + abs(n.y) * res.y;
            perp = dot(q, vec2<f32>(-n.y, n.x));
        } else {
            // Pure front/back: no X/Y extent — the whole matrix cycles together.
            pos = 0.0;
            dir = select(1.0, -1.0, d3.z < 0.0);
        }
    } else if (mode <= 31) {
        let k = mode - 26;            // 0 imp X, 1 exp X, 2 imp Y, 3 exp Y, 4 imp Z, 5 exp Z
        let axis = k / 2;
        if (axis == 0) { pos = a.x; L = h.x; perp = q.y; }
        else if (axis == 1) { pos = a.y; L = h.y; perp = q.x; }
        else { pos = 0.0; }
        dir = select(-1.0, 1.0, k % 2 == 1); // explode = outward
    } else if (mode <= 41) {
        let k = mode - 32;            // pairs: rectángulo, cuadrado, elipse, círculo, diamante
        let shape = k / 2;
        switch shape {
            case 0: { L = max(h.x, h.y); pos = max(a.x / h.x, a.y / h.y) * L; }
            case 1: { L = max(h.x, h.y); pos = max(a.x, a.y); }
            case 2: { L = max(h.x, h.y); pos = length(a / h) * L; }
            case 3: { L = length(h); pos = length(a); }
            default: { L = h.x + h.y; pos = a.x + a.y; }
        }
        perp = atan2(q.y, q.x) / TAU * L;
        dir = select(-1.0, 1.0, k % 2 == 1);
    } else {
        // Cruz: the matrix is cut into segments of `cross_width`; neighbouring segments
        // scroll in opposite directions. Eje 1 = horizontal scrolls in horizontal
        // segments; Eje 2 = vertical scrolls in vertical segments; Ejes 1 y 2 = both,
        // alternating segment by segment.
        let axes = clamp(i32(round(u.cross_axes)), 0, 2);
        let segy = max(u.cross_width / 100.0 * res.y, 1.0);
        let segx = max(u.cross_width / 100.0 * res.x, 1.0);
        let ky = i32(floor(p.y / segy));
        let kx = i32(floor(p.x / segx));
        var horizontal = axes != 2;
        if (axes == 0) { horizontal = (ky + kx) % 2 == 0; }
        if (horizontal) {
            pos = p.x; L = res.x; perp = p.y;
            dir = select(-1.0, 1.0, ky % 2 == 0);
        } else {
            pos = p.y; L = res.y; perp = p.x;
            dir = select(-1.0, 1.0, kx % 2 == 0);
        }
    }

    // Movement: BPM steps per minute, each step `step`% of L (at least one pixel).
    let step_px = max(u.step / 100.0 * L, 1.0);
    let travel = u.time * (u.bpm / 60.0) * step_px;

    // Displacement: a wave across the travel that bends the bands, moving at disp_speed.
    var s = pos - dir * travel;
    if (u.displacement > 0.0) {
        let t01 = clamp(pos / max(L, 1.0), 0.0, 1.0);
        let amount = u.displacement / 100.0 * L * 0.25;
        let wave = sin(TAU * (perp / max(L, 1.0) + u.time * u.disp_speed / 20.0));
        s = s + amount * wave * distribution(clamp(i32(round(u.disp_dist)), 0, 3), t01);
    }

    let count = clamp(i32(round(palette.len)), 1, 255);

    // Bands of `color_width`% of L, cycling through the live palette. "Atenuar" (fade)
    // blends each band into the next; off = hard cuts (professional consoles default).
    let w = max(u.color_width / 100.0 * L, 0.5);
    let f = s / w;
    let band = i32(floor(f));
    let i0 = ((band % count) + count) % count;
    let i1 = (i0 + 1) % count;
    let frac = select(0.0, fract(f), u.fade > 0.5);
    return vec4<f32>(mix(palette.colors[i0].rgb, palette.colors[i1].rgb, frac), 1.0);
}

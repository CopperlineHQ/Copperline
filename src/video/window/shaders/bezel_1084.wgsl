// SPDX-License-Identifier: GPL-3.0-or-later
//
// Monitor-bezel pass: a procedural front frame in the shape of the 1084
// the Amiga shipped with. Two-tone, as that cabinet is: a pale grey case,
// and clipped into it a darker grey-green moulding that drops away in a
// chamfer to the tube it holds. Below the moulding is the chin panel,
// carrying the model badge, the logotype where the 1084 wears its maker's
// name, and the red power lamp.
//
// The cabinet fills the display rect, as a monitor's front fills the
// window it stands in. Everything inside it is placed off the picture
// opening `bezel.rs` chose, in units of that opening's height, by frame
// proportions measured off a straight-on photograph of a real one (see
// FRAME_* below).
//
// Two modes, selected by params.x. Alone (0), this one pass draws both
// the frame and the picture. Under a CRT preset (1, frame-only), the
// preset has already painted the picture into the opening's bounding box
// and this pass runs after it, discarding the opening interior and
// drawing just the frame on top -- the moulding overlaps the tube face,
// so its rounded corners and chamfer clip the preset's square viewport.
//
// Not a window-shader preset: it is not user-replaceable, and its uniform
// block is its own, so it deliberately does not carry the shared contract
// prologue of shaders/{scanlines,mask,crt}.wgsl.

struct BezelUniforms {
    // Display sub-rect of src_tex in UV space: xy origin, zw size. The
    // status bar sits below the display in the same texture and must
    // never be sampled, so all sampling goes through this rect.
    src_rect: vec4<f32>,
    // xy: viewport size in physical pixels.
    // zw: source display region in texels.
    size: vec4<f32>,
    // Picture opening within the viewport, in viewport UV: xy origin,
    // zw size. Same aspect as the viewport, so the picture is scaled,
    // not stretched.
    opening: vec4<f32>,
    // x: 1 = frame-only (a CRT preset has already painted the opening;
    // leave its interior untouched), 0 = full (paint the picture too).
    // y: the active preset's face curvature, raw; the chamfer follows the
    // bowed glass contour it implies, and 0 keeps the flat rounded opening.
    // z: the corner radius that preset clips its face to, already faded by
    // its strength (crt.wgsl fades that one linearly); the aperture opens
    // to at least this so the moulding covers the face's own edge. 0 for a
    // preset that does not clip one, or none.
    // w: that strength, which the bow is faded by here.
    params: vec4<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> u: BezelUniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VOut {
    // Fullscreen triangle: the viewport restricts it to the display rect.
    let tc = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    var out: VOut;
    out.uv = tc;
    out.pos = vec4<f32>(tc * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

// Sample the display region only, with the clamp inset by half a texel so
// a linear sample on the boundary never blends in the status bar's first
// row (see sample_display in the preset contract for the derivation).
fn sample_display(uv: vec2<f32>) -> vec4<f32> {
    let half_texel = 0.5 * u.src_rect.zw / max(u.size.zw, vec2<f32>(1.0));
    let lo = u.src_rect.xy + half_texel;
    let hi = u.src_rect.xy + u.src_rect.zw - half_texel;
    let tc = clamp(u.src_rect.xy + uv * u.src_rect.zw, lo, hi);
    return textureSample(src_tex, src_samp, tc);
}

// Signed distance to a rounded rectangle centred on the origin, in the
// units of p; negative inside.
fn rounded_rect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Outward unit normal of `rounded_rect` at p. A rounded rect's gradient is
// exact in closed form, so the chamfer's slope direction needs no screen
// derivatives.
fn rounded_rect_grad(p: vec2<f32>, half: vec2<f32>, r: f32) -> vec2<f32> {
    let s = sign(p + vec2<f32>(1e-6));
    let q = abs(p) - half + vec2<f32>(r);
    if q.x > 0.0 && q.y > 0.0 {
        return normalize(q) * s;
    }
    if q.x > q.y {
        return vec2<f32>(s.x, 0.0);
    }
    return vec2<f32>(0.0, s.y);
}

// Per-pixel hash in -0.5..0.5, for the moulded plastic grain.
fn grain(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453) - 0.5;
}

// --- the frame's proportions ------------------------------------------
//
// All in units of the opening's height, scaled by FRAME_WEIGHT: the ratios
// are the real cabinet's, measured off a straight-on photograph, and the
// weight trades faithfulness against how much of the window the picture
// keeps. `bezel.rs` places the opening from the vertical three and this
// pass reads all five back, so the two must agree; a test pins them
// together.
const FRAME_WEIGHT: f32 = 0.62;
const FRAME_TOP: f32 = 0.1905 * FRAME_WEIGHT;
const FRAME_WELL_BOTTOM: f32 = 0.1293 * FRAME_WEIGHT;
const FRAME_CHIN: f32 = 0.1497 * FRAME_WEIGHT;
// The design's side margin. The cabinet is wider than the design here --
// it fills the window, and the picture's aspect leaves more room beside
// the tube than the design asks for -- so this no longer says where the
// opening sits, which falls out of the vertical margins alone. It sets
// how far the moulding reaches beside the tube; the cabinet's pillar
// takes whatever is left.
const FRAME_SIDE: f32 = 0.2313 * FRAME_WEIGHT;
// The cabinet band showing above the moulding.
const FRAME_BAND: f32 = 0.064 * FRAME_WEIGHT;

// The moulding's aperture corners, as a fraction of the opening
// half-width: rounder than the plastic around it, as the glass is. Where
// a preset clips its picture to a rounded tube face, the aperture opens
// to that face's radius instead when it is the wider of the two -- the
// moulding overlaps the glass, so it must never cut inside the face and
// leave its edge showing in the corners.
const APERTURE_RADIUS: f32 = 0.068;
// The cabinet's and the moulding's corner arcs, in opening heights: one
// radius, so every corner of both matches, top and bottom.
const R_PLASTIC: f32 = 0.036;
// How much of the moulding's width is chamfer rather than flat, and how
// far that chamfer leans back, in radians off the case front.
const CHAMFER_SPAN: f32 = 0.72;
const CHAMFER_SLOPE: f32 = 0.80;

// The cabinet and the moulding clipped into it, in linear light: sRGB
// #bec3c9 and #8b908c, sampled off a photograph of a real one.
const CASE: vec3<f32> = vec3<f32>(0.5150, 0.5459, 0.5842);
const MOULDING: vec3<f32> = vec3<f32>(0.2581, 0.2789, 0.2622);
// The logotype's ink: Pantone 286, the blue the 1084's printing used.
const INK: vec3<f32> = vec3<f32>(0.0, 0.0331, 0.3516);
// The model badge reads paler and greyer than the logotype: #7b94bc.
const BADGE_INK: vec3<f32> = vec3<f32>(0.1946, 0.2874, 0.5029);
// The lamp and the dark window it sits in.
const LAMP: vec3<f32> = vec3<f32>(0.72, 0.035, 0.014);
const LAMP_WELL: vec3<f32> = vec3<f32>(0.014, 0.012, 0.012);
// Moulded legends: the POWER caption and the standby mark.
const LEGEND: vec3<f32> = vec3<f32>(0.10, 0.10, 0.095);

// Room light, from above and a little to the left, in front of the glass.
const LIGHT: vec3<f32> = vec3<f32>(-0.1, -0.5, 0.86);

// A surface's tone relative to a flat front face, which comes out 1.0. The
// response is deliberately shallow: moulded plastic scatters, so the well
// reads as a shallow pit, not a chrome trough.
fn tone(n: vec3<f32>) -> f32 {
    let l = normalize(LIGHT);
    return 0.42 + 0.58 * clamp(dot(n, l), 0.0, 1.0) / l.z;
}

// The normal of a chamfer sloping up from the glass, given the outward
// direction across it. It leans back against that direction, so the run
// above the tube shades and the run below catches the light.
fn chamfer_normal(outward: vec2<f32>, slope: f32) -> vec3<f32> {
    return vec3<f32>(-outward * sin(slope), cos(slope));
}

// --- the Copperline mark ----------------------------------------------
//
// A narrow C with a bar driven right through it, drawn from signed
// distances rather than a bitmap so its curve stays clean at any size, and
// filled in polished copper. `r` is the ring's radius in physical pixels.
// The logotype's cap height as a fraction of the chin's, and where its
// middle sits down the panel. The model badge stays on the panel's own
// centre: the real front carries the plate centred in its section and the
// maker's badge riding lower.
const LOGO_CAP: f32 = 0.27;
const LOGO_DROP: f32 = 0.56;

const MARK_SQUASH: f32 = 0.84;
const MARK_REACH: f32 = 1.28;
const MARK_MOUTH: f32 = 0.44;
const MARK_BAR: f32 = 0.28;
const MARK_SIDE: f32 = 0.46;

// Coverage of the mark at a fragment, and how far down it the fragment
// sits, for the metal gradient.
fn mark_field(d: vec2<f32>, r: f32) -> vec2<f32> {
    let w = r * 0.50;
    // The ring, narrowed and with its mouth cut out on the right. Its
    // stroke carries more weight through the sides than over the top and
    // under the foot, the way a drawn letter does.
    let dd = vec2<f32>(d.x / MARK_SQUASH, d.y);
    let rad = max(length(dd), 1e-4);
    let wl = w * (1.24 - MARK_SIDE + MARK_SIDE * abs(dd.x / rad));
    var ring = abs(rad - r) - wl * 0.5;
    if dd.x > 0.0 && abs(d.y) < r * MARK_MOUTH {
        ring = 1e6;
    }
    // The bar, crossing the whole letter and reaching past both sides.
    let bar = rounded_rect(d, vec2<f32>(r * MARK_REACH, w * MARK_BAR), w * 0.10);
    let sd = min(ring, bar);
    return vec2<f32>(
        1.0 - smoothstep(-0.5, 0.6, sd),
        clamp(d.y / (2.0 * r) + 0.5, 0.0, 1.0),
    );
}

// One catchlight: a small core with a horizontal and a vertical streak,
// the glint polished metal throws off a cut edge.
fn spark(q: vec2<f32>, s: f32, reach: f32) -> f32 {
    let core = pow(1.0 - clamp(length(q) / s, 0.0, 1.0), 2.2);
    let across = pow(1.0 - clamp(abs(q.x) / (s * reach), 0.0, 1.0), 2.0)
        * pow(1.0 - clamp(abs(q.y) / (s * 0.30), 0.0, 1.0), 1.4);
    let down = pow(1.0 - clamp(abs(q.y) / (s * reach * 0.62), 0.0, 1.0), 2.0)
        * pow(1.0 - clamp(abs(q.x) / (s * 0.26), 0.0, 1.0), 1.4);
    return clamp(core + 0.85 * across + 0.85 * down, 0.0, 1.0);
}

// Both ends of the bar, then a third and quieter one inside the bowl, on
// the right of the letter's spine. That one is drawn tight so it stays its
// own point of light rather than smearing into its neighbour.
fn mark_sparks(d: vec2<f32>, r: f32) -> f32 {
    let a = spark(d + vec2<f32>(r * MARK_REACH, 0.0), r * 0.34, 3.2);
    let b = spark(d - vec2<f32>(r * MARK_REACH, 0.0), r * 0.34, 3.2);
    let c = 0.58 * spark(d + vec2<f32>(r * 0.34, 0.0), r * 0.22, 1.5);
    return max(max(a, b), c);
}

// The glint's own colour: a warm near-white, so the spark reads as light
// on metal rather than a paler paint.
const SPARK_LIGHT: vec3<f32> = vec3<f32>(1.0, 0.8388, 0.7011);

// Polished copper down the mark: the metal itself, not gold -- a red-brown
// body with one pink-warm sheen where the surface turns, deepening to a
// dark oxide at the foot.
fn copper(t: f32) -> vec3<f32> {
    let top = vec3<f32>(0.3250, 0.0865, 0.0273);
    let sheen = vec3<f32>(0.6105, 0.2346, 0.1022);
    let mid = vec3<f32>(0.2519, 0.0578, 0.0168);
    let lo = vec3<f32>(0.0759, 0.0168, 0.0048);
    if t < 0.40 {
        return mix(top, sheen, 1.0 - smoothstep(0.24, 0.40, t));
    }
    if t < 0.60 {
        return mix(sheen, mid, smoothstep(0.40, 0.60, t));
    }
    return mix(mid, lo, smoothstep(0.60, 1.0, t));
}

// --- lettering ---------------------------------------------------------
//
// Three rasterised strings, packed a bit to a cell, LSB-first from the
// left of each row. Each carries the cap height it was drawn to, so text
// is sized by its capitals rather than by whatever cell box its face
// happened to need, and the model badge carries where its ink starts and
// stops so a frame can centre on the lettering rather than on the bearings
// its digits happen to have.

// MARK: 67 x 11 cells, cap 9, ink 0..67
// ..#####.......................................##.##................
// .##..##.......................................##.##................
// ##............................................##...................
// ##.......#####..######..######...#####..##.##.##.##.##.###...#####.
// ##......##...##.##...##.##...##.##...##.#####.##.##.#######.##...##
// ##......##...##.##...##.##...##.#######.##....##.##.##...##.#######
// ##......##...##.##...##.##...##.##......##....##.##.##...##.##.....
// .##..##.##...##.##...##.##...##.##...##.##....##.##.##...##.##...##
// ..#####..#####..######..######...#####..##....##.##.##...##..#####.
// ................##......##.........................................
// ................##......##.........................................
const MARK_COLS: i32 = 67;
const MARK_ROWS: i32 = 11;
const MARK_CAP: f32 = 9.0;
const MARK_INK: vec2<f32> = vec2<f32>(0.0, 67.0);
const MARK: array<vec3<u32>, 11> = array<vec3<u32>, 11>(
    vec3<u32>(0x0000007cu, 0x0006c000u, 0x00000000u),
    vec3<u32>(0x00000066u, 0x0006c000u, 0x00000000u),
    vec3<u32>(0x00000003u, 0x0000c000u, 0x00000000u),
    vec3<u32>(0x3f3f3e03u, 0xe3b6db3eu, 0x00000003u),
    vec3<u32>(0x63636303u, 0x37f6df63u, 0x00000006u),
    vec3<u32>(0x63636303u, 0xf636c37fu, 0x00000007u),
    vec3<u32>(0x63636303u, 0x3636c303u, 0x00000000u),
    vec3<u32>(0x63636366u, 0x3636c363u, 0x00000006u),
    vec3<u32>(0x3f3f3e7cu, 0xe636c33eu, 0x00000003u),
    vec3<u32>(0x03030000u, 0x00000000u, 0x00000000u),
    vec3<u32>(0x03030000u, 0x00000000u, 0x00000000u),
);

// MODEL: 43 x 13 cells, cap 13, ink 1..43
// ...###.....######......######..........###.
// ..####....########....########.......#####.
// .#####...###....###..###....###.....##.###.
// ...###...###....###..###....###....##..###.
// ...###...###....###..###....###...##...###.
// ...###...###....###...########...##########
// ...###...###....###...########...##########
// ...###...###....###...########...##########
// ...###...###....###..###....###........###.
// ...###...###....###..###....###........###.
// ...###...###....###..###....###........###.
// ...###....########....########.........###.
// ...###.....######......######..........###.
const MODEL_COLS: i32 = 43;
const MODEL_ROWS: i32 = 13;
const MODEL_CAP: f32 = 13.0;
const MODEL_INK: vec2<f32> = vec2<f32>(1.0, 43.0);
const MODEL: array<vec2<u32>, 13> = array<vec2<u32>, 13>(
    vec2<u32>(0x1f81f838u, 0x00000380u),
    vec2<u32>(0x3fc3fc3cu, 0x000003e0u),
    vec2<u32>(0x70e70e3eu, 0x000003b0u),
    vec2<u32>(0x70e70e38u, 0x00000398u),
    vec2<u32>(0x70e70e38u, 0x0000038cu),
    vec2<u32>(0x3fc70e38u, 0x000007feu),
    vec2<u32>(0x3fc70e38u, 0x000007feu),
    vec2<u32>(0x3fc70e38u, 0x000007feu),
    vec2<u32>(0x70e70e38u, 0x00000380u),
    vec2<u32>(0x70e70e38u, 0x00000380u),
    vec2<u32>(0x70e70e38u, 0x00000380u),
    vec2<u32>(0x3fc3fc38u, 0x00000380u),
    vec2<u32>(0x1f81f838u, 0x00000380u),
);

// CAPTION: 29 x 8 cells, cap 7, ink 0..29
// ####...###..#...#.#####.####.
// #...#.#...#.#...#.#.....#...#
// #...#.#...#.#...#.#.....#...#
// ####..#...#.#.#.#.####..####.
// #.....#...#.#.#.#.#.....#.#..
// #.....#...#.##.##.#.....#..#.
// #......###..#...#.#####.#...#
// .............................
const CAPTION_COLS: i32 = 29;
const CAPTION_ROWS: i32 = 8;
const CAPTION_CAP: f32 = 7.0;
const CAPTION_INK: vec2<f32> = vec2<f32>(0.0, 29.0);
const CAPTION: array<u32, 8> = array<u32, 8>(
    0x0f7d138fu,
    0x11051451u,
    0x11051451u,
    0x0f3d544fu,
    0x05055441u,
    0x0905b441u,
    0x117d1381u,
    0x00000000u,
);

fn mark_bit(c: i32, r: i32) -> f32 {
    if c < 0 || c >= MARK_COLS || r < 0 || r >= MARK_ROWS {
        return 0.0;
    }
    var rows = MARK;
    let bits = rows[r];
    if c < 32 {
        return f32((bits.x >> u32(c)) & 1u);
    }
    if c < 64 {
        return f32((bits.y >> u32(c - 32)) & 1u);
    }
    return f32((bits.z >> u32(c - 64)) & 1u);
}

fn model_bit(c: i32, r: i32) -> f32 {
    if c < 0 || c >= MODEL_COLS || r < 0 || r >= MODEL_ROWS {
        return 0.0;
    }
    var rows = MODEL;
    let bits = rows[r];
    if c < 32 {
        return f32((bits.x >> u32(c)) & 1u);
    }
    return f32((bits.y >> u32(c - 32)) & 1u);
}

fn caption_bit(c: i32, r: i32) -> f32 {
    if c < 0 || c >= CAPTION_COLS || r < 0 || r >= CAPTION_ROWS {
        return 0.0;
    }
    var rows = CAPTION;
    return f32((rows[r] >> u32(c)) & 1u);
}

// A bitmap sampled bilinearly at a continuous position in cells, then
// taken through a threshold: the printing keeps its weight at any size
// rather than fading to a smear.
fn cover(v00: f32, v10: f32, v01: f32, v11: f32, t: vec2<f32>) -> f32 {
    return smoothstep(0.34, 0.62, mix(mix(v00, v10, t.x), mix(v01, v11, t.x), t.y));
}

fn mark_sample(q: vec2<f32>) -> f32 {
    let f = q - vec2<f32>(0.5);
    let base = floor(f);
    let t = f - base;
    let c = i32(base.x);
    let r = i32(base.y);
    return cover(
        mark_bit(c, r), mark_bit(c + 1, r), mark_bit(c, r + 1), mark_bit(c + 1, r + 1), t);
}

fn model_sample(q: vec2<f32>) -> f32 {
    let f = q - vec2<f32>(0.5);
    let base = floor(f);
    let t = f - base;
    let c = i32(base.x);
    let r = i32(base.y);
    return cover(
        model_bit(c, r), model_bit(c + 1, r), model_bit(c, r + 1), model_bit(c + 1, r + 1), t);
}

fn caption_sample(q: vec2<f32>) -> f32 {
    let f = q - vec2<f32>(0.5);
    let base = floor(f);
    let t = f - base;
    let c = i32(base.x);
    let r = i32(base.y);
    return cover(
        caption_bit(c, r), caption_bit(c + 1, r),
        caption_bit(c, r + 1), caption_bit(c + 1, r + 1), t);
}

// Whether a fragment lands on a string's cell box, so the samplers are
// only paid for where lettering could be.
fn on_text(q: vec2<f32>, cols: i32, rows: i32) -> bool {
    return all(q > vec2<f32>(-1.0)) && all(q < vec2<f32>(f32(cols), f32(rows)) + 1.0);
}

// --- the chin panel ----------------------------------------------------
//
// The cabinet below the moulding: the seam it sits on, the panel joints,
// the engraved model badge, the mark and logotype, and the power lamp.
fn chin(
    px: vec2<f32>,
    case_org: vec2<f32>,
    case_size: vec2<f32>,
    moulding_org: vec2<f32>,
    opening: vec2<f32>,
    top: f32,
    base: vec3<f32>,
) -> vec3<f32> {
    let h = case_org.y + case_size.y - top;
    var col = base * 1.02;

    // The seam the moulding sits on: a groove, with the panel's own top
    // edge catching light just below it.
    let below = px.y - top;
    col *= mix(0.46, 1.0, smoothstep(0.0, 0.085 * h, below));
    col *= 1.0 + 0.07 * (1.0 - smoothstep(0.10 * h, 0.26 * h, below));

    // Vertical joints: the real front is moulded in sections. The power
    // panel's outer joint meets the tube's own right edge, so the two read
    // as one design rather than two measurements.
    let power_right = opening.x + opening.y;
    let power_left = power_right - 0.066 * case_size.x;
    let joints = array<f32, 3>(case_org.x + 0.340 * case_size.x, power_left, power_right);
    for (var i = 0; i < 3; i++) {
        col *= mix(0.66, 1.0, smoothstep(0.0, 1.5, abs(px.x - joints[i]) - 0.5));
    }

    let mid = top + h * 0.50;

    // The model badge: its frame starts flush with the moulding's own left
    // edge, and the lettering sits dead centre in it with a margin either
    // side just shy of another character's width. Top and bottom it nearly
    // touches. The margins are quoted against the printed cap height, not
    // the cell box, so the face's grid does not change the look.
    let bs = h * 0.38 / MODEL_CAP;
    let bcap = 0.5 * MODEL_CAP * bs;
    let ink_w = (MODEL_INK.y - MODEL_INK.x) * bs;
    let bhalf = vec2<f32>(ink_w * 0.5 + bcap * 1.56, bcap * 1.20);
    let bc = vec2<f32>(moulding_org.x + bhalf.x, mid);
    let borg = vec2<f32>(bc.x - ink_w * 0.5 - MODEL_INK.x * bs, mid - bcap);
    let db = rounded_rect(px - bc, bhalf, bs * 0.8);
    col *= mix(0.78, 1.0, smoothstep(0.0, 1.4, abs(db) - 0.6));
    if db < 0.0 {
        col *= 0.99;
    }
    // Gated on the printed height, not the cell size: a finer face has
    // more cells to the same capital.
    if 2.0 * bcap >= 7.0 {
        let q = (px - borg) / bs;
        if on_text(q, MODEL_COLS, MODEL_ROWS) {
            // Laid down at full strength so the badge prints the colour it
            // is given; the glyph's own coverage still softens its edges.
            col = mix(col, BADGE_INK, model_sample(q));
        }
    }

    // The mark and the logotype, where the 1084 wears its maker's badge:
    // set as one group and centred together.
    // The mark and the logotype sit a little below the panel's middle and
    // a little under the badge's size, as the printed one does.
    let logo_mid = top + h * LOGO_DROP;
    let ls = h * LOGO_CAP / MARK_CAP;
    let lw = f32(MARK_COLS) * ls;
    let mr = h * LOGO_CAP * 0.62;
    let mark_w = 2.0 * MARK_REACH * mr;
    let gap = mr * 0.42;
    let gx = case_org.x + case_size.x * 0.47 - (mark_w + gap + lw) * 0.5;
    if ls >= 0.6 {
        let d = px - vec2<f32>(gx + mark_w * 0.5, logo_mid);
        let m = mark_field(d, mr);
        if m.x > 0.0 {
            col = mix(col, copper(m.y), m.x);
        }
        let glint = mark_sparks(d, mr);
        if glint > 0.0 {
            col = mix(col, SPARK_LIGHT, 0.92 * glint);
        }
        let lorg = vec2<f32>(gx + mark_w + gap, logo_mid - 0.5 * MARK_CAP * ls);
        let q = (px - lorg) / ls;
        if on_text(q, MARK_COLS, MARK_ROWS) {
            col = mix(col, INK, 0.95 * mark_sample(q));
        }
    }

    // Power: the lamp in its own dark window, with the caption and the
    // standby mark beneath, in the narrow panel at the right.
    let pcx = (power_left + power_right) * 0.5;
    let lamp_c = vec2<f32>(pcx, top + h * 0.24);
    let lamp_h = max(h * 0.085, 1.0);
    let win = rounded_rect(px - lamp_c, vec2<f32>(lamp_h * 2.1, lamp_h), lamp_h * 0.4);
    col = mix(col, LAMP_WELL, 1.0 - smoothstep(-0.7, 0.7, win));
    let led = rounded_rect(px - lamp_c, vec2<f32>(lamp_h * 1.5, lamp_h * 0.5), lamp_h * 0.25);
    col = mix(col, LAMP, 1.0 - smoothstep(-0.5, 0.8, led));

    let ps = h * 0.145 / CAPTION_CAP;
    if ps >= 0.42 {
        // Below about a cell a pixel the stencil cannot resolve, so it is
        // let down in weight instead of dropped: a legend too small to
        // read is still the mark that belongs under the lamp.
        let weight = 0.7 * mix(0.55, 1.0, smoothstep(0.42, 0.95, ps));
        let corg = vec2<f32>(pcx - f32(CAPTION_COLS) * ps * 0.5, top + h * 0.46);
        let q = (px - corg) / ps;
        if on_text(q, CAPTION_COLS, CAPTION_ROWS) {
            col = mix(col, LEGEND, weight * caption_sample(q));
        }
    }

    // IEC 5009 standby mark: a broken ring with an upright bar.
    let sr = max(h * 0.13, 1.4);
    let d = px - vec2<f32>(pcx, top + h * 0.76);
    let ring = abs(length(d) - sr) - sr * 0.15;
    let bar = rounded_rect(d + vec2<f32>(0.0, sr * 0.34),
                           vec2<f32>(sr * 0.14, sr * 0.60), sr * 0.13);
    var standby = min(ring, bar);
    if d.y < 0.0 && abs(d.x) < sr * 0.40 {
        standby = bar;
    }
    return mix(col, LEGEND, 0.8 * (1.0 - smoothstep(-0.4, 0.7, standby)));
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let vp = u.size.xy;
    let px = in.uv * vp;

    // Opening geometry in physical pixels. Everything else on the front
    // is placed off it, in units of its height.
    let o_org = u.opening.xy * vp;
    let o_size = u.opening.zw * vp;
    let o_half = max(o_size * 0.5, vec2<f32>(1.0));
    let centre = o_org + o_half;
    let p = px - centre;
    let unit = o_size.y;

    // The cabinet fills the display rect, as a monitor's front fills the
    // window it stands in: every vertical proportion is the design's, and
    // the slack its narrower shape would have left either side goes into
    // the side pillars, which is where a real cabinet carries it.
    let case_org = vec2<f32>(0.0);
    let case_size = vp;
    let chin_top = vp.y - FRAME_CHIN * unit;

    // --- the glass contour ---
    // The same warp as crt.wgsl maps the opening onto the source frame,
    // where the face is the source rectangle with the tube's rounded
    // corners, so with the preset's curvature in params.y this distance
    // coincides with the preset's face boundary exactly -- the moulding
    // seats the tube whatever its bow -- and at zero curvature it reduces
    // to a plain rounded opening for the flat presets. Scaling by the
    // half-width turns source half-width units into approximate pixels;
    // the warp's local scale varies a few percent around the perimeter, as
    // a real moulding gap does.
    // Faded by mixing coordinates, exactly as crt.wgsl fades its own warp
    // -- warping by a faded curvature is a different curve, so the two
    // would drift apart between strengths 0 and 1.
    let k = u.params.y;
    let fa = o_half.y / max(o_half.x, 1.0);
    let cn = p / o_half;
    let q = k * 0.25;
    let r2 = cn.x * cn.x + cn.y * cn.y * fa * fa;
    let m = vec2<f32>(1.0 + q, 1.0 + q * fa * fa);
    let wc = mix(cn, cn * (1.0 + q * r2) / m, u.params.w);
    let fh = vec2<f32>(1.0, fa);
    let gp = wc * fh;
    let r_aperture = max(APERTURE_RADIUS, u.params.z);
    let d_glass = rounded_rect(gp, fh, r_aperture) * o_half.x;
    let n_glass = rounded_rect_grad(gp, fh, r_aperture);
    let aa = max(fwidth(d_glass), 1e-4);

    // Frame-only pass: a CRT preset has already painted the opening
    // interior (drawn before this pass, on the opening's bounding box),
    // so leave every interior fragment to it and repaint just the frame,
    // whose rounded corners and chamfer must sit on top of the preset's
    // square viewport -- the plastic overlaps the tube, not the other way
    // round.
    if u.params.x > 0.5 && d_glass < 0.0 {
        discard;
    }

    // --- the cabinet outline ---
    // Barely rounded, like the real one: enough to catch the light on a
    // corner, not enough to read as a lozenge.
    let c_half = case_size * 0.5;
    let cp = px - (case_org + c_half);
    let r_plastic = R_PLASTIC * unit;
    let d_case = rounded_rect(cp, c_half, r_plastic);
    let aa_case = max(fwidth(d_case), 1e-4);

    // --- the moulding outline ---
    // A separate part clipped into the cabinet, its corners echoing the
    // cabinet's exactly. It keeps its designed run around the tube
    // whatever the pillars do: inset from the opening by the frame's side
    // margin less the band the cabinet shows around it, dropped from the
    // cabinet's top edge by that band, and bottomed by the chin seam.
    let m_side = (FRAME_SIDE - FRAME_BAND) * unit;
    let m_org = vec2<f32>(o_org.x - m_side, FRAME_BAND * unit);
    let m_half = vec2<f32>(o_size.x * 0.5 + m_side, (chin_top - m_org.y) * 0.5);
    let mp = px - (m_org + m_half);
    let d_moulding = rounded_rect(mp, m_half, r_plastic);

    // --- the base plastic ---
    // The cabinet lightens toward the top, where the case rolls over into
    // its lit top surface, and carries a fine moulding grain throughout.
    let up = clamp((px.y - case_org.y) / max(case_size.y, 1.0), 0.0, 1.0);
    let g = 1.0 + 0.018 * grain(floor(px));
    let case_plastic = CASE * mix(1.06, 0.94, pow(up, 0.75)) * g;
    let moulding_plastic = MOULDING * mix(1.03, 0.96, up) * g;

    // --- the picture, scaled to fill the opening ---
    let pic_uv = clamp((in.uv - u.opening.xy) / max(u.opening.zw, vec2<f32>(1e-4)),
                       vec2<f32>(0.0), vec2<f32>(1.0));
    let picture = sample_display(pic_uv).rgb;

    // --- the frame surface at this fragment ---
    var frame: vec3<f32>;
    if px.y >= chin_top {
        frame = chin(px, case_org, case_size, m_org,
                     vec2<f32>(o_org.x, o_size.x), chin_top, case_plastic);
    } else if d_moulding >= 0.0 {
        // The cabinet face outside the moulding, and the panel gap the
        // moulding drops into: a fine dark line, with the cabinet's own
        // edge catching light just outside it.
        frame = case_plastic * mix(0.50, 1.0, smoothstep(0.0, 1.8, d_moulding - 0.6));
        frame *= 1.0 + 0.06 * (1.0 - smoothstep(1.8, 4.0, d_moulding));
    } else {
        // Crossing the moulding is measured as a fraction, glass (0) to
        // gap (1), so the short run below the tube compresses the way the
        // real part does instead of spilling into the chin.
        let span = max(d_glass - d_moulding, 1e-3);
        let t = clamp(d_glass / span, 0.0, 1.0);
        // The chamfer that holds the tube rolls over into the flat of the
        // moulding rather than creasing, so the two share one normal that
        // turns through the join.
        let roll = smoothstep(CHAMFER_SPAN - 0.04, CHAMFER_SPAN + 0.04, t);
        let n = normalize(mix(chamfer_normal(n_glass, CHAMFER_SLOPE),
                              vec3<f32>(0.0, 0.0, 1.0), roll));
        // Less of the room reaches the foot of the slope than its head.
        let occ = mix(0.82, 1.0, smoothstep(0.0, CHAMFER_SPAN, t));
        frame = moulding_plastic * tone(n) * occ;
        // The cabinet stands proud of the moulding and shades it just
        // inside the gap.
        frame *= mix(0.68, 1.0, smoothstep(0.0, 0.026 * unit, -d_moulding));
        // The contact line where the moulding overlaps the tube.
        frame *= mix(0.62, 1.0, smoothstep(0.0, 0.008 * unit, d_glass));
    }

    // The moulded outer edge: the face rolls into shadow just inside the
    // outline, and outside it is whatever backs the window (the letterbox
    // black), so the cabinet reads as an object with room behind it.
    // Whichever edge faces up catches the light coming over the case.
    let n_case = rounded_rect_grad(cp, c_half, r_plastic);
    frame *= 1.0 - 0.34 * smoothstep(-0.012 * unit, 0.0, d_case);
    frame *= 1.0 + 0.42 * max(-n_case.y, 0.0)
        * smoothstep(-0.018 * unit, -0.004 * unit, d_case);

    // Compose by signed distance: picture, then frame. In the frame-only
    // pass the inner side of the opening join belongs to the preset
    // underneath, whose face edge there is its off-face black: blending
    // from the raw picture instead smears a picture-coloured hairline
    // around the opening, on top of the preset the pass discarded for.
    let inner = select(picture, vec3<f32>(0.0), u.params.x > 0.5);
    var col = mix(inner, frame, clamp(d_glass / aa + 0.5, 0.0, 1.0));
    col = mix(col, vec3<f32>(0.0), clamp(d_case / aa_case + 0.5, 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}

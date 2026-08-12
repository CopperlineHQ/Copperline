// SPDX-License-Identifier: GPL-3.0-or-later
//
// Monitor-bezel pass: the front of the 1084 the Amiga shipped with, drawn
// from photographs of a real cabinet.
//
// Read from the outside in, the front is four things. A thin outer frame
// in warm greige plastic, deeper across the top than down the sides. A
// groove all round it, cut in section -- a wall off each moulding and a
// floor between them. Then the inner bezel, a separate and much darker
// moulding, flat across its face until it funnels back to the tube down
// four mitred planes. And below all of it the chin, standing forward of
// the front behind a shadowed turn: the striped model badge let into the
// panel on the left, the maker's segment carrying the Copperline mark in
// the middle, and the power switch as its own square button on the right,
// lamp above caption above standby mark.
//
// Every corner on it is square and mitred but two: the cabinet's own
// outermost four, drawn off the tool with the smallest of radii, and the
// tube's face, which curves only when a CRT preset bows it.
//
// The cabinet fills the display rect, as a monitor's front fills the window
// it stands in. Everything inside it is placed off the picture opening
// `bezel.rs` chose, in units of that opening's height, by frame proportions
// measured off a straight-on photograph (see FRAME_* below); the chin's
// own furniture is laid out in fractions of the chin and the case, so the
// panel composes the same at any window size or weight.
//
// Two modes, selected by params.x. Alone (0), this one pass draws both the
// frame and the picture. Under a CRT preset (1, frame-only), the preset
// has already painted the picture into the opening's bounding box and this
// pass runs after it, discarding the opening interior and drawing just the
// frame on top -- the recess overlaps the tube face, so its rounded
// corners and shadow clip the preset's square viewport.
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
// keeps.
//
// `bezel.rs` places the opening from the vertical three, and this pass is
// handed the result rather than deriving it, so FRAME_TOP,
// FRAME_WELL_BOTTOM and FRAME_SIDE go unread here. They are stated anyway,
// beside the ones this pass does use, so that the whole set of the
// cabinet's proportions lives in one place and a test can hold the two
// files to the same numbers.
const FRAME_WEIGHT: f32 = 0.86;
const FRAME_TOP: f32 = 0.1600 * FRAME_WEIGHT;
const FRAME_WELL_BOTTOM: f32 = 0.1170 * FRAME_WEIGHT;
const FRAME_CHIN: f32 = 0.1356 * FRAME_WEIGHT;
// The design's side margin: cabinet edge to glass, beside the tube.
const FRAME_SIDE: f32 = 0.1780 * FRAME_WEIGHT;
// The outer frame's width and the groove between it and the inner bezel.
// FRAME_BAND is the two together: everything outside the inner bezel.
//
// Two of each, because the frame is not uniform: the cabinet carries a
// deeper band across the top than down the sides, which is what stops the
// front reading as a picture mount. The gap itself is the same width all
// round, and runs under the inner bezel as well as beside and above it.
const FRAME_BAND: f32 = 0.0700 * FRAME_WEIGHT;
const FRAME_BAND_TOP: f32 = 0.0840 * FRAME_WEIGHT;
const FRAME_OUTER: f32 = 0.0500 * FRAME_WEIGHT;

// The recess aperture's corners, as a fraction of the opening half-width.
// Where a preset clips its picture to a rounded tube face, the aperture
// opens to that face's radius when it is the wider: the plastic overlaps
// the glass, so it must never cut inside the face and leave its edge
// showing in the corners.
const APERTURE_RADIUS: f32 = 0.090;
// The groove's cut sides: how much of its width each takes, and how far
// each leans off the front. Steep, because the channel is cut square and
// what little of its side shows is nearly edge-on.
const GROOVE_WALL: f32 = 0.30;
const GROOVE_SLOPE: f32 = 1.36;
// The cabinet's corner arcs and the inner bezel's, in opening heights.
// The front is cut and mitred throughout, so the inner bezel's corners
// are square; the cabinet's outermost four are the one exception, taking
// the smallest of radii where the moulding is drawn off the tool.
const R_PLASTIC: f32 = 0.018;
const R_REVEAL: f32 = 0.0;

// --- colours, in linear light ------------------------------------------
//
// The case is the warm greige the cabinet has aged to, not the cool grey
// it left the factory in: sampled off the photographs. sRGB originals in
// the comments.
const CASE: vec3<f32> = vec3<f32>(0.4287, 0.4397, 0.4233); // #afb1ae
// The chin is the same plastic as the outer frame. Its top face rolls
// over rather than meeting the front square, so a few pixels along that
// edge turn toward the room and read brighter than either face.
const CHIN_LIP: vec3<f32> = vec3<f32>(0.4287, 0.4397, 0.4233);
// The inner bezel's flat face: a separate, darker moulding than the
// outer frame.
const MOULDING: vec3<f32> = vec3<f32>(0.1529, 0.1413, 0.1119); // #6d695e
// The sink -- the wall dropping from that face to the tube -- is the same
// plastic, not a colour of its own: it reads darker because it turns away
// from the room, so it is derived here rather than sampled. Only CASE and
// MOULDING come off the cabinet; everything else on the front is one of
// those two under a different light.
const SINK: vec3<f32> = MOULDING * 0.72;
// The logotype's ink: the deep blue of the printed front, #0d2656.
const INK: vec3<f32> = vec3<f32>(0.0040, 0.0194, 0.0931);
// The model badge's striped digits: a muted slate blue, #354366.
const BADGE_INK: vec3<f32> = vec3<f32>(0.0356, 0.0561, 0.1329);
// Printed legends on the power button: near-black ink, #0a0a0a.
const LEGEND: vec3<f32> = vec3<f32>(0.0030, 0.0030, 0.0030);
// The lamp and the dark window it sits in.
const LAMP: vec3<f32> = vec3<f32>(0.6445, 0.0012, 0.0252); // #d2042c
const LAMP_WELL: vec3<f32> = vec3<f32>(0.014, 0.012, 0.012);

// The seams between the chin's panels: narrow grooves in one piece of
// plastic rather than a joint between two, so they take a flat dark value
// and no section of their own.
const GAP_FLOOR: vec3<f32> = vec3<f32>(0.0410, 0.0400, 0.0360);
// The groove between the two mouldings, in section. Its floor is the
// deepest thing on the front, and the two cut sides above it are one
// plastic: which one lifts is left to the facing, so a single colour
// serves both walls. Kept well down toward the floor -- a cut edge is in
// the channel's own shadow, and giving it the face's tone would turn a
// ten-pixel groove into two bright lines with a dark one between them.
const GROOVE_FLOOR: vec3<f32> = vec3<f32>(0.0300, 0.0292, 0.0263);
const GROOVE_CUT: vec3<f32> = vec3<f32>(0.1080, 0.1035, 0.0930);

// Room light, from above and a little to the left, in front of the glass.
const LIGHT: vec3<f32> = vec3<f32>(-0.1, -0.5, 0.86);

// How much of the room between the inner bezel's edge and the tube the
// recess wall takes, per axis; the rest stays flat face. Fractions of the
// room actually there, which `recess_walls` measures off the geometry --
// the sides carry about twice the room the top and bottom do, because the
// picture is the viewport's aspect while the margins are stated in its
// height.
const CHAMFER_SPAN: f32 = 0.47;
const CHAMFER_SPAN_X: f32 = 0.52;
const CHAMFER_SLOPE: f32 = 1.16;

// How far the recess wall reaches beside and above the tube, in physical
// pixels, from the room the cabinet leaves between the inner bezel's edge
// and the glass. The vertical takes the smaller of the runs above and
// below the tube, so one mouth fits both.
//
// The vertical measures both of those runs to lines the moulding does not
// actually reach: the top to the side band rather than the deeper band it
// carries above the tube, the bottom to the chin seam rather than to where
// the moulding stops short of it. That is deliberate and must stay.
// Measured that way, both runs come out pure multiples of the opening
// height, so the mouth is the same fraction of the front at every window
// size and the design scales as one piece. Measured to where the moulding
// really ends, the bottom run picks up `ledge_h`'s constant pixel floor,
// the two runs cross over on small windows, and the funnel's depth starts
// drifting with size -- rendered and measured, the honest version comes
// out *less* proportional than this one, not more. What these two names
// mean, then, is the room the design is laid out against, not the room
// the plastic leaves; `CHAMFER_SPAN` is picked to suit.
fn recess_walls(o_org: vec2<f32>, o_size: vec2<f32>, inset: f32, chin_top: f32) -> vec2<f32> {
    let room_x = max(o_org.x - inset, 1.0);
    let room_top = max(o_org.y - inset, 1.0);
    let room_bottom = max(chin_top - (o_org.y + o_size.y), 1.0);
    let room_y = min(room_top, room_bottom);
    return vec2<f32>(CHAMFER_SPAN_X * room_x, CHAMFER_SPAN * room_y);
}

// A surface's tone relative to a flat front face, which comes out 1.0.
// Shallow, as moulded plastic is: the recess reads as a shadowed pit, not
// a chrome trough.
fn tone(n: vec3<f32>) -> f32 {
    let l = normalize(LIGHT);
    return 0.52 + 0.48 * clamp(dot(n, l), 0.0, 1.0) / l.z;
}

// The normal of a slope rising from the glass, given the outward direction
// across it: it leans back against that direction, so the run above the
// tube shades and the run below catches the light.
fn chamfer_normal(outward: vec2<f32>, slope: f32) -> vec3<f32> {
    return vec3<f32>(-outward * sin(slope), cos(slope));
}

// --- the chin's lettering, sized and placed -----------------------------
//
// The maker's name, in cap heights of the chin's own height, and where its
// middle sits down the panel: below centre, as the real front carries it.
const LOGO_CAP: f32 = 0.30;
const LOGO_DROP: f32 = 0.64;

// --- lettering ---------------------------------------------------------
//
// Three rasterised strings, packed a bit to a cell, LSB-first from the
// left of each row. Each carries the cap height it was drawn to, so text
// is sized by its capitals rather than by whatever cell box its face
// happened to need. The model badge carries one thing more -- where its
// ink starts and stops -- because its plate centres on the lettering
// rather than on whatever bearings its digits happen to have.

// MARK: 67 x 11 cells, cap 9
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

// CAPTION: 29 x 8 cells, cap 7
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
// Laid out in fractions of the chin height and the cabinet width, so the
// same front composes at any size: the badge plate let into the panel on
// the left, the maker's segment between its two seams in the middle, and
// the power button on the right with the lamp over the caption over the
// standby mark. Sizes in physical pixels arrive through `unit`.
//
// The standby mark: a broken ring with a bar dropped through the gap, as
// the moulded front prints it. `r` in physical pixels.
fn standby(q: vec2<f32>, r: f32) -> f32 {
    let w = max(r * 0.34, 1.1);
    var ring = abs(length(q) - r) - w * 0.5;
    // The gap at the top, for the bar.
    if q.y < -r * 0.30 && abs(q.x) < r * 0.55 {
        ring = 1e6;
    }
    let bar = rounded_rect(q + vec2<f32>(0.0, r * 0.45), vec2<f32>(w * 0.5, r * 0.75), w * 0.25);
    let sd = min(ring, bar);
    return 1.0 - smoothstep(-0.5, 0.6, sd);
}

// The chin's panel seams, in physical pixels across the front: the flap's
// left edge, the joint where the flap meets the power button, and the
// button's right edge. The joint is named twice because it is two edges
// meeting -- two mouldings, not one moulding cut -- and reads that much
// deeper for being cut twice.
fn chin_seams(cw: f32, recess: f32) -> vec4<f32> {
    let joint = recess - 0.0735 * cw;
    return vec4<f32>(0.335 * cw, joint, joint, recess);
}

// How wide those seams are cut, for a chin of height `ch`.
fn chin_seam_width(ch: f32) -> f32 {
    return clamp(0.030 * ch, 1.4, 4.0);
}

// How far a seam leans across the chin's turn, as a fraction of the turn's
// height. The turn is a face going away from the room at forty-five
// degrees, and a gap cut straight down through it does not read as
// straight down: it runs back with the surface. Leaning the seam is what
// makes the turn read as a turn between the groove and the chin, rather
// than as a band of lighter plastic with lines ruled across it.
const LEDGE_SEAM_LEAN: f32 = 0.55;
// Which way each seam of `chin_seams` leans on the way up, and by how much
// of `LEDGE_SEAM_LEAN` -- the sign is the direction, the magnitude the
// share. The flap's outer edge runs one way and the power button's pair
// the other, so the two pieces read as set into a receding face rather
// than sliding across a flat one; the flap's takes the longer run because
// it is the one seam with the width of the front behind it.
const LEDGE_SEAM_LEAN_DIR: vec4<f32> = vec4<f32>(1.3, -1.0, -1.0, -1.0);

// The whole chin: everything below the recess. `p` is the fragment in
// physical pixels from the cabinet's top-left, `org`/`size` the chin
// band's rectangle in the same space, `unit` the opening height.
fn chin(p: vec2<f32>, org: vec2<f32>, size: vec2<f32>, unit: f32, inset: f32, recess: f32, base: vec3<f32>) -> vec3<f32> {
    var colour = base;
    let q = p - org;
    let ch = size.y;
    let cw = size.x;
    let aa = 1.0;

    // The rolled top edge: a bright lip where the band's top face catches
    // the room, then the seam's shadow under the recess above it.
    let lip_h = clamp(0.055 * ch, 1.0, 4.0);
    if q.y < lip_h {
        colour = mix(CHIN_LIP * 1.06, colour, smoothstep(0.15 * lip_h, 1.0 * lip_h, q.y));
    }

    // --- the maker's segment, its seams, and the power button ----------
    // The segment sits between two seams; the power button is its own
    // piece near the right edge with a seam either side. Seams are narrow
    // grooves: dark, with a lit right wall.
    // The power button's right edge sits on the line where the inner
    // bezel begins to fall away to the tube -- not on the moulding's outer
    // edge; the flap's seam meets the button where it begins.
    let btn_w = 0.0735 * cw;
    let btn_r = recess;
    let btn_c = btn_r - btn_w * 0.5;
    let btn_hw = btn_w * 0.5;
    let seam_w = chin_seam_width(ch);
    let seams = chin_seams(cw, recess);
    for (var i = 0; i < 4; i = i + 1) {
        let groove = 1.0 - smoothstep(0.2 * seam_w, 1.0 * seam_w, abs(q.x - seams[i]));
        colour = mix(colour, GAP_FLOOR, groove * 0.80);
    }

    // --- the model badge, let into the panel on the left ---------------
    // A recessed plate with the model number in striped digits: the
    // moulding prints 1084 as a stack of hairlines, and reads paler and
    // greyer than the logotype.
    // Hard against the outer frame's inner edge: the badge stops where
    // that moulding stops, with the gap left clear beside it.
    let badge_l = FRAME_OUTER * unit;
    let badge_w = 0.108 * cw;
    let badge_c = vec2<f32>(badge_l + badge_w * 0.5, 0.625 * ch);
    let badge_half = vec2<f32>(badge_w * 0.5, 0.250 * ch);
    let d_badge = rounded_rect(q - badge_c, badge_half, 0.0);
    if d_badge < 2.0 * aa {
        // A shallow square-cornered recess with the digits in it: the
        // floor a touch darker than the panel, its wall shaded along the
        // top and left where the plastic drops away and lit along the
        // bottom and right where the floor turns back up.
        colour = mix(colour, colour * 0.94, 1.0 - smoothstep(-1.5 * aa, 0.5 * aa, d_badge));
        let wall = 1.0 - smoothstep(0.35 * aa, 2.2 * aa, abs(d_badge));
        let top_left = step(q.y, badge_c.y) * 0.6 + step(q.x, badge_c.x) * 0.4;
        colour = mix(colour, colour * mix(1.14, 0.68, clamp(top_left, 0.0, 1.0)), wall * 0.9);

        // The digits, striped: ink rows alternating with the plate.
        let cap = 1.18 * badge_half.y;
        let cell = cap / MODEL_CAP;
        let size_px = vec2<f32>(f32(MODEL_COLS), f32(MODEL_ROWS)) * cell;
        let ink_w = (MODEL_INK.y - MODEL_INK.x) * cell;
        let org_px = badge_c - vec2<f32>(ink_w * 0.5 + MODEL_INK.x * cell, size_px.y * 0.5);
        let g = (q - org_px) / cell;
        if on_text(g, MODEL_COLS, MODEL_ROWS) {
            var stripe = 1.0;
            if cell > 1.6 {
                stripe = 0.62 + 0.38 * sin((g.y + 0.25) * 6.28318);
            }
            let cov = model_sample(g) * clamp(stripe * 1.5, 0.0, 1.0);
            colour = mix(colour, BADGE_INK, cov);
        }
    }

    // --- the maker's name, centred on the front ------------------------
    // The name alone, with no device beside it, so it centres on the
    // cabinet's own middle rather than on the segment it sits in.
    let cap = LOGO_CAP * ch;
    let cell = cap / MARK_CAP;
    let text_px = vec2<f32>(f32(MARK_COLS), f32(MARK_ROWS)) * cell;
    let org_px = vec2<f32>(cw * 0.5 - text_px.x * 0.5, LOGO_DROP * ch - text_px.y * 0.5);
    let g = (q - org_px) / cell;
    if on_text(g, MARK_COLS, MARK_ROWS) {
        colour = mix(colour, INK, mark_sample(g));
    }

    // --- the power button ----------------------------------------------
    // Its own square piece: the lamp's dark window at the top, the caption
    // under it, the standby mark under that.
    let bq = vec2<f32>(q.x - btn_c, q.y);
    if abs(bq.x) < btn_hw {
        let bev = 1.0 - smoothstep(0.10 * ch, 0.22 * ch, q.y);
        colour = mix(colour, colour * 1.10, bev * 0.6);
    }
    // The lamp: a small dark window with the red lamp lit inside it.
    // Sized off its foot rather than its middle: the lamp's bottom edge is
    // the fixed thing on the button, so making it taller grows it upward.
    let lamp_half = vec2<f32>(0.30 * btn_hw, 0.092 * ch);
    let lamp_c = vec2<f32>(0.0, 0.258 * ch - lamp_half.y);
    let d_well = rounded_rect(bq - lamp_c, lamp_half, 0.03 * ch);
    if d_well < 0.0 {
        colour = LAMP_WELL;
        let d_lamp = rounded_rect(bq - lamp_c, lamp_half - vec2<f32>(1.5, 1.5), 0.02 * ch);
        let lit = 1.0 - smoothstep(-2.0, 0.0, d_lamp);
        colour = mix(colour, LAMP, lit);
        // A soft top catchlight on the lamp's plastic.
        let gl = 1.0 - smoothstep(0.0, lamp_half.y, bq.y - (lamp_c.y - lamp_half.y * 0.4));
        colour = mix(colour, colour + vec3<f32>(0.25, 0.10, 0.08), lit * gl * 0.5);
    }
    // The caption, centred under the lamp.
    let ccap = 0.145 * ch;
    let ccell = ccap / CAPTION_CAP;
    let ctext = vec2<f32>(f32(CAPTION_COLS), f32(CAPTION_ROWS)) * ccell;
    let corg = vec2<f32>(-ctext.x * 0.5, 0.575 * ch - ctext.y * 0.5);
    let cg = (bq - corg) / ccell;
    if on_text(cg, CAPTION_COLS, CAPTION_ROWS) {
        colour = mix(colour, LEGEND, caption_sample(cg));
    }
    // The standby mark, under the caption.
    let sb = standby(bq - vec2<f32>(0.0, 0.833 * ch), 0.080 * ch);
    colour = mix(colour, LEGEND, sb);

    return colour;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    // The viewport in physical pixels; the cabinet fills it.
    let vp = u.size.xy;
    let px = in.uv * vp;

    // The opening in the same space, and the design unit.
    let o_org = u.opening.xy * vp;
    let o_size = u.opening.zw * vp;
    let o_half = o_size * 0.5;
    let p = px - (o_org + o_half);
    let unit = o_size.y;

    let case_org = vec2<f32>(0.0, 0.0);
    let case_size = vp;
    let chin_top = vp.y - FRAME_CHIN * unit;

    // --- the glass contour ---
    // The same warp as crt.wgsl maps the opening onto the source frame,
    // where the face is the source rectangle with the tube's rounded
    // corners, so with the preset's curvature in params.y this distance
    // coincides with the preset's face boundary exactly -- the recess
    // seats the tube whatever its bow -- and at zero curvature it reduces
    // to a plain rounded opening for the flat presets. Scaling by the
    // half-width turns source half-width units into approximate pixels.
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
    // interior (drawn before this pass, on the opening's bounding box), so
    // leave every interior fragment to it and repaint just the frame. The
    // aperture's rounded corners and the wall's shadow must sit on top of
    // the preset's square viewport -- the plastic overlaps the tube, not
    // the other way round.
    if u.params.x > 0.5 && d_glass < 0.0 {
        discard;
    }

    // --- the outer frame ---
    // One thin band of light plastic running the whole way round the
    // front, deeper across the top than down the sides. It is the
    // outermost surface: everything else is set into it.
    let c_half = case_size * 0.5;
    let cp = px - (case_org + c_half);
    let r_plastic = R_PLASTIC * unit;
    let d_case = rounded_rect(cp, c_half, r_plastic);

    var colour = CASE * (1.0 + 0.025 * grain(px));
    colour = mix(colour, colour * 0.66, 1.0 - smoothstep(0.5, 2.5, -d_case));

    // --- the inner bezel ---
    // A separate, much darker moulding carrying the tube, set into the
    // outer frame with a uniform groove all round it. Its outer edge is
    // the front inset by the frame and that groove, stopping at the chin;
    // its corners are square.
    let inset = FRAME_BAND * unit;
    let inset_top = FRAME_BAND_TOP * unit;
    let gap_w = (FRAME_BAND - FRAME_OUTER) * unit;
    // Below the tube the outer frame's bottom run is the chin's ledge
    // itself, so the moulding has to stop clear of it: end it where the
    // ledge begins, less the groove, and the groove closes round the
    // bottom instead of being painted over by the step-out. The ledge
    // turns at its top edge and not below it, so the groove seats
    // straight onto the turn with no flat front left showing between.
    let ledge_h = 0.10 * FRAME_CHIN * unit + 2.0;
    let inner_lo = vec2<f32>(inset, inset_top);
    let inner_hi = vec2<f32>(vp.x - inset, chin_top - ledge_h - gap_w);
    let inner_c = (inner_lo + inner_hi) * 0.5;
    let inner_h = (inner_hi - inner_lo) * 0.5;
    let d_inner = rounded_rect(px - inner_c, inner_h, R_REVEAL * unit);

    // The gap: a channel cut clean between the two mouldings. It runs the
    // whole way round, under the inner bezel as well.
    //
    // Across its width it is three flat runs, not one tone: each moulding
    // shows the cut side of its own edge, and the floor lies between them.
    // Which of the two sides catches the light is decided by the facing
    // alone, so the channel reads as a channel on every run -- lit below
    // the shadowed wall above the tube, and the other way round beneath
    // it. Stepped in a pixel at each break, like every other joint on this
    // cabinet: a ramp across ten pixels would only thicken the line.
    // Depth into the channel, measured per run rather than as a true
    // distance: the offset of a square corner is a quarter circle, so
    // stepping the groove off `d_inner` would round its outer edge while
    // its inner one stayed square. Taking the larger of the two axis
    // offsets keeps every contour square and puts the change-over on the
    // diagonal, where the two runs mitre. Inside the moulding this is
    // exactly `d_inner`, so nothing else need change.
    let q_gap = abs(px - inner_c) - inner_h;
    let d_box = max(q_gap.x, q_gap.y);
    let in_gap = smoothstep(-aa, aa, d_box) * (1.0 - smoothstep(gap_w - aa, gap_w + aa, d_box));
    // Outward from the inner bezel along whichever run this is, flipping
    // across the diagonal: that crease is the mitre itself.
    let n_gap = select(
        vec2<f32>(0.0, sign(px.y - inner_c.y + 1e-6)),
        vec2<f32>(sign(px.x - inner_c.x + 1e-6), 0.0),
        q_gap.x > q_gap.y,
    );
    let groove_w = max(GROOVE_WALL * gap_w, 1.0);
    // The inner moulding's cut side faces outward, away from its own
    // body; the outer frame's faces back in toward the tube.
    let wall_in = 1.0 - smoothstep(groove_w - aa, groove_w + aa, d_box);
    let wall_out = smoothstep(gap_w - groove_w - aa, gap_w - groove_w + aa, d_box);
    var groove = GROOVE_FLOOR * (1.0 + 0.02 * grain(px));
    groove = mix(groove, GROOVE_CUT * tone(chamfer_normal(-n_gap, GROOVE_SLOPE)), wall_in);
    groove = mix(groove, GROOVE_CUT * tone(chamfer_normal(n_gap, GROOVE_SLOPE)), wall_out);
    colour = mix(colour, groove, in_gap);

    if d_inner < 0.0 {
        // The face: flat plastic, one tone across its whole width.
        var well = MOULDING * (1.0 + 0.03 * grain(px));
        // The tube is sunk into that face, and the recess changes shape as
        // it falls. Its mouth -- the cut in the flat face -- is square
        // cornered, like the moulding it is cut into. Its floor is the
        // tube: round cornered, and bowed under a CRT preset. The wall
        // between them carries the corner from one to the other.
        let mouth_half = o_half + recess_walls(o_org, o_size, inset, chin_top);
        let d_mouth = rounded_rect(p, mouth_half, 0.0);
        let n_mouth = rounded_rect_grad(p, mouth_half, 0.0);

        // How far down the wall a fragment sits: 0 at the square mouth, 1
        // at the glass. Measured between the two contours, so the corners
        // morph from square to round on the way down.
        let span = max((-d_mouth) + d_glass, 1e-3);
        let drop = clamp((-d_mouth) / span, 0.0, 1.0);
        // Hard-edged: the face stops and the wall starts, in the width of
        // one pixel. This cabinet is made of flat faces meeting at
        // corners, so the shape has to read as facets, not as shading.
        let in_mouth = 1.0 - smoothstep(-aa, aa, d_mouth);
        // The recess is four flat runs, mitred at the corners: the top and
        // bottom runs meet the left and right ones along the diagonals,
        // and each run keeps one facing the whole way along. That crease
        // is the corner -- smoothing it away, as a curved wall does, is
        // what stopped the corners reading as moulded at all.
        let rel = p / max(mouth_half, vec2<f32>(1.0));
        let n_run = select(
            vec2<f32>(0.0, sign(rel.y + 1e-6)),
            vec2<f32>(sign(rel.x + 1e-6), 0.0),
            abs(rel.x) > abs(rel.y),
        );
        // Only the last of the drop turns to meet the tube, so the glass
        // seats without the mitre being rounded off.
        let n_wall = normalize(mix(n_run, n_glass, smoothstep(0.80, 1.0, drop)) + vec2<f32>(1e-6));
        let n = chamfer_normal(n_wall, CHAMFER_SLOPE);
        // One flat tone for the whole run, set only by which way that run
        // faces: the runs above and beside the tube turn away from the
        // light and go dark, the run below turns into it and lifts. No
        // ramp down the wall and no gathering shadow at its foot -- a
        // moulded funnel is four planes, and any gradient across them
        // reads as a curve instead of a facet.
        let facet = SINK * tone(n);
        well = mix(well, facet, in_mouth);
        colour = mix(colour, well, smoothstep(-1.0 * aa, 1.0 * aa, -d_inner));
        // A hard line at the top of the drop, one pixel wide, where the
        // flat face breaks to the wall. Banded on the distance's modulus:
        // keyed on the signed value it reaches 1 across the whole face
        // outside the mouth and repaints it, which is a tint on the one
        // surface whose colour was measured off the cabinet.
        let lip = 1.0 - smoothstep(0.0, 1.6 * aa, abs(d_mouth));
        let facing = clamp(-n_mouth.y, 0.0, 1.0);
        colour = mix(colour, MOULDING * mix(1.06, 1.24, facing), lip * 0.95);
        // A hairline in the angle where the wall meets the glass, one
        // pixel only, so the tube reads as seated rather than floating.
        let foot = (1.0 - smoothstep(0.0, aa, d_glass)) * step(0.0, d_glass);
        colour = mix(colour, SINK * 0.62, foot * 0.8);
    }

    // Where the inner bezel's face ends and its wall begins, on the right:
    // the chin lines its furniture up with this, not with the moulding's
    // outer edge.
    let mouth_right = o_org.x + o_size.x
        + recess_walls(o_org, o_size, inset, chin_top).x;

    // Where the chin's furniture is laid out from. It is the top of a
    // seam's lean -- where the gap meets the groove -- that lines up with
    // the funnel, not the piece's own edge, so the button sits off that
    // line by the lean it carries and in the direction it leans.
    let seam_lean = LEDGE_SEAM_LEAN * ledge_h;
    let chin_recess = mouth_right - seam_lean * LEDGE_SEAM_LEAN_DIR.w;

    // --- the chin ---
    // The chin stands proud of the front. Where it steps out, a ledge of
    // about forty-five degrees turns down and away from the room, so the
    // same plastic reads darker along it than on either face: the shadow
    // under the step is what makes the chin look like it stands forward.
    // It turns at its top edge, in a pixel, so the groove above seats
    // straight onto it.
    if px.y > chin_top - ledge_h && px.y <= chin_top {
        let n = chamfer_normal(vec2<f32>(0.0, -1.0), 0.78);
        let turn = CASE * tone(n) * (1.0 + 0.025 * grain(px));
        colour = mix(colour, turn, smoothstep(0.0, aa, px.y - (chin_top - ledge_h)));
        // The seams are gaps between separate mouldings, not lines drawn
        // on one, so they do not stop where the chin's face does: the
        // flap and the button are pieces in their own right and their
        // edges carry on up the turn. Cut into the lit facet rather than
        // over it, so the ledge still reads as a ledge through them.
        let seams = chin_seams(vp.x, chin_recess);
        let seam_w = chin_seam_width(vp.y - chin_top);
        // 0 where the turn meets the chin, 1 where it meets the groove.
        let up = clamp((chin_top - px.y) / max(ledge_h, 1.0), 0.0, 1.0);
        let lean = seam_lean * up;
        let dir = LEDGE_SEAM_LEAN_DIR;
        for (var i = 0; i < 4; i = i + 1) {
            let d = abs(px.x - (seams[i] + lean * dir[i]));
            colour = mix(colour, GAP_FLOOR, (1.0 - smoothstep(0.2 * seam_w, 1.0 * seam_w, d)) * 0.80);
        }
    }
    if px.y > chin_top && d_case < 0.0 {
        colour = chin(
            px - vec2<f32>(0.0, chin_top),
            vec2<f32>(0.0, 0.0),
            vec2<f32>(vp.x, vp.y - chin_top),
            unit,
            inset,
            chin_recess,
            CASE * (1.0 + 0.025 * grain(px)),
        );
    }

    // --- the picture ---
    // Full mode paints the opening interior itself; frame-only has already
    // discarded it. Black under the glass edge, so the aperture's corners
    // read as the tube's own.
    let display_uv = (p + o_half) / o_size;
    let picture = sample_display(display_uv).rgb;
    let inner = select(picture, vec3<f32>(0.0), u.params.x > 0.5);
    let lit = select(colour, inner, d_glass < 0.0);
    let joined = mix(colour, lit, smoothstep(0.5 * aa, 1.5 * aa, -d_glass + aa));

    // The cabinet's own outline. It fills the viewport but for the four
    // corners, where the moulding is drawn off the tool with the smallest
    // of radii; outside it is the black the front stands against.
    let outside = smoothstep(-aa, aa, d_case);
    return vec4<f32>(mix(joined, vec3<f32>(0.0), outside), 1.0);
}

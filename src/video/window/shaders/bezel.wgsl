// SPDX-License-Identifier: GPL-3.0-or-later
//
// Monitor-bezel pass: a procedural plastic front frame in the spirit of
// the 1084 the Amiga shipped with. The pass covers the whole display rect;
// the picture is re-sampled to fill the rounded opening the frame leaves,
// seated by a moulded insert -- a thin glass-seat shadow and a plastic
// chamfer sloping up to the case front -- inside a bevelled case edge,
// with a moulded outer edge, the power LED on the bottom band and the
// Copperline logotype printed on its left, where the 1084 wears its
// Commodore badge.
//
// Two modes, selected by params.x. Alone (0), this one pass draws both
// the frame and the picture. Under a CRT preset (1, frame-only), the
// preset has already painted the picture into the opening's bounding box
// and this pass runs after it, discarding the opening interior and
// drawing just the frame on top -- the moulding overlaps the tube face,
// so its rounded corners and recess clip the preset's square viewport.
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
    // y: the active preset's face curvature; the insert follows the
    // bowed glass contour it implies, and 0 keeps the flat rounded
    // opening. zw: reserved, zero.
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

// Per-pixel hash in -0.5..0.5, for the moulded plastic grain.
fn grain(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453) - 0.5;
}

// The front-face plastic, a warm Commodore grey (linear light).
const PLASTIC: vec3<f32> = vec3<f32>(0.585, 0.560, 0.500);

// Rounded screen corners as a fraction of the display half-width, the
// 1084 tube's R 11,6 mm arcs on its 280,8 mm screen. Must match
// crt.wgsl's CORNER_RADIUS so the insert seats the preset's face
// exactly; a test pins the two sources together.
const CORNER_RADIUS: f32 = 0.0826;

// "Copperline" logotype for the bottom band: 5x8-pixel glyphs one column
// apart (baseline row 6, the p descenders on row 7), 59x8 bits in all.
// Each row packs LSB-first from the left into a (low, high) word pair:
// column c sets bit c of x (c < 32) or bit c - 32 of y.
//
// .###.................................##....................
// #...#.................................#.....#..............
// #......###..####..####...###..#.##....#.........#.##...###.
// #.....#...#.#...#.#...#.#...#.##..#...#....##...##..#.#...#
// #.....#...#.#...#.#...#.#####.#.......#.....#...#...#.#####
// #...#.#...#.#...#.#...#.#.....#.......#.....#...#...#.#....
// .###...###..####..####...###..#......###...###..#...#..###.
// ............#.....#........................................
const BADGE_COLS: i32 = 59;
const BADGE_ROWS: i32 = 8;
const BADGE: array<vec2<u32>, 8> = array<vec2<u32>, 8>(
    vec2<u32>(0x0000000eu, 0x00000060u),
    vec2<u32>(0x00000011u, 0x00001040u),
    vec2<u32>(0x4e3cf381u, 0x038d0043u),
    vec2<u32>(0xd1451441u, 0x04531844u),
    vec2<u32>(0x5f451441u, 0x07d11040u),
    vec2<u32>(0x41451451u, 0x00511040u),
    vec2<u32>(0x4e3cf38eu, 0x039138e0u),
    vec2<u32>(0x00041000u, 0x00000000u),
);

// The badge ink, near-black with the printed badge's blue cast.
const BADGE_INK: vec3<f32> = vec3<f32>(0.030, 0.036, 0.060);

// One bit of the badge bitmap, 0.0 outside it.
fn badge_bit(c: i32, r: i32) -> f32 {
    if c < 0 || c >= BADGE_COLS || r < 0 || r >= BADGE_ROWS {
        return 0.0;
    }
    var rows = BADGE;
    let bits = rows[r];
    if c < 32 {
        return f32((bits.x >> u32(c)) & 1u);
    }
    return f32((bits.y >> u32(c - 32)) & 1u);
}

// The bitmap sampled bilinearly at a continuous position in font pixels,
// so the lettering stays smooth at any window scale.
fn badge_sample(q: vec2<f32>) -> f32 {
    let f = q - vec2<f32>(0.5);
    let base = floor(f);
    let t = f - base;
    let c = i32(base.x);
    let r = i32(base.y);
    let s00 = badge_bit(c, r);
    let s10 = badge_bit(c + 1, r);
    let s01 = badge_bit(c, r + 1);
    let s11 = badge_bit(c + 1, r + 1);
    return mix(mix(s00, s10, t.x), mix(s01, s11, t.x), t.y);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let vp = u.size.xy;
    let px = in.uv * vp;

    // Opening geometry in physical pixels.
    let o_org = u.opening.xy * vp;
    let o_size = u.opening.zw * vp;
    let o_half = max(o_size * 0.5, vec2<f32>(1.0));
    let centre = o_org + o_half;
    let p = px - centre;
    // Trim widths scale with the opening so the frame keeps its
    // proportions at any window size, with pixel floors so nothing
    // vanishes in a tiny window. The ring between the glass and the case
    // face is the moulded insert that seats the tube: a thin shadow slot
    // where the glass sits, then a plastic chamfer sloping up to the
    // case front; `recess` is where the case face takes over.
    let unit = min(o_size.x, o_size.y);
    let seat = max(0.007 * unit, 1.5);
    let chamfer = max(0.030 * unit, 4.0);
    let recess = seat + chamfer;
    let bevel = max(0.022 * unit, 2.0);

    // The contour the insert follows is the glass: the active preset's
    // bowed face. The same warp as crt.wgsl maps the opening onto the
    // source frame, where the face is the source rectangle with the
    // tube's rounded corners, so with the preset's curvature in params.y
    // this distance coincides with the preset's face boundary exactly --
    // the insert seats the tube whatever its bow -- and at zero
    // curvature it reduces to a plain rounded opening for the flat
    // presets. Scaling by the half-width turns source half-width units
    // into approximate pixels for the trim widths; the warp's local
    // scale varies a few percent around the perimeter, as a real
    // moulding gap does.
    let k = u.params.y;
    let fa = o_half.y / max(o_half.x, 1.0);
    let cn = p / o_half;
    let q = k * 0.25;
    let r2 = cn.x * cn.x + cn.y * cn.y * fa * fa;
    let m = vec2<f32>(1.0 + q, 1.0 + q * fa * fa);
    let wc = cn * (1.0 + q * r2) / m;
    let fh = vec2<f32>(1.0, fa);
    let d = rounded_rect(wc * fh, fh, CORNER_RADIUS) * o_half.x;
    let aa = max(fwidth(d), 1e-4);

    // Frame-only pass: a CRT preset has already painted the opening
    // interior (drawn before this pass, on the opening's bounding box),
    // so leave every interior fragment to it and repaint just the frame,
    // whose rounded corners and recess must sit on top of the preset's
    // square viewport -- the plastic overlaps the tube, not the other way
    // round.
    if u.params.x > 0.5 && d < 0.0 {
        discard;
    }

    // Vertical position through the opening, -1 at its top edge to +1 at
    // its bottom: the shading key for room light falling from above.
    let dir_y = clamp(p.y / o_half.y, -1.0, 1.0);

    // The picture, scaled to fill the opening.
    let pic_uv = clamp((in.uv - u.opening.xy) / max(u.opening.zw, vec2<f32>(1e-4)),
                       vec2<f32>(0.0), vec2<f32>(1.0));
    let picture = sample_display(pic_uv).rgb;

    // The glass seat: the thin unlit slot where the tube face meets the
    // insert, catching a little room light along its lower run.
    let seat_col = vec3<f32>(0.012) * (1.0 + 0.8 * dir_y) + vec3<f32>(0.004);

    // The insert chamfer: moulded plastic a step darker than the case
    // face, sloping up from the glass seat. Deeper is darker, and the
    // slope faces the room light, so its top run sits in its own shadow
    // while its bottom run catches the light -- the cues that read as a
    // part holding the glass in rather than a void behind the case.
    let slope = clamp((d - seat) / chamfer, 0.0, 1.0);
    var insert =
        PLASTIC * 0.88 * (0.45 + 0.40 * slope) * (1.0 + 0.30 * dir_y * (1.0 - 0.5 * slope));
    insert *= 1.0 + 0.03 * grain(floor(px));

    // Front-face plastic: lit from above, so it shades down slightly
    // toward the bottom band, with a faint moulding grain.
    var plastic = PLASTIC * (1.0 - 0.10 * in.uv.y);
    plastic *= 1.0 + 0.03 * grain(floor(px));

    // The bevelled lip sloping into the recess: in shadow above the
    // opening, catching the light below it.
    let lip = smoothstep(recess, recess + bevel, d);
    plastic *= mix(1.0 + 0.22 * dir_y, 1.0, lip);

    // Moulded outer edge of the case: the face rolls off into shadow just
    // inside the outline, and outside it is whatever backs the window
    // (the letterbox black), so the corners read as a rounded cabinet.
    let v_half = vp * 0.5;
    let d_case = rounded_rect(px - v_half, v_half, 0.05 * min(vp.x, vp.y));
    let aa_case = max(fwidth(d_case), 1e-4);
    plastic *= 1.0 - 0.35 * smoothstep(-6.0, 0.0, d_case);

    // Power LED on the right of the bottom band, in a small dark well.
    let band_top = o_org.y + o_size.y + recess;
    let band_mid_y = (band_top + vp.y) * 0.5;
    let led_pos = vec2<f32>(0.91 * vp.x, band_mid_y);
    let led_d = length(px - led_pos);
    let led_r = max(0.007 * unit, 1.5);
    let well = 1.0 - smoothstep(led_r + 1.0, led_r + 3.0, led_d);
    let led = 1.0 - smoothstep(led_r - 1.0, led_r + 1.0, led_d);
    plastic = mix(plastic, vec3<f32>(0.03, 0.03, 0.03), well);
    plastic = mix(plastic, vec3<f32>(0.06, 0.55, 0.10), led);

    // The printed logotype on the left of the bottom band, where the 1084
    // wears its Commodore badge: left-aligned with the opening, vertically
    // centred in the band, scaling with the frame. Skipped when the band
    // is too shallow for the lettering to resolve.
    let badge_h = 0.34 * (vp.y - band_top);
    if badge_h >= 5.0 {
        let badge_org = vec2<f32>(o_org.x, band_mid_y - 0.5 * badge_h);
        let q = (px - badge_org) * (f32(BADGE_ROWS) / badge_h);
        let hi = vec2<f32>(f32(BADGE_COLS) + 1.0, f32(BADGE_ROWS) + 1.0);
        if all(q > vec2<f32>(-1.0)) && all(q < hi) {
            plastic = mix(plastic, BADGE_INK, 0.92 * badge_sample(q));
        }
    }

    // Compose by signed distance: picture, glass seat, insert chamfer,
    // then case plastic, each join antialiased over about a pixel. In the
    // frame-only pass the inner side of the opening join belongs to the
    // preset underneath, whose face edge there is its off-face black:
    // blending from the raw picture instead smears a picture-coloured
    // hairline around the opening, on top of the preset the pass
    // discarded for.
    let inner = select(picture, vec3<f32>(0.0), u.params.x > 0.5);
    let ring = mix(seat_col, insert, smoothstep(seat - aa, seat + aa, d));
    var col = mix(inner, ring, clamp(d / aa + 0.5, 0.0, 1.0));
    col = mix(col, plastic, smoothstep(recess - aa, recess + aa, d));
    col = mix(col, vec3<f32>(0.0), clamp(d_case / aa_case + 0.5, 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}

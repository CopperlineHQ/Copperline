// SPDX-License-Identifier: GPL-3.0-or-later
//
// Monitor-bezel pass: a procedural plastic front frame in the spirit of
// the 1084 the Amiga shipped with. The pass covers the whole display rect;
// the picture is re-sampled to fill the rounded opening the frame leaves,
// with a dark recess between the tube face and the plastic, a bevelled
// inner lip, a moulded outer edge and the power LED on the bottom band.
//
// Not a window-shader preset: it is not user-replaceable, runs before any
// preset (which then re-draws the picture inside the opening), and its
// uniform block is its own, so it deliberately does not carry the shared
// contract prologue of shaders/{scanlines,mask,crt}.wgsl.

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
    // yzw: reserved, zero.
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
    // vanishes in a tiny window.
    let unit = min(o_size.x, o_size.y);
    let r_open = 0.055 * unit;
    let recess = max(0.016 * unit, 2.0);
    let bevel = max(0.022 * unit, 2.0);

    let d = rounded_rect(p, o_half, r_open);
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

    // The unlit gap where the tube face sits behind the plastic: nearly
    // black, catching a little light along its lower run.
    let gap = vec3<f32>(0.012) * (1.0 + 0.8 * dir_y) + vec3<f32>(0.004);

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
    let band_mid_y = (o_org.y + o_size.y + recess + vp.y) * 0.5;
    let led_pos = vec2<f32>(0.91 * vp.x, band_mid_y);
    let led_d = length(px - led_pos);
    let led_r = max(0.007 * unit, 1.5);
    let well = 1.0 - smoothstep(led_r + 1.0, led_r + 3.0, led_d);
    let led = 1.0 - smoothstep(led_r - 1.0, led_r + 1.0, led_d);
    plastic = mix(plastic, vec3<f32>(0.03, 0.03, 0.03), well);
    plastic = mix(plastic, vec3<f32>(0.06, 0.55, 0.10), led);

    // Compose by signed distance: picture, recess gap, then plastic, each
    // join antialiased over about a pixel.
    var col = mix(picture, gap, clamp(d / aa + 0.5, 0.0, 1.0));
    col = mix(col, plastic, smoothstep(recess - aa, recess + aa, d));
    col = mix(col, vec3<f32>(0.0), clamp(d_case / aa_case + 0.5, 0.0, 1.0));
    return vec4<f32>(col, 1.0);
}

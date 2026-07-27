// SPDX-License-Identifier: GPL-3.0-or-later
//
// Full CRT preset, in the spirit of the 1084 the Amiga shipped with: a
// bowed tube face, scanlines that bow with it, an aperture grille and a
// corner vignette, all faded in together by the one strength knob.
//
// The block between the two "shared contract" markers below is the
// Copperline window-shader contract, byte-identical in every preset and
// pinned by a test: a custom .wgsl must declare exactly these bindings
// and both entry points. Everything after it is this preset's own look.

// --- begin shared contract ---

struct CrtUniforms {
    // Display sub-rect of src_tex in UV space: xy origin, zw size. The
    // status bar sits below the display in the same texture and must
    // never be sampled, so all sampling goes through this rect.
    src_rect: vec4<f32>,
    // xy: viewport size in physical pixels.
    // zw: source display region in texels.
    size: vec4<f32>,
    // x: effect strength, 0 (no-op) to 1 (full).
    // y: scanline count across the display height.
    // z: mask kind. w: curvature.
    params: vec4<f32>,
    // x: vignette, 0 to 1. yzw: reserved, zero.
    params2: vec4<f32>,
};

// All three bindings are FRAGMENT-visibility only: vs_main cannot read
// them, so a custom shader must do all its work in fs_main.
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> u: CrtUniforms;

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

// Sample the display region only.
//
// The clamp is inset by half a texel on every side, not to the edge of
// src_rect: a linear sample taken exactly on the boundary is a 50/50 blend
// with the texel on the far side, which for the bottom edge is the status
// bar's first row. That reaches the picture whenever the display rect is
// magnified (the last fragment row lands past the last texel centre) or a
// preset warps a coordinate off the face. Inset, the worst case is the
// outermost texel's own centre. src_rect.zw / size.zw is one texel in UV.
fn sample_display(uv: vec2<f32>) -> vec4<f32> {
    let half_texel = 0.5 * u.src_rect.zw / max(u.size.zw, vec2<f32>(1.0));
    let lo = u.src_rect.xy + half_texel;
    let hi = u.src_rect.xy + u.src_rect.zw - half_texel;
    let tc = clamp(u.src_rect.xy + uv * u.src_rect.zw, lo, hi);
    return textureSample(src_tex, src_samp, tc);
}

// --- end shared contract ---

const TAU: f32 = 6.283185307179586;

// Scanline gap floor and its brightness compensation; see scanlines.wgsl
// for the derivation.
const FLOOR: f32 = 0.55;
const SCAN_BOOST: f32 = 1.15;

// Aperture grille: continuous vertical phosphor stripes (no row stagger,
// unlike a dot mask). Slightly gentler than the shadow-mask preset because
// scanlines already take a bite out of the brightness here.
const GRILLE_DIM: f32 = 0.55;
const GRILLE_BOOST: f32 = 1.25;

// Barrel distortion about the centre of the display rect: the tube face is
// a section of a sphere, so the picture bows outwards and the corners fall
// outside the visible rect.
fn warp(uv: vec2<f32>, k: f32) -> vec2<f32> {
    let c = uv * 2.0 - vec2<f32>(1.0);
    let r2 = dot(c, c);
    return (c * (1.0 + k * r2 * 0.25)) * 0.5 + vec2<f32>(0.5);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let strength = clamp(u.params.x, 0.0, 1.0);
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    // Curvature fades in with strength, so strength 0 samples straight
    // through and every later term collapses to the plain sample.
    let wuv = mix(uv, warp(uv, u.params.w), strength);
    let base = sample_display(wuv);

    // Scanlines follow the bowed geometry.
    let lines = max(u.params.y, 1.0);
    let profile = 0.5 - 0.5 * cos(TAU * wuv.y * lines);
    let scan = (FLOOR + (1.0 - FLOOR) * profile) * SCAN_BOOST;

    // The grille sits on the glass, so it is keyed to physical pixels of
    // the display rect rather than to the bowed picture behind it.
    let px = uv * u.size.xy;
    let col = i32(floor(px.x)) % 3;
    let grille = vec3<f32>(
        select(GRILLE_DIM, 1.0, col == 0),
        select(GRILLE_DIM, 1.0, col == 1),
        select(GRILLE_DIM, 1.0, col == 2),
    ) * GRILLE_BOOST;

    // Corner falloff: the electron gun is further from the corners of the
    // face than from its centre.
    let c = uv * 2.0 - vec2<f32>(1.0);
    let vig = max(1.0 - clamp(u.params2.x, 0.0, 1.0) * dot(c, c), 0.0);

    let shaded = mix(base.rgb, base.rgb * scan * grille * vig, strength);

    // Anything the bow pushed off the face is the unlit inside of the
    // tube, and a real face ends at a hard edge: off-face pixels are
    // opaque black whatever the strength. Only the *area* of the region
    // scales, and it already does, through wuv above -- at strength 0 the
    // bow is flat, nothing falls off the face, and the no-op invariant
    // holds. Mixing the black back toward `shaded` instead would fill the
    // region with a fraction of the edge colour the sampler smears there.
    //
    // d is a signed distance to the face in UV, positive outside; fwidth
    // rescales it to pixels, so the fade covers about one pixel and the
    // bowed edge does not staircase. Well inside, face is exactly 1 and
    // the shaded result comes through untouched.
    let d = max(max(-wuv.x, wuv.x - 1.0), max(-wuv.y, wuv.y - 1.0));
    let aa = max(fwidth(d), 1e-6);
    let face = 1.0 - clamp(d / aa + 0.5, 0.0, 1.0);
    return vec4<f32>(shaded * face, 1.0);
}

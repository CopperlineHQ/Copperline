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

// Rounded screen corners, as a fraction of the display half-width: the
// 1084's tube has R 11,6 mm corner arcs on a 280,8 mm wide screen
// (11,6 / 140,4).
const CORNER_RADIUS: f32 = 0.0826;

// The glass is never black: room light reflects off the face and the
// phosphor layer behind it, so a lit-room CRT shows black at roughly a
// hundredth of full white (linear). Lifting on-face black by that much
// keeps the face silhouette -- bow, rounded corners -- visible against
// the true black beyond the glass even when the picture itself is dark.
const GLASS_GLOW: f32 = 0.01;

// Barrel distortion about the centre of the display rect, matched to the
// 1084's tube (Philips M34EAQ10X, 1986 Philips T08 databook): the face is
// a section of a sphere (R 640 mm centre blending to R 530 mm at the
// edge), and the datasheet draws the useful screen (280,8 x 210,6 mm)
// with barrel-arced edges, R 1545 mm top and bottom, R 1173 mm at the
// sides. Depth on the face grows with *physical* distance from the
// centre, so in display-normalised coordinates the y term is weighted by
// the viewport aspect squared; the top/bottom edges then bow roughly
// (w/h)^2 as far as the sides, as the datasheet arcs do, and k = 0.30
// reproduces the arcs' curvature.
//
// The raster overscans the face, as on the real monitor: the per-axis
// normalisation rescales the bowed field so the source edge lands exactly
// on the face's mid-edges. The picture therefore fills the whole face --
// no black ring between picture and face edge -- and the bow shows as the
// crop deepening toward the corners, where the source content falls into
// the rounded-corner clip instead of the visible glass.
fn warp(uv: vec2<f32>, k: f32, aspect: f32) -> vec2<f32> {
    let c = uv * 2.0 - vec2<f32>(1.0);
    let r2 = c.x * c.x + c.y * c.y * aspect * aspect;
    let bowed = c * (1.0 + k * r2 * 0.25);
    let m = vec2<f32>(1.0 + k * 0.25, 1.0 + k * 0.25 * aspect * aspect);
    return (bowed / m) * 0.5 + vec2<f32>(0.5);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let strength = clamp(u.params.x, 0.0, 1.0);
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    // Height over width of the display rect on screen, for warp and
    // corner geometry that must be circular in physical pixels.
    let aspect = u.size.y / max(u.size.x, 1.0);
    // Curvature fades in with strength, so strength 0 samples straight
    // through and every later term collapses to the plain sample.
    let wuv = mix(uv, warp(uv, u.params.w, aspect), strength);
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
    // The face is a rounded rectangle, not a sharp one: the corner arcs
    // are CORNER_RADIUS of the half-width, like the tube's R 11,6 mm
    // screen corners. The distance is measured in the warped source frame
    // (the corner is a property of the screen surface, seen through the
    // same projection as the picture) and in half-width units on both
    // axes, so the arc stays circular on screen instead of stretching
    // with the display aspect. The radius fades with strength like the
    // bow does, keeping the strength-0 no-op; at radius 0 the expression
    // reduces to the plain rectangle distance.
    //
    // d is a signed distance to the face, positive outside; fwidth
    // rescales it to pixels, so the fade covers about one pixel and the
    // bowed edge does not staircase. Well inside, face is exactly 1 and
    // the shaded result comes through untouched.
    let half = vec2<f32>(1.0, aspect);
    let rc = CORNER_RADIUS * strength;
    let q = abs((wuv * 2.0 - vec2<f32>(1.0)) * half) - half + vec2<f32>(rc);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rc;
    let aa = max(fwidth(d), 1e-6);
    let face = 1.0 - clamp(d / aa + 0.5, 0.0, 1.0);
    // The reflection glow is flat -- ambient light, not beam emission --
    // so it rides on top of the shaded picture untouched by scanlines,
    // grille or vignette, and fades in with strength like every other
    // term so strength 0 stays a no-op.
    let glow = vec3<f32>(GLASS_GLOW * strength);
    return vec4<f32>((shaded + glow) * face, 1.0);
}

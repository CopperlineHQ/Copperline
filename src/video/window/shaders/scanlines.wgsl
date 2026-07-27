// SPDX-License-Identifier: GPL-3.0-or-later
//
// Scanline preset: the horizontal line structure a 15 kHz CRT leaves
// between beam passes. Geometry and phosphor colour are untouched.
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

// Brightness in the gap between beam passes, and the compensating boost.
// A raised cosine running between FLOOR and 1.0 averages (1 + FLOOR) / 2
// = 0.775 of the source, so BOOST lifts the mean back to about 0.89:
// visible dark gaps and slightly hot line centres, the way a consumer set
// looks, instead of a picture at half brightness. Values are linear (the
// source and target are sRGB textures), so the perceived loss is smaller
// still.
const FLOOR: f32 = 0.55;
const BOOST: f32 = 1.15;

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let base = sample_display(uv);
    let strength = clamp(u.params.x, 0.0, 1.0);
    let lines = max(u.params.y, 1.0);
    // Beam profile: brightest at the centre of each emulated line (phase
    // 0.5, 1.5, ...), dimmest in the gap between two passes.
    let profile = 0.5 - 0.5 * cos(TAU * uv.y * lines);
    let gain = (FLOOR + (1.0 - FLOOR) * profile) * BOOST;
    return vec4<f32>(mix(base.rgb, base.rgb * gain, strength), 1.0);
}

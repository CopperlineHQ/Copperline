// SPDX-License-Identifier: GPL-3.0-or-later
//
// Shadow-mask preset: the RGB phosphor triads of a slot/dot-mask tube,
// with no scanline or geometry modelling.
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

// Brightness of the two phosphors a pixel does not sit on, and the
// compensating boost: the triad averages (1 + 2 * DIM) / 3 = 0.633 of the
// source, and BOOST lifts the mean back to about 0.86.
const DIM: f32 = 0.45;
const BOOST: f32 = 1.35;

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let base = sample_display(uv);
    let strength = clamp(u.params.x, 0.0, 1.0);
    // The mask belongs to the tube face, not to the emulated image, so it
    // is keyed to physical pixels inside the display rect and does not
    // scale with the Amiga resolution.
    let px = uv * u.size.xy;
    // Classic shadow mask: every band of three rows is offset two thirds of
    // a triad (equivalently, one column back) from the band above, so the
    // phosphor dots sit on a staggered lattice rather than in continuous
    // vertical stripes. Both the band index and
    // the shift are integers: a fractional shift lands floor() on an exact
    // pixel boundary, one ulp from flipping a whole column per driver.
    let band = i32(floor(px.y / 3.0));
    let shift = select(0, 2, (band & 1) == 1);
    let col = (i32(floor(px.x)) + shift) % 3;
    let mask = vec3<f32>(
        select(DIM, 1.0, col == 0),
        select(DIM, 1.0, col == 1),
        select(DIM, 1.0, col == 2),
    ) * BOOST;
    return vec4<f32>(mix(base.rgb, base.rgb * mask, strength), 1.0);
}

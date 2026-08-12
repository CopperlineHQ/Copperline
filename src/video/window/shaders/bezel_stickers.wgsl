// SPDX-License-Identifier: GPL-3.0-or-later
//
// Sticker decals over the drawn monitor bezel: one rotated quad per
// sticker, sampling a packed atlas of the user's PNGs (stickers.rs
// loads and places them; this pass runs after the bezel's, over the
// same viewport, so the decals land on the finished plastic).
//
// Each quad is padded around the sticker so the fragment stage can lay
// a soft drop shadow off the sticker's own silhouette -- the alpha edge
// of a die-cut logo -- and the face is toned very slightly brighter
// toward the top, like the plastic it is stuck to, so it reads as part
// of the front rather than pasted over the frame. Output is
// premultiplied: the decal and its shadow are composed in one fragment,
// which straight alpha cannot carry.
//
// try.js carries a GLSL ES port of this pass for the web player's
// #bezel-stickers page hook; keep them in step.

struct Sticker {
    // xy: centre in viewport px. zw: half-size in px.
    geo: vec4<f32>,
    // xy: cos/sin of the clockwise tilt. z: opacity. w: unused.
    rot: vec4<f32>,
    // Atlas sub-rect in UV space: xy origin, zw size.
    uv: vec4<f32>,
};

struct Uniforms {
    // xy: viewport size in px. z: shadow offset in px. w: unused.
    info: vec4<f32>,
    st: array<Sticker, 16>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_samp: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Quad coordinates with the sticker spanning [-1, 1]; the shadow
    // padding carries them a little beyond.
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) index: u32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) ii: u32,
) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0),
    );
    let s = u.st[ii];
    let half = max(s.geo.zw, vec2(0.5));
    // Room for the shadow's furthest tap, on every side: the offset and
    // the half-offset spread beyond it.
    let padded = half + vec2(u.info.z * 2.0);
    let local = corners[vi] * padded / half;
    // The tilt turns clockwise on screen: y runs down, so the plain
    // rotation matrix already does.
    let p = corners[vi] * padded;
    let turned = vec2(
        p.x * s.rot.x - p.y * s.rot.y,
        p.x * s.rot.y + p.y * s.rot.x,
    );
    let px = s.geo.xy + turned;
    let ndc = vec2(
        px.x / u.info.x * 2.0 - 1.0,
        1.0 - px.y / u.info.y * 2.0,
    );
    return VsOut(vec4(ndc, 0.0, 1.0), local, ii);
}

// The sticker's texels at a quad position, transparent outside its
// bounds. Sampling is unconditional and by explicit level, so the mask
// costs no control flow; the clamp keeps the lookup inside the cell,
// whose transparent padding closes the edge.
fn decal(s: Sticker, local: vec2<f32>) -> vec4<f32> {
    let t = clamp(local * 0.5 + vec2(0.5), vec2(0.0), vec2(1.0));
    let inside = step(abs(local.x), 1.0) * step(abs(local.y), 1.0);
    let c = textureSampleLevel(atlas_tex, atlas_samp, s.uv.xy + t * s.uv.zw, 0.0);
    return c * inside;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = u.st[in.index];
    let half = max(s.geo.zw, vec2(0.5));
    let colour = decal(s, in.local);
    // The shadow is the silhouette dropped down-right, softened by taps
    // at the offset and half again beyond it.
    let o = vec2(u.info.z) / half;
    var sh = decal(s, in.local - o).a;
    sh += decal(s, in.local - o - vec2(o.x * 0.5, 0.0)).a;
    sh += decal(s, in.local - o - vec2(0.0, o.y * 0.5)).a;
    sh += decal(s, in.local - o * 1.5).a;
    let shadow = sh * 0.25 * 0.38;
    // Stuck to lit plastic: a touch brighter toward the light at the
    // top, falling off down the face, like the bezel's own tone.
    let tone = 1.04 - 0.07 * clamp(in.local.y * 0.5 + 0.5, 0.0, 1.0);
    let a_decal = colour.a * s.rot.z;
    let a_shadow = shadow * s.rot.z;
    // Premultiplied out: the decal over its own shadow.
    return vec4(
        colour.rgb * tone * a_decal,
        a_decal + a_shadow * (1.0 - a_decal),
    );
}

// SPDX-License-Identifier: GPL-3.0-or-later

//! The optional monitor-bezel pass: a procedural monitor front drawn over
//! the display rect on the GPU, with the picture re-sampled into the
//! rounded opening the frame leaves.
//!
//! One [`BezelStyle`] per frame, each a `shaders/bezel_*.wgsl` source and
//! the opening it leaves ([`opening_rect`]). They share this pass, its
//! uniform block and its pipeline cache, so adding a style is a shader
//! file and two match arms.
//!
//! Runs inside the `pixels` `render_with` pass after the scaling
//! renderer, on the same viewport the CRT pass uses (the display sub-rect
//! of the letterboxed clip rect), and samples only the display region of
//! the backing texture, so the status bar underneath is neither read nor
//! overdrawn. On its own, one pass draws both the frame and the picture
//! scaled into the opening. With a CRT preset active the preset draws the
//! picture first, re-aimed at the opening's bounding box
//! ([`super::crt_shader::CrtUniforms::with_viewport`]), and the bezel
//! follows in frame-only mode, discarding the opening interior: the
//! moulding overlaps the tube face like the real part, so the frame's
//! rounded corners and chamfer clip the preset's square viewport rather
//! than being buried under it.
//!
//! Purely a presentation stage, like the CRT pass: screenshots, frame
//! dumps, recordings and headless runs never include the bezel. Unlike
//! the CRT presets it is not user-replaceable and carries no strength
//! knob: it is either drawn or skipped.

use pixels::wgpu;
use zerocopy::{Immutable, IntoBytes};

use super::crt_shader;
use crate::config::BezelStyle;

/// One WGSL source per style, embedded like the preset sources. A new
/// style is a file here, an arm in [`source`] and one in [`opening_rect`];
/// nothing else in the window needs to know it exists.
const M1084_WGSL: &str = include_str!("shaders/bezel_1084.wgsl");
const CLASSIC_WGSL: &str = include_str!("shaders/bezel_classic.wgsl");

/// The shader a style draws with. [`BezelStyle::None`] draws nothing, so
/// the pass is skipped before this is reached; it answers with the default
/// style's source rather than widening every caller's return type.
fn source(style: BezelStyle) -> &'static str {
    match style {
        BezelStyle::Model1084 | BezelStyle::None => M1084_WGSL,
        BezelStyle::Classic => CLASSIC_WGSL,
    }
}

/// Each style's `APERTURE_RADIUS`, restated for the overlays. A test pins
/// these to the shaders' own, the way the frame proportions are pinned.
const APERTURE_RADIUS_1084: f32 = 0.090;
const APERTURE_RADIUS_CLASSIC: f32 = 0.0826;

/// The smallest inset, in texture pixels, that puts an overlay's corner
/// inside the picture the presentation actually draws.
///
/// The picture is not the rectangle it is placed in. A drawn front rounds
/// its aperture, and a preset that bows its face rounds and pulls it in
/// further -- at the very edge a bowed face exists only at mid-height, so
/// the corners are where the least of it is. An overlay parked at its
/// usual margin lands in what has been taken away and is clipped.
///
/// Both effects are known here in closed form: the same warp both shaders
/// apply, against the same rounded rect. Rather than allow for them with a
/// factor picked to look right, walk the corner outward until it is inside
/// the face, using that geometry. It costs a few dozen floats once a frame
/// and cannot drift from the shaders when a constant moves.
///
/// `curvature` and `strength` are the active preset's, 0 for one that
/// draws the picture flat, and the result is 0 when nothing shapes the
/// picture at all.
pub(super) fn corner_inset(
    style: BezelStyle,
    curvature: f32,
    strength: f32,
    texture_scale: usize,
) -> usize {
    let aperture = match style {
        BezelStyle::None => 0.0,
        BezelStyle::Model1084 => APERTURE_RADIUS_1084,
        BezelStyle::Classic => APERTURE_RADIUS_CLASSIC,
    };
    let strength = strength.clamp(0.0, 1.0);
    let k = curvature * strength;
    // A preset clips its face to its own radius, so that is the corner
    // when it is the wider -- or the only corner, with no front drawn.
    let r = if k > 0.0 {
        aperture.max(crt_shader::FACE_CORNER_RADIUS)
    } else {
        aperture
    };
    if r <= 0.0 {
        return 0;
    }

    let w = (crate::video::FB_WIDTH * texture_scale) as f32;
    let h = (crate::video::present_height() * texture_scale) as f32;
    if w < 2.0 || h < 2.0 {
        return 0;
    }
    // The picture keeps its aspect in the rect it is drawn into, so the
    // face's height in half-widths is the canvas's.
    let fa = h / w;
    let q = k * 0.25;

    // Is a corner `d` texture pixels in from the bottom-left of the
    // picture inside the face? Mirrors the warp and the aperture test both
    // shaders run, in the normalised space they run them in.
    let inside = |d: f32| {
        let cn = (2.0 * d / w - 1.0, 1.0 - 2.0 * d / h);
        let r2 = cn.0 * cn.0 + cn.1 * cn.1 * fa * fa;
        let bulge = 1.0 + q * r2;
        let warped = (cn.0 * bulge / (1.0 + q), cn.1 * bulge / (1.0 + q * fa * fa));
        // mix(cn, warped, strength), then into the face's own units.
        let wc = (
            cn.0 + (warped.0 - cn.0) * strength,
            (cn.1 + (warped.1 - cn.1) * strength) * fa,
        );
        // rounded_rect(wc, (1, fa), r) < 0, i.e. on the glass.
        let qx = wc.0.abs() - 1.0 + r;
        let qy = wc.1.abs() - fa + r;
        (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r < 0.0
    };

    // The overlays sit at this margin; anything less is already clear.
    let margin = 8.0 * texture_scale as f32;
    let limit = (h / 3.0) as usize;
    (0..=limit)
        .find(|&d| inside(margin + d as f32))
        .unwrap_or(limit)
}

/// The cabinet's proportions, in units of the picture opening's height:
/// the moulding is that thick around the tube, measured off a straight-on
/// photograph of a real 1084. [`FRAME_WEIGHT`] scales all of them
/// together, trading faithfulness against how much of the window the
/// picture keeps -- at 1.0 the frame is the real one's and the picture
/// holds 68% of the window's width; at 0.62 it holds 78%.
///
/// These three place the opening. `shaders/bezel_1084.wgsl` reads them
/// back to place everything else on the front, along with the two below,
/// so the two sources must agree; a test pins them together.
const FRAME_WEIGHT: f32 = 0.86;
const FRAME_TOP: f32 = 0.1600 * FRAME_WEIGHT;
/// Below the glass, down to the seam the chin panel starts at.
const FRAME_WELL_BOTTOM: f32 = 0.1170 * FRAME_WEIGHT;
/// The chin panel: model badge, logotype, power lamp.
const FRAME_CHIN: f32 = 0.1356 * FRAME_WEIGHT;
/// The design's side margin, and the cabinet band above the moulding.
/// Only the shader draws with these: the cabinet fills the viewport, which
/// leaves more room beside the tube than the design asks for, so the side
/// margin is not what places the opening (the vertical three do that) and
/// [`FRAME_SIDE`] only sets how far the moulding reaches beside the glass.
/// They are here to be pinned to the shader's copies, and to let a test
/// place what the shader does put there.
#[cfg(test)]
const FRAME_SIDE: f32 = 0.1780 * FRAME_WEIGHT;
#[cfg(test)]
const FRAME_BAND: f32 = 0.0700 * FRAME_WEIGHT;

/// Size of the uniform block, in bytes. The shared bind group layout pins
/// its `min_binding_size` to the CRT block's 64 bytes, so this block must
/// stay the same size to build against it.
const UNIFORM_BYTES: u64 = std::mem::size_of::<BezelUniforms>() as u64;

/// The uniform block every `shaders/bezel_*.wgsl` sees. Mirrors the WGSL
/// struct exactly; `#[repr(C)]` with 16-byte-aligned members only.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, IntoBytes, Immutable)]
pub(super) struct BezelUniforms {
    /// Display sub-rect of the source texture in UV space: xy origin, zw
    /// size. The status bar lives below it and is never sampled.
    src_rect: [f32; 4],
    /// xy: viewport size in physical pixels. zw: source display region
    /// in texels.
    size: [f32; 4],
    /// Picture opening within the viewport, in viewport UV: xy origin,
    /// zw size.
    opening: [f32; 4],
    /// x: 1 = frame-only (leave the opening interior to the preset that
    /// painted it), 0 = full. y: the active preset's face curvature, raw
    /// -- the chamfer follows the bowed glass contour it implies, 0 = flat
    /// rounded opening. z: the radius that preset clips its face to, faded
    /// by strength as the preset fades it, which the aperture opens to
    /// when it is the wider. w: that strength, to fade the bow by.
    params: [f32; 4],
}

/// Fraction of the display rect the Classic frame leaves to the picture,
/// and how the freed height splits around it: the top margin takes this
/// share and the bottom band the rest. Both axes scale by the one factor,
/// so the picture keeps its aspect.
const CLASSIC_OPENING_SCALE: f32 = 0.85;
const CLASSIC_TOP_SHARE: f32 = 0.42;

/// The picture opening a style leaves inside a display viewport rect
/// (physical surface pixels), keeping the picture's aspect.
///
/// The 1084's cabinet fills the viewport, as a monitor's front fills the
/// window it stands in. Its shape is a little taller in proportion than
/// the picture -- a real one's is -- so it cannot hold every margin at the
/// design's weight at once. The vertical budget wins, because the deep
/// chin is what the front is recognised by; the slack that leaves goes
/// into the side pillars, which is where a real cabinet carries it.
///
/// Classic scales the rect about both axes instead and sits the opening
/// high, which is all its frame needs to know.
pub(super) fn opening_rect(
    style: BezelStyle,
    viewport: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = viewport;
    match style {
        BezelStyle::Classic => {
            let ow = w * CLASSIC_OPENING_SCALE;
            let oh = h * CLASSIC_OPENING_SCALE;
            (x + (w - ow) * 0.5, y + (h - oh) * CLASSIC_TOP_SHARE, ow, oh)
        }
        BezelStyle::Model1084 | BezelStyle::None => {
            let oh = h / (1.0 + FRAME_TOP + FRAME_WELL_BOTTOM + FRAME_CHIN);
            let ow = oh * (w / h.max(1.0));
            (x + (w - ow) * 0.5, y + FRAME_TOP * oh, ow, oh)
        }
    }
}

/// Build the bezel uniforms for one presented frame from the CRT pass's
/// uniforms for the same frame (which already carry the source mapping
/// and the display viewport size) plus the two rects in physical surface
/// pixels. `frame_only` is set when a preset has already painted the
/// opening interior, so the pass leaves it untouched. Pure arithmetic,
/// unit testable on its own.
pub(super) fn uniforms_from(
    crt: &crt_shader::CrtUniforms,
    viewport: (f32, f32, f32, f32),
    opening: (f32, f32, f32, f32),
    frame_only: bool,
) -> BezelUniforms {
    let (vx, vy, vw, vh) = viewport;
    let (ox, oy, ow, oh) = opening;
    let w = vw.max(1.0);
    let h = vh.max(1.0);
    let strength = crt.params[0].clamp(0.0, 1.0);
    BezelUniforms {
        src_rect: crt.src_rect,
        size: crt.size,
        opening: [(ox - vx) / w, (oy - vy) / h, ow / w, oh / h],
        // The preset's bowed face rides along so the moulding can seat it
        // at any strength: its curvature, the radius it clips its corners
        // to, and the strength itself.
        //
        // The two fade differently, so both go over as the preset states
        // them. `crt.wgsl` fades its corner radius linearly, so that one
        // arrives pre-faded; it fades the bow by mixing *coordinates*
        // (`mix(uv, warp(uv, k), strength)`), which is not the same curve
        // as warping by a faded `k` -- `warp` is rational in `k` -- so the
        // curvature goes over raw and each bezel source mixes the same way.
        //
        // Curvature is non-zero for exactly the one preset that clips a
        // face at all; a flat preset, or none, carries zeroes and leaves
        // the aperture's own corners to govern.
        params: [
            if frame_only { 1.0 } else { 0.0 },
            crt.params[3],
            if crt.params[3] > 0.0 {
                crt_shader::FACE_CORNER_RADIUS * strength
            } else {
                0.0
            },
            strength,
        ],
    }
}

/// The bezel pass: the bindings every style shares, and a pipeline for
/// each style drawn so far.
pub(super) struct BezelShader {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    target_format: wgpu::TextureFormat,
    /// One pipeline per style drawn so far. Compiling a shader costs
    /// milliseconds, so a run pays only for the styles it actually picks,
    /// and a style switch mid-run pays once.
    pipelines: Vec<(BezelStyle, wgpu::RenderPipeline)>,
    bind_group: Option<wgpu::BindGroup>,
    /// The texture the current bind group views, compared by identity:
    /// `pixels` recreates its backing texture on a buffer resize, and the
    /// stale view has to be dropped with it.
    bound_texture: Option<wgpu::Texture>,
}

impl BezelShader {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = crt_shader::shader_bind_group_layout(device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bezel_shader_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bezel_shader_uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            bind_group_layout,
            sampler,
            target_format,
            uniforms,
            pipelines: Vec::new(),
            bind_group: None,
            bound_texture: None,
        }
    }

    /// The pipeline for a style, compiling its shader the first time.
    fn pipeline_for(&mut self, device: &wgpu::Device, style: BezelStyle) -> &wgpu::RenderPipeline {
        if let Some(at) = self.pipelines.iter().position(|(s, _)| *s == style) {
            return &self.pipelines[at].1;
        }
        let pipeline = crt_shader::build_pipeline(
            device,
            &self.bind_group_layout,
            source(style),
            "bezel_shader",
            self.target_format,
        );
        self.pipelines.push((style, pipeline));
        &self.pipelines.last().expect("just pushed").1
    }

    /// Draw the bezel over the `(x, y, w, h)` viewport rect of `target`
    /// (physical surface pixels), sampling `src_texture` through
    /// `uniforms.src_rect`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        style: BezelStyle,
        uniforms: BezelUniforms,
    ) {
        let (x, y, w, h) = viewport;
        if w < 1.0 || h < 1.0 || !style.is_on() {
            return;
        }
        // Resolve the pipeline first: it may compile a shader, and it
        // borrows self mutably, which the bind group below does not allow.
        let _ = self.pipeline_for(device, style);
        if self.bound_texture.as_ref() != Some(src_texture) {
            let view = src_texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bezel_shader_bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniforms.as_entire_binding(),
                    },
                ],
            }));
            self.bound_texture = Some(src_texture.clone());
        }
        let (Some(bind_group), Some((_, pipeline))) = (
            &self.bind_group,
            self.pipelines.iter().find(|(s, _)| *s == style),
        ) else {
            return;
        };
        queue.write_buffer(&self.uniforms, 0, uniforms.as_bytes());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bezel_shader_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShaderKind;

    // --- WGSL validation and geometry (no GPU) --------------------------

    #[test]
    fn every_bezel_source_validates() {
        for style in drawn_styles() {
            if let Err(e) = crt_shader::validate_wgsl_source(source(style)) {
                panic!("{} bezel shader failed validation:\n{e}", style.label());
            }
        }
    }

    /// The bezel is not a preset: it must not carry the preset contract
    /// markers, or the contract test would be expected to cover it.
    #[test]
    fn no_bezel_source_is_a_contract_preset() {
        for style in drawn_styles() {
            assert!(
                !source(style).contains("--- begin shared contract ---"),
                "{} carries the preset contract",
                style.label()
            );
        }
    }

    /// The shared bind group layout pins `min_binding_size` to the CRT
    /// block's 64 bytes; this block must stay that size to build against
    /// it.
    #[test]
    fn the_uniform_block_matches_the_shared_layout() {
        assert_eq!(
            std::mem::size_of::<BezelUniforms>(),
            std::mem::size_of::<crt_shader::CrtUniforms>()
        );
    }

    #[test]
    fn the_opening_keeps_the_picture_aspect_and_sits_high() {
        let (vw, vh) = (716.0_f32, 537.0_f32);
        let (ox, oy, ow, oh) = opening_rect(BezelStyle::Model1084, (0.0, 0.0, vw, vh));
        // Both axes scale by the one factor, so the picture is not
        // stretched.
        assert!((ow / oh - vw / vh).abs() < 1e-4, "aspect drifted");
        // The frame is a frame: the picture keeps most of the window but
        // not nearly all of it.
        let kept = ow / vw;
        assert!((0.6..0.9).contains(&kept), "picture keeps {kept} of width");
        // Centred, with the chin deeper than the top margin like the
        // 1084's face.
        assert!((ox - (vw - ow) * 0.5).abs() < 1e-3);
        let top = oy;
        let bottom = vh - (oy + oh);
        assert!(top > 0.0, "top margin missing: {top}");
        assert!(
            bottom > top,
            "bottom margin ({bottom}) not deeper than top ({top})"
        );
    }

    /// The cabinet fills the display rect exactly: the vertical margins
    /// account for the whole height, and the opening leaves a side pillar
    /// wide enough to draw at every window shape.
    #[test]
    fn the_cabinet_fills_the_viewport() {
        for (w, h) in [
            (716.0, 537.0),
            (1920.0, 1080.0),
            (640.0, 512.0),
            (97.0, 61.0),
        ] {
            let (ox, oy, ow, oh) = opening_rect(BezelStyle::Model1084, (0.0, 0.0, w, h));
            let used = oy + oh + (FRAME_WELL_BOTTOM + FRAME_CHIN) * oh;
            assert!(
                (used - h).abs() < 1e-3,
                "vertical margins leave {} of {h} unaccounted",
                h - used
            );
            assert!(ox > 0.0, "no side pillar at {w}x{h}");
            assert!(
                (ox + ow - (w - ox)).abs() < 1e-3,
                "pillars uneven at {w}x{h}"
            );
            // The moulding sits inside the pillar, not through it.
            assert!(
                ox - (FRAME_SIDE - FRAME_BAND) * oh > 0.0,
                "moulding overruns the cabinet at {w}x{h}"
            );
        }
    }

    #[test]
    fn a_letterboxed_viewport_offsets_the_opening() {
        let flat = opening_rect(BezelStyle::Model1084, (0.0, 0.0, 640.0, 480.0));
        let off = opening_rect(BezelStyle::Model1084, (12.0, 34.0, 640.0, 480.0));
        assert_eq!(off.0, flat.0 + 12.0);
        assert_eq!(off.1, flat.1 + 34.0);
        assert_eq!(off.2, flat.2);
        assert_eq!(off.3, flat.3);
    }

    #[test]
    fn uniforms_carry_the_source_mapping_and_the_opening_in_viewport_uv() {
        let (crt, viewport) = crt_shader::uniforms_for(
            ShaderKind::None,
            0.0,
            (12, 34, 640, 512),
            537,
            581,
            (716, 581),
            537.0,
        );
        let opening = opening_rect(BezelStyle::Model1084, viewport);
        let u = uniforms_from(&crt, viewport, opening, false);
        assert_eq!(u.src_rect, crt.src_rect);
        assert_eq!(u.size, crt.size);
        // The opening is expressed relative to the viewport, so the
        // letterbox offset must cancel out.
        let flat = opening_rect(BezelStyle::Model1084, (0.0, 0.0, viewport.2, viewport.3));
        assert!((u.opening[0] - flat.0 / viewport.2).abs() < 1e-6);
        assert!((u.opening[1] - flat.1 / viewport.3).abs() < 1e-6);
        assert!((u.opening[2] - flat.2 / viewport.2).abs() < 1e-6);
        assert!((u.opening[3] - flat.3 / viewport.3).abs() < 1e-6);
        assert_eq!(u.params[0], 0.0);
        // Frame-only mode differs in nothing but the flag.
        let frame = uniforms_from(&crt, viewport, opening, true);
        assert_eq!(frame.params[0], 1.0);
        assert_eq!(frame.opening, u.opening);
    }

    /// Re-aiming the CRT pass at the opening changes only its viewport
    /// size: the source mapping stays, so the preset draws the same
    /// picture into the smaller rect.
    #[test]
    fn retargeting_the_crt_pass_keeps_its_source_mapping() {
        let (crt, viewport) = crt_shader::uniforms_for(
            ShaderKind::Crt,
            1.0,
            (0, 0, 716, 581),
            537,
            581,
            (716, 581),
            537.0,
        );
        let opening = opening_rect(BezelStyle::Model1084, viewport);
        let re = crt.with_viewport(opening);
        assert_eq!(re.src_rect, crt.src_rect);
        assert_eq!(re.params, crt.params);
        assert_eq!(re.size[0], opening.2);
        assert_eq!(re.size[1], opening.3);
        assert_eq!(re.size[2..], crt.size[2..]);
    }

    // --- offscreen render (needs a GPU adapter) -------------------------

    /// The styles that actually draw a frame, which is what the source
    /// contracts below are about. [`BezelStyle::None`] draws nothing and
    /// borrows a source to keep [`source`] total, so including it would
    /// check one file twice and name it "off" when it failed.
    fn drawn_styles() -> impl Iterator<Item = BezelStyle> {
        BezelStyle::MENU_ORDER.into_iter().filter(|s| s.is_on())
    }

    /// A scalar constant as a shader source states it. Parsed rather than
    /// pinned to an exact substring, so only semantic drift fails a test,
    /// not formatting -- and so a test's replica of the geometry cannot
    /// drift from the shader it is checking.
    fn shader_constant(src: &str, name: &str, what: &str) -> f32 {
        let key = format!("const {name}: f32 =");
        let at = src
            .find(&key)
            .unwrap_or_else(|| panic!("{what}: no {name} constant"));
        let rest = &src[at + key.len()..];
        let end = rest.find(';').expect("terminated constant");
        rest[..end]
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{what}: unparsable {name}: {e}"))
    }

    /// The radius a style's opening actually carries when a preset is
    /// clipping its picture to a tube face at full strength: every source
    /// opens to the wider of its own aperture and that face.
    fn effective_aperture(style: BezelStyle) -> f32 {
        shader_constant(source(style), "APERTURE_RADIUS", style.label())
            .max(crt_shader::FACE_CORNER_RADIUS)
    }

    const TEX: u32 = 64;
    const DISPLAY_ROWS: u32 = 48;
    const GREY: u8 = 128;
    /// Magnification of the render target over the source, as a real
    /// window magnifies the composited buffer. Big enough that the case
    /// band, the moulding and its chamfer are all pixels wide and the chin
    /// panel is deep enough for both lines of lettering to draw (the
    /// shader drops each below the height it would stop resolving at).
    const SCALE: u32 = 8;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    /// Target clear colour; pure blue survives the sRGB encode exactly,
    /// so any surviving sentinel pixel inside the viewport means the pass
    /// left a hole, and any outside means it overdrew its viewport.
    const SENTINEL: [u8; 4] = [0, 0, 255, 255];

    /// A serialized device, or `None` without a usable hardware adapter:
    /// see `crt_shader::test_gpu`.
    fn gpu() -> Option<crt_shader::TestGpu> {
        crt_shader::test_gpu("bezel_shader_test")
    }

    /// The `pixels` backing texture in miniature: `display_rows` rows of
    /// the given texels, any remaining rows filled as a magenta "status
    /// bar" (a full-height `display_rows` leaves no bar).
    fn source_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        display_rows: u32,
        display: &dyn Fn(u32, u32) -> [u8; 4],
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bezel_shader_test_src"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut texels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let px = if y < display_rows {
                    display(x, y)
                } else {
                    [255, 0, 255, 255]
                };
                let off = ((y * width + x) * 4) as usize;
                texels[off..off + 4].copy_from_slice(&px);
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    /// Read an RGBA8 render target back into per-pixel bytes.
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: wgpu::CommandEncoder,
        target: &wgpu::Texture,
        dim: (u32, u32),
    ) -> Vec<[u8; 4]> {
        let (w, h) = dim;
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bezel_shader_test_readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = encoder;
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().expect("map callback").expect("buffer mapped");
        let mapped = readback.slice(..).get_mapped_range();
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let base = (y * padded) as usize;
            for x in 0..w {
                let off = base + (x * 4) as usize;
                px.push([
                    mapped[off],
                    mapped[off + 1],
                    mapped[off + 2],
                    mapped[off + 3],
                ]);
            }
        }
        drop(mapped);
        readback.unmap();
        px
    }

    /// Clear a sentinel target and render as the window does: the bezel
    /// alone in one full pass, or, with a preset, the preset retargeted
    /// at the opening first and the bezel framing it on top in
    /// frame-only mode. Read the result back.
    fn render_bezel(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::Texture,
        style: BezelStyle,
        crt_kind: Option<ShaderKind>,
        strength: f32,
    ) -> (Vec<[u8; 4]>, u32, u32) {
        let dim = TEX * SCALE;
        let rows = DISPLAY_ROWS * SCALE;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bezel_shader_test_target"),
            size: wgpu::Extent3d {
                width: dim,
                height: dim,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bezel_shader_test_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        let (crt_uniforms, viewport) = crt_shader::uniforms_for(
            crt_kind.unwrap_or(ShaderKind::None),
            strength,
            (0, 0, dim, dim),
            DISPLAY_ROWS as usize,
            TEX as usize,
            (TEX, TEX),
            DISPLAY_ROWS as f32,
        );
        assert_eq!(viewport, (0.0, 0.0, dim as f32, rows as f32));
        let opening = opening_rect(style, viewport);
        if let Some(kind) = crt_kind {
            let mut crt = crt_shader::CrtShader::new(device, FORMAT);
            crt.render(
                device,
                queue,
                src,
                &mut encoder,
                &view,
                opening,
                kind,
                crt_uniforms.with_viewport(opening),
            );
        }
        let mut shader = BezelShader::new(device, FORMAT);
        shader.render(
            device,
            queue,
            src,
            &mut encoder,
            &view,
            viewport,
            style,
            uniforms_from(&crt_uniforms, viewport, opening, crt_kind.is_some()),
        );
        (
            read_back(device, queue, encoder, &target, (dim, dim)),
            dim,
            rows,
        )
    }

    fn at(px: &[[u8; 4]], dim: u32, x: u32, y: u32) -> [u8; 4] {
        px[(y * dim + x) as usize]
    }

    /// One of `bezel_1084.wgsl`'s colour constants, as the sRGB triple it
    /// will land on in the render target. The shader states them in linear
    /// light because the target is `Rgba8UnormSrgb`, so the encode here is
    /// the one the hardware does on the way out.
    ///
    /// Reading the shader rather than restating its values is the point:
    /// a test that carries its own idea of what colour the plastic is
    /// silently becomes a test of last year's design.
    fn shader_colour(name: &str) -> [u8; 3] {
        let key = format!("const {name}: vec3<f32> = vec3<f32>(");
        let at = M1084_WGSL
            .find(&key)
            .unwrap_or_else(|| panic!("bezel_1084.wgsl: no {name}"));
        let rest = &M1084_WGSL[at + key.len()..];
        let end = rest.find(')').expect("terminated constant");
        let mut out = [0u8; 3];
        let mut seen = 0;
        for (i, t) in rest[..end].split(',').enumerate() {
            let v: f32 = t.trim().parse().unwrap_or_else(|e| panic!("{name}: {e}"));
            let s = if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            out[i] = (s * 255.0).round().clamp(0.0, 255.0) as u8;
            seen += 1;
        }
        assert_eq!(seen, 3, "{name}: expected three channels");
        out
    }

    /// Assert a rendered pixel is one of the shader's declared colours,
    /// within the tolerance the plastic grain and antialiasing need.
    fn is_colour(got: [u8; 4], name: &str, tol: u8, what: &str) {
        let want = shader_colour(name);
        let ok = (0..3).all(|c| got[c].abs_diff(want[c]) <= tol);
        assert!(ok, "{what}: got {got:?}, {name} is {want:?} (±{tol})");
    }

    /// Whether a rendered pixel is a shaded facet of one of the shader's
    /// plastics: the same tint, at some tone between deep shadow and the
    /// full front light. `tone()`'s range is deliberately shallow, so what
    /// identifies a surface is its tint and not its value -- which also
    /// makes this the way to tell plastic from a picture a preset has
    /// dimmed and tinted out of all recognition.
    fn looks_shaded(got: [u8; 4], name: &str) -> bool {
        let want = shader_colour(name);
        let lit = f32::from(got[1]) / f32::from(want[1].max(1));
        let tinted = (0..3).all(|c| (f32::from(got[c]) - f32::from(want[c]) * lit).abs() <= 8.0);
        (0.3..=1.4).contains(&lit) && tinted
    }

    /// The same, as an assertion.
    fn is_shaded(got: [u8; 4], name: &str, what: &str) {
        assert!(
            looks_shaded(got, name),
            "{what}: got {got:?}, not a shaded {name} ({:?})",
            shader_colour(name)
        );
    }

    /// The cabinet rect and the chin seam for a viewport. The cabinet is
    /// the viewport; only the seam is derived from the opening, exactly as
    /// `shaders/bezel_1084.wgsl` derives it.
    fn cabinet_of(dim: u32, rows: u32) -> (f32, f32, f32, f32, f32) {
        let oh = opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32)).3;
        (
            0.0,
            0.0,
            dim as f32,
            rows as f32,
            rows as f32 - FRAME_CHIN * oh,
        )
    }

    /// The moulding's left edge, which the model badge is flush with.
    fn moulding_left(dim: u32, rows: u32) -> f32 {
        let (ox, _, _, oh) =
            opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32));
        ox - (FRAME_SIDE - FRAME_BAND) * oh
    }

    #[test]
    fn the_bezel_covers_the_display_rect_and_nothing_below() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(device, queue, &src, BezelStyle::Model1084, None, 1.0);
        for y in 0..rows {
            for x in 0..dim {
                let p = at(&px, dim, x, y);
                assert_ne!(p, SENTINEL, "hole in the pass at ({x}, {y})");
                assert!(
                    !(p[0] > 180 && p[1] < 80 && p[2] > 180),
                    "status-bar magenta bled into the display at ({x}, {y}): {p:?}"
                );
            }
        }
        for y in rows..dim {
            for x in 0..dim {
                assert_eq!(
                    at(&px, dim, x, y),
                    SENTINEL,
                    "pass wrote outside its viewport at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_picture_fills_the_opening_and_the_frame_is_two_tone_plastic() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(device, queue, &src, BezelStyle::Model1084, None, 1.0);

        // Centre of the opening: the flat grey source, resampled 1:1 in
        // colour terms.
        let (ox, oy, ow, oh) =
            opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32));
        let centre = at(&px, dim, (ox + ow * 0.5) as u32, (oy + oh * 0.5) as u32);
        for (c, v) in centre[..3].iter().enumerate() {
            assert!(
                v.abs_diff(GREY) <= 3,
                "channel {c} at the opening centre is {v}, not the source grey: {centre:?}"
            );
        }

        // The front is two-tone: the outer frame's pale plastic with the
        // inner bezel's much darker moulding set into it. Sample the
        // middle of each run to the left of the tube, and hold each to the
        // colour the shader declares for it rather than to a hue this test
        // remembers.
        let (cx, _, _, _, chin_top) = cabinet_of(dim, rows);
        let mid_y = (oy + oh * 0.5) as u32;
        let cabinet = at(&px, dim, (cx + 3.0) as u32, mid_y);
        let moulding = at(&px, dim, ((cx + ox) * 0.5) as u32, mid_y);
        is_colour(cabinet, "CASE", 6, "the outer frame");
        is_colour(moulding, "MOULDING", 6, "the inner bezel");
        assert!(
            cabinet[1] > moulding[1] + 20,
            "moulding ({moulding:?}) is not darker than the cabinet ({cabinet:?})"
        );

        // The cabinet fills the viewport: no letterbox and no bars at the
        // sides, so the middle of all four edges is its plastic and the
        // front reaches the window it stands in.
        for (x, y) in [
            (0, rows / 2),
            (dim - 1, rows / 2),
            (dim / 2, 0),
            (dim / 2, rows - 1),
        ] {
            is_shaded(
                at(&px, dim, x, y),
                "CASE",
                &format!("the edge at ({x}, {y})"),
            );
        }

        // Its four corners are the one place it does not: they take the
        // smallest of radii, so the extreme pixel falls outside the
        // outline and shows the black behind it. The arc is small -- a
        // pixel in from it along both edges is already plastic.
        let arc = (shader_constant(M1084_WGSL, "R_PLASTIC", "bezel_1084.wgsl") * oh).ceil() as u32;
        for (x, y) in [(0, 0), (dim - 1, 0), (0, rows - 1), (dim - 1, rows - 1)] {
            let p = at(&px, dim, x, y);
            assert!(
                p[0] < 16 && p[1] < 16 && p[2] < 16,
                "the cabinet's corner at ({x}, {y}) is not cut: {p:?}"
            );
            let inx = if x == 0 { arc } else { dim - 1 - arc };
            let iny = if y == 0 { arc } else { rows - 1 - arc };
            is_shaded(
                at(&px, dim, inx, iny),
                "CASE",
                &format!("just inside the corner at ({x}, {y})"),
            );
        }

        // The power lamp glows red, near the top of the button at the
        // chin's right. That button hangs off the line where the inner
        // bezel begins to fall away to the tube -- not off the moulding's
        // outer edge -- so find the lamp and hold it to that line, which
        // is what the whole right of the front is laid out from.
        let span_x = shader_constant(M1084_WGSL, "CHAMFER_SPAN_X", "bezel_1084.wgsl");
        let recess_x = ox + ow + span_x * (ox - FRAME_BAND * oh).max(1.0);
        let mut lamp: Option<(u32, u32, u32)> = None;
        for y in (chin_top as u32)..rows {
            for x in 0..dim {
                let p = at(&px, dim, x, y);
                if p[0] > p[1].saturating_add(60) && p[0] > p[2].saturating_add(60) {
                    lamp = Some(match lamp {
                        None => (x, x, y),
                        Some((x0, x1, y0)) => (x0.min(x), x1.max(x), y0.min(y)),
                    });
                }
            }
        }
        let (lx0, lx1, ly0) = lamp.expect("no red power lamp anywhere on the chin");
        assert!(
            (lx1 as f32) < recess_x && (lx0 as f32) > recess_x - 0.09 * dim as f32,
            "the lamp ({lx0}..{lx1}) is not in the button that meets the recess line \
             at {recess_x:.1}"
        );
        assert!(
            (ly0 as f32) < chin_top + 0.3 * (rows as f32 - chin_top),
            "the lamp sits too low on the button: row {ly0} of the chin below {chin_top:.1}"
        );
    }

    #[test]
    fn the_badges_print_on_the_chin_panel() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(device, queue, &src, BezelStyle::Model1084, None, 1.0);

        // The chin as the shader lays it out: the model badge at 0.068 of
        // the cabinet width, the logotype centred at 0.47, both on the
        // panel's own middle.
        let (case_x, _, case_w, _, chin_top) = cabinet_of(dim, rows);
        let chin_h = rows as f32 - chin_top;
        assert!(
            chin_h * 0.38 >= 7.0,
            "test target too small for the chin lettering: {chin_h}"
        );
        let mid = chin_top + chin_h * 0.5;
        let ink = |x0: f32, x1: f32| {
            let mut n = 0;
            for y in (mid - 0.28 * chin_h) as u32..=(mid + 0.28 * chin_h) as u32 {
                for x in x0.max(0.0) as u32..=x1.min(dim as f32 - 1.0) as u32 {
                    let p = at(&px, dim, x, y);
                    // Blue ink on pale plastic: the logotype's saturated
                    // blue and the badge's paler one both read as clearly
                    // blue and darker than the case around them.
                    if p[0] < 170 && p[2] > p[0] + 20 {
                        n += 1;
                    }
                }
            }
            n
        };
        // The badge as the shader lays it out: 43 cells of lettering, the
        // first of which carries no ink, in a frame with a margin of 1.56
        // half-caps either side.
        let bs = chin_h * 0.38 / 13.0;
        let bcap = 0.5 * 13.0 * bs;
        let badge_x = moulding_left(dim, rows);
        let badge_w = (43.0 - 1.0) * bs + 2.0 * bcap * 1.56;
        let badge = ink(badge_x, badge_x + badge_w);
        assert!(badge >= 12, "model badge missing: {badge} ink pixels");
        let mark_w = chin_h * 0.27 * (67.0 / 9.0 + 2.0 * 1.28 * 0.62 + 0.42 * 0.62);
        let mark_x = case_x + 0.47 * case_w - mark_w * 0.5;
        let mark = ink(mark_x, mark_x + mark_w);
        assert!(mark >= 20, "logotype missing: {mark} ink pixels");
        // The stretch between them stays plain plastic: neither may smear
        // across the panel.
        let plain = ink(badge_x + badge_w + 6.0, mark_x - 6.0);
        assert_eq!(plain, 0, "ink between the badge and the logotype: {plain}");
    }

    /// The window pairs the bezel with a preset by re-aiming the preset at
    /// the opening; the combined result must still be framed (the
    /// moulding intact) with the preset's scanline structure inside it.
    #[test]
    fn a_crt_preset_lands_inside_the_opening() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(
            device,
            queue,
            &src,
            BezelStyle::Model1084,
            Some(ShaderKind::Scanlines),
            1.0,
        );

        let (ox, oy, ow, oh) =
            opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32));
        // Scanlines modulate the opening interior: one column of it must
        // not be flat.
        let x = (ox + ow * 0.5) as u32;
        let y0 = (oy + oh * 0.25) as u32;
        let y1 = (oy + oh * 0.75) as u32;
        let (mut min, mut max) = (255u8, 0u8);
        for y in y0..y1 {
            let v = at(&px, dim, x, y)[1];
            min = min.min(v);
            max = max.max(v);
        }
        assert!(
            max - min > 10,
            "no scanline structure inside the opening (min {min}, max {max})"
        );

        // The frame survives the preset pass untouched: the inner bezel's
        // flat face beside the tube is still its own plastic, not the
        // preset's picture and not its off-face black.
        let (cx, _, _, _, _) = cabinet_of(dim, rows);
        let band = at(&px, dim, ((cx + ox) * 0.5) as u32, (oy + oh * 0.5) as u32);
        is_colour(
            band,
            "MOULDING",
            6,
            "the left moulding after the preset pass",
        );

        // The frame is drawn on top of the preset: at the opening's
        // bounding-box corner -- inside the preset's square viewport but
        // outside the rounded opening -- the plastic lip must show, not
        // the preset's off-face black. This is the layering the
        // frame-only pass exists for: the moulding overlaps the tube.
        let corner = at(&px, dim, ox as u32, oy as u32);
        is_shaded(corner, "MOULDING", "the opening's corner");
    }

    /// The antialiased join at the opening edge in the frame-only pass
    /// must blend toward the preset's off-face black, not the raw
    /// picture: a bright source used to smear a picture-coloured
    /// hairline around the opening, over the preset output the pass
    /// discards its interior for. With the chamfer meeting the picture
    /// directly, "no leak" is exactly source-independence: everything
    /// past the join's half-pixel blend must render identically whatever
    /// the picture holds, so the frame is compared between an all-white
    /// and an all-black source.
    #[test]
    fn the_frame_only_join_does_not_leak_the_picture() {
        // Full strength, and half: the moulding has to seat the preset's
        // face at every strength, and the bow and the face's corner radius
        // fade by different curves on the way there.
        for strength in [1.0, 0.5] {
            frame_only_join_is_source_independent(strength);
        }
    }

    fn frame_only_join_is_source_independent(strength: f32) {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let white = |_x: u32, _y: u32| [255, 255, 255, 255];
        let black = |_x: u32, _y: u32| [0, 0, 0, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &white);
        let (px, dim, rows) = render_bezel(
            device,
            queue,
            &src,
            BezelStyle::Model1084,
            Some(ShaderKind::Crt),
            strength,
        );
        let dark = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &black);
        let (px_dark, dim_dark, rows_dark) = render_bezel(
            device,
            queue,
            &dark,
            BezelStyle::Model1084,
            Some(ShaderKind::Crt),
            strength,
        );
        assert_eq!((dim, rows), (dim_dark, rows_dark));

        // The shader's glass-contour distance at a pixel centre: the same
        // warp and rounded rectangle the preset's face uses, driven by
        // the preset's own curvature so the replica tracks the shader
        // whatever value the preset table carries.
        let (ox, oy, ow, oh) =
            opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32));
        let k = crt_shader::uniforms_for(
            ShaderKind::Crt,
            strength,
            (0, 0, dim, rows),
            rows as usize,
            rows as usize,
            (dim, rows),
            1.0,
        )
        .0
        .params[3];
        let (hx, hy) = (ow * 0.5, oh * 0.5);
        let fa = hy / hx;
        // The aperture opens to the preset's faded face when that is the
        // wider, exactly as the shader does.
        let rc = shader_constant(M1084_WGSL, "APERTURE_RADIUS", "bezel_1084.wgsl")
            .max(crt_shader::FACE_CORNER_RADIUS * strength);
        let opening_d = |x: u32, y: u32| {
            let cx = (x as f32 + 0.5 - ox - hx) / hx;
            let cy = (y as f32 + 0.5 - oy - hy) / hy;
            let q = k * 0.25;
            let r2 = cx * cx + cy * cy * fa * fa;
            let bow = 1.0 + q * r2;
            // Faded by mixing coordinates, as the shader and crt.wgsl do.
            let wx = cx + (cx * bow / (1.0 + q) - cx) * strength;
            let wy = (cy + (cy * bow / (1.0 + q * fa * fa) - cy) * strength) * fa;
            let qx = wx.abs() - 1.0 + rc;
            let qy = wy.abs() - fa + rc;
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            (outside + qx.max(qy).min(0.0) - rc) * hx
        };

        // The picture may only reach the output through the join's
        // half-pixel antialias band (the mix weight hits exactly 1.0 past
        // d = aa / 2, about half a pixel out). Everything beyond it -- the
        // chamfer's innermost run included -- must not change with the
        // source; any difference is the picture leaking into the frame.
        // The margin of 0.6 px keeps legitimately-blended boundary pixels
        // out of the comparison whatever their sub-pixel alignment.
        let mut checked = 0;
        for y in 0..rows {
            for x in 0..dim {
                if opening_d(x, y) >= 0.6 {
                    checked += 1;
                    let p = at(&px, dim, x, y);
                    let q = at(&px_dark, dim, x, y);
                    for c in 0..3 {
                        assert!(
                            p[c].abs_diff(q[c]) <= 1,
                            "frame pixel follows the picture at ({x}, {y}): white {p:?} vs black {q:?}"
                        );
                    }
                }
            }
        }
        assert!(
            checked > 100,
            "frame region barely sampled: {checked} pixels"
        );
    }

    /// WGSL leaves `smoothstep` undefined when the first edge is not below
    /// the second, and backends disagree there, so a reversed interval is a
    /// portability bug that looks fine on the machine it was written on.
    /// Every edge pair a source states in comparable terms must ascend.
    #[test]
    fn every_smoothstep_interval_ascends() {
        /// An edge as a coefficient and the symbol it scales, so
        /// `0.26 * h` and `0.10 * h` can be compared and a pair in
        /// unrelated terms can be skipped rather than guessed at.
        fn edge(text: &str) -> Option<(f32, &str)> {
            let (num, sym) = match text.split_once('*') {
                Some((n, s)) => (n.trim(), s.trim()),
                None => (text.trim(), ""),
            };
            Some((num.parse().ok()?, sym))
        }
        for style in drawn_styles() {
            let src = source(style);
            for (at, _) in src.match_indices("smoothstep(") {
                let open = at + "smoothstep(".len();
                let rest = &src[open..];
                // Split the call's own arguments: the edges are the first
                // two, and neither contains a nested call in these sources.
                let end = rest.find(')').expect("terminated smoothstep");
                let args: Vec<&str> = rest[..end].split(',').collect();
                assert!(
                    args.len() >= 3,
                    "{}: smoothstep needs 3 arguments",
                    style.label()
                );
                let (Some((lo, lo_sym)), Some((hi, hi_sym))) = (edge(args[0]), edge(args[1]))
                else {
                    continue;
                };
                if lo_sym != hi_sym {
                    continue;
                }
                assert!(
                    lo < hi,
                    "{}: smoothstep({}, {}, ..) is a reversed interval",
                    style.label(),
                    args[0].trim(),
                    args[1].trim()
                );
            }
        }
    }

    /// The moulding must cover the preset's face: `crt.wgsl` states the
    /// radius it clips that face to and `crt_shader` mirrors it for the
    /// bezel to open to, so the two must agree -- and the bezel's own
    /// aperture must be the tighter of the pair for opening to it to mean
    /// anything.
    #[test]
    fn the_aperture_is_never_tighter_than_the_face_it_covers() {
        let face = shader_constant(
            include_str!("shaders/crt.wgsl"),
            "CORNER_RADIUS",
            "crt.wgsl",
        );
        assert_eq!(
            face,
            crt_shader::FACE_CORNER_RADIUS,
            "crt.wgsl's face radius and the constant the bezel opens to drifted apart"
        );
        // The plastic overlaps the glass. An aperture tighter than the
        // face it covers would cut inside it and leave the preset's own
        // off-face black showing in the opening's corners, so every source
        // takes the wider of the two -- and every source must therefore
        // state an aperture for that `max()` to reach.
        for style in drawn_styles() {
            let src = source(style);
            let aperture = shader_constant(src, "APERTURE_RADIUS", style.label());
            assert!(
                aperture > 0.0,
                "{}: aperture must be a real radius",
                style.label()
            );
            assert!(
                src.contains("max(APERTURE_RADIUS, u.params.z)"),
                "{}: does not open its aperture to the preset's face",
                style.label()
            );
            // The bow is faded by mixing coordinates, as crt.wgsl fades
            // it. Warping by a faded curvature is a different curve --
            // `warp` is rational in the curvature -- so a source that
            // pre-fades it drifts off the face between strengths.
            assert!(
                src.contains("mix(cn, cn * (1.0 + q * r2) / m, u.params.w)"),
                "{}: does not fade the bow the way crt.wgsl does",
                style.label()
            );
            assert!(
                effective_aperture(style) >= face,
                "{}: opening cuts inside the face it covers",
                style.label()
            );
        }
    }

    /// The shader derives the whole cabinet back from the opening this
    /// module placed, using its own copy of the frame proportions. If the
    /// two copies drift the frame stops matching its own opening: the
    /// picture would sit off-centre in a cabinet of the wrong size.
    #[test]
    fn the_frame_proportions_match_the_shader() {
        fn constant(name: &str) -> f32 {
            let key = format!("const {name}: f32 =");
            let at = M1084_WGSL
                .find(&key)
                .unwrap_or_else(|| panic!("bezel_1084.wgsl: no {name}"));
            let rest = &M1084_WGSL[at + key.len()..];
            let end = rest.find(';').expect("terminated constant");
            // Written as `ratio * FRAME_WEIGHT`, or bare for the weight.
            rest[..end]
                .split('*')
                .map(|t| {
                    let t = t.trim();
                    if t == "FRAME_WEIGHT" {
                        constant("FRAME_WEIGHT")
                    } else {
                        t.parse().unwrap_or_else(|e| panic!("{name}: {e}"))
                    }
                })
                .product()
        }
        for (name, ours) in [
            ("FRAME_WEIGHT", FRAME_WEIGHT),
            ("FRAME_SIDE", FRAME_SIDE),
            ("FRAME_TOP", FRAME_TOP),
            ("FRAME_WELL_BOTTOM", FRAME_WELL_BOTTOM),
            ("FRAME_CHIN", FRAME_CHIN),
            ("FRAME_BAND", FRAME_BAND),
        ] {
            let theirs = constant(name);
            assert!(
                (ours - theirs).abs() < 1e-6,
                "{name}: bezel.rs has {ours}, bezel_1084.wgsl has {theirs}"
            );
        }
    }

    /// Both fronts' aperture radii are restated outside the test module,
    /// so the overlays can be kept out of the corners they cut without
    /// parsing WGSL every frame. Hold the copies to the originals.
    #[test]
    fn the_aperture_radii_match_the_shaders() {
        for (style, ours) in [
            (BezelStyle::Model1084, APERTURE_RADIUS_1084),
            (BezelStyle::Classic, APERTURE_RADIUS_CLASSIC),
        ] {
            let theirs = shader_constant(source(style), "APERTURE_RADIUS", style.label());
            assert!(
                (ours - theirs).abs() < 1e-6,
                "{}: bezel.rs has {ours}, its shader has {theirs}",
                style.label()
            );
        }
    }

    /// A plain rectangle of a picture keeps its corners, so nothing moves;
    /// anything that shapes it takes some, and the overlay clears what it
    /// takes.
    #[test]
    fn the_corner_inset_answers_to_what_shapes_the_picture() {
        use crate::config::ShaderKind;
        let flat = crt_shader::face_curvature(ShaderKind::Scanlines);
        let bow = crt_shader::face_curvature(ShaderKind::Crt);
        assert_eq!(flat, 0.0, "only the full preset shapes a face");
        assert!(bow > 0.0);

        // Nothing shaping the picture: the overlays stay where they are.
        assert_eq!(corner_inset(BezelStyle::None, 0.0, 1.0, 1), 0);
        assert_eq!(corner_inset(BezelStyle::None, flat, 1.0, 1), 0);
        assert_eq!(
            corner_inset(BezelStyle::None, bow, 0.0, 1),
            0,
            "a preset at nil strength draws a flat picture"
        );

        for scale in [1, 2] {
            let front = corner_inset(BezelStyle::Model1084, 0.0, 1.0, scale);
            let both = corner_inset(BezelStyle::Model1084, bow, 1.0, scale);
            let preset = corner_inset(BezelStyle::None, bow, 1.0, scale);
            for (what, got) in [("front", front), ("front and bow", both), ("bow", preset)] {
                assert!(got > 0, "{what} at {scale}x takes nothing off the corner");
                assert!(
                    got < crate::video::present_height() * scale / 3,
                    "{what} at {scale}x pushes the overlay {got} into the picture"
                );
            }
            // The bow is the dominant term by a long way; a front alone
            // only rounds the corner off.
            assert!(
                both > front * 3,
                "the bow should cost far more: {front} vs {both}"
            );
            assert!(
                preset > 0 && preset <= both,
                "a front widens the aperture, never narrows it"
            );
        }
        // The picture scales with the texture, so the inset tracks it.
        let one = corner_inset(BezelStyle::Model1084, bow, 1.0, 1);
        let two = corner_inset(BezelStyle::Model1084, bow, 1.0, 2);
        let ratio = two as f32 / one as f32;
        assert!(
            (1.8..=2.2).contains(&ratio),
            "inset should track the texture: {one} at 1x, {two} at 2x"
        );
    }

    /// The figure is only worth anything if the overlay lands on the
    /// picture. Render the front over a known source and check the corner
    /// the overlays are placed at: outside the inset it is plastic, inside
    /// it is picture. This is the check the arithmetic exists to pass, and
    /// it holds the solver to the shader rather than to itself.
    #[test]
    fn an_overlay_at_the_corner_inset_lands_on_the_picture() {
        use crate::config::ShaderKind;
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(device, queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let bow = crt_shader::face_curvature(ShaderKind::Crt);

        for (label, preset, curvature) in [
            ("no preset", None, 0.0),
            ("the CRT preset", Some(ShaderKind::Crt), bow),
        ] {
            let (px, dim, rows) =
                render_bezel(device, queue, &src, BezelStyle::Model1084, preset, 1.0);
            let (ox, oy, ow, oh) =
                opening_rect(BezelStyle::Model1084, (0.0, 0.0, dim as f32, rows as f32));
            let margin = 8.0;
            let inset = corner_inset(BezelStyle::Model1084, curvature, 1.0, 1) as f32;
            // Both are stated in texture pixels; what carries across to
            // the render is the fraction of the picture they sit at.
            let fx = |t: f32| ox + t / crate::video::FB_WIDTH as f32 * ow;
            let fy = |t: f32| oy + oh - t / crate::video::present_height() as f32 * oh;
            // Not "is it the source grey": a preset dims the picture and
            // tints it into the mask's stripes, so the grey is long gone
            // by the corner. What the picture is not, at any strength, is
            // the moulding -- so the plastic's own tint is the test.
            let is_picture =
                |x: f32, y: f32| !looks_shaded(at(&px, dim, x as u32, y as u32), "MOULDING");
            assert!(
                is_picture(fx(margin + inset), fy(margin + inset)),
                "{label}: the overlay's corner at +{inset} is still off the picture"
            );
            if inset > 1.0 {
                assert!(
                    !is_picture(fx(margin), fy(margin)),
                    "{label}: nothing was taken off the corner, so the inset is waste"
                );
            }
        }
    }

    /// Not a check: renders the bezel (optionally with the CRT preset the
    /// window would pair it with) over a source image and writes a PNG for
    /// eyeballing look changes. Runs only when
    /// COPPERLINE_BEZEL_PREVIEW_OUT names the output file; with
    /// COPPERLINE_BEZEL_PREVIEW_SRC set, that PNG becomes the picture
    /// (its full height, no status bar), otherwise a test card is used.
    /// Setting COPPERLINE_BEZEL_PREVIEW_SHADER (to any value) adds the
    /// preset pass; COPPERLINE_BEZEL_PREVIEW_BARE instead renders the
    /// preset alone over the full frame, no bezel, for eyeballing the
    /// preset's own face geometry. COPPERLINE_BEZEL_PREVIEW_STYLE names
    /// the front to draw, defaulting to 1084.
    #[test]
    #[ignore = "preview dump; set COPPERLINE_BEZEL_PREVIEW_OUT"]
    fn dump_bezel_preview_png() {
        let Ok(out) = std::env::var("COPPERLINE_BEZEL_PREVIEW_OUT") else {
            eprintln!("skipping: COPPERLINE_BEZEL_PREVIEW_OUT not set");
            return;
        };
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let with_crt = std::env::var("COPPERLINE_BEZEL_PREVIEW_SHADER").is_ok();
        let style = std::env::var("COPPERLINE_BEZEL_PREVIEW_STYLE")
            .ok()
            .and_then(|s| crate::config::parse_bezel(&s).ok())
            .unwrap_or(BezelStyle::Model1084);
        let bare = std::env::var("COPPERLINE_BEZEL_PREVIEW_BARE").is_ok();
        let (src, w, h) = match std::env::var("COPPERLINE_BEZEL_PREVIEW_SRC") {
            Ok(path) => {
                let decoder = png::Decoder::new(std::fs::File::open(&path).expect("open src"));
                let mut reader = decoder.read_info().expect("png info");
                let mut buf = vec![0; reader.output_buffer_size()];
                let info = reader.next_frame(&mut buf).expect("png frame");
                let (w, h) = (info.width, info.height);
                let rgba: Vec<[u8; 4]> = match info.color_type {
                    // Source alpha is deliberately forced opaque: the
                    // texture stands in for the composited display
                    // buffer, which has no transparency, and the pass
                    // samples it as an opaque monitor picture.
                    png::ColorType::Rgba => buf
                        .chunks_exact(4)
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect(),
                    png::ColorType::Rgb => buf
                        .chunks_exact(3)
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect(),
                    other => panic!("unsupported source colour type {other:?}"),
                };
                let tex = source_texture(device, queue, w, h, h, &move |x, y| {
                    rgba[(y * w + x) as usize]
                });
                (tex, w, h)
            }
            Err(_) => {
                let card = |x: u32, y: u32| {
                    let bar = (x * 8 / TEX) as u8;
                    [bar * 32, 255 - bar * 32, (y * 4) as u8, 255]
                };
                (
                    source_texture(device, queue, TEX, TEX, TEX, &card),
                    TEX,
                    TEX,
                )
            }
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bezel_preview_target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let (crt_uniforms, viewport) = crt_shader::uniforms_for(
            if with_crt || bare {
                ShaderKind::Crt
            } else {
                ShaderKind::None
            },
            1.0,
            (0, 0, w, h),
            h as usize,
            h as usize,
            (w, h),
            (h / 2) as f32,
        );
        let opening = opening_rect(style, viewport);
        if bare {
            let mut crt = crt_shader::CrtShader::new(device, FORMAT);
            crt.render(
                device,
                queue,
                &src,
                &mut encoder,
                &view,
                viewport,
                ShaderKind::Crt,
                crt_uniforms,
            );
        } else {
            if with_crt {
                let mut crt = crt_shader::CrtShader::new(device, FORMAT);
                crt.render(
                    device,
                    queue,
                    &src,
                    &mut encoder,
                    &view,
                    opening,
                    ShaderKind::Crt,
                    crt_uniforms.with_viewport(opening),
                );
            }
            let mut shader = BezelShader::new(device, FORMAT);
            shader.render(
                device,
                queue,
                &src,
                &mut encoder,
                &view,
                viewport,
                style,
                uniforms_from(&crt_uniforms, viewport, opening, with_crt),
            );
        }
        let px = read_back(device, queue, encoder, &target, (w, h));
        let file = std::fs::File::create(&out).expect("create preview");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        let bytes: Vec<u8> = px.iter().flatten().copied().collect();
        writer.write_image_data(&bytes).expect("png data");
        eprintln!("bezel preview written to {out}");
    }
}

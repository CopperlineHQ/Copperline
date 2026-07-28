// SPDX-License-Identifier: GPL-3.0-or-later

//! The optional monitor-bezel pass: a procedural plastic front frame in
//! the spirit of the 1084, drawn over the display rect on the GPU with
//! the picture re-sampled into the rounded opening the frame leaves.
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
//! plastic overlaps the tube face like the real moulding, so the frame's
//! rounded corners and recess clip the preset's square viewport rather
//! than being buried under it.
//!
//! Purely a presentation stage, like the CRT pass: screenshots, frame
//! dumps, recordings and headless runs never include the bezel. Unlike
//! the CRT presets it is not user-replaceable and carries no strength
//! knob: it is either drawn or skipped.

use pixels::wgpu;
use zerocopy::{Immutable, IntoBytes};

use super::crt_shader;

/// The bezel WGSL source, embedded like the preset sources.
const BEZEL_WGSL: &str = include_str!("shaders/bezel.wgsl");

/// Fraction of the display rect the picture keeps when the bezel is on;
/// the rest becomes the plastic frame. Both axes scale by this one
/// factor, so the picture keeps its aspect.
pub(super) const OPENING_SCALE: f32 = 0.85;

/// How the freed height splits around the opening: the top margin takes
/// this share and the bottom band the rest, so the bottom comes out
/// wider than the top like the 1084's face.
const TOP_SHARE: f32 = 0.42;

/// Size of the uniform block, in bytes. The shared bind group layout pins
/// its `min_binding_size` to the CRT block's 64 bytes, so this block must
/// stay the same size to build against it.
const UNIFORM_BYTES: u64 = std::mem::size_of::<BezelUniforms>() as u64;

/// The uniform block `shaders/bezel.wgsl` sees. Mirrors the WGSL struct
/// exactly; `#[repr(C)]` with 16-byte-aligned members only.
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
    /// painted it), 0 = full. yzw: reserved.
    params: [f32; 4],
}

/// The picture opening the bezel leaves inside a display viewport rect
/// (physical surface pixels): scaled by [`OPENING_SCALE`] about both
/// axes, centred horizontally, sitting high by [`TOP_SHARE`].
pub(super) fn opening_rect(viewport: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = viewport;
    let ow = w * OPENING_SCALE;
    let oh = h * OPENING_SCALE;
    (x + (w - ow) * 0.5, y + (h - oh) * TOP_SHARE, ow, oh)
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
    BezelUniforms {
        src_rect: crt.src_rect,
        size: crt.size,
        opening: [(ox - vx) / w, (oy - vy) / h, ow / w, oh / h],
        params: [if frame_only { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
    }
}

/// The bezel pass: one fixed pipeline and the bindings it needs.
pub(super) struct BezelShader {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
    bind_group: Option<wgpu::BindGroup>,
    /// The texture the current bind group views, compared by identity:
    /// `pixels` recreates its backing texture on a buffer resize, and the
    /// stale view has to be dropped with it.
    bound_texture: Option<wgpu::Texture>,
}

impl BezelShader {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = crt_shader::shader_bind_group_layout(device);
        let pipeline = crt_shader::build_pipeline(
            device,
            &bind_group_layout,
            BEZEL_WGSL,
            "bezel_shader",
            target_format,
        );
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
            uniforms,
            pipeline,
            bind_group: None,
            bound_texture: None,
        }
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
        uniforms: BezelUniforms,
    ) {
        let (x, y, w, h) = viewport;
        if w < 1.0 || h < 1.0 {
            return;
        }
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
        let Some(bind_group) = &self.bind_group else {
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
        pass.set_pipeline(&self.pipeline);
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
    fn the_bezel_source_validates() {
        if let Err(e) = crt_shader::validate_wgsl_source(BEZEL_WGSL) {
            panic!("bezel shader failed validation:\n{e}");
        }
    }

    /// The bezel is not a preset: it must not carry the preset contract
    /// markers, or the contract test would be expected to cover it.
    #[test]
    fn the_bezel_is_not_a_contract_preset() {
        assert!(!BEZEL_WGSL.contains("--- begin shared contract ---"));
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
        let (ox, oy, ow, oh) = opening_rect((0.0, 0.0, 716.0, 537.0));
        // Both axes scale by the one factor, so the picture is not
        // stretched.
        assert!((ow / 716.0 - OPENING_SCALE).abs() < 1e-6);
        assert!((oh / 537.0 - OPENING_SCALE).abs() < 1e-6);
        // Centred horizontally, with the bottom band wider than the top
        // margin like the 1084's face.
        assert!((ox - (716.0 - ow) * 0.5).abs() < 1e-3);
        let top = oy;
        let bottom = 537.0 - (oy + oh);
        assert!(top > 0.0, "top margin missing: {top}");
        assert!(
            bottom > top,
            "bottom band ({bottom}) not wider than top ({top})"
        );
    }

    #[test]
    fn a_letterboxed_viewport_offsets_the_opening() {
        let flat = opening_rect((0.0, 0.0, 640.0, 480.0));
        let off = opening_rect((12.0, 34.0, 640.0, 480.0));
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
        let opening = opening_rect(viewport);
        let u = uniforms_from(&crt, viewport, opening, false);
        assert_eq!(u.src_rect, crt.src_rect);
        assert_eq!(u.size, crt.size);
        // The opening is expressed relative to the viewport, so the
        // letterbox offset must cancel out.
        assert!((u.opening[0] - (1.0 - OPENING_SCALE) * 0.5).abs() < 1e-6);
        assert!((u.opening[2] - OPENING_SCALE).abs() < 1e-6);
        assert!((u.opening[3] - OPENING_SCALE).abs() < 1e-6);
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
        let opening = opening_rect(viewport);
        let re = crt.with_viewport(opening);
        assert_eq!(re.src_rect, crt.src_rect);
        assert_eq!(re.params, crt.params);
        assert_eq!(re.size[0], opening.2);
        assert_eq!(re.size[1], opening.3);
        assert_eq!(re.size[2..], crt.size[2..]);
    }

    // --- offscreen render (needs a GPU adapter) -------------------------

    const TEX: u32 = 64;
    const DISPLAY_ROWS: u32 = 48;
    const GREY: u8 = 128;
    /// Magnification of the render target over the source, as a real
    /// window magnifies the composited buffer. Big enough that the frame's
    /// trim zones (recess, bevel, plastic band) are pixels wide.
    const SCALE: u32 = 4;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    /// Target clear colour; pure blue survives the sRGB encode exactly,
    /// so any surviving sentinel pixel inside the viewport means the pass
    /// left a hole, and any outside means it overdrew its viewport.
    const SENTINEL: [u8; 4] = [0, 0, 255, 255];

    /// A device, or `None` on a machine with no usable adapter (headless
    /// CI): the render tests then pass without asserting anything.
    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            match crt_shader::poll_once(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
            })) {
                Some(Ok(adapter)) => adapter,
                _ => return None,
            };
        match crt_shader::poll_once(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("bezel_shader_test"),
            ..Default::default()
        })) {
            Some(Ok(pair)) => Some(pair),
            _ => None,
        }
    }

    /// The `pixels` backing texture in miniature: `DISPLAY_ROWS` rows of
    /// the given texels (or flat grey) with a magenta "status bar" below.
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
        crt_kind: Option<ShaderKind>,
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
            1.0,
            (0, 0, dim, dim),
            DISPLAY_ROWS as usize,
            TEX as usize,
            (TEX, TEX),
            DISPLAY_ROWS as f32,
        );
        assert_eq!(viewport, (0.0, 0.0, dim as f32, rows as f32));
        let opening = opening_rect(viewport);
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

    #[test]
    fn the_bezel_covers_the_display_rect_and_nothing_below() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(&device, &queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(&device, &queue, &src, None);
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
    fn the_picture_fills_the_opening_and_the_frame_is_plastic() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(&device, &queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(&device, &queue, &src, None);

        // Centre of the opening: the flat grey source, resampled 1:1 in
        // colour terms.
        let (ox, oy, ow, oh) = opening_rect((0.0, 0.0, dim as f32, rows as f32));
        let centre = at(&px, dim, (ox + ow * 0.5) as u32, (oy + oh * 0.5) as u32);
        for (c, v) in centre[..3].iter().enumerate() {
            assert!(
                v.abs_diff(GREY) <= 3,
                "channel {c} at the opening centre is {v}, not the source grey: {centre:?}"
            );
        }

        // Middle of the left band: warm plastic, not the grey source and
        // not black. Warm means the red channel clearly leads the blue.
        let band = at(&px, dim, 10, (oy + oh * 0.5) as u32);
        assert!(
            band[0] > band[2] + 6,
            "left band is not warm plastic: {band:?}"
        );
        assert!(
            (120..=245).contains(&band[0]),
            "left band brightness out of range: {band:?}"
        );

        // The case corners are rounded off to the letterbox black.
        for (x, y) in [(0, 0), (dim - 1, 0), (0, rows - 1), (dim - 1, rows - 1)] {
            let p = at(&px, dim, x, y);
            assert!(
                p[0] < 16 && p[1] < 16 && p[2] < 16,
                "case corner at ({x}, {y}) is not dark: {p:?}"
            );
        }

        // The power LED glows green on the bottom band, right of centre.
        let led_x = (0.91 * dim as f32) as i32;
        let led_y = ((oy + oh + rows as f32) * 0.5) as i32;
        let found = (-3..=3).any(|dy| {
            (-3..=3).any(|dx| {
                let p = at(&px, dim, (led_x + dx) as u32, (led_y + dy) as u32);
                p[1] > p[0].saturating_add(50) && p[1] > p[2].saturating_add(50)
            })
        });
        assert!(found, "no green power LED near ({led_x}, {led_y})");
    }

    /// The window pairs the bezel with a preset by re-aiming the preset at
    /// the opening; the combined result must still be framed (plastic band
    /// intact) with the preset's scanline structure inside the opening.
    #[test]
    fn a_crt_preset_lands_inside_the_opening() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let grey = |_x: u32, _y: u32| [GREY, GREY, GREY, 255];
        let src = source_texture(&device, &queue, TEX, TEX, DISPLAY_ROWS, &grey);
        let (px, dim, rows) = render_bezel(&device, &queue, &src, Some(ShaderKind::Scanlines));

        let (ox, oy, ow, oh) = opening_rect((0.0, 0.0, dim as f32, rows as f32));
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

        // The plastic band survives the preset pass untouched.
        let band = at(&px, dim, 10, (oy + oh * 0.5) as u32);
        assert!(
            band[0] > band[2] + 6,
            "left band lost its plastic after the preset pass: {band:?}"
        );

        // The frame is drawn on top of the preset: at the opening's
        // bounding-box corner -- inside the preset's square viewport but
        // outside the rounded opening -- the plastic lip must show, not
        // the preset's off-face black. This is the layering the
        // frame-only pass exists for: the moulding overlaps the tube.
        let corner = at(&px, dim, ox as u32, oy as u32);
        assert!(
            corner[0] > 100 && corner[0] > corner[2] + 6,
            "opening corner shows the preset instead of the frame: {corner:?}"
        );
    }

    /// Not a check: renders the bezel (optionally with the CRT preset the
    /// window would pair it with) over a source image and writes a PNG for
    /// eyeballing look changes. Runs only when
    /// COPPERLINE_BEZEL_PREVIEW_OUT names the output file; with
    /// COPPERLINE_BEZEL_PREVIEW_SRC set, that PNG becomes the picture
    /// (its full height, no status bar), otherwise a test card is used.
    /// COPPERLINE_BEZEL_PREVIEW_SHADER=crt adds the preset pass.
    #[test]
    #[ignore = "preview dump; set COPPERLINE_BEZEL_PREVIEW_OUT"]
    fn dump_bezel_preview_png() {
        let Ok(out) = std::env::var("COPPERLINE_BEZEL_PREVIEW_OUT") else {
            eprintln!("skipping: COPPERLINE_BEZEL_PREVIEW_OUT not set");
            return;
        };
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let with_crt = std::env::var("COPPERLINE_BEZEL_PREVIEW_SHADER").is_ok();
        let (src, w, h) = match std::env::var("COPPERLINE_BEZEL_PREVIEW_SRC") {
            Ok(path) => {
                let decoder = png::Decoder::new(std::fs::File::open(&path).expect("open src"));
                let mut reader = decoder.read_info().expect("png info");
                let mut buf = vec![0; reader.output_buffer_size()];
                let info = reader.next_frame(&mut buf).expect("png frame");
                let (w, h) = (info.width, info.height);
                let rgba: Vec<[u8; 4]> = match info.color_type {
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
                let tex = source_texture(&device, &queue, w, h, h, &move |x, y| {
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
                    source_texture(&device, &queue, TEX, TEX, TEX, &card),
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
            if with_crt {
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
        let opening = opening_rect(viewport);
        if with_crt {
            let mut crt = crt_shader::CrtShader::new(&device, FORMAT);
            crt.render(
                &device,
                &queue,
                &src,
                &mut encoder,
                &view,
                opening,
                ShaderKind::Crt,
                crt_uniforms.with_viewport(opening),
            );
        }
        let mut shader = BezelShader::new(&device, FORMAT);
        shader.render(
            &device,
            &queue,
            &src,
            &mut encoder,
            &view,
            viewport,
            uniforms_from(&crt_uniforms, viewport, opening, with_crt),
        );
        let px = read_back(&device, &queue, encoder, &target, (w, h));
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

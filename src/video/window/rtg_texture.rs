// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU presentation of the RTG board frame at native resolution.
//!
//! The desktop window composites the display and the UI into one fixed
//! `FB_WIDTH`-wide `pixels` buffer, so an RTG mode wider than that buffer
//! (or shown in a window larger than it) is sampled from a lower-resolution
//! intermediate and looks soft. This module keeps a GPU texture at the RTG
//! mode's native resolution and, in the `pixels` `render_with` pass, draws
//! it straight into the display sub-rect of the surface after the UI buffer
//! -- one hardware-filtered scale from native pixels to the physical display
//! rect, bypassing the intermediate entirely.
//!
//! The UI buffer (status bar, menus, cursor mapping) is untouched: the RTG
//! texture only overdraws the display region on top of it.
//!
//! Under `[display] scaling = "integer"` the same pass draws the board
//! frame at a whole-number multiple of its native resolution instead of
//! stretching it to the rect. The viewport stays the whole display rect --
//! the soft low-resolution intermediate underneath it must never peek out
//! around the picture -- and the [`RtgUniforms`] mapping puts the image in
//! the middle of it, with the fragments around it painted black.

use pixels::wgpu;
use std::borrow::Cow;
use zerocopy::{Immutable, IntoBytes};

const SHADER: &str = r#"
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct RtgUniforms {
    // xy: where the picture starts inside the viewport, zw: how much of the
    // viewport it covers, both in the viewport's own 0..1 space. The
    // identity (0, 0, 1, 1) fills the viewport, which is the smooth-scaling
    // mapping.
    rect: vec4<f32>,
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

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: RtgUniforms;

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let t = (in.uv - u.rect.xy) / u.rect.zw;
    // Sampled unconditionally: textureSample needs uniform control flow, so
    // the border is chosen after the fact rather than returned early. Reads
    // outside the picture are harmless, the sampler clamping them to the
    // edge, and are discarded here.
    let texel = textureSample(tex, samp, t);
    let inside = t.x >= 0.0 && t.x <= 1.0 && t.y >= 0.0 && t.y <= 1.0;
    return select(vec4<f32>(0.0, 0.0, 0.0, 1.0), texel, inside);
}
"#;

/// The uniform block the RTG pass sees; mirrors `RtgUniforms` in the WGSL
/// source. `#[repr(C)]` with one 16-byte member, so the two layouts agree
/// with no padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, IntoBytes, Immutable)]
pub(super) struct RtgUniforms {
    rect: [f32; 4],
}

/// Size of the uniform block, in bytes. Also the layout's
/// `min_binding_size`.
const UNIFORM_BYTES: u64 = std::mem::size_of::<RtgUniforms>() as u64;

/// The mapping that stretches the frame across the whole viewport: what
/// smooth scaling always uses, and what integer scaling falls back to.
const FILL_VIEWPORT: RtgUniforms = RtgUniforms {
    rect: [0.0, 0.0, 1.0, 1.0],
};

/// Where a `native`-sized board frame sits inside a display rect `w` x `h`
/// physical pixels when every board pixel must be the same square block of
/// host pixels.
///
/// The picture is drawn at `floor(min(w / native_w, h / native_h))` times
/// its native size and centred, which leaves black margins on two sides
/// unless the fit is exact. A rect too small to hold even a 1:1 copy has no
/// whole-number scale to offer, so it keeps the smooth behaviour of filling
/// the rect -- shrinking the picture is better than cropping it, the same
/// trade the native path makes in `effective_scaling_mode`.
///
/// Pure arithmetic, no GPU state: unit testable on its own.
pub(super) fn integer_fit_uniforms(rect: (f32, f32), native: (u32, u32)) -> RtgUniforms {
    let (rect_w, rect_h) = rect;
    let (native_w, native_h) = (native.0 as f32, native.1 as f32);
    if rect_w <= 0.0 || rect_h <= 0.0 || native_w <= 0.0 || native_h <= 0.0 {
        return FILL_VIEWPORT;
    }
    let scale = (rect_w / native_w).min(rect_h / native_h).floor();
    if scale < 1.0 {
        return FILL_VIEWPORT;
    }
    let w = native_w * scale / rect_w;
    let h = native_h * scale / rect_h;
    RtgUniforms {
        rect: [(1.0 - w) / 2.0, (1.0 - h) / 2.0, w, h],
    }
}

/// A native-resolution RTG display texture and the pipeline that scales it
/// into the window's display rect.
pub(super) struct RtgTexture {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Interpolating sampler for the smooth fit, and the point sampler
    /// integer scaling shows the board's own pixels through. Both are bound
    /// when the texture is uploaded, so the mode is a per-frame choice.
    sampler_linear: wgpu::Sampler,
    sampler_nearest: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    texture: Option<wgpu::Texture>,
    bind_group_linear: Option<wgpu::BindGroup>,
    bind_group_nearest: Option<wgpu::BindGroup>,
    dims: (u32, u32),
}

impl RtgTexture {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rtg_texture_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rtg_texture_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rtg_texture_pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rtg_texture_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rtg_texture_sampler_linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rtg_texture_sampler_nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rtg_texture_uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind_group_layout,
            sampler_linear,
            sampler_nearest,
            uniforms,
            texture: None,
            bind_group_linear: None,
            bind_group_nearest: None,
            dims: (0, 0),
        }
    }

    /// Upload the native RTG frame (`src`, `w` x `h` RGBA words). The texture
    /// (and its bind group) is recreated when the resolution changes.
    pub(super) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &[u32],
        w: u32,
        h: u32,
    ) {
        if w == 0 || h == 0 || (src.len() as u32) < w * h {
            return;
        }
        if self.dims != (w, h) || self.texture.is_none() {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rtg_texture"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = |sampler: &wgpu::Sampler, label: &str| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.uniforms.as_entire_binding(),
                        },
                    ],
                })
            };
            let linear = bind_group(&self.sampler_linear, "rtg_texture_bg_linear");
            let nearest = bind_group(&self.sampler_nearest, "rtg_texture_bg_nearest");
            self.texture = Some(texture);
            self.bind_group_linear = Some(linear);
            self.bind_group_nearest = Some(nearest);
            self.dims = (w, h);
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, (w * h * 4) as usize) };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: self.texture.as_ref().unwrap(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draw the uploaded texture into the `(x, y, w, h)` viewport rect of
    /// `target` (physical surface pixels), on top of what is already there.
    ///
    /// With `integer_scaling` the frame is drawn at a whole-number multiple
    /// of its native size, centred in the rect and point-sampled, and the
    /// rest of the rect is painted black; otherwise it is filtered out to
    /// the whole rect as before.
    pub(super) fn render(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        integer_scaling: bool,
    ) {
        let bind_group = match integer_scaling {
            true => &self.bind_group_nearest,
            false => &self.bind_group_linear,
        };
        let Some(bind_group) = bind_group else {
            return;
        };
        let (x, y, w, h) = viewport;
        if w < 1.0 || h < 1.0 {
            return;
        }
        let uniforms = if integer_scaling {
            integer_fit_uniforms((w, h), self.dims)
        } else {
            FILL_VIEWPORT
        };
        queue.write_buffer(&self.uniforms, 0, uniforms.as_bytes());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rtg_texture_pass"),
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

    /// The pass's own WGSL has to compile like the preset shaders do, and
    /// validating the source says so on a machine with no GPU adapter.
    #[test]
    fn the_rtg_shader_source_validates() {
        assert_eq!(
            super::super::crt_shader::validate_wgsl_source(SHADER),
            Ok(())
        );
    }

    /// Smooth scaling maps the frame onto the whole viewport, which is the
    /// identity: the mapping the pass had before integer scaling existed.
    #[test]
    fn smooth_scaling_fills_the_display_rect() {
        assert_eq!(FILL_VIEWPORT.rect, [0.0, 0.0, 1.0, 1.0]);
    }

    /// A rect an exact multiple of the board resolution is filled edge to
    /// edge, with no border to distinguish it from the smooth fit.
    #[test]
    fn an_exact_multiple_fills_the_rect() {
        assert_eq!(
            integer_fit_uniforms((1280.0, 960.0), (640, 480)),
            FILL_VIEWPORT
        );
        assert_eq!(
            integer_fit_uniforms((640.0, 480.0), (640, 480)),
            FILL_VIEWPORT
        );
    }

    /// The usual case: the rect holds a whole number of board pixels with
    /// some left over, and the picture is centred in what it does not use.
    #[test]
    fn a_partial_multiple_centres_the_picture() {
        // 1300x1000 holds 640x480 twice over (2.03 x 2.08), so the picture
        // is 1280x960 with 20 physical pixels spare across and 40 down.
        let u = integer_fit_uniforms((1300.0, 1000.0), (640, 480));
        assert_eq!(u.rect[2], 1280.0 / 1300.0);
        assert_eq!(u.rect[3], 960.0 / 1000.0);
        assert_eq!(u.rect[0], (1.0 - 1280.0 / 1300.0) / 2.0);
        assert_eq!(u.rect[1], (1.0 - 960.0 / 1000.0) / 2.0);
        // The margins are equal, so the picture is centred.
        assert_eq!(u.rect[0] * 2.0 + u.rect[2], 1.0);
        assert_eq!(u.rect[1] * 2.0 + u.rect[3], 1.0);

        // The limiting axis decides the scale for both: a wide rect around
        // a 800x600 mode still scales by the height it can afford.
        let u = integer_fit_uniforms((3000.0, 1300.0), (800, 600));
        assert_eq!(u.rect[2], 1600.0 / 3000.0);
        assert_eq!(u.rect[3], 1200.0 / 1300.0);
    }

    /// A rect too small for a 1:1 copy has no whole-number scale to offer.
    /// Cropping the board's screen would be worse than softening it, so the
    /// pass keeps filling the rect, as the native path falls back to Fill.
    #[test]
    fn a_rect_smaller_than_the_frame_keeps_the_smooth_fit() {
        assert_eq!(
            integer_fit_uniforms((639.0, 480.0), (640, 480)),
            FILL_VIEWPORT
        );
        assert_eq!(
            integer_fit_uniforms((1280.0, 479.0), (640, 480)),
            FILL_VIEWPORT
        );
        // Degenerate extents (no frame uploaded yet, empty rect) too.
        assert_eq!(integer_fit_uniforms((0.0, 0.0), (640, 480)), FILL_VIEWPORT);
        assert_eq!(integer_fit_uniforms((1280.0, 960.0), (0, 0)), FILL_VIEWPORT);
    }

    // --- offscreen render (needs a GPU adapter) -------------------------

    /// A 2x2 board frame, one saturated colour per quadrant, so a readback
    /// says which source texel each output pixel came from. Only 0 and 255
    /// channel values, which survive the sRGB round trip exactly.
    const RED: u32 = 0xFF00_00FF;
    const GREEN: u32 = 0xFF00_FF00;
    const BLUE: u32 = 0xFFFF_0000;
    const WHITE: u32 = 0xFFFF_FFFF;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    /// Cleared into the target before the pass, to catch fragments it never
    /// wrote: nothing of the window buffer underneath may show around an
    /// integer-scaled picture.
    const SENTINEL: [u8; 4] = [255, 0, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// A device, or `None` on a machine with no usable adapter (headless
    /// CI): the render test then passes without asserting anything, as the
    /// bezel pass's own offscreen tests do.
    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        use super::super::crt_shader::poll_once;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = match poll_once(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })) {
            Some(Ok(adapter)) => adapter,
            _ => return None,
        };
        match poll_once(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rtg_texture_test"),
            ..Default::default()
        })) {
            Some(Ok(pair)) => Some(pair),
            _ => None,
        }
    }

    /// Draw the 2x2 frame into a `w` x `h` target through the real pass,
    /// and read the result back per pixel.
    fn render_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (w, h): (u32, u32),
        integer_scaling: bool,
    ) -> Vec<[u8; 4]> {
        let mut rtg = RtgTexture::new(device, FORMAT);
        rtg.upload(device, queue, &[RED, GREEN, BLUE, WHITE], 2, 2);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rtg_texture_test_target"),
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
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rtg_texture_test_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
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
        rtg.render(
            queue,
            &mut encoder,
            &view,
            (0.0, 0.0, w as f32, h as f32),
            integer_scaling,
        );

        // Read the target back through a padded copy, as the bezel tests do.
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rtg_texture_test_readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
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

    /// The whole GPU path, offscreen: pipeline, uniform block, both
    /// samplers and the border, checked against a frame whose every texel
    /// is identifiable in the readback.
    ///
    /// A 2x2 frame in a 12x8 rect scales by 4 (the height allows no more),
    /// so the picture is 8x8 with two black columns each side. Integer
    /// scaling must land every source texel on an exact 4x4 block, and
    /// smooth scaling must still stretch the frame across the whole rect.
    #[test]
    fn integer_scaling_lands_the_board_frame_on_whole_pixel_blocks() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let (w, h) = (12u32, 8u32);
        let at = |px: &[[u8; 4]], x: u32, y: u32| px[(y * w + x) as usize];

        let px = render_frame(&device, &queue, (w, h), true);
        // The margins the picture does not reach are painted black by the
        // pass itself -- the window buffer underneath must not show.
        for y in 0..h {
            for x in [0, 1, 10, 11] {
                assert_eq!(at(&px, x, y), BLACK, "margin at ({x}, {y}) not black");
            }
        }
        // Each source texel fills its own 4x4 block, with hard edges
        // between them: point sampling, not interpolation.
        for (x, y, want, texel) in [
            (2, 0, RED, "top-left"),
            (5, 3, RED, "top-left"),
            (6, 0, GREEN, "top-right"),
            (9, 3, GREEN, "top-right"),
            (2, 4, BLUE, "bottom-left"),
            (5, 7, BLUE, "bottom-left"),
            (6, 4, WHITE, "bottom-right"),
            (9, 7, WHITE, "bottom-right"),
        ] {
            assert_eq!(
                at(&px, x, y),
                want.to_le_bytes(),
                "({x}, {y}) should be the {texel} texel"
            );
        }
        assert!(
            px.iter().all(|p| *p != SENTINEL),
            "the pass left a hole in its viewport"
        );

        // Smooth scaling is unchanged: the frame is stretched over the
        // whole rect, so the corners hold the corner texels and there is
        // no border at all.
        let px = render_frame(&device, &queue, (w, h), false);
        assert_eq!(at(&px, 0, 0), RED.to_le_bytes());
        assert_eq!(at(&px, 11, 0), GREEN.to_le_bytes());
        assert_eq!(at(&px, 0, 7), BLUE.to_le_bytes());
        assert_eq!(at(&px, 11, 7), WHITE.to_le_bytes());
    }
}

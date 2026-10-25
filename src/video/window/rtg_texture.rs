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
//! The board has its own native fit. Menus and panels retain a uniform fit
//! of the host UI canvas; corner sprites retain that scale at board corners.
//! Painted sprites are packed into a byte-encoded atlas and composited here
//! in the same encoded colour space as the CPU. Empty UI frames use only the
//! native board pass, with no atlas upload or overlay texture reads.

use super::rtg_ui::{Frame as UiFrame, Layer, EMPTY_LAYER, MAX_LAYERS};
use pixels::wgpu;
use std::borrow::Cow;
use zerocopy::{Immutable, IntoBytes};

const SHADER: &str = r#"
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct UiLayer { src: vec4<f32>, dst: vec4<f32>, backdrop: vec4<f32> };

struct RtgUniforms {
    // xy: where the picture starts inside the viewport, zw: how much of the
    // viewport it covers, both in the viewport's own 0..1 space. The
    // identity (0, 0, 1, 1) fills the viewport, which is the smooth-scaling
    // mapping.
    rect: vec4<f32>,
    ui_info: vec4<u32>,
    layers: array<UiLayer, 5>,
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

@group(0) @binding(3) var ui: texture_2d<f32>;

fn encoded(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055,
                  c * 12.92, c <= vec3<f32>(0.0031308));
}
fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)),
                  c / 12.92, c <= vec3<f32>(0.04045));
}
fn ui_texel(p: vec2<f32>, src: vec4<f32>) -> vec4<f32> {
    let at = clamp(vec2<i32>(p), vec2<i32>(src.xy), vec2<i32>(src.xy + src.zw) - 1);
    let c = textureLoad(ui, at, 0);
    return vec4<f32>(c.rgb * c.a, c.a);
}
fn ui_sample(position: vec2<f32>, layer: UiLayer) -> vec4<f32> {
    let uv = (position - layer.dst.xy) / layer.dst.zw;
    if (any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0))) {
        return vec4<f32>(layer.backdrop.rgb * layer.backdrop.a, layer.backdrop.a);
    }
    let t = layer.src.xy + uv * layer.src.zw;
    let tf = fract(t);
    let tpp = max(layer.src.zw / layer.dst.zw, vec2<f32>(1e-6));
    let sharp = clamp(tf / tpp, vec2<f32>(0.0), vec2<f32>(0.5))
        + clamp((tf - 1.0) / tpp + 0.5, vec2<f32>(0.0), vec2<f32>(0.5));
    let p = floor(t) + sharp - 0.5;
    let p0 = floor(p);
    let f = fract(p);
    return mix(mix(ui_texel(p0, layer.src), ui_texel(p0 + vec2<f32>(1.0, 0.0), layer.src), f.x),
               mix(ui_texel(p0 + vec2<f32>(0.0, 1.0), layer.src), ui_texel(p0 + 1.0, layer.src), f.x), f.y);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let t = (in.uv - u.rect.xy) / u.rect.zw;
    // Sampled unconditionally: textureSample needs uniform control flow, so
    // the border is chosen after the fact rather than returned early. Reads
    // outside the picture are harmless, the sampler clamping them to the
    // edge, and are discarded here.
    let texel = textureSample(tex, samp, t);
    let inside = t.x >= 0.0 && t.x <= 1.0 && t.y >= 0.0 && t.y <= 1.0;
    let board = select(vec4<f32>(0.0, 0.0, 0.0, 1.0), texel, inside);
    if (u.ui_info.x == 0u) { return board; }
    var colour = encoded(board.rgb);
    for (var i = 0u; i < u.ui_info.x; i += 1u) {
        let over = ui_sample(in.pos.xy, u.layers[i]);
        // CPU UI blends byte-encoded colours, rounding each composite.
        colour = round((colour * (1.0 - over.a) + over.rgb) * 255.0) / 255.0;
    }
    return vec4<f32>(linear(colour), 1.0);
}
"#;

/// The uniform block the RTG pass sees; mirrors `RtgUniforms` in the WGSL
/// source. Every member occupies whole 16-byte uniform slots.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, IntoBytes, Immutable)]
pub(super) struct RtgUniforms {
    rect: [f32; 4],
    ui_info: [u32; 4],
    layers: [Layer; MAX_LAYERS],
}

/// Size of the uniform block, in bytes. Also the layout's
/// `min_binding_size`.
const UNIFORM_BYTES: u64 = std::mem::size_of::<RtgUniforms>() as u64;

/// The mapping that stretches the frame across the whole viewport: what
/// smooth scaling always uses, and what integer scaling falls back to.
const FILL_VIEWPORT: RtgUniforms = RtgUniforms {
    rect: [0.0, 0.0, 1.0, 1.0],
    ui_info: [0; 4],
    layers: [EMPTY_LAYER; MAX_LAYERS],
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
        ..FILL_VIEWPORT
    }
}

/// The part of a `viewport`-sized rect the pass draws the board picture
/// over, as `[x, y, w, h]` fractions of it: the whole-number fit under
/// integer scaling, the whole viewport under smooth scaling.
pub(super) fn picture_rect(
    viewport: (f32, f32),
    native: (u32, u32),
    integer_scaling: bool,
) -> [f32; 4] {
    if integer_scaling {
        integer_fit_uniforms(viewport, native).rect
    } else {
        FILL_VIEWPORT.rect
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
    ui_texture: wgpu::Texture,
    ui_dims: (u32, u32),
}

fn ui_texture(device: &wgpu::Device, dims: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rtg_ui_atlas"),
        size: wgpu::Extent3d {
            width: dims.0,
            height: dims.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            ui_texture: ui_texture(device, (1, 1)),
            ui_dims: (1, 1),
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
            self.texture = Some(texture);
            self.rebind(device);
            self.dims = (w, h);
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, (w * h * 4) as usize) };
        // The block above just created the texture when it was missing, so
        // this is Some in practice; skip the frame rather than panic if a
        // future change ever breaks that.
        let Some(texture) = self.texture.as_ref() else {
            log::warn!("rtg texture missing at upload; skipping frame");
            return;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
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

    fn rebind(&mut self, device: &wgpu::Device) {
        let Some(texture) = &self.texture else {
            return;
        };
        let view = texture.create_view(&Default::default());
        let ui_view = self.ui_texture.create_view(&Default::default());
        let bind = |sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rtg_compositor_bind_group"),
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&ui_view),
                    },
                ],
            })
        };
        self.bind_group_linear = Some(bind(&self.sampler_linear));
        self.bind_group_nearest = Some(bind(&self.sampler_nearest));
    }

    pub(super) fn upload_ui(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, ui: &UiFrame) {
        if ui.layers.is_empty() {
            return;
        }
        let dims = (ui.width as u32, (ui.pixels.len() / ui.width) as u32);
        if dims != self.ui_dims {
            self.ui_texture = ui_texture(device, dims);
            self.ui_dims = dims;
            self.rebind(device);
        }
        queue.write_texture(
            self.ui_texture.as_image_copy(),
            ui.pixels.as_slice().as_bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(dims.0 * 4),
                rows_per_image: Some(dims.1),
            },
            wgpu::Extent3d {
                width: dims.0,
                height: dims.1,
                depth_or_array_layers: 1,
            },
        );
    }

    pub(super) fn render_composited(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        board: (f32, f32, f32, f32),
        integer: bool,
        ui: &UiFrame,
    ) {
        if ui.layers.is_empty() {
            self.render(queue, encoder, target, board, integer);
            return;
        }
        let mut uniforms = RtgUniforms {
            rect: [
                (board.0 - viewport.0) / viewport.2,
                (board.1 - viewport.1) / viewport.3,
                board.2 / viewport.2,
                board.3 / viewport.3,
            ],
            ..FILL_VIEWPORT
        };
        uniforms.ui_info[0] = ui.layers.len() as u32;
        uniforms.layers[..ui.layers.len()].copy_from_slice(&ui.layers);
        self.render_uniforms(queue, encoder, target, viewport, integer, uniforms);
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
        let uniforms = RtgUniforms {
            rect: picture_rect((viewport.2, viewport.3), self.dims, integer_scaling),
            ..FILL_VIEWPORT
        };
        self.render_uniforms(queue, encoder, target, viewport, integer_scaling, uniforms);
    }

    fn render_uniforms(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        integer_scaling: bool,
        uniforms: RtgUniforms,
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

    /// A serialized device, or `None` without a usable hardware adapter:
    /// see `crt_shader::test_gpu`.
    fn gpu() -> Option<super::super::crt_shader::TestGpu> {
        super::super::crt_shader::test_gpu("rtg_texture_test")
    }

    /// Draw the 2x2 frame into a `w` x `h` target through the real pass,
    /// and read the result back per pixel.
    fn render_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (w, h): (u32, u32),
        integer_scaling: bool,
    ) -> Vec<[u8; 4]> {
        render_board(
            device,
            queue,
            &[RED, GREEN, BLUE, WHITE],
            (2, 2),
            (w, h),
            integer_scaling,
        )
    }

    /// Draw a `native`-sized `board` frame into a `w` x `h` target through
    /// the real pass, and read the result back per pixel.
    fn render_board(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        board: &[u32],
        native: (u32, u32),
        (w, h): (u32, u32),
        integer_scaling: bool,
    ) -> Vec<[u8; 4]> {
        render_board_ui(device, queue, board, native, (w, h), integer_scaling, None)
    }

    type TestRect = (f32, f32, f32, f32);

    fn render_board_ui(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        board: &[u32],
        native: (u32, u32),
        (w, h): (u32, u32),
        integer_scaling: bool,
        ui: Option<(&UiFrame, TestRect, TestRect)>,
    ) -> Vec<[u8; 4]> {
        let mut rtg = RtgTexture::new(device, FORMAT);
        rtg.upload(device, queue, board, native.0, native.1);
        if let Some((ui, ..)) = ui {
            rtg.upload_ui(device, queue, ui);
        }
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
        if let Some((ui, viewport, board)) = ui {
            rtg.render_composited(
                queue,
                &mut encoder,
                &view,
                viewport,
                board,
                integer_scaling,
                ui,
            );
        } else {
            rtg.render(
                queue,
                &mut encoder,
                &view,
                (0.0, 0.0, w as f32, h as f32),
                integer_scaling,
            );
        }

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
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let (w, h) = (12u32, 8u32);
        let at = |px: &[[u8; 4]], x: u32, y: u32| px[(y * w + x) as usize];

        let px = render_frame(device, queue, (w, h), true);
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
        let px = render_frame(device, queue, (w, h), false);
        assert_eq!(at(&px, 0, 0), RED.to_le_bytes());
        assert_eq!(at(&px, 11, 0), GREEN.to_le_bytes());
        assert_eq!(at(&px, 0, 7), BLUE.to_le_bytes());
        assert_eq!(at(&px, 11, 7), WHITE.to_le_bytes());
    }

    /// Smooth scaling stretches the picture over the whole viewport;
    /// integer scaling places it where the whole-number fit does.
    #[test]
    fn the_picture_rect_follows_the_scaling_mode() {
        assert_eq!(
            picture_rect((1300.0, 1000.0), (640, 480), false),
            FILL_VIEWPORT.rect
        );
        assert_eq!(
            picture_rect((1300.0, 1000.0), (640, 480), true),
            integer_fit_uniforms((1300.0, 1000.0), (640, 480)).rect
        );
    }

    fn menu() -> crate::video::ui::UiState {
        use crate::video::{
            menu::{MenuAction, MenuRow, MenuRowKind},
            ui::UiState,
        };
        UiState {
            menu_open: true,
            menu_rows: vec![MenuRow {
                label: "Quit".into(),
                value: None,
                enabled: true,
                kind: MenuRowKind::Action(MenuAction::Quit),
            }],
            ..Default::default()
        }
    }

    /// The same real menu veil must have the same brightness over a board
    /// texture as over an opaque CPU chipset frame, including at high DPI.
    #[test]
    fn menu_veil_matches_cpu_encoded_colour_blending() {
        use super::super::{texture_height, texture_width, Rect};
        use crate::video::{present_height, ui, FB_WIDTH};
        let Some(gpu) = gpu() else {
            return;
        };
        let (w, h) = (FB_WIDTH as u32, present_height() as u32);
        for scale in [1, 2] {
            let stride = texture_width(scale);
            let rect = Rect {
                x: 0,
                y: 0,
                w: stride,
                h: present_height() * scale,
            };
            for value in [0u32, 64, 128, 255] {
                let colour = 0xFF00_0000 | value | value << 8 | value << 16;
                let mut reference = colour.to_le_bytes().repeat(stride * texture_height(scale));
                ui::draw(&mut reference, scale, &menu(), None, None);
                let mut layer = vec![0; reference.len()];
                let scrims = ui::draw(&mut layer, scale, &menu(), None, None);
                let mut frame = UiFrame::default();
                frame.capture(
                    &mut layer,
                    stride,
                    rect,
                    [0.0, 0.0, w as f32, h as f32],
                    ui::scrim_pixel(scrims),
                );
                let px = render_board_ui(
                    gpu.device(),
                    gpu.queue(),
                    &[colour],
                    (1, 1),
                    (w, h),
                    false,
                    Some((
                        &frame,
                        (0.0, 0.0, w as f32, h as f32),
                        (0.0, 0.0, w as f32, h as f32),
                    )),
                );
                for (actual, expected) in px[0].into_iter().zip(reference[..4].iter().copied()) {
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "board {value}, scale {scale}: {actual} != CPU {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn encoded_ui_filter_ignores_transparent_neighbour_colours() {
        let Some(gpu) = gpu() else {
            return;
        };
        let mut source: Vec<u8> = [WHITE, BLUE & 0x00FF_FFFF]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let mut ui = UiFrame::default();
        ui.capture(
            &mut source,
            2,
            super::super::Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 1,
            },
            [0.0, 0.0, 1.0, 1.0],
            0,
        );
        let px = render_board_ui(
            gpu.device(),
            gpu.queue(),
            &[RED],
            (1, 1),
            (1, 1),
            false,
            Some((&ui, (0.0, 0.0, 1.0, 1.0), (0.0, 0.0, 1.0, 1.0))),
        );
        for (actual, expected) in px[0].into_iter().zip([255u8, 128, 128, 255]) {
            assert!(actual.abs_diff(expected) <= 1, "{actual} != {expected}");
        }
    }

    #[test]
    fn compositor_preserves_the_order_and_brightness_of_all_five_layers() {
        use super::super::{statusbar::blend_pixel, texture_height, texture_width, Rect};
        let Some(gpu) = gpu() else {
            return;
        };
        let base = 0xFF20_4080u32;
        let mut reference = base
            .to_le_bytes()
            .repeat(texture_width(1) * texture_height(1));
        let mut ui = UiFrame::default();
        for colour in [
            0x8000_00FFu32,
            0x4000_FF00,
            0xADFF_0000,
            0x73FF_FFFF,
            0x730B_0908,
        ] {
            let alpha = colour.to_le_bytes()[3] as f32 / 255.0;
            blend_pixel(&mut reference, 0, 0, colour, alpha, 1);
            let mut source = colour.to_le_bytes().repeat(16);
            ui.capture(
                &mut source,
                4,
                Rect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                },
                [0.0, 0.0, 4.0, 4.0],
                0,
            );
        }
        assert_eq!(ui.layers.len(), MAX_LAYERS);
        let px = render_board_ui(
            gpu.device(),
            gpu.queue(),
            &[base],
            (1, 1),
            (4, 4),
            false,
            Some((&ui, (0.0, 0.0, 4.0, 4.0), (0.0, 0.0, 4.0, 4.0))),
        );
        for pixel in px {
            for (actual, expected) in pixel.into_iter().zip(reference[..4].iter().copied()) {
                assert!(actual.abs_diff(expected) <= 1, "{actual} != CPU {expected}");
            }
        }
    }

    #[test]
    fn menu_preserves_native_blocks_with_independent_ui_fit() {
        use super::super::{
            integer_rtg_present_layout, texture_height, texture_width, ui_canvas_layout, Rect,
        };
        use crate::video::{present_height, ui, FB_WIDTH};
        let Some(gpu) = gpu() else {
            return;
        };
        for native in [(640u32, 480u32), (1280, 720), (320, 200)] {
            let board: Vec<u32> = (0..native.0 * native.1)
                .map(|p| {
                    if (p % native.0 + p / native.0).is_multiple_of(2) {
                        RED
                    } else {
                        BLUE
                    }
                })
                .collect();
            for chrome in [None, Some((602, 1036, 716, 44))] {
                let layout = integer_rtg_present_layout((0, 0, 1920, 1080), native, chrome);
                let avail_h = 1080 - chrome.map_or(0, |c| c.3);
                let ui_layout = ui_canvas_layout(
                    (0, 0, 1920, avail_h),
                    true,
                    chrome,
                    (FB_WIDTH, present_height()),
                );
                let mut source = vec![0; texture_width(1) * texture_height(1) * 4];
                let count = ui::draw(&mut source, 1, &menu(), None, None);
                let mut ui = UiFrame::default();
                let (x, y, w, h) = ui_layout.rtg_viewport();
                ui.capture(
                    &mut source,
                    texture_width(1),
                    Rect {
                        x: 0,
                        y: 0,
                        w: FB_WIDTH,
                        h: present_height(),
                    },
                    [x, y, w, h],
                    ui::scrim_pixel(count),
                );
                let viewport = (0.0, 0.0, 1920.0, avail_h as f32);
                let render = |ui: &UiFrame| {
                    render_board_ui(
                        gpu.device(),
                        gpu.queue(),
                        &board,
                        native,
                        (1920, 1080),
                        true,
                        Some((ui, viewport, layout.rtg_viewport())),
                    )
                };
                let closed = render(&UiFrame::default());
                let open = render(&ui);
                let (dx, dy, dw, dh) = layout.display_dst;
                let m = dw / native.0;
                assert_eq!(dh / native.1, m);
                for by in 0..native.1 / 3 {
                    for bx in 0..native.0 {
                        let at = |px: &[[u8; 4]], x, y| px[(y * 1920 + x) as usize];
                        let (x, y) = (dx + m * bx, dy + m * by);
                        let unshaded = board[(by * native.0 + bx) as usize].to_le_bytes();
                        let shaded = at(&open, x, y);
                        assert_ne!(shaded, unshaded);
                        for oy in 0..m {
                            for ox in 0..m {
                                assert_eq!(at(&closed, x + ox, y + oy), unshaded);
                                assert_eq!(
                                    at(&open, x + ox, y + oy),
                                    shaded,
                                    "{native:?} ({bx},{by}) chrome {chrome:?}"
                                );
                            }
                        }
                    }
                }
                assert_eq!(render(&UiFrame::default()), closed);
            }
        }
    }

    /// The board pixel the pass shows at `centre` -- a pixel centre, as a
    /// fraction of the viewport along one axis -- when the picture starts at
    /// `start` and spans `len` of it, `native` board pixels across; `None` in
    /// the black margin. The CPU form of the shader's per-fragment test and
    /// point sample: both ends of the picture are inside, and the sample
    /// clamps to the last pixel as the sampler does.
    fn picture_texel(centre: f32, start: f32, len: f32, native: u32) -> Option<u32> {
        let t = (centre - start) / len;
        ((0.0..=1.0).contains(&t) && native > 0)
            .then(|| ((t * native as f32) as u32).min(native - 1))
    }

    /// Every native texel reaches the expected uniform host-pixel block,
    /// including black margins and the too-small fallback.
    #[test]
    fn the_native_pass_puts_every_board_pixel_in_its_uniform_blocks() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        // Eight colours, every channel 0 or 255; along a row neighbours
        // step by one, down a column by three, so no two adjacent pixels
        // share one.
        let palette: Vec<u32> = (0..8u32)
            .map(|i| {
                0xFF00_0000
                    | if i & 1 != 0 { 0xFF } else { 0 }
                    | if i & 2 != 0 { 0xFF00 } else { 0 }
                    | if i & 4 != 0 { 0xFF_0000 } else { 0 }
            })
            .collect();
        let native = (7u32, 5u32);
        let board: Vec<u32> = (0..native.1)
            .flat_map(|y| (0..native.0).map(move |x| (x + 3 * y) as usize % 8))
            .map(|i| palette[i])
            .collect();
        for (w, h) in [
            // 3x, one black pixel each side on both axes.
            (23u32, 17u32),
            // 2x, held down by the height: a wide margin across.
            (30, 12),
            // An exact 2x fit: no margin at all.
            (14, 10),
            // Too small for 1x: the smooth fill, point-sampled.
            (6, 4),
        ] {
            let px = render_board(device, queue, &board, native, (w, h), true);
            let picture = picture_rect((w as f32, h as f32), native, true);
            for y in 0..h {
                let row = picture_texel(
                    (y as f32 + 0.5) / h as f32,
                    picture[1],
                    picture[3],
                    native.1,
                );
                for x in 0..w {
                    let column = picture_texel(
                        (x as f32 + 0.5) / w as f32,
                        picture[0],
                        picture[2],
                        native.0,
                    );
                    let want = match (column, row) {
                        (Some(bx), Some(by)) => board[(by * native.0 + bx) as usize].to_le_bytes(),
                        _ => BLACK,
                    };
                    assert_eq!(
                        px[(y * w + x) as usize],
                        want,
                        "{w}x{h} at ({x}, {y}): board {column:?}, {row:?}"
                    );
                }
            }
        }
    }
}

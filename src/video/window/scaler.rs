// SPDX-License-Identifier: GPL-3.0-or-later

//! The main-window presentation scaler: draws the composited `pixels`
//! buffer (display, status bar, panels, overlays) onto the surface.
//!
//! This pass replaces the `pixels` crate's built-in scaling renderer for
//! the emulator window. The built-in renderer can only place the texture
//! at whole multiples of *itself* (PixelPerfect) or fill the surface
//! (Fill), which welds the displayed integer multiple to the supersample
//! factor of the backing texture: a multiple past the texture cap was
//! unreachable, so a large display stopped zooming at
//! `MAX_INTEGER_TEXTURE_SCALE` times the canvas however much room it had.
//! This pass takes the destination rect (`clip_rect_for`) and the filter
//! as inputs instead, so the planned multiple is drawn whatever the
//! texture's own factor is.
//!
//! Point-sampling a texture whose supersample factor S does not divide
//! the displayed multiple M is still exact for the display region: the
//! CPU present copy builds the texture by replicating each canvas pixel
//! into an S-wide block, so every texel a sample can land on inside a
//! canvas pixel holds the same colour, and a canvas-pixel *boundary* in
//! surface space sits at a multiple of M while sample centres sit at
//! half-integers -- `(p + 0.5) * S / M` is never an integer multiple of
//! S, so float jitter cannot flip a sample across a boundary. UI drawn
//! into the texture at S (status bar, open menus) has per-texel detail,
//! so at M > S its nearest resample picks uneven texel runs; that only
//! arises past the texture cap (M > 4), where the picture is large
//! enough that the unevenness is a few per cent of a glyph stroke.
//!
//! The smooth filter is the same texel-snapped sharp bilinear the
//! `pixels` Fill renderer uses: texel centres are sampled flat and the
//! transition between texels is spread over one surface pixel, which
//! keeps a magnified picture sharp without the shimmer of raw nearest
//! sampling at a fractional scale.
//!
//! The pass paints the whole surface opaque black first, then draws the
//! picture and chrome in their destination rectangles. A background draw
//! is needed because a render-pass clear alone leaves corrupt letterbox
//! pixels on Intel Mac Metal presentation surfaces.

use pixels::wgpu;
use std::borrow::Cow;
use zerocopy::{Immutable, IntoBytes};

const SHADER: &str = r#"
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct ScalerUniforms {
    // xy: uv origin of the sampled source rect, zw: its size. The
    // identity (0, 0, 1, 1) maps the whole texture across the viewport.
    rect: vec4<f32>,
    // x: 1.0 = nearest (texel-centre) sampling, 0.0 = sharp bilinear.
    // y: 1.0 = sample the picture through the map below, 0.0 = the
    // composed texture. zw: reserved.
    params: vec4<f32>,
    // The picture map (`PictureMap::uniforms`), integer texel units.
    pic_a: vec4<i32>,
    pic_b: vec4<i32>,
    pic_c: vec4<i32>,
    pic_d: vec4<i32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VOut {
    // Fullscreen triangle: the viewport restricts it to the destination
    // rect.
    let tc = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    var out: VOut;
    out.uv = tc;
    out.pos = vec4<f32>(tc * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: ScalerUniforms;
@group(0) @binding(3) var pic: texture_2d<f32>;

// The picture texel at (x, y) of the presentation buffer, as the encoded
// byte values 0..255 per channel (the texture is not sRGB-typed).
fn picture_load(x: i32, y: i32) -> vec3<i32> {
    let v = textureLoad(pic, vec2<i32>(x, y), 0);
    return vec3<i32>(round(v.rgb * 255.0));
}

// The composed texel at (tx, ty) of the texture the CPU copy would have
// written: the row selection and the glass column blend of
// present.rs copy_tv_aperture_to_window / copy_present_frame, in the
// same integer arithmetic. Encoded values 0..1; black off the raster.
fn picture_texel(tx: i32, ty: i32) -> vec3<f32> {
    let scale = u.pic_a.x;
    let out_rows = u.pic_a.y;
    let src_rows = u.pic_a.w;
    let src_width = u.pic_b.x;
    let mode = u.pic_b.y;
    let fbw = u.pic_d.w;
    var src_y: i32;
    if (mode == 0) {
        src_y = min(((2 * ty + 1) * src_rows) / (2 * out_rows), src_rows - 1);
    } else {
        let pad = u.pic_b.z;
        let content = u.pic_b.w;
        let aperture = u.pic_c.x;
        if (ty < pad || ty >= pad + content) {
            return vec3<f32>(0.0);
        }
        let crop = min(((2 * (ty - pad) + 1) * aperture) / (2 * content), aperture - 1);
        src_y = min(u.pic_c.y + crop, src_rows - 1) + u.pic_c.z;
        if (src_y < 0 || src_y >= src_rows) {
            return vec3<f32>(0.0);
        }
    }
    if (mode == 0) {
        let texw = fbw * scale;
        if (src_width == texw) {
            return vec3<f32>(picture_load(tx, src_y)) / 255.0;
        }
        if (src_width != fbw) {
            return vec3<f32>(picture_load((tx * src_width) / texw, src_y)) / 255.0;
        }
        return vec3<f32>(picture_load(tx / scale, src_y)) / 255.0;
    }
    let out_x = tx / scale;
    if (mode == 2) {
        let pad_x = u.pic_d.z;
        let width = u.pic_d.y;
        if (out_x < pad_x || out_x >= pad_x + width) {
            return vec3<f32>(0.0);
        }
        let sx = u.pic_d.x + u.pic_c.w + (out_x - pad_x);
        if (sx < 0 || sx >= fbw) {
            return vec3<f32>(0.0);
        }
        return vec3<f32>(picture_load(sx, src_y)) / 255.0;
    }
    let s = (u.pic_d.x + u.pic_c.w) * 256 + ((2 * out_x + 1) * u.pic_d.y * 256) / (2 * fbw) - 128;
    if (s < -128 || s > (fbw - 1) * 256 + 128) {
        return vec3<f32>(0.0);
    }
    let sc = clamp(s, 0, (fbw - 1) * 256);
    let i = sc >> 8u;
    let frac = sc & 255;
    let a = picture_load(i, src_y);
    let b = picture_load(min(i + 1, fbw - 1), src_y);
    let c = (a * (256 - frac) + b * frac) >> vec3<u32>(8u);
    return vec3<f32>(c) / 255.0;
}

// The decode the sampler applies to the sRGB composed texture, so the
// picture path filters in linear light exactly like the composed one.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Sample the virtual composed texture at texel-space `coord` the way the
// linear clamp-to-edge sampler would, from the picture.
// A composed texel in linear light: the picture rows through the map,
// the rows below them -- the CPU-drawn chrome band of the classic
// single-draw layout -- from the composed texture itself, whose sRGB
// typing has textureLoad decode them.
fn composed_texel_linear(x: i32, y: i32, picture_rows: i32) -> vec3<f32> {
    if (y < picture_rows) {
        return srgb_to_linear(picture_texel(x, y));
    }
    return textureLoad(tex, vec2<i32>(x, y), 0).rgb;
}

fn picture_sample(coord: vec2<f32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(tex));
    let w = u.pic_d.w * u.pic_a.x;
    let h = u.pic_a.y;
    let p = coord - 0.5;
    let p0 = floor(p);
    let f = p - p0;
    let x0 = clamp(i32(p0.x), 0, w - 1);
    let y0 = clamp(i32(p0.y), 0, dims.y - 1);
    let x1 = clamp(i32(p0.x) + 1, 0, w - 1);
    let y1 = clamp(i32(p0.y) + 1, 0, dims.y - 1);
    let c00 = composed_texel_linear(x0, y0, h);
    let c10 = composed_texel_linear(x1, y0, h);
    let c01 = composed_texel_linear(x0, y1, h);
    let c11 = composed_texel_linear(x1, y1, h);
    let c = mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y);
    return vec4<f32>(c, 1.0);
}

@fragment
fn fs_background() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex));
    // Sample position in texel coordinates.
    let t = (u.rect.xy + in.uv * u.rect.zw) * dims;
    let tf = fract(t);
    // Texels per surface pixel, from the rasterizer's own derivatives.
    let tpp = max(vec2<f32>(abs(dpdx(t.x)), abs(dpdy(t.y))), vec2<f32>(1e-6));
    // Sharp bilinear: hold the texel centre across the texel and spread
    // the transition to the next one over a single surface pixel.
    let sharp = clamp(tf / tpp, vec2<f32>(0.0), vec2<f32>(0.5))
        + clamp((tf - vec2<f32>(1.0)) / tpp + vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(0.5));
    // Nearest: always the texel centre. Both run through the one linear
    // sampler; sampling exactly at a centre returns that texel alone.
    let sub = select(sharp, vec2<f32>(0.5), u.params.x > 0.5);
    let coord = (floor(t) + sub) / dims;
    // Sampled unconditionally so the derivative-taking sample stays in
    // uniform control flow; the uniform picks the source per draw.
    let composed = textureSample(tex, samp, coord);
    if (u.params.y > 0.5) {
        return picture_sample(floor(t) + sub);
    }
    return composed;
}
"#;

/// How the pass resamples the texture onto the destination rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScaleFilter {
    /// Texel-centre point sampling: integer scaling's exact blocks.
    Nearest,
    /// Texel-snapped sharp bilinear: the smooth fit's filter.
    SharpBilinear,
}

/// The uniform block the pass sees; mirrors `ScalerUniforms` in the WGSL
/// source. `#[repr(C)]` with 16-byte members only, so the two layouts
/// agree with no padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, IntoBytes, Immutable)]
struct ScalerUniforms {
    rect: [f32; 4],
    params: [f32; 4],
    pic_a: [i32; 4],
    pic_b: [i32; 4],
    pic_c: [i32; 4],
    pic_d: [i32; 4],
}

/// The column map of a picture draw: how the shader reaches the
/// presentation buffer's columns for a texel of the composed texture it
/// stands in for (`copy_window_present_frame`'s three copies).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PictureColumns {
    /// `copy_present_frame`: full overscan, columns duplicated across the
    /// texture scale (or a wider canvas mapped by nearest sample).
    Full,
    /// The TV glass: the captured aperture's columns resampled onto the
    /// glass width with the 8.8 two-column blend.
    Glass,
    /// The square-pixel canvas: unit columns centred between black pads.
    Square,
}

/// One frame's map for a picture draw, built by `present::picture_map`
/// from the inputs the CPU copy takes. Rows are texture texels (already
/// times the texture scale); columns and offsets are canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PictureMap {
    pub(super) columns: PictureColumns,
    pub(super) scale: i32,
    pub(super) out_rows: i32,
    pub(super) src_rows: i32,
    pub(super) src_width: i32,
    pub(super) pad_rows: i32,
    pub(super) content_rows: i32,
    pub(super) aperture_rows: i32,
    pub(super) source_y: i32,
    pub(super) y_offset: i32,
    pub(super) x_offset: i32,
    pub(super) glass_source_x: i32,
    pub(super) glass_width: i32,
    pub(super) pad_x: i32,
    pub(super) fb_width: i32,
}

impl PictureMap {
    fn uniforms(&self) -> [[i32; 4]; 4] {
        let mode = match self.columns {
            PictureColumns::Full => 0,
            PictureColumns::Glass => 1,
            PictureColumns::Square => 2,
        };
        [
            [self.scale, self.out_rows, 0, self.src_rows],
            [self.src_width, mode, self.pad_rows, self.content_rows],
            [
                self.aperture_rows,
                self.source_y,
                self.y_offset,
                self.x_offset,
            ],
            [
                self.glass_source_x,
                self.glass_width,
                self.pad_x,
                self.fb_width,
            ],
        ]
    }
}

/// Size of the uniform block, in bytes. Also the layout's
/// `min_binding_size`.
const UNIFORM_BYTES: u64 = std::mem::size_of::<ScalerUniforms>() as u64;

/// One resample the pass performs: a source sub-rect of the texture onto
/// a destination rect of the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ScalerDraw {
    /// Source rect in the texture's 0..1 uv space; `[0, 0, 1, 1]` maps
    /// the whole texture.
    pub(super) src: [f32; 4],
    /// Destination rect on the surface, physical pixels.
    pub(super) dst: (u32, u32, u32, u32),
    pub(super) filter: ScaleFilter,
    /// Sample the uploaded picture through this map instead of the bound
    /// texture (`PresentScaler::upload_picture`).
    pub(super) picture: Option<PictureMap>,
}

impl ScalerDraw {
    /// The classic whole-texture draw: everything the buffer holds onto
    /// one letterboxed rect. (The live layout builds its draws itself;
    /// the offscreen tests use this shorthand.)
    #[cfg(test)]
    pub(super) fn full(dst: (u32, u32, u32, u32), filter: ScaleFilter) -> Self {
        Self {
            src: [0.0, 0.0, 1.0, 1.0],
            dst,
            filter,
            picture: None,
        }
    }
}

/// Most draws the pass accepts per frame: the display picture and the
/// chrome band below it (panels and status bar) under the autocrop
/// layout. Each needs its own uniform buffer, written once per frame.
const MAX_DRAWS: usize = 2;

/// Where the picture lands on the surface: the destination rect for a
/// `canvas`-sized logical picture on a `surface`, both in physical
/// pixels, centred either at the whole-canvas-pixel `multiple` or at the
/// aspect-preserving smooth fit when `multiple` is `None`.
///
/// The smooth arithmetic mirrors the `pixels` Fill renderer's
/// `ScalingMatrix` so the picture occupies the same pixels it always
/// did; the integer rect is exact by construction. A degenerate surface
/// or canvas collapses to an empty rect rather than dividing by zero.
pub(super) fn clip_rect_for(
    surface: (u32, u32),
    canvas: (u32, u32),
    multiple: Option<usize>,
) -> (u32, u32, u32, u32) {
    let (sw, sh) = surface;
    let (cw, ch) = canvas;
    if sw == 0 || sh == 0 || cw == 0 || ch == 0 {
        return (0, 0, 0, 0);
    }
    let (w, h) = match multiple {
        Some(m) => {
            let m = m as u32;
            (cw.saturating_mul(m).min(sw), ch.saturating_mul(m).min(sh))
        }
        None => {
            let scale = (sw as f32 / cw as f32).min(sh as f32 / ch as f32);
            (
                ((cw as f32 * scale) as u32).min(sw).max(1),
                ((ch as f32 * scale) as u32).min(sh).max(1),
            )
        }
    };
    ((sw - w) / 2, (sh - h) / 2, w, h)
}

/// Background and resampling pipelines, one linear sampler, and per-draw
/// uniform buffers with their bind groups against the current `pixels`
/// backing texture.
pub(super) struct PresentScaler {
    pipeline: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: [wgpu::Buffer; MAX_DRAWS],
    bind_groups: Option<[wgpu::BindGroup; MAX_DRAWS]>,
    /// The texture the bind groups view, compared by identity: `pixels`
    /// recreates its backing texture on a buffer resize, and the stale
    /// views have to be dropped with it.
    bound_texture: Option<wgpu::Texture>,
    /// The presentation buffer as uploaded for picture draws, at the
    /// canvas's own size; a 1x1 placeholder until the first upload.
    picture: wgpu::Texture,
    picture_dims: (u32, u32),
    bound_picture: Option<wgpu::Texture>,
}

fn picture_texture(device: &wgpu::Device, dims: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("present_scaler_picture"),
        size: wgpu::Extent3d {
            width: dims.0,
            height: dims.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Not sRGB-typed: the shader blends the encoded byte values as
        // the CPU copy does, then decodes them for the filter.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl PresentScaler {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present_scaler_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present_scaler_bgl"),
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("present_scaler_pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let create_pipeline = |label, entry_point, layout| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout,
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
                    entry_point: Some(entry_point),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline =
            create_pipeline("present_scaler_pipeline", "fs_main", Some(&pipeline_layout));
        let background_pipeline =
            create_pipeline("present_scaler_background", "fs_background", None);
        // One linear sampler serves both filters: the nearest path
        // samples exactly at texel centres, where linear filtering
        // returns that texel alone.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("present_scaler_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = std::array::from_fn(|i| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("present_scaler_uniforms_{i}")),
                size: UNIFORM_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        Self {
            pipeline,
            background_pipeline,
            bind_group_layout,
            sampler,
            uniforms,
            bind_groups: None,
            bound_texture: None,
            picture: picture_texture(device, (1, 1)),
            picture_dims: (1, 1),
            bound_picture: None,
        }
    }

    /// Upload the presentation buffer (`w` x `h` texels of `src`) for the
    /// picture draws of the next `render`.
    pub(super) fn upload_picture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &[u32],
        w: u32,
        h: u32,
    ) {
        if w == 0 || h == 0 || src.len() < (w * h) as usize {
            return;
        }
        if self.picture_dims != (w, h) {
            self.picture = picture_texture(device, (w, h));
            self.picture_dims = (w, h);
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, (w * h * 4) as usize) };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.picture,
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

    /// Draw `texture` onto `target`: paint the whole surface black,
    /// then run each of `draws` (at most [`MAX_DRAWS`]; extras are
    /// ignored) in order. An empty draw list, or one of empty rects,
    /// still clears, so a surface too small for any picture goes black
    /// rather than stale.
    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        draws: &[ScalerDraw],
    ) {
        if self
            .bound_texture
            .as_ref()
            .is_none_or(|bound| bound != texture)
            || self
                .bound_picture
                .as_ref()
                .is_none_or(|bound| *bound != self.picture)
        {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let picture_view = self
                .picture
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_groups = Some(std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("present_scaler_bg_{i}")),
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
                            resource: self.uniforms[i].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&picture_view),
                        },
                    ],
                })
            }));
            self.bound_texture = Some(texture.clone());
            self.bound_picture = Some(self.picture.clone());
        }
        for (i, draw) in draws.iter().take(MAX_DRAWS).enumerate() {
            let [pic_a, pic_b, pic_c, pic_d] = draw
                .picture
                .map(|map| map.uniforms())
                .unwrap_or([[0; 4]; 4]);
            let uniforms = ScalerUniforms {
                rect: draw.src,
                params: [
                    match draw.filter {
                        ScaleFilter::Nearest => 1.0,
                        ScaleFilter::SharpBilinear => 0.0,
                    },
                    if draw.picture.is_some() { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ],
                pic_a,
                pic_b,
                pic_c,
                pic_d,
            };
            queue.write_buffer(&self.uniforms[i], 0, uniforms.as_bytes());
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present_scaler_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // The default viewport covers the whole attachment. Write every
        // pixel, including alpha: on Intel Mac Metal drawables a load-op
        // clear can leave a red pattern and invalid alpha in untouched
        // areas, even though clearing an offscreen texture works.
        pass.set_pipeline(&self.background_pipeline);
        pass.draw(0..3, 0..1);
        let Some(bind_groups) = self.bind_groups.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        for (i, draw) in draws.iter().take(MAX_DRAWS).enumerate() {
            let (x, y, w, h) = draw.dst;
            if w == 0 || h == 0 {
                continue;
            }
            pass.set_bind_group(0, &bind_groups[i], &[]);
            pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pass's WGSL has to compile like the preset shaders do, and
    /// validating the source says so on a machine with no GPU adapter.
    #[test]
    fn the_scaler_shader_source_validates() {
        assert_eq!(
            super::super::crt_shader::validate_wgsl_source(SHADER),
            Ok(())
        );
    }

    /// An integer multiple places an exact `canvas * m` rect, centred with
    /// the odd spare pixel biased the way the smooth fit's cast biases it.
    #[test]
    fn integer_clip_rect_is_the_exact_multiple_centred() {
        // 716x581 canvas at 3x on a laptop panel: 2148x1743, centred.
        assert_eq!(
            clip_rect_for((3024, 1964), (716, 581), Some(3)),
            ((3024 - 2148) / 2, (1964 - 1743) / 2, 2148, 1743)
        );
        // An exact fit fills the axis.
        assert_eq!(
            clip_rect_for((1432, 1162), (716, 581), Some(2)),
            (0, 0, 1432, 1162)
        );
        // A multiple past the surface is clamped rather than cropped
        // (the planner never asks for one; the clamp is the safety net).
        assert_eq!(
            clip_rect_for((700, 500), (716, 581), Some(1)),
            (0, 0, 700, 500)
        );
    }

    /// The smooth fit preserves the canvas aspect and touches the
    /// limiting axis, like the `pixels` Fill matrix it replaces.
    #[test]
    fn smooth_clip_rect_is_the_aspect_fit() {
        // Width-limited: a square surface around the 716x581 canvas.
        let (x, y, w, h) = clip_rect_for((1000, 1000), (716, 581), None);
        assert_eq!(w, 1000);
        assert_eq!(x, 0);
        assert_eq!(h, (581.0 * (1000.0 / 716.0)) as u32);
        assert_eq!(y, (1000 - h) / 2);
        // Height-limited: a wide surface.
        let (x, y, w, h) = clip_rect_for((3000, 581), (716, 581), None);
        assert_eq!(h, 581);
        assert_eq!(y, 0);
        assert_eq!(w, 716);
        assert_eq!(x, (3000 - 716) / 2);
        // Degenerate inputs collapse to empty rather than dividing by
        // zero.
        assert_eq!(clip_rect_for((0, 100), (716, 581), None), (0, 0, 0, 0));
        assert_eq!(clip_rect_for((100, 100), (0, 581), None), (0, 0, 0, 0));
    }

    // --- offscreen render (needs a GPU adapter) -------------------------

    const RED: u32 = 0xFF00_00FF;
    const GREEN: u32 = 0xFF00_FF00;
    const BLUE: u32 = 0xFFFF_0000;
    const WHITE: u32 = 0xFFFF_FFFF;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    /// Cleared into the target before the pass to catch fragments it
    /// never wrote; the pass's own clear must replace every one.
    const SENTINEL_CLEAR: wgpu::Color = wgpu::Color {
        r: 1.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    const SENTINEL: [u8; 4] = [255, 0, 255, 255];

    fn gpu() -> Option<super::super::crt_shader::TestGpu> {
        super::super::crt_shader::test_gpu("present_scaler_test")
    }

    /// Upload `texels` as a `w` x `h` source texture, run the real pass
    /// into a `target` surface with the given clip and filter, and read
    /// the result back per pixel.
    fn render_pass(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texels: &[u32],
        tex_size: (u32, u32),
        target_size: (u32, u32),
        clip: (u32, u32, u32, u32),
        filter: ScaleFilter,
    ) -> Vec<[u8; 4]> {
        render_draws(
            device,
            queue,
            texels,
            tex_size,
            target_size,
            FORMAT,
            &[ScalerDraw::full(clip, filter)],
            None,
        )
    }

    fn render_draws(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texels: &[u32],
        tex_size: (u32, u32),
        target_size: (u32, u32),
        target_format: wgpu::TextureFormat,
        draws: &[ScalerDraw],
        picture: Option<(&[u32], (u32, u32))>,
    ) -> Vec<[u8; 4]> {
        let (tw, th) = tex_size;
        let (w, h) = target_size;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("present_scaler_test_source"),
            size: wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes =
            unsafe { std::slice::from_raw_parts(texels.as_ptr() as *const u8, texels.len() * 4) };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(tw * 4),
                rows_per_image: Some(th),
            },
            wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("present_scaler_test_target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present_scaler_test_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(SENTINEL_CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        let mut scaler = PresentScaler::new(device, target_format);
        if let Some((texels, (w, h))) = picture {
            scaler.upload_picture(device, queue, texels, w, h);
        }
        scaler.render(device, queue, &texture, &mut encoder, &view, draws);

        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present_scaler_test_readback"),
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

    #[test]
    fn retina_letterbox_borders_are_black() {
        let Some(gpu) = gpu() else {
            return;
        };
        for format in [FORMAT, wgpu::TextureFormat::Bgra8UnormSrgb] {
            for (w, h) in [(2880, 1800), (1800, 2880)] {
                let clip = clip_rect_for((w, h), (716, 581), None);
                let px = render_draws(
                    gpu.device(),
                    gpu.queue(),
                    &[WHITE; 4],
                    (2, 2),
                    (w, h),
                    format,
                    &[ScalerDraw::full(clip, ScaleFilter::SharpBilinear)],
                    None,
                );
                for y in 0..h {
                    for x in 0..w {
                        let inside = (clip.0..clip.0 + clip.2).contains(&x)
                            && (clip.1..clip.1 + clip.3).contains(&y);
                        let expected = if inside { [255; 4] } else { BLACK };
                        assert_eq!(
                            px[(y * w + x) as usize],
                            expected,
                            "{format:?} {w}x{h} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn empty_draws_still_paint_the_whole_target_opaque_black() {
        let Some(gpu) = gpu() else {
            return;
        };
        for draws in [
            vec![],
            vec![ScalerDraw::full((3, 2, 0, 5), ScaleFilter::Nearest)],
        ] {
            let px = render_draws(
                gpu.device(),
                gpu.queue(),
                &[WHITE],
                (1, 1),
                (13, 11),
                FORMAT,
                &draws,
                None,
            );
            assert!(px.iter().all(|p| *p == BLACK));
        }
    }

    #[test]
    fn display_and_chrome_draws_preserve_the_black_background_between_them() {
        let Some(gpu) = gpu() else {
            return;
        };
        let px = render_draws(
            gpu.device(),
            gpu.queue(),
            &[RED, GREEN],
            (1, 2),
            (12, 10),
            FORMAT,
            &[
                ScalerDraw {
                    src: [0.0, 0.0, 1.0, 0.5],
                    dst: (3, 1, 6, 4),
                    filter: ScaleFilter::Nearest,
                    picture: None,
                },
                ScalerDraw {
                    src: [0.0, 0.5, 1.0, 0.5],
                    dst: (1, 7, 10, 2),
                    filter: ScaleFilter::Nearest,
                    picture: None,
                },
            ],
            None,
        );
        for y in 0..10 {
            for x in 0..12 {
                let expected = if (3..9).contains(&x) && (1..5).contains(&y) {
                    RED.to_le_bytes()
                } else if (1..11).contains(&x) && (7..9).contains(&y) {
                    GREEN.to_le_bytes()
                } else {
                    BLACK
                };
                assert_eq!(px[y * 12 + x], expected, "({x}, {y})");
            }
        }
    }

    /// A 2x2-canvas frame replicated into a 4x4 texture (supersample
    /// factor 2), drawn at a 3x canvas multiple the factor does not
    /// divide: the module contract says the blocks still come out exact,
    /// and the surface around the rect is the pass's own black.
    #[test]
    fn nearest_blocks_are_exact_when_the_texture_factor_does_not_divide_the_multiple() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        // Canvas [[RED, GREEN], [BLUE, WHITE]] at supersample 2.
        #[rustfmt::skip]
        let texels = [
            RED, RED, GREEN, GREEN,
            RED, RED, GREEN, GREEN,
            BLUE, BLUE, WHITE, WHITE,
            BLUE, BLUE, WHITE, WHITE,
        ];
        let (w, h) = (10u32, 8u32);
        // 3x of the 2x2 canvas: a 6x6 picture centred at (2, 1).
        let clip = (2u32, 1u32, 6u32, 6u32);
        let px = render_pass(
            device,
            queue,
            &texels,
            (4, 4),
            (w, h),
            clip,
            ScaleFilter::Nearest,
        );
        let at = |x: u32, y: u32| px[(y * w + x) as usize];
        for y in 0..h {
            for x in 0..w {
                let inside = (2..8).contains(&x) && (1..7).contains(&y);
                if !inside {
                    assert_eq!(at(x, y), BLACK, "border at ({x}, {y}) not black");
                    continue;
                }
                let want = match ((x - 2) / 3, (y - 1) / 3) {
                    (0, 0) => RED,
                    (1, 0) => GREEN,
                    (0, 1) => BLUE,
                    _ => WHITE,
                };
                assert_eq!(
                    at(x, y),
                    want.to_le_bytes(),
                    "({x}, {y}) not its canvas pixel's exact colour"
                );
            }
        }
        assert!(
            px.iter().all(|p| *p != SENTINEL),
            "the pass left a hole in the surface"
        );
    }

    /// The sharp-bilinear fit holds each texel flat away from its edges:
    /// block centres are exact even though the scale is fractional, and
    /// the corners of the rect hold the corner texels.
    #[test]
    fn sharp_bilinear_holds_texel_centres_flat() {
        let Some(gpu) = gpu() else {
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let texels = [RED, GREEN, BLUE, WHITE];
        // 2x2 texture over a 9x9 rect: 4.5 surface pixels per texel, a
        // fractional scale raw nearest would shimmer at.
        let px = render_pass(
            device,
            queue,
            &texels,
            (2, 2),
            (9, 9),
            (0, 0, 9, 9),
            ScaleFilter::SharpBilinear,
        );
        let at = |x: u32, y: u32| px[(y * 9 + x) as usize];
        // Texel interiors, away from the blended seam at 4..5.
        for (x, y, want) in [
            (1, 1, RED),
            (7, 1, GREEN),
            (1, 7, BLUE),
            (7, 7, WHITE),
            (0, 0, RED),
            (8, 8, WHITE),
        ] {
            assert_eq!(at(x, y), want.to_le_bytes(), "({x}, {y}) not flat");
        }
    }

    /// The picture draw stands in for the CPU-composed texture: drawing
    /// the presentation buffer through its map gives the surface pixels
    /// that composing the texture on the CPU and drawing that gives, on
    /// the TV glass and the full-overscan copies, at an exact multiple
    /// point-sampled and at a fractional fit through the sharp filter.
    #[test]
    fn picture_draws_match_the_composed_texture() {
        use super::super::present::{
            copy_window_present_frame, picture_map, texture_height, texture_width,
        };
        use super::super::window_present_height;
        use crate::config::{Overscan, TvCentre};
        use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
        use crate::video::present_common::TV_PAL_PRESENT_HEIGHT;
        use crate::video::{present_height, FB_WIDTH};
        let Some(gpu) = gpu() else {
            return;
        };
        let mut seed = 0x9E37_79B9u32;
        let mut src = vec![0u32; OUT_PIXELS];
        for px in src.iter_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *px = seed | 0xFF00_0000;
        }
        let scale = 2;
        let tex = (texture_width(scale) as u32, texture_height(scale) as u32);
        let src_rect = [
            0.0,
            0.0,
            1.0,
            present_height() as f32 / window_present_height() as f32,
        ];
        for (overscan, aperture, centre) in [
            (
                Overscan::Tv,
                Some(TV_PAL_PRESENT_HEIGHT),
                TvCentre::default(),
            ),
            (
                Overscan::Tv,
                Some(TV_PAL_PRESENT_HEIGHT),
                TvCentre { h: 5, v: -3 },
            ),
            (Overscan::Full, None, TvCentre::default()),
        ] {
            let mut frame = vec![0u8; tex.0 as usize * tex.1 as usize * 4];
            copy_window_present_frame(
                &src, OUT_HEIGHT, FB_WIDTH, &mut frame, scale, overscan, centre, aperture, false,
            );
            // The chrome band below the picture, as the CPU draws it into
            // the composed texture whatever path the picture takes.
            let stride = tex.0 as usize * 4;
            for (y, row) in frame
                .chunks_exact_mut(stride)
                .enumerate()
                .skip(present_height() * scale)
            {
                for (x, px) in row.chunks_exact_mut(4).enumerate() {
                    px.copy_from_slice(&[(x * 7 + y * 3) as u8, y as u8, 0x40, 0xFF]);
                }
            }
            let composed: Vec<u32> = frame
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let map = picture_map(
                OUT_HEIGHT, FB_WIDTH, scale, overscan, centre, aperture, false,
            );
            // The autocrop layout's display draw, then the classic layout's
            // single draw over picture and chrome band alike.
            let full_rect = [0.0, 0.0, 1.0, 1.0];
            for (target, filter, src_rect) in [
                ((1432u32, 1074u32), ScaleFilter::Nearest, src_rect),
                ((1000, 750), ScaleFilter::SharpBilinear, src_rect),
                ((tex.0, tex.1), ScaleFilter::Nearest, full_rect),
                ((1000, 780), ScaleFilter::SharpBilinear, full_rect),
            ] {
                let clip = (0, 0, target.0, target.1);
                let reference = render_draws(
                    gpu.device(),
                    gpu.queue(),
                    &composed,
                    tex,
                    target,
                    FORMAT,
                    &[ScalerDraw {
                        src: src_rect,
                        dst: clip,
                        filter,
                        picture: None,
                    }],
                    None,
                );
                let through_map = render_draws(
                    gpu.device(),
                    gpu.queue(),
                    &composed,
                    tex,
                    target,
                    FORMAT,
                    &[ScalerDraw {
                        src: src_rect,
                        dst: clip,
                        filter,
                        picture: Some(map),
                    }],
                    Some((&src, (FB_WIDTH as u32, OUT_HEIGHT as u32))),
                );
                // Point sampling reproduces the composed texel bit for bit.
                // The sharp filter blends in linear light on both paths, but
                // the sampler's weights and sRGB decode carry less precision
                // than the shader's float maths, so the two differ by a
                // little in linear light -- which the steep foot of the sRGB
                // curve turns into several encoded LSBs near black. Measure
                // the difference in linear light, then: a mapping error is
                // tens of LSBs over most of the picture, not this.
                let linear = |v: u8| {
                    let c = f32::from(v) / 255.0;
                    if c <= 0.04045 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    }
                };
                let allowed_linear = match filter {
                    ScaleFilter::Nearest => 0.0,
                    ScaleFilter::SharpBilinear => 2.5 / 255.0,
                };
                let mut worst = 0.0f32;
                let mut worst_at = (0usize, [0u8; 4], [0u8; 4]);
                let mut off = 0usize;
                for (i, (a, b)) in reference.iter().zip(&through_map).enumerate() {
                    let d = (0..3)
                        .map(|c| (linear(a[c]) - linear(b[c])).abs())
                        .fold(0.0f32, f32::max);
                    if d > worst {
                        worst = d;
                        worst_at = (i, *a, *b);
                    }
                    off += usize::from(a[..3] != b[..3]);
                }
                eprintln!(
                    "picture draw vs composed: {overscan:?} centre {centre:?} {target:?} {filter:?}: worst {:.2}/255 linear at ({}, {}) {:?} vs {:?}, {off} of {} pixels differ",
                    worst * 255.0,
                    worst_at.0 % target.0 as usize,
                    worst_at.0 / target.0 as usize,
                    worst_at.1,
                    worst_at.2,
                    reference.len()
                );
                assert!(
                    worst <= allowed_linear,
                    "{overscan:?} {target:?} {filter:?}: worst linear difference {:.2}/255",
                    worst * 255.0
                );
                assert!(
                    off * 4 < reference.len(),
                    "{overscan:?} {target:?} {filter:?}: {off} pixels differ, a bias not noise"
                );
            }
        }
    }
}

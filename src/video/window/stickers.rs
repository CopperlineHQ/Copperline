// SPDX-License-Identifier: GPL-3.0-or-later

//! Sticker decals over the drawn monitor bezel: user PNGs from
//! `[display] bezel_stickers`, composited onto the plastic after the
//! bezel pass so a community logo can be "stuck on" the cabinet the way
//! owners dress a real monitor.
//!
//! The folder is the whole interface. Every `*.png` in it becomes one
//! decal; an optional `stickers.toml` beside them places each image, and
//! without one they line up along the cabinet's top band, each with a
//! slight alternating tilt. Placement coordinates are fractions of the
//! drawn monitor front (the bezel viewport), so a sheet lays out the same
//! at any window size and on the web player, whose page hook takes the
//! same keys as JSON (docs/guide/browser.md).
//!
//! Like the bezel it decorates, this is purely a presentation stage:
//! stickers exist only in the window pass, never in screenshots, frame
//! dumps, recordings or headless runs, and they are skipped whenever the
//! bezel itself is (no bezel style, an open overlay, RTG scanout).
//!
//! The images are decoded and packed into one atlas on the CPU at load
//! time; the pass draws one rotated quad per sticker from a single
//! uniform array, sampling the atlas with a drop shadow and a slight
//! vertical tone so a die-cut decal reads as stuck to lit plastic rather
//! than pasted over the frame. `shaders/bezel_stickers.wgsl` holds the
//! shader; try.js carries a GLSL port of it -- keep them in step.

use std::path::Path;

use pixels::wgpu;
use serde::Deserialize;
use zerocopy::{Immutable, IntoBytes};

/// The most stickers a sheet may carry: the uniform array's length, and
/// past it a front stops reading as a monitor anyway.
pub(super) const MAX_STICKERS: usize = 16;

/// Longest side an image keeps, in texels. The bands a sticker sits on
/// are a small fraction of the window, so more source than this is never
/// visible, and the cap keeps a folder of camera-sized PNGs from costing
/// a large atlas upload.
const MAX_DIM: u32 = 512;

/// Transparent padding between atlas cells, so linear sampling at a
/// sticker's edge fades into clear space instead of the neighbour.
const PAD: u32 = 2;

/// The atlas shelf width cells wrap at.
const ATLAS_MAX_W: u32 = 2048;

/// Width an explicitly placed sticker takes when the manifest names none,
/// as a fraction of the monitor front's width.
const DEFAULT_WIDTH_FRAC: f32 = 0.08;

/// Tilt cycle for auto-placed stickers, degrees clockwise: a hand puts no
/// two stickers on straight, so the row alternates a little either way.
const AUTO_TILT: [f32; 4] = [-3.0, 2.2, -1.6, 2.8];

/// A decoded RGBA image.
struct Rgba {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

/// One sticker of a loaded sheet: where its texels sit in the atlas, its
/// source proportions, and how the manifest asked for it to be placed
/// (all `None` for a bare folder: an auto slot in the top band).
#[derive(Debug)]
struct SheetEntry {
    /// Atlas sub-rect in UV space: xy origin, zw size.
    uv: [f32; 4],
    /// Source size after the [`MAX_DIM`] cap, for the aspect.
    w: u32,
    h: u32,
    /// Explicit centre, fractions of the monitor front; `None` = auto.
    at: Option<[f32; 2]>,
    /// Explicit width as a fraction of the front's width.
    width_frac: Option<f32>,
    /// Explicit tilt, degrees clockwise; auto slots without one take the
    /// [`AUTO_TILT`] cycle.
    rotate: Option<f32>,
    opacity: f32,
}

/// A loaded sticker folder: the packed atlas and each image's entry, in
/// manifest order (or file-name order for a bare folder).
#[derive(Debug)]
pub(super) struct StickerSheet {
    atlas_w: u32,
    atlas_h: u32,
    atlas_rgba: Vec<u8>,
    entries: Vec<SheetEntry>,
}

/// `stickers.toml` as written: a `[[sticker]]` table per image. The same
/// keys, as JSON, are the web player's `#bezel-stickers` page hook.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    sticker: Vec<RawManifestSticker>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifestSticker {
    /// File name in the folder.
    image: String,
    /// Sticker centre, fractions of the drawn monitor front (x right,
    /// y down). Both or neither: half a position places nothing.
    x: Option<f32>,
    y: Option<f32>,
    /// Width as a fraction of the front's width; height follows the
    /// image's aspect.
    width: Option<f32>,
    /// Degrees clockwise.
    rotate: Option<f32>,
    /// 0.0 to 1.0, default fully opaque.
    opacity: Option<f32>,
}

/// Load a sticker folder into a sheet. Any problem fails the whole load
/// with a message naming the file, the way a bad custom shader does: the
/// caller falls back to no stickers, never to a stale or partial sheet.
pub(super) fn load_sheet(dir: &Path) -> Result<StickerSheet, String> {
    let manifest_path = dir.join("stickers.toml");
    let picks: Vec<RawManifestSticker> = if manifest_path.is_file() {
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest: RawManifest =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        manifest.sticker
    } else {
        scan_folder(dir)?
            .into_iter()
            .map(|name| RawManifestSticker {
                image: name,
                x: None,
                y: None,
                width: None,
                rotate: None,
                opacity: None,
            })
            .collect()
    };
    if picks.is_empty() {
        return Err(format!("{}: no PNG stickers found", dir.display()));
    }
    if picks.len() > MAX_STICKERS {
        return Err(format!(
            "{}: {} stickers, the front carries at most {MAX_STICKERS}",
            dir.display(),
            picks.len()
        ));
    }

    let mut images = Vec::with_capacity(picks.len());
    for pick in &picks {
        if pick.x.is_some() != pick.y.is_some() {
            return Err(format!(
                "{}: x and y place a sticker together (only one given)",
                pick.image
            ));
        }
        let path = dir.join(&pick.image);
        let img = decode_png(&path)?;
        images.push(cap_size(img));
    }

    let (atlas_w, atlas_h, cells) = pack(&images);
    let mut atlas_rgba = vec![0u8; (atlas_w * atlas_h * 4) as usize];
    let mut entries = Vec::with_capacity(images.len());
    for (i, img) in images.iter().enumerate() {
        let (cx, cy) = cells[i];
        for row in 0..img.h {
            let src = (row * img.w * 4) as usize;
            let dst = (((cy + row) * atlas_w + cx) * 4) as usize;
            atlas_rgba[dst..dst + (img.w * 4) as usize]
                .copy_from_slice(&img.px[src..src + (img.w * 4) as usize]);
        }
        let pick = &picks[i];
        entries.push(SheetEntry {
            uv: [
                cx as f32 / atlas_w as f32,
                cy as f32 / atlas_h as f32,
                img.w as f32 / atlas_w as f32,
                img.h as f32 / atlas_h as f32,
            ],
            w: img.w,
            h: img.h,
            at: match (pick.x, pick.y) {
                (Some(x), Some(y)) => Some([x, y]),
                _ => None,
            },
            width_frac: pick.width,
            rotate: pick.rotate,
            opacity: pick.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        });
    }
    Ok(StickerSheet {
        atlas_w,
        atlas_h,
        atlas_rgba,
        entries,
    })
}

/// The folder's PNGs by file name, sorted so a bare folder lays out the
/// same on every run and every machine. Dot-files are skipped: macOS
/// leaves AppleDouble `._*.png` companions that are not PNGs at all.
fn scan_folder(dir: &Path) -> Result<Vec<String>, String> {
    let listing = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let is_png = Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if is_png && entry.path().is_file() {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Decode one PNG to straight RGBA8. `EXPAND | STRIP_16` normalises
/// palette, sub-byte greyscale and 16-bit sources, so anything a paint
/// program saves is taken.
fn decode_png(path: &Path) -> Result<Rgba, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    buf.truncate(info.buffer_size());
    let px = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        other => {
            return Err(format!(
                "{}: unsupported colour type {other:?}",
                path.display()
            ))
        }
    };
    if info.width == 0 || info.height == 0 {
        return Err(format!("{}: empty image", path.display()));
    }
    Ok(Rgba {
        w: info.width,
        h: info.height,
        px,
    })
}

/// Cap an image to [`MAX_DIM`] on its longest side, box-filtered so a
/// large source arrives smooth rather than decimated.
fn cap_size(img: Rgba) -> Rgba {
    let longest = img.w.max(img.h);
    if longest <= MAX_DIM {
        return img;
    }
    let nw = (img.w * MAX_DIM / longest).max(1);
    let nh = (img.h * MAX_DIM / longest).max(1);
    let mut px = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        let y0 = y * img.h / nh;
        let y1 = ((y + 1) * img.h).div_ceil(nh).clamp(y0 + 1, img.h);
        for x in 0..nw {
            let x0 = x * img.w / nw;
            let x1 = ((x + 1) * img.w).div_ceil(nw).clamp(x0 + 1, img.w);
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let off = ((sy * img.w + sx) * 4) as usize;
                    for (acc, &c) in sum.iter_mut().zip(&img.px[off..off + 4]) {
                        *acc += u32::from(c);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            px.extend(sum.iter().map(|&s| (s / n) as u8));
        }
    }
    Rgba { w: nw, h: nh, px }
}

/// Shelf-pack the images left to right, wrapping at [`ATLAS_MAX_W`], with
/// [`PAD`] clear texels around every cell. Returns the atlas size and each
/// image's top-left cell.
fn pack(images: &[Rgba]) -> (u32, u32, Vec<(u32, u32)>) {
    let mut cells = Vec::with_capacity(images.len());
    let (mut x, mut y, mut shelf, mut atlas_w) = (PAD, PAD, 0, 1);
    for img in images {
        if x + img.w + PAD > ATLAS_MAX_W && x > PAD {
            x = PAD;
            y += shelf + PAD;
            shelf = 0;
        }
        cells.push((x, y));
        x += img.w + PAD;
        shelf = shelf.max(img.h);
        atlas_w = atlas_w.max(x);
    }
    (atlas_w, y + shelf + PAD, cells)
}

/// One sticker resolved against a concrete viewport, in physical pixels
/// relative to the viewport's origin: what the uniform array carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Instance {
    pub(super) centre: [f32; 2],
    pub(super) half: [f32; 2],
    /// cos/sin of the clockwise tilt.
    pub(super) rot: [f32; 2],
    pub(super) opacity: f32,
    pub(super) uv: [f32; 4],
}

/// Resolve a sheet's placements against one frame's monitor front:
/// `(vw, vh)` is the bezel viewport in physical pixels and `band_bottom`
/// the top of the picture opening within it, which bounds the cabinet's
/// top band. Explicit placements are fraction-of-front; auto slots line
/// up along the top band in order, tilted by the [`AUTO_TILT`] cycle.
/// An auto slot that would run past the band's end is left off rather
/// than drawn over the corner -- only that one: a narrower slot after it
/// may still fit, and explicit placements never depend on the row. Pure
/// arithmetic, unit testable on its own.
pub(super) fn instances(sheet: &StickerSheet, vw: f32, vh: f32, band_bottom: f32) -> Vec<Instance> {
    let mut out = Vec::new();
    let band = band_bottom.max(0.0);
    let auto_h = (band * 0.52).max(8.0);
    let margin = vw * 0.055;
    let gap = auto_h * 0.45;
    let mut cursor = margin;
    let mut tilt = 0usize;
    for e in sheet.entries.iter().take(MAX_STICKERS) {
        let aspect = e.h as f32 / e.w.max(1) as f32;
        let (centre, w_px, rotate) = match e.at {
            Some([x, y]) => {
                let w_px = e.width_frac.unwrap_or(DEFAULT_WIDTH_FRAC) * vw;
                ([x * vw, y * vh], w_px, e.rotate.unwrap_or(0.0))
            }
            None => {
                let w_px = e
                    .width_frac
                    .map(|f| f * vw)
                    .unwrap_or_else(|| auto_h / aspect.max(1e-3));
                let rotate = e.rotate.unwrap_or(AUTO_TILT[tilt % AUTO_TILT.len()]);
                tilt += 1;
                if cursor + w_px > vw - margin {
                    continue;
                }
                let centre = [cursor + w_px * 0.5, band * 0.5];
                cursor += w_px + gap;
                (centre, w_px, rotate)
            }
        };
        if w_px < 1.0 {
            continue;
        }
        let rad = rotate.to_radians();
        out.push(Instance {
            centre,
            half: [w_px * 0.5, w_px * aspect * 0.5],
            rot: [rad.cos(), rad.sin()],
            opacity: e.opacity,
            uv: e.uv,
        });
    }
    out
}

const STICKER_WGSL: &str = include_str!("shaders/bezel_stickers.wgsl");

/// The uniform array's element: mirrors the WGSL `Sticker` struct.
#[repr(C)]
#[derive(Clone, Copy, Debug, IntoBytes, Immutable)]
struct GpuSticker {
    /// xy: centre in viewport px. zw: half-size in px.
    geo: [f32; 4],
    /// xy: cos/sin of the tilt. z: opacity. w: unused.
    rot: [f32; 4],
    /// Atlas sub-rect in UV space: xy origin, zw size.
    uv: [f32; 4],
}

const ZERO_STICKER: GpuSticker = GpuSticker {
    geo: [0.0; 4],
    rot: [0.0; 4],
    uv: [0.0; 4],
};

/// The uniform block `shaders/bezel_stickers.wgsl` sees. Mirrors the WGSL
/// struct exactly; `#[repr(C)]` with 16-byte-aligned members only.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct GpuUniforms {
    /// xy: viewport size in px. z: shadow offset in px. w: unused.
    info: [f32; 4],
    st: [GpuSticker; MAX_STICKERS],
}

const UNIFORM_BYTES: u64 = std::mem::size_of::<GpuUniforms>() as u64;

/// The decal pass: the loaded sheet and the GPU objects that draw it.
/// Follows [`super::bezel::BezelShader`]'s shape, but with its own bind
/// group layout: the shared one pins a 64-byte uniform block and has no
/// vertex-stage binding, and this pass needs both.
pub(super) struct StickerPass {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    target_format: wgpu::TextureFormat,
    pipeline: Option<wgpu::RenderPipeline>,
    sheet: Option<StickerSheet>,
    /// The sheet's atlas, uploaded on the first draw after [`Self::set_sheet`]
    /// (with the bind group viewing it); dropped with the sheet.
    atlas: Option<wgpu::Texture>,
    bind_group: Option<wgpu::BindGroup>,
}

impl StickerPass {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bezel_stickers_bgl"),
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
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bezel_stickers_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bezel_stickers_uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            bind_group_layout,
            sampler,
            uniforms,
            target_format,
            pipeline: None,
            sheet: None,
            atlas: None,
            bind_group: None,
        }
    }

    /// Install a loaded sheet (or none). The atlas and bind group are
    /// rebuilt lazily on the next draw, when the device is in hand.
    pub(super) fn set_sheet(&mut self, sheet: Option<StickerSheet>) {
        self.sheet = sheet;
        self.atlas = None;
        self.bind_group = None;
    }

    fn pipeline(&mut self, device: &wgpu::Device) -> &wgpu::RenderPipeline {
        if self.pipeline.is_none() {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("bezel_stickers"),
                source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(STICKER_WGSL)),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bezel_stickers_pl"),
                bind_group_layouts: &[Some(&self.bind_group_layout)],
                immediate_size: 0,
            });
            self.pipeline = Some(
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("bezel_stickers"),
                    layout: Some(&layout),
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
                            format: self.target_format,
                            // The shader hands over premultiplied colour:
                            // the decal and its shadow are composed in one
                            // fragment, which straight alpha cannot carry.
                            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    multiview_mask: None,
                    cache: None,
                }),
            );
        }
        self.pipeline.as_ref().expect("just built")
    }

    /// Draw the sheet over the `(x, y, w, h)` viewport rect of `target`
    /// (physical surface pixels), the same rect the bezel pass just drew,
    /// with `opening` bounding the picture so the auto row knows the top
    /// band. A pass with no sheet draws nothing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        opening: (f32, f32, f32, f32),
    ) {
        let (x, y, w, h) = viewport;
        if w < 1.0 || h < 1.0 || self.sheet.is_none() {
            return;
        }
        // Resolve the pipeline first: it borrows self mutably, which the
        // sheet borrow below does not allow.
        let _ = self.pipeline(device);
        let Some(sheet) = &self.sheet else { return };
        let resolved = instances(sheet, w, h, opening.1 - y);
        if resolved.is_empty() {
            return;
        }
        if self.atlas.is_none() {
            // sRGB, like the surface: the PNG's bytes are sRGB and the
            // shader tones them in linear light.
            let atlas = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("bezel_stickers_atlas"),
                size: wgpu::Extent3d {
                    width: sheet.atlas_w,
                    height: sheet.atlas_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &sheet.atlas_rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(sheet.atlas_w * 4),
                    rows_per_image: Some(sheet.atlas_h),
                },
                wgpu::Extent3d {
                    width: sheet.atlas_w,
                    height: sheet.atlas_h,
                    depth_or_array_layers: 1,
                },
            );
            let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bezel_stickers_bg"),
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
            self.atlas = Some(atlas);
        }
        let (Some(bind_group), Some(pipeline)) = (&self.bind_group, &self.pipeline) else {
            return;
        };
        let mut st = [ZERO_STICKER; MAX_STICKERS];
        for (slot, inst) in st.iter_mut().zip(&resolved) {
            *slot = GpuSticker {
                geo: [inst.centre[0], inst.centre[1], inst.half[0], inst.half[1]],
                rot: [inst.rot[0], inst.rot[1], inst.opacity, 0.0],
                uv: inst.uv,
            };
        }
        let uniforms = GpuUniforms {
            info: [w, h, (h * 0.004).clamp(1.0, 6.0), 0.0],
            st,
        };
        queue.write_buffer(&self.uniforms, 0, uniforms.as_bytes());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bezel_stickers_pass"),
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
        pass.draw(0..6, 0..resolved.len() as u32);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::crt_shader;
    use super::*;

    /// A fresh directory under the target-local tmp dir, unique per test
    /// so parallel tests never collide on a path.
    fn scratch_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "copperline-stickers-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Write a `w x h` RGBA PNG of one solid colour.
    fn write_png(path: &Path, w: u32, h: u32, colour: [u8; 4]) {
        let file = std::fs::File::create(path).expect("create png");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        let px: Vec<u8> = std::iter::repeat_n(colour, (w * h) as usize)
            .flatten()
            .collect();
        writer.write_image_data(&px).expect("png data");
    }

    #[test]
    fn the_sticker_shader_validates() {
        crt_shader::validate_wgsl_source(STICKER_WGSL).expect("bezel_stickers.wgsl validates");
    }

    #[test]
    fn the_uniform_block_matches_the_shader() {
        // 16-byte info header plus the vec4x3 array the WGSL declares.
        assert_eq!(UNIFORM_BYTES, 16 + (MAX_STICKERS as u64) * 48);
    }

    #[test]
    fn a_bare_folder_loads_every_png_in_name_order() {
        let dir = scratch_dir("bare");
        write_png(&dir.join("b.png"), 8, 4, [0, 255, 0, 255]);
        write_png(&dir.join("a.PNG"), 4, 4, [255, 0, 0, 255]);
        std::fs::write(dir.join("notes.txt"), "not a sticker").unwrap();
        std::fs::write(dir.join(".hidden.png"), "apple double junk").unwrap();
        let sheet = load_sheet(&dir).expect("load");
        assert_eq!(sheet.entries.len(), 2);
        // Name order: a.PNG first, then b.png; both auto slots.
        assert_eq!((sheet.entries[0].w, sheet.entries[0].h), (4, 4));
        assert_eq!((sheet.entries[1].w, sheet.entries[1].h), (8, 4));
        assert!(sheet.entries.iter().all(|e| e.at.is_none()));
        // The atlas holds the first image's colour at its cell.
        let (cx, cy) = (
            (sheet.entries[0].uv[0] * sheet.atlas_w as f32) as u32,
            (sheet.entries[0].uv[1] * sheet.atlas_h as f32) as u32,
        );
        let off = ((cy * sheet.atlas_w + cx) * 4) as usize;
        assert_eq!(&sheet.atlas_rgba[off..off + 4], &[255, 0, 0, 255]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_places_names_and_orders_the_sheet() {
        let dir = scratch_dir("manifest");
        write_png(&dir.join("logo.png"), 8, 8, [1, 2, 3, 255]);
        write_png(&dir.join("badge.png"), 8, 2, [4, 5, 6, 255]);
        std::fs::write(
            dir.join("stickers.toml"),
            r#"
[[sticker]]
image = "badge.png"
x = 0.5
y = 0.93
width = 0.1
rotate = -4.0
opacity = 0.8

[[sticker]]
image = "logo.png"
"#,
        )
        .unwrap();
        let sheet = load_sheet(&dir).expect("load");
        assert_eq!(sheet.entries.len(), 2);
        let placed = &sheet.entries[0];
        assert_eq!(placed.at, Some([0.5, 0.93]));
        assert_eq!(placed.width_frac, Some(0.1));
        assert_eq!(placed.rotate, Some(-4.0));
        assert_eq!(placed.opacity, 0.8);
        assert!(sheet.entries[1].at.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_problem_fails_the_whole_load() {
        let dir = scratch_dir("bad");
        write_png(&dir.join("logo.png"), 4, 4, [0, 0, 0, 255]);
        // Half a position.
        std::fs::write(
            dir.join("stickers.toml"),
            "[[sticker]]\nimage = \"logo.png\"\nx = 0.5\n",
        )
        .unwrap();
        assert!(load_sheet(&dir).unwrap_err().contains("x and y"));
        // A missing image.
        std::fs::write(
            dir.join("stickers.toml"),
            "[[sticker]]\nimage = \"gone.png\"\n",
        )
        .unwrap();
        assert!(load_sheet(&dir).unwrap_err().contains("gone.png"));
        // An unknown key, so a typo places nothing silently.
        std::fs::write(
            dir.join("stickers.toml"),
            "[[sticker]]\nimage = \"logo.png\"\nrotat = 4.0\n",
        )
        .unwrap();
        assert!(load_sheet(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_or_overfull_folder_is_refused() {
        let dir = scratch_dir("empty");
        assert!(load_sheet(&dir).unwrap_err().contains("no PNG"));
        for i in 0..=MAX_STICKERS {
            write_png(&dir.join(format!("s{i:02}.png")), 2, 2, [0, 0, 0, 255]);
        }
        assert!(load_sheet(&dir)
            .unwrap_err()
            .contains(&format!("at most {MAX_STICKERS}")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_oversized_source_is_capped_smooth() {
        let dir = scratch_dir("large");
        write_png(&dir.join("big.png"), 1200, 300, [10, 20, 30, 255]);
        let sheet = load_sheet(&dir).expect("load");
        assert_eq!((sheet.entries[0].w, sheet.entries[0].h), (MAX_DIM, 128));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_packer_pads_and_never_overlaps() {
        let images: Vec<Rgba> = [(500, 40), (500, 60), (500, 30), (500, 500), (2, 2)]
            .iter()
            .map(|&(w, h)| Rgba {
                w,
                h,
                px: vec![0; (w * h * 4) as usize],
            })
            .collect();
        let (aw, ah, cells) = pack(&images);
        assert!(aw <= ATLAS_MAX_W);
        let rects: Vec<(u32, u32, u32, u32)> = cells
            .iter()
            .zip(&images)
            .map(|(&(x, y), img)| (x, y, img.w, img.h))
            .collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.0 + a.2 + PAD <= aw && a.1 + a.3 + PAD <= ah, "inside");
            for b in &rects[i + 1..] {
                let clear_x = a.0 + a.2 + PAD <= b.0 || b.0 + b.2 + PAD <= a.0;
                let clear_y = a.1 + a.3 + PAD <= b.1 || b.1 + b.3 + PAD <= a.1;
                assert!(clear_x || clear_y, "cells {a:?} and {b:?} touch");
            }
        }
    }

    /// A synthetic sheet with `n` entries of one aspect, no manifest.
    fn auto_sheet(n: usize, w: u32, h: u32) -> StickerSheet {
        StickerSheet {
            atlas_w: 1,
            atlas_h: 1,
            atlas_rgba: vec![0; 4],
            entries: (0..n)
                .map(|_| SheetEntry {
                    uv: [0.0, 0.0, 1.0, 1.0],
                    w,
                    h,
                    at: None,
                    width_frac: None,
                    rotate: None,
                    opacity: 1.0,
                })
                .collect(),
        }
    }

    #[test]
    fn auto_slots_row_the_top_band_and_stop_at_its_end() {
        let (vw, vh, band) = (1400.0, 1050.0, 106.0);
        let sheet = auto_sheet(12, 100, 50);
        let placed = instances(&sheet, vw, vh, band);
        // Every slot sits centred in the band, marching right in order.
        for inst in &placed {
            assert_eq!(inst.centre[1], band * 0.5);
            assert_eq!(inst.half[1], inst.half[0] * 0.5);
        }
        for pair in placed.windows(2) {
            assert!(pair[0].centre[0] < pair[1].centre[0]);
        }
        // The row ran out before the far corner, and nothing crossed it.
        assert!(placed.len() < 12);
        let last = placed.last().expect("some fit");
        assert!(last.centre[0] + last.half[0] <= vw - vw * 0.055 + 0.5);
        // The tilt alternates: no two neighbours lean the same way.
        for pair in placed.windows(2) {
            assert!(pair[0].rot[1].signum() != pair[1].rot[1].signum());
        }
    }

    #[test]
    fn a_full_row_drops_only_the_slots_that_do_not_fit() {
        let (vw, vh, band) = (1400.0, 1050.0, 106.0);
        let mut sheet = auto_sheet(12, 100, 50);
        // After the wide slots fill the row: an explicit placement, and a
        // much narrower auto slot.
        sheet.entries[10].at = Some([0.5, 0.9]);
        sheet.entries[10].width_frac = Some(0.1);
        sheet.entries[11].w = 10;
        sheet.entries[11].h = 50;
        let placed = instances(&sheet, vw, vh, band);
        // The row filled before the wide slots ran out...
        let on_row = |i: &&Instance| i.centre[1] == band * 0.5;
        assert!(
            placed
                .iter()
                .filter(on_row)
                .filter(|i| i.half[0] > 30.0)
                .count()
                < 10
        );
        // ...but the explicit placement never used the row...
        assert!(
            placed.iter().any(|i| i.centre == [0.5 * vw, 0.9 * vh]),
            "explicit placement dropped with the full row"
        );
        // ...and the narrow slot after the oversize ones still fits.
        assert!(
            placed
                .iter()
                .any(|i| i.half[0] < 30.0 && i.centre[1] == band * 0.5),
            "a narrow auto slot was dropped with the full row"
        );
    }

    #[test]
    fn explicit_placement_maps_fractions_to_the_viewport() {
        let mut sheet = auto_sheet(1, 200, 100);
        sheet.entries[0].at = Some([0.25, 0.9]);
        sheet.entries[0].width_frac = Some(0.1);
        sheet.entries[0].rotate = Some(90.0);
        sheet.entries[0].opacity = 0.5;
        let placed = instances(&sheet, 1000.0, 800.0, 100.0);
        assert_eq!(placed.len(), 1);
        let inst = &placed[0];
        assert_eq!(inst.centre, [250.0, 720.0]);
        assert_eq!(inst.half, [50.0, 25.0]);
        assert!((inst.rot[0]).abs() < 1e-6 && (inst.rot[1] - 1.0).abs() < 1e-6);
        assert_eq!(inst.opacity, 0.5);
    }
}

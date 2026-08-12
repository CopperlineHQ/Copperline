// SPDX-License-Identifier: GPL-3.0-or-later
//
// Renders a bezel shader to a PNG, off-screen, without building Copperline.
//
// The shader is read from disk at run time and the uniforms are filled the
// way `src/video/window/bezel.rs` fills them, on the same wgpu version and
// the same Rgba8UnormSrgb target the window presents to. So what lands in
// the PNG is what the emulator will draw -- the only thing this does not
// reproduce is the picture itself, which is a test pattern here.
//
//   cargo run --release -- [--out FILE] [--size WxH] [--style NAME]
//                          [--crt CURVATURE] [--frame-only]
//
// Editing the .wgsl alone rebuilds nothing: run it again and look.

use std::path::{Path, PathBuf};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BezelUniforms {
    src_rect: [f32; 4],
    size: [f32; 4],
    opening: [f32; 4],
    params: [f32; 4],
}

/// Mirrors `crt_shader::FACE_CORNER_RADIUS`. Only read when a curvature is
/// asked for, which is the one preset that clips a face.
const FACE_CORNER_RADIUS: f32 = 0.0826;

struct Args {
    out: PathBuf,
    width: u32,
    height: u32,
    shader: PathBuf,
    curvature: f32,
    frame_only: bool,
}

fn parse_args() -> Args {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/bezel-preview lives two levels down")
        .to_path_buf();
    let mut a = Args {
        out: PathBuf::from("/tmp/bezel.png"),
        width: 1280,
        height: 960,
        shader: root.join("src/video/window/shaders/bezel_1084.wgsl"),
        curvature: 0.0,
        frame_only: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--out" => a.out = PathBuf::from(it.next().expect("--out FILE")),
            "--size" => {
                let s = it.next().expect("--size WxH");
                let (w, h) = s.split_once('x').expect("--size WxH");
                a.width = w.parse().expect("width");
                a.height = h.parse().expect("height");
            }
            "--style" => {
                let name = it.next().expect("--style NAME");
                a.shader = root.join(format!("src/video/window/shaders/bezel_{name}.wgsl"));
            }
            "--shader" => a.shader = PathBuf::from(it.next().expect("--shader FILE")),
            "--crt" => a.curvature = it.next().expect("--crt K").parse().expect("curvature"),
            "--frame-only" => a.frame_only = true,
            other => panic!("unknown flag {other}"),
        }
    }
    a
}

/// The opening `bezel.rs::opening_rect` would choose for Model1084, given a
/// viewport at the origin. Kept in step by reading the frame constants out
/// of the shader itself rather than repeating them here, exactly as the
/// in-tree test pins the two together.
fn opening_rect(shader: &str, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let top = shader_constant(shader, "FRAME_TOP");
    let well = shader_constant(shader, "FRAME_WELL_BOTTOM");
    let chin = shader_constant(shader, "FRAME_CHIN");
    let oh = h / (1.0 + top + well + chin);
    let ow = oh * (w / h.max(1.0));
    ((w - ow) * 0.5, top * oh, ow, oh)
}

/// Reads `const NAME: f32 = a * b;` out of the shader, the way bezel.rs's
/// `the_frame_proportions_match_the_shader` test does.
fn shader_constant(src: &str, name: &str) -> f32 {
    let key = format!("const {name}: f32 =");
    let at = src
        .find(&key)
        .unwrap_or_else(|| panic!("shader has no {name}"));
    let rest = &src[at + key.len()..];
    let end = rest.find(';').expect("terminated constant");
    rest[..end]
        .split('*')
        .map(|t| {
            let t = t.trim();
            if t == "FRAME_WEIGHT" {
                shader_constant(src, "FRAME_WEIGHT")
            } else {
                t.parse::<f32>().unwrap_or_else(|e| panic!("{name}: {e}"))
            }
        })
        .product()
}

/// A stand-in for the emulated picture: Workbench-ish blue with a grid and
/// corner markers, so the opening's edges and any stretch are obvious.
fn test_pattern(w: u32, h: u32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let edge = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
            let grid = x % 32 == 0 || y % 32 == 0;
            let corner = (x < w / 12 || x + w / 12 >= w) && (y < h / 12 || y + h / 12 >= h);
            let c: [u8; 4] = if edge {
                [255, 80, 80, 255]
            } else if corner {
                [255, 220, 60, 255]
            } else if grid {
                [90, 110, 170, 255]
            } else {
                [40, 60, 130, 255]
            };
            px[i..i + 4].copy_from_slice(&c);
        }
    }
    px
}

fn main() {
    let args = parse_args();
    let shader_src = std::fs::read_to_string(&args.shader)
        .unwrap_or_else(|e| panic!("{}: {e}", args.shader.display()));
    pollster::block_on(run(args, shader_src));
}

async fn run(args: Args, shader_src: String) {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .expect("no GPU adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("bezel-preview"),
            ..Default::default()
        })
        .await
        .expect("no device");

    // The window presents through an sRGB target, so the shader's linear
    // output is encoded by the hardware. Anything else here would shift
    // every colour.
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    let (vw, vh) = (args.width as f32, args.height as f32);

    // The source texture stands in for the `pixels` backing buffer: the
    // display region on top, a status-bar strip below that must never be
    // sampled -- if any magenta appears in the PNG, the sampling is wrong.
    let (sw, sh) = (args.width, args.height);
    let display_rows = sh;
    let mut src = test_pattern(sw, display_rows);
    src.extend(std::iter::repeat([255u8, 0, 255, 255]).take(0).flatten());

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("src"),
        size: wgpu::Extent3d {
            width: sw,
            height: sh,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &src,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(sw * 4),
            rows_per_image: Some(sh),
        },
        wgpu::Extent3d {
            width: sw,
            height: sh,
            depth_or_array_layers: 1,
        },
    );

    let (ox, oy, ow, oh) = opening_rect(&shader_src, vw, vh);
    let strength = if args.curvature > 0.0 { 1.0 } else { 0.0 };
    let uniforms = BezelUniforms {
        src_rect: [0.0, 0.0, 1.0, display_rows as f32 / sh as f32],
        size: [vw, vh, sw as f32, display_rows as f32],
        opening: [ox / vw, oy / vh, ow / vw, oh / vh],
        params: [
            if args.frame_only { 1.0 } else { 0.0 },
            args.curvature,
            if args.curvature > 0.0 {
                FACE_CORNER_RADIUS * strength
            } else {
                0.0
            },
            strength,
        ],
    };

    let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("u"),
        size: std::mem::size_of::<BezelUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&ubuf, 0, bytemuck::bytes_of(&uniforms));

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bezel"),
        source: wgpu::ShaderSource::Wgsl(shader_src.as_str().into()),
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: ubuf.as_entire_binding(),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(FORMAT.into())],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: args.width,
            height: args.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());

    // Blue clear: anything left of it inside the frame is a hole the pass
    // failed to cover.
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tview,
                resolve_target: None,
                depth_slice: None,
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
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    let row = (args.width * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * args.height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
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
                bytes_per_row: Some(row),
                rows_per_image: Some(args.height),
            },
        },
        wgpu::Extent3d {
            width: args.width,
            height: args.height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll");
    let data = slice.get_mapped_range();

    let mut png_data = Vec::with_capacity((args.width * args.height * 4) as usize);
    for y in 0..args.height {
        let start = (y * row) as usize;
        png_data.extend_from_slice(&data[start..start + (args.width * 4) as usize]);
    }
    drop(data);
    readback.unmap();

    let file = std::fs::File::create(&args.out).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), args.width, args.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("header")
        .write_image_data(&png_data)
        .expect("write");
    println!(
        "{}  {}x{}  opening {:.1},{:.1} {:.1}x{:.1}",
        args.out.display(),
        args.width,
        args.height,
        ox,
        oy,
        ow,
        oh
    );
}

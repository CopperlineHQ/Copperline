// SPDX-License-Identifier: GPL-3.0-or-later

//! The optional CRT presentation pass: scanlines, phosphor mask and tube
//! geometry drawn over the finished window image on the GPU.
//!
//! Like [`super::rtg_texture`], this runs inside the `pixels`
//! `render_with` pass, after the scaling renderer has put the composited
//! buffer on the surface. The viewport is the display sub-rect of the
//! letterboxed clip rect, and the shader samples only the display region
//! of the `pixels` backing texture (`src_rect`), so the status bar
//! underneath the display is neither read nor overdrawn.
//!
//! Purely a presentation stage: screenshots, frame dumps, recordings and
//! headless runs never go through it, so captures stay comparable
//! whatever preset is selected.
//!
//! The three presets are WGSL sources in `shaders/`, embedded at build
//! time. All three -- and any user shader loaded with
//! [`CrtShader::load_custom`] -- share one binding layout: a display
//! texture, a linear sampler, and a 64-byte [`CrtUniforms`] block.
//!
//! Every preset's arithmetic is exact at strength 0: the fragment shader
//! returns the sample it took, untouched. The pass itself is still a
//! resample, though, and it reads through a plain linear sampler where
//! the `pixels` scaling renderer uses a texel-snapped sharp bilinear, so
//! turning any preset on softens a magnified picture slightly against the
//! pass-through renderer whatever the strength. That softening is part of
//! the tube look and is deliberately not corrected for.
//! [`ShaderKind::None`] skips the pass altogether and is the only true
//! zero-cost path.

use crate::config::ShaderKind;
use pixels::wgpu;
use std::borrow::Cow;
use std::future::Future;
use std::path::Path;
use zerocopy::{Immutable, IntoBytes};

/// Preset sources, embedded so a stock build needs no files on disk.
const SCANLINES_WGSL: &str = include_str!("shaders/scanlines.wgsl");
const MASK_WGSL: &str = include_str!("shaders/mask.wgsl");
const CRT_WGSL: &str = include_str!("shaders/crt.wgsl");

/// Largest custom shader accepted from disk. A window shader is a few
/// kilobytes of WGSL; anything past a megabyte is a mistyped path.
const MAX_CUSTOM_SHADER_BYTES: u64 = 1024 * 1024;

/// Size of the uniform block, in bytes. Also the bind group layout's
/// `min_binding_size`, so a shader declaring a larger block fails to
/// build rather than reading past the buffer.
const UNIFORM_BYTES: u64 = std::mem::size_of::<CrtUniforms>() as u64;

/// The uniform block every window shader sees. Mirrors `CrtUniforms` in
/// the WGSL sources exactly; `#[repr(C)]` with 16-byte-aligned members
/// only, so the Rust and WGSL layouts agree with no padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, IntoBytes, Immutable)]
pub(super) struct CrtUniforms {
    /// Display sub-rect of the source texture in UV space: xy origin, zw
    /// size. The status bar lives below it and is never sampled.
    pub(super) src_rect: [f32; 4],
    /// xy: viewport size in physical pixels. zw: source display region
    /// in texels.
    pub(super) size: [f32; 4],
    /// x: strength 0..1. y: scanline count. z: mask kind. w: curvature.
    pub(super) params: [f32; 4],
    /// x: vignette 0..1. yzw: reserved.
    pub(super) params2: [f32; 4],
}

impl CrtUniforms {
    /// Re-aim a frame's uniforms at a smaller viewport (the bezel
    /// opening): the source mapping is unchanged, but the pixel-keyed
    /// phosphor mask and the AA arithmetic follow the pass's own
    /// viewport size.
    pub(super) fn with_viewport(mut self, viewport: (f32, f32, f32, f32)) -> Self {
        self.size[0] = viewport.2;
        self.size[1] = viewport.3;
        self
    }
}

/// Preset pipeline slots, in the order [`CrtShader::presets`] holds them.
const PRESET_SCANLINES: usize = 0;
const PRESET_MASK: usize = 1;
const PRESET_CRT: usize = 2;

/// The CRT pass: one pipeline per preset, plus a user shader if one was
/// loaded, and the bindings they share.
pub(super) struct CrtShader {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    presets: [wgpu::RenderPipeline; 3],
    custom: Option<wgpu::RenderPipeline>,
    bind_group: Option<wgpu::BindGroup>,
    /// The texture the current bind group views, compared by identity:
    /// `pixels` recreates its backing texture on a buffer resize, and the
    /// stale view has to be dropped with it.
    bound_texture: Option<wgpu::Texture>,
}

/// The binding layout every shader-preset pass shares: a display texture, a
/// linear sampler and a 64-byte uniform block. The bezel pass declares the
/// same bindings with its own uniform contents, so it builds against this
/// too.
pub(super) fn shader_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("crt_shader_bgl"),
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
    })
}

/// Build one shader pipeline against the shared bind group layout. All
/// presets and user shaders (and the bezel pass) use the same entry points
/// and target state, so this is the only place a preset pipeline is
/// created (the RTG texture pass builds its own).
pub(super) fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &str,
    label: &str,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("crt_shader_pl"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
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
    })
}

/// Poll a future exactly once on a no-op waker, for the wgpu-core futures
/// that are already resolved when they are handed back.
pub(super) fn poll_once<F: Future>(fut: F) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

impl CrtShader {
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = shader_bind_group_layout(device);
        let presets = [
            build_pipeline(
                device,
                &bind_group_layout,
                SCANLINES_WGSL,
                "crt_shader_scanlines",
                target_format,
            ),
            build_pipeline(
                device,
                &bind_group_layout,
                MASK_WGSL,
                "crt_shader_mask",
                target_format,
            ),
            build_pipeline(
                device,
                &bind_group_layout,
                CRT_WGSL,
                "crt_shader_crt",
                target_format,
            ),
        ];
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crt_shader_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt_shader_uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            bind_group_layout,
            sampler,
            uniforms,
            presets,
            custom: None,
            bind_group: None,
            bound_texture: None,
        }
    }

    /// The pipeline a selection draws with, or `None` when the pass is off
    /// (`ShaderKind::None`) or a custom shader was selected but failed to
    /// load.
    fn pipeline_for(&self, kind: ShaderKind) -> Option<&wgpu::RenderPipeline> {
        match kind {
            ShaderKind::None => None,
            ShaderKind::Scanlines => Some(&self.presets[PRESET_SCANLINES]),
            ShaderKind::Mask => Some(&self.presets[PRESET_MASK]),
            ShaderKind::Crt => Some(&self.presets[PRESET_CRT]),
            ShaderKind::Custom => self.custom.as_ref(),
        }
    }

    /// Compile a user WGSL shader and make it the `Custom` pipeline.
    ///
    /// The source is checked by naga first, so a syntax or validation
    /// error is reported with its source location instead of surfacing as
    /// a device error later. A failed load leaves no custom pipeline: the
    /// caller falls back to no shader rather than to a stale one.
    pub(super) fn load_custom(
        &mut self,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        path: &Path,
    ) -> Result<(), String> {
        self.custom = None;
        let src = read_shader_file(path)?;
        validate_wgsl_source(&src).map_err(|e| format!("shader {}: {e}", path.display()))?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = build_pipeline(
            device,
            &self.bind_group_layout,
            &src,
            "crt_shader_custom",
            target_format,
        );
        // On wgpu-core (every native backend) the popped scope's future is
        // already resolved, so one poll reads it. A `Pending` can only come
        // from a backend that defers validation, and naga has already
        // accepted the module, so treat it as success.
        if let Some(err) = poll_once(scope.pop()).flatten() {
            return Err(format!("shader {}: {err}", path.display()));
        }
        self.custom = Some(pipeline);
        Ok(())
    }

    /// Drop any loaded custom shader, so `ShaderKind::Custom` draws
    /// nothing until one is loaded again.
    pub(super) fn clear_custom(&mut self) {
        self.custom = None;
    }

    /// Draw the CRT pass over the `(x, y, w, h)` viewport rect of
    /// `target` (physical surface pixels), sampling `src_texture`
    /// through `uniforms.src_rect`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: (f32, f32, f32, f32),
        kind: ShaderKind,
        uniforms: CrtUniforms,
    ) {
        if self.pipeline_for(kind).is_none() {
            return;
        }
        let (x, y, w, h) = viewport;
        if w < 1.0 || h < 1.0 {
            return;
        }
        if self.bound_texture.as_ref() != Some(src_texture) {
            let view = src_texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("crt_shader_bg"),
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
        let (Some(pipeline), Some(bind_group)) = (self.pipeline_for(kind), &self.bind_group) else {
            return;
        };
        queue.write_buffer(&self.uniforms, 0, uniforms.as_bytes());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("crt_shader_pass"),
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

/// Read a custom shader from disk, refusing anything implausibly large.
///
/// The path comes from a config file or the `COPPERLINE_SHADER` env var,
/// so it is not necessarily a regular file at all. Nothing here reads an
/// unbounded amount: a non-regular file is rejected on its type before any
/// read (a FIFO or character device has no meaningful length and never
/// reaches EOF, which no read bound alone would make safe), and the read
/// that follows is itself capped.
fn read_shader_file(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let io_err = |e: std::io::Error| format!("cannot read shader {}: {e}", path.display());
    let too_big = || {
        format!(
            "shader {} is over the {MAX_CUSTOM_SHADER_BYTES} byte limit",
            path.display()
        )
    };
    let file = std::fs::File::open(path).map_err(io_err)?;
    // Stat the open handle, not the path: nothing can be swapped in
    // between the check and the read.
    let meta = file.metadata().map_err(io_err)?;
    if !meta.file_type().is_file() {
        return Err(format!("shader {} is not a regular file", path.display()));
    }
    if meta.len() > MAX_CUSTOM_SHADER_BYTES {
        return Err(too_big());
    }
    // The length above is only advisory -- the file can grow between the
    // stat and the read -- so bound the read as well, by one byte more
    // than the cap: if that byte arrives, the file is over the limit.
    let mut buf = Vec::new();
    file.take(MAX_CUSTOM_SHADER_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(io_err)?;
    if buf.len() as u64 > MAX_CUSTOM_SHADER_BYTES {
        return Err(too_big());
    }
    String::from_utf8(buf).map_err(|e| format!("shader {} is not valid UTF-8: {e}", path.display()))
}

/// Parse and validate WGSL, and check it declares the entry points the
/// pass calls. Pure CPU work: no device is needed, so a shader can be
/// checked before any GPU resources exist.
pub(super) fn validate_wgsl_source(src: &str) -> Result<(), String> {
    use wgpu::naga;
    let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .map_err(|e| e.emit_to_string(src))?;
    let has = |stage: naga::ShaderStage, name: &str| {
        module
            .entry_points
            .iter()
            .any(|ep| ep.stage == stage && ep.name == name)
    };
    if !has(naga::ShaderStage::Vertex, "vs_main") {
        return Err("missing @vertex entry point vs_main".to_string());
    }
    if !has(naga::ShaderStage::Fragment, "fs_main") {
        return Err("missing @fragment entry point fs_main".to_string());
    }
    Ok(())
}

/// Build the uniform block and the viewport rect for one presented frame.
///
/// `clip` is the `pixels` scaling renderer's letterboxed clip rect in
/// physical surface pixels; `present_h` / `window_present_h` are the
/// display height and the whole composited buffer height, so their ratio
/// is the fraction of the buffer (and of the clip rect) the display
/// occupies, with the status bar making up the rest. `texture_extent` is
/// the `pixels` backing texture size in texels.
///
/// Pure arithmetic: no GPU state is touched, so the mapping is unit
/// testable on its own.
pub(super) fn uniforms_for(
    kind: ShaderKind,
    strength: f32,
    clip: (u32, u32, u32, u32),
    present_h: usize,
    window_present_h: usize,
    texture_extent: (u32, u32),
    scanlines: f32,
) -> (CrtUniforms, (f32, f32, f32, f32)) {
    let (cx, cy, cw, ch) = clip;
    // Same multiply-then-divide order as the RTG display rect in
    // window.rs, so the two passes land on identical viewports.
    let display_fraction = |v: f32| {
        if window_present_h == 0 {
            v
        } else {
            v * present_h as f32 / window_present_h as f32
        }
    };
    let viewport = (cx as f32, cy as f32, cw as f32, display_fraction(ch as f32));
    // mask kind, curvature, vignette
    let (mask, curvature, vignette) = match kind {
        ShaderKind::Scanlines => (0.0, 0.0, 0.0),
        // 2: staggered dot/shadow mask.
        ShaderKind::Mask => (2.0, 0.0, 0.0),
        // 1: aperture grille, with a bowed face and corner falloff.
        ShaderKind::Crt => (1.0, 0.35, 0.15),
        // A user shader gets the frame geometry and the two knobs it can
        // sensibly honour; the preset look table means nothing to it.
        ShaderKind::None | ShaderKind::Custom => (0.0, 0.0, 0.0),
    };
    let uniforms = CrtUniforms {
        src_rect: [0.0, 0.0, 1.0, display_fraction(1.0)],
        size: [
            viewport.2,
            viewport.3,
            texture_extent.0 as f32,
            display_fraction(texture_extent.1 as f32),
        ],
        params: [strength, scanlines, mask, curvature],
        params2: [vignette, 0.0, 0.0, 0.0],
    };
    (uniforms, viewport)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESET_SOURCES: [(&str, &str); 3] = [
        ("scanlines", SCANLINES_WGSL),
        ("mask", MASK_WGSL),
        ("crt", CRT_WGSL),
    ];

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copperline-crt-shader-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    // --- WGSL validation (no GPU) ---------------------------------------

    const CONTRACT_BEGIN: &str = "// --- begin shared contract ---";
    const CONTRACT_END: &str = "// --- end shared contract ---";

    /// The uniform block, bindings, vertex stage and sampling helper the
    /// presets share, cut out of one preset source.
    fn contract_block<'a>(src: &'a str, name: &str) -> &'a str {
        let begin = src
            .find(CONTRACT_BEGIN)
            .unwrap_or_else(|| panic!("{name}: no {CONTRACT_BEGIN}"));
        let end = src
            .find(CONTRACT_END)
            .unwrap_or_else(|| panic!("{name}: no {CONTRACT_END}"));
        &src[begin..end + CONTRACT_END.len()]
    }

    #[test]
    fn every_preset_source_validates() {
        for (name, src) in PRESET_SOURCES {
            if let Err(e) = validate_wgsl_source(src) {
                panic!("preset {name} failed validation:\n{e}");
            }
        }
    }

    /// The contract prologue is triplicated across the three preset files
    /// (each is a standalone example of what a user shader must declare),
    /// so nothing but this keeps the copies in step.
    #[test]
    fn every_preset_shares_one_byte_identical_contract() {
        let reference = contract_block(SCANLINES_WGSL, "scanlines");
        assert!(
            reference.contains("fn sample_display")
                && reference.contains("fn vs_main")
                && reference.contains("struct CrtUniforms"),
            "the contract block should cover the uniforms, vs_main and sampling"
        );
        for (name, src) in PRESET_SOURCES {
            assert_eq!(
                contract_block(src, name),
                reference,
                "{name}.wgsl: shared contract has drifted from scanlines.wgsl"
            );
        }
    }

    #[test]
    fn garbage_source_fails_with_a_location() {
        let err = validate_wgsl_source("not wgsl @@@").expect_err("garbage must not validate");
        assert!(
            err.contains("wgsl:1:"),
            "error should name a source location: {err}"
        );
    }

    #[test]
    fn a_module_without_fs_main_names_the_missing_entry_point() {
        const ONLY_VERTEX: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32(idx), 0.0, 0.0, 1.0);
}
"#;
        let err = validate_wgsl_source(ONLY_VERTEX).expect_err("no fragment stage");
        assert!(err.contains("fs_main"), "{err}");

        const ONLY_FRAGMENT: &str = r#"
@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
"#;
        let err = validate_wgsl_source(ONLY_FRAGMENT).expect_err("no vertex stage");
        assert!(err.contains("vs_main"), "{err}");
    }

    // --- reading a shader off disk (no GPU) ------------------------------

    #[test]
    fn read_shader_file_caps_the_size_and_names_the_path() {
        let dir = temp_dir("read");
        let huge = dir.join("huge.wgsl");
        std::fs::write(&huge, vec![b'\n'; MAX_CUSTOM_SHADER_BYTES as usize + 1]).expect("write");
        let err = read_shader_file(&huge).expect_err("over the cap");
        assert!(err.contains("huge.wgsl"), "{err}");
        assert!(err.contains("byte limit"), "{err}");

        let small = dir.join("small.wgsl");
        std::fs::write(&small, "// fine\n").expect("write");
        assert_eq!(
            read_shader_file(&small).expect("under the cap"),
            "// fine\n"
        );

        let err = read_shader_file(&dir.join("absent.wgsl")).expect_err("missing file");
        assert!(err.contains("cannot read shader"), "{err}");
        assert!(err.contains("absent.wgsl"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A shader path comes from a config file, so it can name something
    /// that is not a regular file. Those are rejected on their type,
    /// before any read: a character device or a FIFO has no meaningful
    /// length and never reaches EOF, so reading one would hang or run the
    /// process out of memory however the read is bounded. A directory
    /// stands in for the class here because it cannot block the test.
    #[test]
    fn read_shader_file_rejects_a_non_regular_file() {
        let dir = temp_dir("nonregular");
        let err = read_shader_file(&dir).expect_err("a directory is not a shader");
        // Unix opens the directory and the type check refuses it; Windows
        // refuses at File::open (Access is denied), so the rejection there
        // is the open error. Either way nothing is read as shader source.
        #[cfg(unix)]
        assert!(err.contains("not a regular file"), "{err}");
        #[cfg(windows)]
        assert!(err.contains("cannot read shader"), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- uniforms_for arithmetic (no GPU) -------------------------------

    /// A PAL window: 716x581 composited buffer, 537 rows of display and a
    /// 44-row status bar under it.
    #[test]
    fn uniforms_split_the_clip_rect_at_the_status_bar() {
        let (u, viewport) = uniforms_for(
            ShaderKind::Scanlines,
            1.0,
            (0, 0, 716, 581),
            537,
            581,
            (716, 581),
            537.0,
        );
        assert_eq!(viewport, (0.0, 0.0, 716.0, 581.0 * 537.0 / 581.0));
        assert_eq!(viewport.3, 537.0);
        assert_eq!(u.src_rect, [0.0, 0.0, 1.0, 537.0 / 581.0]);
        assert_eq!(u.size, [716.0, 537.0, 716.0, 581.0 * 537.0 / 581.0]);
        assert_eq!(u.params[0], 1.0);
        assert_eq!(u.params[1], 537.0);
    }

    #[test]
    fn a_letterboxed_clip_rect_offsets_the_viewport() {
        let (_, viewport) = uniforms_for(
            ShaderKind::Crt,
            1.0,
            (12, 34, 640, 512),
            537,
            581,
            (716, 581),
            537.0,
        );
        assert_eq!(viewport.0, 12.0);
        assert_eq!(viewport.1, 34.0);
        assert_eq!(viewport.2, 640.0);
        assert_eq!(viewport.3, 512.0 * 537.0 / 581.0);
    }

    /// On a retina surface the clip rect and the texture both double; the
    /// display fraction is a ratio, so it must not move.
    #[test]
    fn doubling_the_surface_leaves_the_source_rect_alone() {
        let args = |scale: u32| {
            uniforms_for(
                ShaderKind::Crt,
                0.75,
                (0, 0, 716 * scale, 581 * scale),
                537,
                581,
                (716 * scale, 581 * scale),
                537.0,
            )
        };
        let (one, vp_one) = args(1);
        let (two, vp_two) = args(2);
        assert_eq!(one.src_rect, two.src_rect);
        assert_eq!(vp_two.2, vp_one.2 * 2.0);
        assert_eq!(vp_two.3, vp_one.3 * 2.0);
        assert_eq!(two.size[0], one.size[0] * 2.0);
        assert_eq!(two.size[1], one.size[1] * 2.0);
        assert_eq!(two.size[2], one.size[2] * 2.0);
        assert_eq!(two.size[3], one.size[3] * 2.0);
    }

    /// With the status bar hidden the display is the whole buffer.
    #[test]
    fn a_hidden_status_bar_gives_the_whole_clip_rect() {
        let (u, viewport) = uniforms_for(
            ShaderKind::Scanlines,
            1.0,
            (0, 0, 716, 581),
            581,
            581,
            (716, 581),
            581.0,
        );
        assert_eq!(u.src_rect, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(viewport, (0.0, 0.0, 716.0, 581.0));
        assert_eq!(u.size, [716.0, 581.0, 716.0, 581.0]);
    }

    #[test]
    fn each_preset_gets_its_own_look_parameters() {
        let get = |kind| uniforms_for(kind, 0.5, (0, 0, 716, 581), 537, 581, (716, 581), 537.0).0;
        let scan = get(ShaderKind::Scanlines);
        assert_eq!(scan.params, [0.5, 537.0, 0.0, 0.0]);
        assert_eq!(scan.params2, [0.0; 4]);

        let mask = get(ShaderKind::Mask);
        assert_eq!(mask.params, [0.5, 537.0, 2.0, 0.0]);
        assert_eq!(mask.params2, [0.0; 4]);

        let crt = get(ShaderKind::Crt);
        assert_eq!(crt.params, [0.5, 537.0, 1.0, 0.35]);
        assert_eq!(crt.params2, [0.15, 0.0, 0.0, 0.0]);

        let custom = get(ShaderKind::Custom);
        assert_eq!(custom.params, [0.5, 537.0, 0.0, 0.0]);
        assert_eq!(custom.params2, [0.0; 4]);
        // The frame geometry a user shader still needs is filled in.
        assert_eq!(custom.src_rect, scan.src_rect);
        assert_eq!(custom.size, scan.size);
    }

    // --- offscreen render (needs a GPU adapter) -------------------------

    const TEX: u32 = 64;
    const DISPLAY_ROWS: u32 = 48;
    const GREY: u8 = 128;
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    /// What the target holds before the pass runs. Pure blue because the
    /// clear colour is linear and the target sRGB: 0 and 1 are the only
    /// components that survive the encode to an exact byte, so anything
    /// else in the untouched rows is the pass writing out of bounds.
    const SENTINEL: [u8; 4] = [0, 0, 255, 255];

    /// One readback of the render target.
    struct Frame {
        px: Vec<[u8; 4]>,
        /// Square target edge, in pixels.
        dim: u32,
        /// Rows the display viewport covered. Below this is all sentinel.
        rows: u32,
    }

    impl Frame {
        fn at(&self, x: u32, y: u32) -> [u8; 4] {
            self.px[(y * self.dim + x) as usize]
        }

        fn luma(p: [u8; 4]) -> f32 {
            0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
        }

        /// Mean luminance of one output row across the display viewport.
        fn row_mean(&self, y: u32) -> f32 {
            let sum: f32 = (0..self.dim).map(|x| Self::luma(self.at(x, y))).sum();
            sum / self.dim as f32
        }

        /// Mean luminance of an `n` x `n` block with its top-left at (x, y).
        fn block_mean(&self, x: u32, y: u32, n: u32) -> f32 {
            let mut sum = 0.0;
            for dy in 0..n {
                for dx in 0..n {
                    sum += Self::luma(self.at(x + dx, y + dy));
                }
            }
            sum / (n * n) as f32
        }

        /// The status bar must never reach the output, and the pass must
        /// stay inside its viewport: rows below the display keep the clear
        /// colour.
        fn assert_display_region_only(&self, what: &str) {
            for y in 0..self.rows {
                for x in 0..self.dim {
                    let p = self.at(x, y);
                    assert!(
                        !(p[0] > 180 && p[1] < 80 && p[2] > 180),
                        "{what}: status-bar magenta bled into the display at ({x}, {y}): {p:?}"
                    );
                }
            }
            for y in self.rows..self.dim {
                for x in 0..self.dim {
                    assert_eq!(
                        self.at(x, y),
                        SENTINEL,
                        "{what}: pass wrote outside its viewport at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// A device, or `None` on a machine with no usable adapter (headless
    /// CI): the render tests then pass without asserting anything.
    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = match poll_once(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })) {
            Some(Ok(adapter)) => adapter,
            _ => return None,
        };
        // Software rasterizers do not render these passes the way real
        // hardware does: DX12 WARP on the Windows CI runners puts flat-grey
        // pixels up to 14 8-bit steps off the source, far beyond the
        // tolerances real adapters need, so pixel asserts against it test
        // WARP, not the shaders. Skip them like a missing adapter; the
        // render tests run for real on the macOS CI runners' Metal GPU and
        // on developer machines.
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            eprintln!(
                "skipping: software rasterizer ({})",
                adapter.get_info().name
            );
            return None;
        }
        match poll_once(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("crt_shader_test"),
            ..Default::default()
        })) {
            Some(Ok(pair)) => Some(pair),
            _ => None,
        }
    }

    /// The `pixels` backing texture, in miniature: `DISPLAY_ROWS` of flat
    /// `display` colour with a magenta "status bar" filling the rest.
    fn source_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        display: [u8; 4],
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crt_shader_test_src"),
            size: wgpu::Extent3d {
                width: TEX,
                height: TEX,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut texels = vec![0u8; (TEX * TEX * 4) as usize];
        for y in 0..TEX {
            for x in 0..TEX {
                let px = if y < DISPLAY_ROWS {
                    display
                } else {
                    [255, 0, 255, 255]
                };
                let off = ((y * TEX + x) * 4) as usize;
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
                bytes_per_row: Some(TEX * 4),
                rows_per_image: Some(TEX),
            },
            wgpu::Extent3d {
                width: TEX,
                height: TEX,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    fn grey_source(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
        source_texture(device, queue, [GREY, GREY, GREY, 255])
    }

    /// Clear a `TEX * scale` square target to the sentinel, run one preset
    /// over its display viewport, and read the result back.
    ///
    /// `scale` magnifies the window without touching the source texture,
    /// which is what a real window does: the composited buffer is a fixed
    /// size and the surface is whatever the user dragged it to.
    #[allow(clippy::too_many_arguments)]
    fn render_preset(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shader: &mut CrtShader,
        src: &wgpu::Texture,
        kind: ShaderKind,
        strength: f32,
        scanlines: f32,
        scale: u32,
    ) -> Frame {
        let dim = TEX * scale;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crt_shader_test_target"),
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
            label: Some("crt_shader_test_clear"),
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
        let (uniforms, viewport) = uniforms_for(
            kind,
            strength,
            (0, 0, dim, dim),
            DISPLAY_ROWS as usize,
            TEX as usize,
            (TEX, TEX),
            scanlines,
        );
        let rows = DISPLAY_ROWS * scale;
        assert_eq!(viewport, (0.0, 0.0, dim as f32, rows as f32));
        shader.render(
            device,
            queue,
            src,
            &mut encoder,
            &view,
            viewport,
            kind,
            uniforms,
        );

        let padded = (dim * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt_shader_test_readback"),
            size: (padded * dim) as u64,
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
                    rows_per_image: Some(dim),
                },
            },
            wgpu::Extent3d {
                width: dim,
                height: dim,
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
        let mut px = Vec::with_capacity((dim * dim) as usize);
        for y in 0..dim {
            let base = (y * padded) as usize;
            for x in 0..dim {
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
        Frame { px, dim, rows }
    }

    /// Every channel of every pixel in the display region is the flat grey
    /// the source carried, within `tol`.
    fn assert_flat_grey(frame: &Frame, what: &str, tol: u8) {
        for y in 0..frame.rows {
            for x in 0..frame.dim {
                let p = frame.at(x, y);
                for (c, v) in p[..3].iter().enumerate() {
                    assert!(
                        v.abs_diff(GREY) <= tol,
                        "{what}: channel {c} at ({x}, {y}) is {v}, not the source grey: {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn presets_render_the_display_region_only() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);

        for kind in [ShaderKind::Scanlines, ShaderKind::Mask, ShaderKind::Crt] {
            let label = kind.label();
            // Strength 0 leaves the shader arithmetic an identity, so a
            // 1:1 pass reproduces the flat grey exactly.
            let off = render_preset(&device, &queue, &mut shader, &src, kind, 0.0, 16.0, 1);
            off.assert_display_region_only(label);
            assert_flat_grey(&off, &format!("{label} at strength 0"), 2);

            let on = render_preset(&device, &queue, &mut shader, &src, kind, 1.0, 16.0, 1);
            on.assert_display_region_only(label);
        }
    }

    /// Magnified, the last fragment row of the display lands past the last
    /// display texel's centre. Clamping the sample to the edge of
    /// `src_rect` rather than half a texel inside it would blend in the
    /// status bar's first row -- its bright separator hairline -- along the
    /// whole bottom of the picture.
    #[test]
    fn a_magnified_display_never_blends_in_the_status_bar() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);

        for kind in [ShaderKind::Scanlines, ShaderKind::Mask, ShaderKind::Crt] {
            let label = kind.label();
            let frame = render_preset(&device, &queue, &mut shader, &src, kind, 0.0, 16.0, 2);
            assert_eq!((frame.dim, frame.rows), (TEX * 2, DISPLAY_ROWS * 2));
            frame.assert_display_region_only(label);
            // Every row, the last one included: a magenta blend shows up
            // as a channel imbalance long before it looks like magenta.
            assert_flat_grey(&frame, &format!("{label} magnified"), 2);
        }
    }

    /// The bowed face of the CRT preset pushes sample coordinates past the
    /// bottom of the display rect. At full strength those pixels are
    /// blacked out anyway, but at an intermediate strength they are only
    /// partly dimmed, so anything the clamp picked up shows through.
    #[test]
    fn the_crt_warp_never_reaches_the_status_bar() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);

        for scale in [1, 2] {
            let frame = render_preset(
                &device,
                &queue,
                &mut shader,
                &src,
                ShaderKind::Crt,
                0.5,
                16.0,
                scale,
            );
            for y in 0..frame.rows {
                for x in 0..frame.dim {
                    let p = frame.at(x, y);
                    // The grille lifts exactly one channel per column, so
                    // red and blue can never both run ahead of green.
                    // Status-bar magenta is the only thing that does.
                    assert!(
                        !(p[0] as i32 > p[1] as i32 + 25 && p[2] as i32 > p[1] as i32 + 25),
                        "crt at {scale}x: status-bar magenta at ({x}, {y}): {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scanlines_modulate_rows_periodically() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);

        // 16 lines over 48 output rows: three rows per emulated line, so
        // the raised cosine is sampled at its peak and at both flanks.
        let frame = render_preset(
            &device,
            &queue,
            &mut shader,
            &src,
            ShaderKind::Scanlines,
            1.0,
            16.0,
            1,
        );
        let means: Vec<f32> = (0..frame.rows).map(|y| frame.row_mean(y)).collect();
        let hi = means.iter().cloned().fold(f32::MIN, f32::max);
        let lo = means.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            hi - lo > 20.0,
            "scanlines should modulate row brightness, got spread {:.1} ({:?})",
            hi - lo,
            &means[..6]
        );
        // Periodic, not a gradient: the profile repeats every three rows.
        for y in 0..frame.rows - 3 {
            assert!(
                (means[y as usize] - means[(y + 3) as usize]).abs() < 3.0,
                "row {y} and row {} should sit at the same phase",
                y + 3
            );
        }

        // At exactly two output rows per emulated line every row lands on
        // one of the two half-points of the beam profile, which are equal
        // by symmetry, so the modulation cancels. A property of sampling a
        // symmetric profile at Nyquist, not a bug: the line structure only
        // shows once the window is scaled past 2x the emulated lines.
        let flat = render_preset(
            &device,
            &queue,
            &mut shader,
            &src,
            ShaderKind::Scanlines,
            1.0,
            (DISPLAY_ROWS / 2) as f32,
            1,
        );
        let means: Vec<f32> = (0..flat.rows).map(|y| flat.row_mean(y)).collect();
        let hi = means.iter().cloned().fold(f32::MIN, f32::max);
        let lo = means.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            hi - lo < 3.0,
            "spread at Nyquist should cancel, got {hi} {lo}"
        );
    }

    #[test]
    fn the_mask_preset_colours_pixels_into_triads() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);
        let frame = render_preset(
            &device,
            &queue,
            &mut shader,
            &src,
            ShaderKind::Mask,
            1.0,
            16.0,
            1,
        );

        // Flat grey in, phosphor triads out: some pixels favour red, some
        // green, some blue, on a source with no colour of its own.
        let mut reddest = 0;
        let mut greenest = 0;
        let mut bluest = 0;
        for y in 0..frame.rows {
            for x in 0..frame.dim {
                let p = frame.at(x, y);
                if p[0] > p[1] && p[0] > p[2] {
                    reddest += 1;
                } else if p[1] > p[0] && p[1] > p[2] {
                    greenest += 1;
                } else if p[2] > p[0] && p[2] > p[1] {
                    bluest += 1;
                }
            }
        }
        let total = (frame.dim * frame.rows) as usize;
        for (name, n) in [("red", reddest), ("green", greenest), ("blue", bluest)] {
            assert!(
                n > total / 6,
                "{name} phosphor should cover about a third of the display, got {n}/{total}"
            );
        }

        // The row stagger shifts the triad by two columns every three
        // rows, so a column is not one solid phosphor stripe.
        let column: Vec<[u8; 4]> = (0..frame.rows).map(|y| frame.at(7, y)).collect();
        assert!(
            column.iter().any(|p| p[0] > p[1]) && column.iter().any(|p| p[1] > p[0]),
            "a shadow mask staggers its triads; column 7 came out as one stripe"
        );
    }

    #[test]
    fn the_crt_preset_darkens_the_corners() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);
        let frame = render_preset(
            &device,
            &queue,
            &mut shader,
            &src,
            ShaderKind::Crt,
            1.0,
            16.0,
            1,
        );

        let centre = frame.block_mean(frame.dim / 2 - 3, frame.rows / 2 - 3, 6);
        for (name, x, y) in [
            ("top left", 0, 0),
            ("top right", frame.dim - 6, 0),
            ("bottom left", 0, frame.rows - 6),
            ("bottom right", frame.dim - 6, frame.rows - 6),
        ] {
            let corner = frame.block_mean(x, y, 6);
            assert!(
                corner < centre * 0.7,
                "{name} corner ({corner:.1}) should be well below the centre ({centre:.1})"
            );
        }
    }

    /// The tube face ends at a hard edge: what the bow pushed off it is
    /// the unlit inside of the tube, black at any strength. Mixing that
    /// black back toward the sample left the off-face region holding a
    /// fraction of the edge colour the clamp smears there -- 30 percent of
    /// it at strength 0.7 -- which read as a coloured halo around the
    /// bowed picture on every bright border.
    #[test]
    fn the_crt_face_edge_is_hard_at_partial_strength() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        // A saturated display colour, so a fractional leak is unmistakable
        // rather than a slightly-off grey.
        let src = source_texture(&device, &queue, [200, 60, 200, 255]);
        let mut shader = CrtShader::new(&device, FORMAT);
        let frame = render_preset(
            &device,
            &queue,
            &mut shader,
            &src,
            ShaderKind::Crt,
            0.7,
            16.0,
            1,
        );

        // At 0.7 the bow carries the extreme corners about 5% of the face
        // past its edge, far outside the one-pixel antialias band.
        for (name, x, y) in [
            ("top left", 0, 0),
            ("top right", frame.dim - 1, 0),
            ("bottom left", 0, frame.rows - 1),
            ("bottom right", frame.dim - 1, frame.rows - 1),
        ] {
            let p = frame.at(x, y);
            for (c, v) in p[..3].iter().enumerate() {
                assert!(
                    *v <= 4,
                    "{name} corner is off the face and must be black, channel {c} is {v}: {p:?}"
                );
            }
        }
        // The picture itself is still lit: the edge treatment must not
        // have taken the whole frame down with it.
        let centre = frame.block_mean(frame.dim / 2 - 3, frame.rows / 2 - 3, 6);
        assert!(centre > 30.0, "the picture went dark too: {centre:.1}");
    }

    /// `pixels` recreates its backing texture whenever the buffer is
    /// resized, so the cached bind group has to follow the new texture
    /// instead of holding a view of the old one.
    #[test]
    fn a_new_source_texture_rebuilds_the_bind_group() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let mut shader = CrtShader::new(&device, FORMAT);
        let grey = grey_source(&device, &queue);
        let green = source_texture(&device, &queue, [0, GREY, 0, 255]);

        let first = render_preset(
            &device,
            &queue,
            &mut shader,
            &grey,
            ShaderKind::Scanlines,
            0.0,
            16.0,
            1,
        );
        assert_flat_grey(&first, "first texture", 2);

        let second = render_preset(
            &device,
            &queue,
            &mut shader,
            &green,
            ShaderKind::Scanlines,
            0.0,
            16.0,
            1,
        );
        for y in 0..second.rows {
            for x in 0..second.dim {
                let p = second.at(x, y);
                assert!(
                    p[0] <= 4 && p[2] <= 4 && p[1].abs_diff(GREY) <= 2,
                    "the pass is still sampling the first texture at ({x}, {y}): {p:?}"
                );
            }
        }
    }

    /// Off, and a custom selection with nothing loaded, must not draw:
    /// the target keeps whatever the scaling renderer put there.
    #[test]
    fn a_disabled_pass_leaves_the_target_untouched() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let src = grey_source(&device, &queue);
        let mut shader = CrtShader::new(&device, FORMAT);

        for kind in [ShaderKind::None, ShaderKind::Custom] {
            let frame = render_preset(&device, &queue, &mut shader, &src, kind, 1.0, 16.0, 1);
            for y in 0..frame.dim {
                for x in 0..frame.dim {
                    assert_eq!(
                        frame.at(x, y),
                        SENTINEL,
                        "{}: the pass drew with no pipeline at ({x}, {y})",
                        kind.label()
                    );
                }
            }
        }
    }

    #[test]
    fn a_custom_shader_loads_and_a_broken_one_does_not() {
        let Some((device, _queue)) = gpu() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let mut shader = CrtShader::new(&device, FORMAT);
        let dir = temp_dir("custom");

        let good = dir.join("good.wgsl");
        std::fs::write(&good, CRT_WGSL).expect("write");
        shader
            .load_custom(&device, FORMAT, &good)
            .expect("preset source must load as a custom shader");
        assert!(shader.custom.is_some());

        shader.clear_custom();
        assert!(shader.custom.is_none());

        shader
            .load_custom(&device, FORMAT, &good)
            .expect("reload after clear");
        let bad = dir.join("bad.wgsl");
        std::fs::write(&bad, "fn nope() -> i32 { return 0; }").expect("write");
        let err = shader
            .load_custom(&device, FORMAT, &bad)
            .expect_err("no entry points");
        assert!(err.contains("vs_main"), "{err}");
        // A failed load must not leave the previous shader selected.
        assert!(shader.custom.is_none());

        let missing = dir.join("does-not-exist.wgsl");
        let err = shader
            .load_custom(&device, FORMAT, &missing)
            .expect_err("missing file");
        assert!(err.contains("does-not-exist.wgsl"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

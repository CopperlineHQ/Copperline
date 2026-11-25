// SPDX-License-Identifier: GPL-3.0-or-later

//! egui's paint on the emulator window: the configuration screen, the menu
//! and the status bar, drawn over the passes on whichever thread presents
//! the frame -- the present worker as a rule, the main thread when the frame
//! carries something only it has.
//!
//! The main thread runs egui and tessellates; a frame's meshes and texture
//! updates travel to the GPU side in its [`EguiJob`]. Texture updates are
//! queued as a job arrives and applied inside the render callback, as the
//! frame's own upload is: a present the surface skips (a covered window)
//! runs no callback, and the updates wait for the next one rather than
//! being lost.
//!
//! egui draws into a layer of its own before the picture goes onto the
//! surface. The surface is sRGB, and a translucent fill drawn onto it
//! straight would blend in linear light, where the texture -- which every
//! one of these surfaces was drawn into before -- blends the encoded
//! values. The layer is [`LAYER_FORMAT`], whose values are the encoded
//! colours, so egui's blending there is the texture's; the layer is then
//! copied onto the surface through an sRGB view of itself, which hands the
//! same colours over unchanged.

use pixels::wgpu;

/// The layer's format: unorm, so egui blends the encoded colours.
pub(super) const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The surfaces egui draws, each from its own context: the display canvas
/// (configuration screen and menu) and the chrome band (status bar), which
/// a sub-rect layout places at a scale of its own.
pub(super) const SURFACES: usize = 2;

/// One surface's frame: its meshes, the texture updates they depend on,
/// and the surface they are laid out for. The meshes are shared, so a
/// surface that has not changed (the status bar, as a rule) sends the
/// last frame's again without copying them.
#[derive(Clone)]
pub(super) struct EguiSurfaceJob {
    pub(super) jobs: std::sync::Arc<Vec<egui::ClippedPrimitive>>,
    pub(super) textures: egui::TexturesDelta,
    pub(super) size_in_pixels: [u32; 2],
    pub(super) pixels_per_point: f32,
}

impl EguiSurfaceJob {
    fn screen(&self) -> egui_wgpu::ScreenDescriptor {
        egui_wgpu::ScreenDescriptor {
            size_in_pixels: self.size_in_pixels,
            pixels_per_point: self.pixels_per_point,
        }
    }
}

/// What a presented frame asks egui to paint. Boxed, so the frame's
/// `PresentJob` stays small and a frame with nothing on egui allocates
/// nothing for it.
#[derive(Default)]
pub(super) struct EguiJob {
    pub(super) surfaces: [Option<Box<EguiSurfaceJob>>; SURFACES],
}

impl EguiJob {
    pub(super) fn is_empty(&self) -> bool {
        self.surfaces.iter().all(Option::is_none)
    }
}

struct Layer {
    size: (u32, u32),
    unorm: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// The GPU side: a renderer per surface, the texture updates waiting to be
/// applied, the layer and the pass that copies it onto the surface.
pub(super) struct EguiPaint {
    renderers: [egui_wgpu::Renderer; SURFACES],
    pending: [egui::TexturesDelta; SURFACES],
    layer: Option<Layer>,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

const COMPOSITE_WGSL: &str = r#"
@group(0) @binding(0) var layer: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(layer, vec2<i32>(at.xy), 0);
}
"#;

impl EguiPaint {
    pub(super) fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let renderers = std::array::from_fn(|_| {
            egui_wgpu::Renderer::new(
                device,
                LAYER_FORMAT,
                egui_wgpu::RendererOptions {
                    dithering: false,
                    ..Default::default()
                },
            )
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("egui_layer"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui_layer"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui_layer"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui_layer"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    // egui leaves its colours premultiplied in the layer.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            renderers,
            pending: Default::default(),
            layer: None,
            pipeline,
            bind_group_layout,
        }
    }

    /// Take a frame's texture updates in, to be applied when it is drawn.
    pub(super) fn receive(&mut self, job: &EguiJob) {
        for (pending, surface) in self.pending.iter_mut().zip(&job.surfaces) {
            if let Some(surface) = surface {
                pending.append(surface.textures.clone());
            }
        }
    }

    fn layer(&mut self, device: &wgpu::Device, size: (u32, u32)) -> &Layer {
        if self.layer.as_ref().is_none_or(|layer| layer.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("egui_layer"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LAYER_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
            });
            let unorm = texture.create_view(&Default::default());
            // Read back through sRGB eyes: the surface encodes again on the
            // way out, so the encoded colours egui left arrive unchanged.
            let srgb = texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("egui_layer"),
                layout: &self.bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&srgb),
                }],
            });
            self.layer = Some(Layer {
                size,
                unorm,
                bind_group,
            });
        }
        self.layer.as_ref().expect("just made")
    }

    /// Apply the texture updates waiting from every job received: inside
    /// the render callback, before anything is drawn.
    pub(super) fn begin(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        for (renderer, pending) in self.renderers.iter_mut().zip(&mut self.pending) {
            for (id, delta) in std::mem::take(&mut pending.set) {
                renderer.update_texture(device, queue, id, &delta);
            }
        }
    }

    /// Draw `job` over `target`, `size` pixels, after the passes. A frame
    /// may draw it twice: into a screenshot's capture, then the surface.
    pub(super) fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        job: &EguiJob,
    ) {
        if job.is_empty() {
            return;
        }
        let unorm = self.layer(device, size).unorm.clone();
        let mut first = true;
        for (renderer, surface) in self.renderers.iter_mut().zip(&job.surfaces) {
            let Some(surface) = surface else {
                continue;
            };
            let screen = surface.screen();
            let commands = renderer.update_buffers(device, queue, encoder, &surface.jobs, &screen);
            queue.submit(commands);
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &unorm,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: if first {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.render(&mut pass.forget_lifetime(), &surface.jobs, &screen);
            first = false;
        }
        let layer = self.layer.as_ref().expect("made above");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui_layer_composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &layer.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Release the textures egui is done with, once the frame that drew
    /// with them is recorded.
    pub(super) fn end(&mut self) {
        for (renderer, pending) in self.renderers.iter_mut().zip(&mut self.pending) {
            for id in std::mem::take(&mut pending.free) {
                renderer.free_texture(&id);
            }
        }
    }
}

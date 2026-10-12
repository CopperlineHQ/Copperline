// SPDX-License-Identifier: GPL-3.0-or-later

//! Read back the window's GPU output at its current physical pixel size.
//! Allocated only for an interactive screenshot; scheduled captures remain
//! deterministic CPU captures that need no GPU or window.

use anyhow::{bail, Context, Result};
use pixels::wgpu;
use std::{path::Path, time::Duration};

pub(super) struct OutputCapture {
    target: wgpu::Texture,
    buffer: wgpu::Buffer,
    rect: (u32, u32, u32, u32),
    stride: u32,
    bgra: bool,
    copied: bool,
}

impl OutputCapture {
    pub(super) fn new(
        device: &wgpu::Device,
        surface: (u32, u32),
        rect: (u32, u32, u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        let (x, y, w, h) = rect;
        if w == 0
            || h == 0
            || u64::from(x) + u64::from(w) > u64::from(surface.0)
            || u64::from(y) + u64::from(h) > u64::from(surface.1)
        {
            bail!("no visible picture to capture");
        }
        let bgra = match format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
            _ => bail!("unsupported screenshot texture format: {format:?}"),
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("output_screenshot"),
            size: wgpu::Extent3d {
                width: surface.0,
                height: surface.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let stride = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output_screenshot_readback"),
            size: u64::from(stride) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Self {
            target,
            buffer,
            rect,
            stride,
            bgra,
            copied: false,
        })
    }

    pub(super) fn view(&self) -> wgpu::TextureView {
        self.target.create_view(&Default::default())
    }

    pub(super) fn copy(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let (x, y, w, h) = self.rect;
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.stride),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.copied = true;
    }

    fn read(&self, device: &wgpu::Device) -> Result<Vec<u32>> {
        if !self.copied {
            bail!("the window did not render a picture");
        }
        let timeout = Duration::from_secs(5);
        let (tx, rx) = std::sync::mpsc::channel();
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(timeout),
            })
            .context("waiting for screenshot GPU readback")?;
        rx.recv_timeout(timeout)
            .context("waiting for screenshot pixels")??;
        let mapped = self.buffer.slice(..).get_mapped_range();
        let (_, _, w, h) = self.rect;
        let mut pixels = Vec::with_capacity(w as usize * h as usize);
        for row in mapped.chunks_exact(self.stride as usize) {
            for pixel in row[..w as usize * 4].chunks_exact(4) {
                let mut rgba: [u8; 4] = pixel.try_into().unwrap();
                if self.bgra {
                    rgba.swap(0, 2);
                }
                pixels.push(u32::from_le_bytes(rgba));
            }
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(pixels)
    }

    pub(super) fn save(self, device: &wgpu::Device, path: &Path) -> Result<()> {
        let pixels = self.read(device)?;
        crate::screenshot::save(path, &pixels, self.rect.2, self.rect.3)
    }
}

#[cfg(test)]
mod tests {
    use super::super::scaler::{PresentScaler, ScaleFilter, ScalerDraw};
    use super::*;

    #[test]
    fn gpu_capture_preserves_picture_pixels_and_excludes_chrome() {
        let Some(gpu) = super::super::crt_shader::test_gpu("output_screenshot_test") else {
            return;
        };
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            let (device, queue) = (gpu.device(), gpu.queue());
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 2,
                    height: 2,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            // Red picture, green chrome. The crop has an unaligned row width
            // and starts away from the surface origin, like a letterboxed window.
            queue.write_texture(
                texture.as_image_copy(),
                &[
                    255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
                ],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(8),
                    rows_per_image: Some(2),
                },
                texture.size(),
            );
            let mut capture = OutputCapture::new(device, (20, 15), (3, 2, 13, 7), format).unwrap();
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut scaler = PresentScaler::new(device, format);
            scaler.render(
                device,
                queue,
                &texture,
                &mut encoder,
                &capture.view(),
                &[
                    ScalerDraw {
                        src: [0.0, 0.0, 1.0, 0.5],
                        dst: (3, 2, 13, 7),
                        filter: ScaleFilter::Nearest,
                        picture: None,
                    },
                    ScalerDraw {
                        src: [0.0, 0.5, 1.0, 0.5],
                        dst: (3, 9, 13, 3),
                        filter: ScaleFilter::Nearest,
                        picture: None,
                    },
                ],
            );
            capture.copy(&mut encoder);
            queue.submit(Some(encoder.finish()));
            assert_eq!(capture.read(device).unwrap(), vec![0xff0000ff; 13 * 7]);

            // Read the actual shader output, rather than the unchanged CPU
            // source, and keep the same crop and physical dimensions.
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut shader = super::super::crt_shader::CrtShader::new(device, format);
            let kind = crate::config::ShaderKind::Scanlines;
            let (uniforms, viewport) = super::super::crt_shader::uniforms_for_rect(
                kind,
                1.0,
                (3, 2, 13, 7),
                (0, 0, 2, 1),
                (2, 2),
                (2, 2),
                2.0,
            );
            shader.render(
                device,
                queue,
                &texture,
                &mut encoder,
                &capture.view(),
                viewport,
                kind,
                uniforms,
            );
            capture.copy(&mut encoder);
            queue.submit(Some(encoder.finish()));
            let shaded = capture.read(device).unwrap();
            assert_eq!(shaded.len(), 13 * 7);
            assert!(shaded.iter().any(|&pixel| pixel != 0xff0000ff));
            assert!(shaded.iter().any(|&pixel| pixel & 0xff > 0));
        }
    }
}

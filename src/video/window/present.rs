// SPDX-License-Identifier: GPL-3.0-or-later

//! Presentation path: render-worker output to presentation buffer, GPU
//! texture scaling, present-frame copies (window and TV aperture), the
//! TV overscan mask, recentring, and PNG capture. Split out of
//! `window.rs` for size; same module family, full access to the
//! parent's private items.

use super::*;
use crate::config::{Tint, TvCentre};

// The pure post-render helpers live in `video/present_common.rs` so headless
// consumers can present frames without the winit frontend; re-exported here so
// the rest of the window module family keeps its unqualified names.
pub(super) use crate::video::present_common::*;

#[derive(Clone, Copy, PartialEq, Eq)]
struct RepeatedPresentationSettings {
    h_shift: usize,
    overscan: Overscan,
    deinterlace: bool,
}

#[derive(Clone, Copy)]
struct RepeatedPresentationMetadata {
    present_rows: usize,
    present_width: usize,
    tv_aperture: TvApertureFrame,
    programmable: bool,
}

/// Exact previous-frame detector owned by the render worker. The bitplane
/// detector compares every pixel-affecting captured input; this wrapper also
/// covers the frontend presentation settings and retains the dimensions that
/// accompany a reused buffer.
#[derive(Default)]
pub(super) struct RepeatedPresentationCache {
    detector: bitplane::RepeatedFrameDetector,
    settings: Option<RepeatedPresentationSettings>,
    metadata: Option<RepeatedPresentationMetadata>,
}

impl RepeatedPresentationCache {
    fn can_reuse(
        &self,
        input: &bitplane::RenderInput,
        settings: RepeatedPresentationSettings,
        phosphor: f32,
    ) -> Option<RepeatedPresentationMetadata> {
        // Phosphor deliberately changes the presented pixels on every field
        // while its trail converges. Interlace is rejected by the bitplane
        // detector because the deinterlacer's opposite field is history.
        (phosphor == 0.0 && self.settings == Some(settings) && self.detector.can_reuse(input))
            .then_some(self.metadata)
            .flatten()
    }

    fn note_rendered(
        &mut self,
        input: &bitplane::RenderInput,
        result: &mut bitplane::RenderResult,
        settings: RepeatedPresentationSettings,
        phosphor: f32,
        metadata: RepeatedPresentationMetadata,
    ) {
        if phosphor == 0.0 {
            self.detector.note_rendered(input, result);
            self.settings = Some(settings);
            self.metadata = Some(metadata);
        } else {
            self.clear();
        }
    }

    fn should_track_read_dependencies(
        &self,
        input: &bitplane::RenderInput,
        settings: RepeatedPresentationSettings,
        phosphor: f32,
    ) -> bool {
        phosphor == 0.0
            && self.settings == Some(settings)
            && self.detector.should_track_read_dependencies(input)
    }

    pub(super) fn clear(&mut self) {
        self.detector.clear();
        self.settings = None;
        self.metadata = None;
    }
}

pub(super) fn render_job_to_presentation(
    job: RenderJob,
    fb: &mut [u32],
    deinterlacer: &mut Deinterlacer,
    repeated_frame_cache: &mut RepeatedPresentationCache,
) -> RenderWorkerResult {
    let RenderJob {
        generation,
        input,
        h_shift,
        overscan,
        deinterlace,
        phosphor,
        mut presentation_fb,
    } = job;
    deinterlacer.set_deinterlace(deinterlace);
    deinterlacer.set_phosphor(phosphor);
    let settings = RepeatedPresentationSettings {
        h_shift,
        overscan,
        deinterlace,
    };
    if let Some(metadata) = repeated_frame_cache.can_reuse(&input, settings, phosphor) {
        return RenderWorkerResult {
            generation,
            emulated_frame: input.emulated_frames(),
            timing: VideoRenderFrameTiming::default(),
            reused_previous: true,
            presentation_fb,
            present_rows: metadata.present_rows,
            present_width: metadata.present_width,
            tv_aperture: metadata.tv_aperture,
            programmable: metadata.programmable,
            input,
        };
    }

    let mut render_result =
        if repeated_frame_cache.should_track_read_dependencies(&input, settings, phosphor) {
            bitplane::render_from_input_tracking_reuse(&input, fb)
        } else {
            bitplane::render_from_input(&input, fb)
        };
    let geometry = input.geometry();
    let canvas_scale = input.canvas_scale();
    let canvas_width = FB_WIDTH * canvas_scale;
    let visible_start_vpos = input.visible_start_vpos();
    let field_rows = post_process_rendered_field(
        fb,
        geometry,
        canvas_scale,
        input.presentation_h_window(),
        input.presentation_v_window(),
        visible_start_vpos,
        h_shift,
        overscan,
    );
    let base = input.render_base();
    let (present_rows, present_width) = deinterlacer.present_field_into(
        fb,
        field_rows,
        canvas_width,
        base.bplcon0 & 0x0004 != 0,
        base.long_field,
        !geometry.programmable,
        &mut presentation_fb,
    );
    let metadata = RepeatedPresentationMetadata {
        present_rows,
        present_width,
        tv_aperture: standard_tv_aperture_frame(geometry, present_rows, &base),
        programmable: geometry.programmable,
    };
    repeated_frame_cache.note_rendered(&input, &mut render_result, settings, phosphor, metadata);
    RenderWorkerResult {
        generation,
        emulated_frame: input.emulated_frames(),
        timing: render_result.timing,
        reused_previous: false,
        presentation_fb,
        present_rows,
        present_width,
        tv_aperture: metadata.tv_aperture,
        programmable: metadata.programmable,
        input,
    }
}

pub(super) fn presentation_pixels_equal(
    current: &[u32],
    current_rows: usize,
    current_width: usize,
    next: &[u32],
    next_rows: usize,
    next_width: usize,
) -> bool {
    if current_rows != next_rows || current_width != next_width {
        return false;
    }
    let Some(active) = next_rows.checked_mul(next_width) else {
        return false;
    };
    match (current.get(..active), next.get(..active)) {
        (Some(current), Some(next)) => current == next,
        _ => false,
    }
}

pub(super) fn log_frame_dump_metadata(index: u32, emu: &Emulator) {
    let bus = emu.bus();
    let base = bus.frame_render_base();
    let (top, bottom, bottom_valid) = bus.frame_palette_split();
    let events = bus.frame_render_events();
    let captured_rows = bus
        .frame_captured_bitplane_rows()
        .iter()
        .filter(|row| row.is_some())
        .count();
    let mut cpu_colors = 0usize;
    let mut copper_colors = 0usize;
    let mut control_events = 0usize;
    for event in events {
        let off = event.offset & 0x01FE;
        if matches!(off, 0x180..=0x1BE) {
            match event.source {
                BeamWriteSource::Cpu | BeamWriteSource::CpuCopperIrq => cpu_colors += 1,
                BeamWriteSource::Copper => copper_colors += 1,
            }
        }
        if matches!(off, 0x100 | 0x102 | 0x104) {
            control_events += 1;
        }
    }
    info!(
        "frame-meta idx={} emu_frame={} emu_secs={:.3} pc={:#08X} beam=({}, {}) dmacon={:#06X} bplcon0={:#06X} bplcon1={:#06X} bplcon2={:#06X} bplpt={:06X?} sprpt={:06X?} sprctl={:04X?} bplmod=({},{}) ddf=({:#05X},{:#05X}) diw=({:#06X},{:#06X}) base_pal={:03X?} top_pal={:03X?} bottom_valid={} bottom_pal={:03X?} events={} cpu_colors={} copper_colors={} controls={} captured_rows={}",
        index,
        bus.emulated_frames(),
        bus.emulated_seconds(),
        emu.machine.pc(),
        bus.agnus.vpos,
        bus.agnus.hpos,
        base.dmacon,
        base.bplcon0,
        base.bplcon1,
        base.bplcon2,
        base.bplpt,
        base.sprpt,
        base.sprctl,
        base.bpl1mod,
        base.bpl2mod,
        base.ddfstrt,
        base.ddfstop,
        base.diwstrt,
        base.diwstop,
        &base.palette.hi_words()[..16],
        &top.hi_words()[..16],
        bottom_valid,
        &bottom.hi_words()[..16],
        events.len(),
        cpu_colors,
        copper_colors,
        control_events,
        captured_rows
    );
    if crate::envcfg::flag("COPPERLINE_DUMP_RENDER_META_VERBOSE") {
        let render_events: Vec<_> = events
            .iter()
            .map(|event| {
                (
                    event.vpos,
                    event.hpos,
                    event.offset & 0x01FE,
                    event.value,
                    event.source,
                )
            })
            .collect();
        info!(
            "frame-meta-events idx={} events={:03X?}",
            index, render_events
        );
        let color_events: Vec<_> = events
            .iter()
            .filter_map(|event| {
                let off = event.offset & 0x01FE;
                matches!(off, 0x180..=0x1BE).then_some((
                    event.vpos,
                    event.hpos,
                    off,
                    event.value & 0x0FFF,
                    event.source,
                ))
            })
            .collect();
        info!(
            "frame-meta-colors idx={} events={:03X?}",
            index, color_events
        );
        let setup_events: Vec<_> = events
            .iter()
            .filter_map(|event| {
                let off = event.offset & 0x01FE;
                matches!(
                    off,
                    0x08E | 0x090 | 0x092 | 0x094 | 0x100..=0x10A | 0x0E0..=0x0F7
                )
                .then_some((
                    event.vpos,
                    event.hpos,
                    off,
                    event.value,
                    event.source,
                ))
            })
            .collect();
        info!(
            "frame-meta-setup idx={} events={:03X?}",
            index, setup_events
        );
        let mut nonzero_ranges: [(Option<usize>, Option<usize>); 6] =
            std::array::from_fn(|_| (None, None));
        let mut row_shape_ranges: Vec<(usize, usize, usize, usize)> = Vec::new();
        for (y, row) in bus.frame_captured_bitplane_rows().iter().enumerate() {
            let Some(row) = row else {
                continue;
            };
            match row_shape_ranges.last_mut() {
                Some((_, end, nplanes, words_per_row))
                    if *end + 1 == y
                        && *nplanes == row.nplanes
                        && *words_per_row == row.words_per_row =>
                {
                    *end = y;
                }
                _ => row_shape_ranges.push((y, y, row.nplanes, row.words_per_row)),
            }
            for (plane, range) in nonzero_ranges.iter_mut().enumerate() {
                if row.planes[plane].iter().any(|word| *word != 0) {
                    if range.0.is_none() {
                        range.0 = Some(y);
                    }
                    range.1 = Some(y);
                }
            }
        }
        info!(
            "frame-meta-bitplanes idx={} nonzero_ranges={:?} row_shape_ranges={:?}",
            index, nonzero_ranges, row_shape_ranges
        );
        let bottom_events: Vec<_> = bus
            .frame_bottom_palette_events()
            .iter()
            .map(|event| {
                (
                    event.vpos,
                    event.hpos,
                    event.offset & 0x01FE,
                    event.value & 0x0FFF,
                    event.source,
                )
            })
            .collect();
        info!(
            "frame-meta-bottom-events idx={} events={:03X?}",
            index, bottom_events
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) struct Rect {
    pub(in crate::video) x: usize,
    pub(in crate::video) y: usize,
    pub(in crate::video) w: usize,
    pub(in crate::video) h: usize,
}

pub(super) fn texture_scale_for_window(window: &Window) -> usize {
    texture_scale_for_factor(window.scale_factor())
}

/// Integer supersample factor for the backing texture at a given host DPI
/// scale factor. The texture is rendered at this multiple of the logical
/// FB_WIDTH x window-height size so a 2x display stays crisp.
pub(super) fn texture_scale_for_factor(scale_factor: f64) -> usize {
    (scale_factor.round() as usize).clamp(1, MAX_TEXTURE_SCALE)
}

/// The presentation plan for a window: the supersample factor its backing
/// texture is rendered at, and the `pixels` scaling mode that puts the
/// texture on the surface. The pure form of [`plan_present_scaling`], with
/// the canvas size (in canvas pixels) passed in.
///
/// Smooth scaling supersamples by the rounded host DPI factor and fits with
/// filtering. Integer scaling instead takes the fit in whole *canvas*
/// pixels against the physical surface: the factor is the largest whole
/// number of physical pixels per canvas pixel that fits, the canvas is
/// rendered at that factor, and `ScalingMode::PixelPerfect` draws the
/// texture 1:1 and point-sampled. Deriving the texture from the fit rather
/// than the DPI is what makes every whole multiple reachable: a
/// DPI-supersampled texture can only be drawn at whole multiples of
/// *itself*, which on a 2x display skips every odd number of physical
/// pixels per canvas pixel -- a Retina laptop tall enough for a 3x picture
/// but not a 4x one would show no zoom at all. The factor is capped at
/// `MAX_INTEGER_TEXTURE_SCALE` to bound the texture and the per-frame
/// present copy; past the cap, PixelPerfect's own whole multiples of the
/// capped texture keep the fit integer (`floor` at that stage, so the
/// result never exceeds the surface).
///
/// A surface smaller than the canvas has no whole multiple to offer
/// (PixelPerfect floors its scale at 1 and would crop), so it falls back to
/// the smooth plan -- shrinking the picture beats cropping it.
pub(super) fn plan_present_scaling_for(
    integer_requested: bool,
    scale_factor: f64,
    surface: (u32, u32),
    canvas: (u32, u32),
) -> (usize, ScalingMode) {
    if integer_requested && canvas.0 > 0 && canvas.1 > 0 {
        let fit = (surface.0 / canvas.0).min(surface.1 / canvas.1) as usize;
        if fit >= 1 {
            return (
                fit.min(MAX_INTEGER_TEXTURE_SCALE),
                ScalingMode::PixelPerfect,
            );
        }
    }
    (texture_scale_for_factor(scale_factor), ScalingMode::Fill)
}

/// [`plan_present_scaling_for`] against the live canvas: `FB_WIDTH` by the
/// window height for the active pixel aspect and status-bar state.
pub(super) fn plan_present_scaling(
    integer_requested: bool,
    scale_factor: f64,
    surface: (u32, u32),
) -> (usize, ScalingMode) {
    plan_present_scaling_for(
        integer_requested,
        scale_factor,
        surface,
        (FB_WIDTH as u32, window_present_height() as u32),
    )
}

/// Whether the emulator window presents at whole-number scale
/// (`[display] scaling = "integer"`, or the menu's Scaling item). Only the
/// machine's own display follows it; the tool windows are always fitted.
pub(super) fn integer_scaling_requested() -> bool {
    crate::video::display_scaling() == crate::config::DisplayScaling::Integer
}

/// Re-plan the emulator window's presentation for the given surface size
/// (physical pixels), its live canvas and the scaling setting: apply the
/// scaling mode and, when the planned supersample factor or the canvas
/// underneath it changed, resize the backing texture to match.
///
/// `set_scaling_mode` only stores the mode: the scaling matrix and the clip
/// rect are recomputed by the next `resize_surface`/`resize_buffer`, so a
/// caller whose own resize is not the next thing to run must re-apply the
/// current surface size itself.
///
/// On `Ok` the texture and `r.texture_scale` agree with the plan; on `Err`
/// both keep their old extent, like `resize_buffer` (the stored mode is
/// re-decided by the next call, and the matrix never saw it).
pub(super) fn sync_main_present_scaling(
    r: &mut Render,
    surface: (u32, u32),
) -> std::result::Result<(), pixels::TextureError> {
    let (scale, mode) = plan_present_scaling(
        integer_scaling_requested(),
        r.window.scale_factor(),
        (surface.0.max(1), surface.1.max(1)),
    );
    r.pixels.set_scaling_mode(mode);
    let want = (texture_width(scale) as u32, texture_height(scale) as u32);
    let have = r.pixels.context().texture_extent;
    if (have.width, have.height) != want {
        r.pixels.resize_buffer(want.0, want.1)?;
    }
    r.texture_scale = scale;
    Ok(())
}

/// React to a host DPI scale-factor change for a *tool* window's pixel
/// surface (the emulator window re-plans through
/// `sync_main_present_scaling`, whose supersample factor is not the DPI's
/// under integer scaling).
///
/// `cursor_texture_position` maps a host click into texture space using
/// both the surface size (which the following Resized event updates) and the
/// texture extent (which nothing updated before this). When the supersample
/// factor changes -- e.g. dragging between a 1x and a 2x monitor -- the texture
/// must be rebuilt to the new size, otherwise the two halves of the mapping
/// disagree and clicks land in the wrong region. The rebuild reallocates a GPU
/// texture, so it is skipped when the rounded factor is unchanged (a slow drag
/// across a fractional-scale monitor seam can emit many events); the surface
/// itself is re-synced by the Resized event that always follows.
pub(super) fn resync_render_scale(
    pixels: &mut Pixels<'static>,
    texture_scale: &mut usize,
    scale_factor: f64,
) {
    let new_scale = texture_scale_for_factor(scale_factor);
    if new_scale == *texture_scale {
        return;
    }
    match pixels.resize_buffer(
        texture_width(new_scale) as u32,
        texture_height(new_scale) as u32,
    ) {
        Ok(()) => *texture_scale = new_scale,
        Err(e) => warn!("resize texture buffer for scale {scale_factor} failed: {e}"),
    }
}

/// The size a redraw has to apply to the presentation surface before it draws,
/// or `None` when the surface already matches the host window and the frame can
/// go out as it stands.
///
/// A resize that has not reached the app as a Resized event yet leaves `pixels`
/// configured for the old size, and it rebuilds the swapchain from that stored
/// size alone, in a retry loop with no bound -- see `App::resync_surface_size`
/// for what that costs. A 0x0 window is reported like any other mismatch, so
/// the caller's minimized guard gets to see it.
pub(super) fn surface_resize_for_draw(
    configured: (u32, u32),
    inner: PhysicalSize<u32>,
) -> Option<PhysicalSize<u32>> {
    ((inner.width, inner.height) != configured).then_some(inner)
}

pub(super) fn build_pixels_for_window(
    window: Arc<Window>,
    texture_scale: usize,
    vsync: bool,
    scaling_mode: ScalingMode,
) -> std::result::Result<Pixels<'static>, pixels::Error> {
    let inner = window.inner_size();
    let surface = (inner.width.max(1), inner.height.max(1));
    let texture = (
        texture_width(texture_scale) as u32,
        texture_height(texture_scale) as u32,
    );
    let surface_texture = SurfaceTexture::new(surface.0, surface.1, window);
    let builder = PixelsBuilder::new(texture.0, texture.1, surface_texture).enable_vsync(vsync);
    let builder = if cfg!(target_os = "linux") {
        builder.wgpu_backend(
            pixels::wgpu::Backends::from_env().unwrap_or(pixels::wgpu::Backends::VULKAN),
        )
    } else {
        builder
    };
    let mut pixels = builder.build()?;
    pixels.set_scaling_mode(scaling_mode);
    // The scaling matrix and clip rect stay the builder's PixelPerfect ones
    // until a resize recomputes them. Re-apply the surface size so the cursor
    // mapping and the render scissor agree with the mode just set from the
    // first frame, not the first resize.
    pixels.resize_surface(surface.0, surface.1)?;
    Ok(pixels)
}

pub(in crate::video) fn texture_width(scale: usize) -> usize {
    FB_WIDTH * scale
}

pub(in crate::video) fn texture_height(scale: usize) -> usize {
    window_present_height() * scale
}

pub(in crate::video) fn scale_rect(rect: Rect, scale: usize) -> Rect {
    Rect {
        x: rect.x * scale,
        y: rect.y * scale,
        w: rect.w * scale,
        h: rect.h * scale,
    }
}

/// Map a host cursor position (surface physical pixels) into a logical canvas
/// position, or None outside the presented picture.
///
/// Deliberately not pixels' `window_pos_to_pixel`: that helper re-centres
/// through `min(texture, surface) / 2`, which is only correct while the
/// texture fits inside the surface. The supersampled texture is *larger* than
/// the surface whenever the rounded texture scale exceeds a fractional host
/// scale factor (a 2x texture over a 1.5x surface on a 150% desktop), and the
/// shifted mapping it produces there lands every status-bar click in the
/// display region, where it takes the mouse capture instead of the control.
/// Mapping through the scaling renderer's clip rect -- the surface rect the
/// Fill pass draws the picture into -- holds on both sides of that boundary,
/// and agrees with the render by construction: the rect derives from the same
/// surface and texture extents the render pass scissors with.
pub(super) fn cursor_texture_position(
    pixels: &Pixels<'_>,
    position: winit::dpi::PhysicalPosition<f64>,
    texture_scale: usize,
) -> Option<(i32, i32)> {
    let context = pixels.context();
    let (x, y) = cursor_position_in_texture(
        (position.x, position.y),
        context.scaling_renderer.clip_rect(),
        (context.texture_extent.width, context.texture_extent.height),
    )?;
    Some(((x / texture_scale) as i32, (y / texture_scale) as i32))
}

/// The pure half of [`cursor_texture_position`]: position and clip rect in
/// surface physical pixels to supersampled-texture pixels.
pub(super) fn cursor_position_in_texture(
    position: (f64, f64),
    clip: (u32, u32, u32, u32),
    texture: (u32, u32),
) -> Option<(usize, usize)> {
    let (clip_x, clip_y, clip_w, clip_h) = clip;
    if clip_w == 0 || clip_h == 0 {
        return None;
    }
    let x = (position.0 - f64::from(clip_x)) * f64::from(texture.0) / f64::from(clip_w);
    let y = (position.1 - f64::from(clip_y)) * f64::from(texture.1) / f64::from(clip_h);
    if x < 0.0 || x >= f64::from(texture.0) || y < 0.0 || y >= f64::from(texture.1) {
        return None;
    }
    Some((x as usize, y as usize))
}

pub(super) fn cursor_in_status_bar(pos: (i32, i32)) -> bool {
    pos.1 >= present_height() as i32 && pos.1 < window_present_height() as i32
}

pub(super) fn cursor_in_display(pos: (i32, i32)) -> bool {
    pos.0 >= 0 && pos.0 < FB_WIDTH as i32 && pos.1 >= 0 && pos.1 < present_height() as i32
}

pub(super) fn volume_percent_from_pos(pos: (i32, i32)) -> u8 {
    let track = volume_slider_track_rect();
    let x = pos.0.clamp(track.x as i32, (track.x + track.w - 1) as i32) as usize;
    let range = track.w.saturating_sub(1).max(1);
    (((x - track.x) * 100 + range / 2) / range) as u8
}

pub(super) fn volume_scroll_steps(delta: MouseScrollDelta) -> Option<i16> {
    let amount = match delta {
        MouseScrollDelta::LineDelta(_, y) => f64::from(y),
        MouseScrollDelta::PixelDelta(pos) => pos.y,
    };
    if amount > 0.0 {
        Some(1)
    } else if amount < 0.0 {
        Some(-1)
    } else {
        None
    }
}

pub(super) fn owner_name_from_code(code: u8) -> &'static str {
    match code {
        b'R' => "refresh",
        b'B' => "bitplane",
        b'S' => "sprite",
        b'D' => "disk",
        b'A' => "audio",
        b'C' => "copper",
        b'L' => "blitter",
        b'P' => "cpu",
        _ => "idle",
    }
}

pub(super) fn copy_present_frame(
    src_fb: &[u32],
    src_rows: usize,
    src_width: usize,
    frame: &mut [u8],
    texture_scale: usize,
) {
    debug_assert!(src_fb.len() >= src_rows * src_width);
    debug_assert_eq!(
        frame.len(),
        texture_width(texture_scale) * texture_height(texture_scale) * 4
    );
    let dst_stride_px = texture_width(texture_scale);
    let dst_stride = dst_stride_px * 4;
    let out_rows = present_height() * texture_scale;
    // The 570 woven scanlines map onto the 537-row 4:3 presentation (times
    // the HiDPI texture scale). Select whole source rows instead of blending
    // adjacent Amiga scanlines; normal presentation should not synthesize
    // intermediate colours from line-to-line dithering.
    for y in 0..out_rows {
        let src_y = screenshot::scaled_source_row(y, src_rows, out_rows);
        let row = &src_fb[src_y * src_width..(src_y + 1) * src_width];

        let dst_off = y * dst_stride;
        if src_width == dst_stride_px {
            // A 35 ns canvas whose width matches the HiDPI texture row
            // (the common Retina case): every canvas pixel is one texture
            // pixel, no resampling.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr() as *const u8,
                    frame.as_mut_ptr().add(dst_off),
                    src_width * 4,
                );
            }
            continue;
        }
        if src_width != FB_WIDTH {
            // Generic canvas-to-texture width map (nearest sample).
            let dst = &mut frame[dst_off..dst_off + dst_stride];
            for x in 0..dst_stride_px {
                let src_x = x * src_width / dst_stride_px;
                dst[x * 4..x * 4 + 4].copy_from_slice(&row[src_x].to_le_bytes());
            }
            continue;
        }
        match texture_scale {
            1 => unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr() as *const u8,
                    frame.as_mut_ptr().add(dst_off),
                    FB_WIDTH * 4,
                );
            },
            2 => {
                for (x, &pixel) in row.iter().enumerate() {
                    let pair = pixel as u64 | ((pixel as u64) << 32);
                    unsafe {
                        (frame.as_mut_ptr().add(dst_off + x * 8) as *mut u64).write_unaligned(pair);
                    }
                }
            }
            _ => {
                let dst = &mut frame[dst_off..dst_off + dst_stride];
                for x in 0..FB_WIDTH * texture_scale {
                    dst[x * 4..x * 4 + 4].copy_from_slice(&row[x / texture_scale].to_le_bytes());
                }
            }
        }
    }
}

pub(super) fn copy_window_present_frame(
    src_fb: &[u32],
    src_rows: usize,
    src_width: usize,
    frame: &mut [u8],
    texture_scale: usize,
    overscan: Overscan,
    tv_centre: TvCentre,
    tv_aperture_rows: Option<usize>,
    tube_glass: bool,
) {
    // The TV aperture is a standard-scan crop; standard scans always render
    // the classic canvas width. With a monitor bezel drawn the crop widens
    // to the tube aperture: the whole rendered field of the same standard,
    // which the bezel shaders underscan inside the tube face.
    match tv_aperture_rows {
        Some(aperture_rows) if overscan == Overscan::Tv && src_width == FB_WIDTH => {
            let (source_y, aperture_rows) = if tube_glass {
                (0, tube_aperture_rows(aperture_rows))
            } else {
                (TV_PRESENT_SOURCE_Y, aperture_rows)
            };
            copy_tv_aperture_to_window(
                src_fb,
                src_rows,
                frame,
                texture_scale,
                aperture_rows,
                present_height(),
                source_y,
                tv_centre_source_offset(tv_centre),
            )
        }
        _ => copy_present_frame(src_fb, src_rows, src_width, frame, texture_scale),
    }
}

/// The live-window TV copy presents the captured aperture
/// (`TV_CAPTURED_*`): the standard window plus the symmetric overscan
/// margin the framebuffer actually captures, ending exactly on the
/// framebuffer's edges. On the 4:3 canvas -- the glass itself -- the
/// aperture's columns resample onto the full texture width
/// (`tv_glass_sample`), the way a real set's raster fills its glass:
/// every glass pixel derives from real captured pixels and nothing is
/// padded black. The square-pixel canvas keeps unit columns instead, so
/// there the aperture sits centred between off-capture black side pads,
/// the horizontal counterpart of its vertical bands; the pads stay black
/// rather than replicating the edge columns, which carry picture when a
/// display fetches or parks sprites in the deepest overscan (the Gen-X
/// logo slide-in streaks).
///
/// `source_y` is the woven row the crop starts at: `TV_PRESENT_SOURCE_Y`
/// for the TV aperture, 0 for the tube aperture (whose row count spans
/// the whole rendered field). `source_offset` slides the whole crop
/// window across the capture (the H/V-centre controls,
/// `tv_centre_source_offset`); glass pushed past the captured raster is
/// unscanned and stays black.
#[allow(clippy::too_many_arguments)]
pub(super) fn copy_tv_aperture_to_window(
    src_fb: &[u32],
    src_rows: usize,
    frame: &mut [u8],
    texture_scale: usize,
    aperture_rows: usize,
    present_rows: usize,
    source_y: usize,
    source_offset: (i32, i32),
) {
    debug_assert!(src_fb.len() >= src_rows * FB_WIDTH);
    let (source_x_offset, source_y_offset) = source_offset;
    let dst_stride = texture_width(texture_scale) * 4;
    let out_rows = present_rows * texture_scale;
    debug_assert!(frame.len() >= dst_stride * out_rows);
    let black_px = rgba(0, 0, 0);
    let black = black_px.to_le_bytes();
    let square = present_rows == crate::video::PRESENT_HEIGHT_SQUARE;
    let pixel_at = |row: &[u32], out_x: usize| -> u32 {
        if square {
            if (TV_LIVE_PAD_X..TV_LIVE_PAD_X + TV_CAPTURED_WIDTH).contains(&out_x) {
                let src_x =
                    TV_CAPTURED_SOURCE_X as i32 + source_x_offset + (out_x - TV_LIVE_PAD_X) as i32;
                if (0..FB_WIDTH as i32).contains(&src_x) {
                    row[src_x as usize]
                } else {
                    black_px
                }
            } else {
                black_px
            }
        } else {
            tv_glass_sample(row, out_x, source_x_offset)
        }
    };
    for y in 0..out_rows {
        let src_y = tv_aperture_source_row(y, present_rows, texture_scale, aperture_rows)
            .map(|crop_y| (source_y + crop_y).min(src_rows - 1) as i32 + source_y_offset)
            .filter(|src_y| (0..src_rows as i32).contains(src_y));
        let Some(src_y) = src_y else {
            let dst = &mut frame[y * dst_stride..(y + 1) * dst_stride];
            for px in dst.chunks_exact_mut(4) {
                px.copy_from_slice(&black);
            }
            continue;
        };
        let src_y = src_y as usize;
        let row = &src_fb[src_y * FB_WIDTH..(src_y + 1) * FB_WIDTH];
        let dst_off = y * dst_stride;
        match texture_scale {
            1 => {
                let dst = &mut frame[dst_off..dst_off + dst_stride];
                for x in 0..FB_WIDTH {
                    let pixel = pixel_at(row, x);
                    dst[x * 4..x * 4 + 4].copy_from_slice(&pixel.to_le_bytes());
                }
            }
            2 => {
                for x in 0..FB_WIDTH {
                    let pixel = pixel_at(row, x);
                    let pair = pixel as u64 | ((pixel as u64) << 32);
                    unsafe {
                        (frame.as_mut_ptr().add(dst_off + x * 8) as *mut u64).write_unaligned(pair);
                    }
                }
            }
            _ => {
                let dst = &mut frame[dst_off..dst_off + dst_stride];
                for x in 0..FB_WIDTH * texture_scale {
                    let pixel = pixel_at(row, x / texture_scale);
                    dst[x * 4..x * 4 + 4].copy_from_slice(&pixel.to_le_bytes());
                }
            }
        }
    }
}

/// Map an output texture row to a row of the `aperture_rows`-line TV
/// aperture crop, or None for rows that fall on the black bezel of the
/// square-pixel presentation. The square canvas (570 rows) maps woven rows
/// 1:1, so an aperture shorter than it is centred between black bands --
/// the vertical counterpart of the black TV_LIVE_PAD_X side pads. The 4:3
/// canvas (537 rows) is the glass itself, which both standards' apertures
/// fill: all aperture rows rescale onto the whole output (540 onto 537 for
/// a 50 Hz scan, 428 for a 60 Hz one).
pub(super) fn tv_aperture_source_row(
    y: usize,
    present_rows: usize,
    texture_scale: usize,
    aperture_rows: usize,
) -> Option<usize> {
    let out_rows = present_rows * texture_scale;
    let pad_rows = if present_rows == crate::video::PRESENT_HEIGHT_SQUARE {
        present_rows.saturating_sub(aperture_rows) / 2 * texture_scale
    } else {
        0
    };
    let content_rows = out_rows - 2 * pad_rows;
    if y < pad_rows || y >= pad_rows + content_rows {
        return None;
    }
    Some(screenshot::scaled_source_row(
        y - pad_rows,
        aperture_rows,
        content_rows,
    ))
}

/// Paint a TV-style test pattern into the presentation framebuffer,
/// shown while the machine is powered off. SMPTE-style colour bars over
/// a grayscale step wedge: instantly readable as "no signal", and handy
/// for setting up video capture levels before the machine boots.
///
/// The layout is calibrated to the TV glass -- the captured aperture the
/// presentation shows -- so the bars, wedge and logo sit centred on
/// screen; the outermost bars and steps extend to the capture edges the
/// way a signal generator fills the active line around its calibrated
/// area, so the full-overscan view shows the same card with slightly
/// wider outer bars.
pub(super) fn paint_test_screen(fb: &mut [u32]) {
    debug_assert!(fb.len() >= FB_PIXELS);
    const BARS: [u32; 7] = [
        rgba(192, 192, 192), // grey
        rgba(192, 192, 0),   // yellow
        rgba(0, 192, 192),   // cyan
        rgba(0, 192, 0),     // green
        rgba(192, 0, 192),   // magenta
        rgba(192, 0, 0),     // red
        rgba(0, 0, 192),     // blue
    ];
    const STEPS: usize = 8;
    // The glass box in field coordinates: the captured aperture's
    // columns, and its woven row window halved back to field rows.
    let x0 = TV_CAPTURED_SOURCE_X;
    let glass_w = TV_CAPTURED_WIDTH;
    let glass_top = TV_PRESENT_SOURCE_Y / 2;
    let glass_h = TV_PAL_PRESENT_HEIGHT / 2;
    let bars_h = glass_top + glass_h * 4 / 5;
    for y in 0..FB_HEIGHT {
        let row = &mut fb[y * FB_WIDTH..(y + 1) * FB_WIDTH];
        if y < bars_h {
            for (x, px) in row.iter_mut().enumerate() {
                let xa = x.clamp(x0, x0 + glass_w - 1) - x0;
                *px = BARS[xa * BARS.len() / glass_w];
            }
        } else {
            for (x, px) in row.iter_mut().enumerate() {
                let xa = x.clamp(x0, x0 + glass_w - 1) - x0;
                let level = (xa * STEPS / glass_w) as u32 * 255 / (STEPS as u32 - 1);
                *px = rgba(level, level, level);
            }
        }
    }
    draw_test_screen_logo(fb, glass_top, bars_h);
}

pub(super) fn draw_test_screen_logo(fb: &mut [u32], glass_top: usize, bars_h: usize) {
    let Some(image) = copperline_logo_image() else {
        return;
    };
    // Centred on the glass, not the capture: the aperture the
    // presentation shows starts TV_CAPTURED_SOURCE_X columns in and
    // TV_PRESENT_SOURCE_Y woven rows down.
    let x = TV_CAPTURED_SOURCE_X + TV_CAPTURED_WIDTH.saturating_sub(image.width) / 2;
    let y = glass_top + (bars_h - glass_top).saturating_sub(image.height) / 2;
    alpha_blit_rgba(fb, FB_WIDTH, FB_HEIGHT, x, y, image);
}

pub(super) fn alpha_blit_rgba(
    dst: &mut [u32],
    dst_w: usize,
    dst_h: usize,
    x0: usize,
    y0: usize,
    src: &EmbeddedRgbaImage,
) {
    for sy in 0..src.height {
        let dy = y0 + sy;
        if dy >= dst_h {
            break;
        }
        for sx in 0..src.width {
            let dx = x0 + sx;
            if dx >= dst_w {
                break;
            }
            let src_off = (sy * src.width + sx) * 4;
            let sr = src.rgba[src_off] as u32;
            let sg = src.rgba[src_off + 1] as u32;
            let sb = src.rgba[src_off + 2] as u32;
            let sa = src.rgba[src_off + 3] as u32;
            if sa == 0 {
                continue;
            }
            let dst_px = &mut dst[dy * dst_w + dx];
            *dst_px = if sa == 0xFF {
                rgba(sr, sg, sb)
            } else {
                blend_rgba_over_opaque(*dst_px, sr, sg, sb, sa)
            };
        }
    }
}

// --- screen tint ---------------------------------------------------------
//
// The [display] tint knob: a monochrome-monitor look over the window's
// picture, matching the web front-end's screen filter. Presentation only,
// like the CRT shader pass: it runs on the composited window frame after
// the display copy, so screenshots, frame dumps, recordings and headless
// runs stay untinted, and the status bar and UI overlays (drawn after it)
// stay in colour.
//
// Every tint is a luma ramp: the pixel collapses to its luminance, which
// indexes a 256-entry table of pre-tinted colours. The ramps reproduce the
// web frontend's CSS filter chains (Filter Effects Module Level 1
// colour matrices, results clamped to [0, 1] between stages), evaluated on
// the grey axis once at build time, so the desktop and browser tints match
// on grey input (the browser's sepia chain has no leading grayscale, so
// saturated pixels can differ slightly there).

/// Rec. 709-style luma weights in 8.8 fixed point, summing to exactly 256
/// so a grey pixel maps to its own level and the b/w ramp is an identity.
const LUMA_R: u32 = 54;
const LUMA_G: u32 = 183;
const LUMA_B: u32 = 19;

fn clamp01(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| v.clamp(0.0, 1.0))
}

fn mat_mul(m: [[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    clamp01([
        m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
        m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
        m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
    ])
}

/// CSS `sepia(1)`.
fn sepia(c: [f32; 3]) -> [f32; 3] {
    mat_mul(
        [
            [0.393, 0.769, 0.189],
            [0.349, 0.686, 0.168],
            [0.272, 0.534, 0.131],
        ],
        c,
    )
}

/// CSS `saturate(s)`.
fn saturate(c: [f32; 3], s: f32) -> [f32; 3] {
    mat_mul(
        [
            [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
            [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
            [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
        ],
        c,
    )
}

/// CSS `hue-rotate(deg)`.
fn hue_rotate(c: [f32; 3], degrees: f32) -> [f32; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    mat_mul(
        [
            [
                0.213 + cos * 0.787 - sin * 0.213,
                0.715 - cos * 0.715 - sin * 0.715,
                0.072 - cos * 0.072 + sin * 0.928,
            ],
            [
                0.213 - cos * 0.213 + sin * 0.143,
                0.715 + cos * 0.285 + sin * 0.140,
                0.072 - cos * 0.072 - sin * 0.283,
            ],
            [
                0.213 - cos * 0.213 - sin * 0.787,
                0.715 - cos * 0.715 + sin * 0.715,
                0.072 + cos * 0.928 + sin * 0.072,
            ],
        ],
        c,
    )
}

/// CSS `brightness(b)`.
fn brightness(c: [f32; 3], b: f32) -> [f32; 3] {
    clamp01(c.map(|v| v * b))
}

/// The tinted colour one grey level maps to: the web frontend's CSS
/// filter chain for the tint, evaluated on grey. For most tints the
/// chain's leading `grayscale(1)` is the luma collapse the per-pixel
/// step performs; the sepia chain has no grayscale term, so the browser
/// feeds it colour where this path feeds it luma.
fn tint_ramp(tint: Tint, level: f32) -> [f32; 3] {
    let grey = [level, level, level];
    match tint {
        Tint::None | Tint::Bw => grey,
        Tint::Green => brightness(hue_rotate(saturate(sepia(grey), 4.0), 80.0), 0.92),
        Tint::Amber => hue_rotate(saturate(sepia(grey), 4.0), -8.0),
        Tint::Sepia => sepia(grey),
    }
}

/// Build the luma-indexed colour table for a tint, or `None` for
/// [`Tint::None`] so the untinted path costs nothing per frame.
pub(super) fn tint_lut(tint: Tint) -> Option<Box<[u32; 256]>> {
    if tint == Tint::None {
        return None;
    }
    let mut lut = Box::new([0u32; 256]);
    for (level, out) in lut.iter_mut().enumerate() {
        let [r, g, b] = tint_ramp(tint, level as f32 / 255.0);
        *out = rgba(
            (r * 255.0 + 0.5) as u32,
            (g * 255.0 + 0.5) as u32,
            (b * 255.0 + 0.5) as u32,
        );
    }
    Some(lut)
}

/// Tint a run of RGBA pixels in place through a [`tint_lut`] table.
pub(super) fn tint_rows_in_place(px: &mut [u8], lut: &[u32; 256]) {
    for p in px.chunks_exact_mut(4) {
        let luma =
            (LUMA_R * u32::from(p[0]) + LUMA_G * u32::from(p[1]) + LUMA_B * u32::from(p[2])) >> 8;
        let tinted = lut[luma as usize].to_le_bytes();
        p[0] = tinted[0];
        p[1] = tinted[1];
        p[2] = tinted[2];
    }
}

/// Tint the display region of the composited window frame in place. Runs
/// between the display copy and the status-bar/UI drawing, so only the
/// emulated picture is tinted.
pub(super) fn tint_display_rows(frame: &mut [u8], texture_scale: usize, lut: &[u32; 256]) {
    let rows = present_height() * texture_scale;
    let stride = texture_width(texture_scale) * 4;
    tint_rows_in_place(&mut frame[..rows * stride], lut);
}

pub(super) fn blend_rgba_over_opaque(dst: u32, sr: u32, sg: u32, sb: u32, sa: u32) -> u32 {
    let [dr, dg, db, _] = dst.to_le_bytes();
    let inv = 0xFF - sa;
    let r = (sr * sa + u32::from(dr) * inv + 127) / 0xFF;
    let g = (sg * sa + u32::from(dg) * inv + 127) / 0xFF;
    let b = (sb * sa + u32::from(db) * inv + 127) / 0xFF;
    rgba(r, g, b)
}

/// Whether horizontal recentring is enabled. On unless COPPERLINE_HCENTER is
/// set to a falsey value (0/false/off/no), so full-overscan presentation can
/// show the standard display exactly as rendered when debugging alignment.
pub(super) fn hcenter_enabled() -> bool {
    match crate::envcfg::var("COPPERLINE_HCENTER") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

pub(super) fn threaded_render_enabled() -> bool {
    match crate::envcfg::var("COPPERLINE_THREADED_RENDER") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

pub(super) fn status_with_latched_fdd_track(
    status: FrontPanelStatus,
    last_fdd_track: &mut Option<u8>,
) -> FrontPanelStatus {
    let fdd_track = match status.fdd_track {
        Some(track) => {
            *last_fdd_track = Some(track);
            Some(track)
        }
        None => *last_fdd_track,
    };
    FrontPanelStatus {
        fdd_track,
        ..status
    }
}

pub(super) fn take_integral_mouse_delta(value: &mut f64) -> i32 {
    let whole = value.trunc();
    if whole > i32::MAX as f64 {
        *value = 0.0;
        i32::MAX
    } else if whole < i32::MIN as f64 {
        *value = 0.0;
        i32::MIN
    } else {
        *value -= whole;
        whole as i32
    }
}

pub(super) fn save_present_frame(
    path: &std::path::Path,
    present_fb: &[u32],
    src_rows: usize,
    src_width: usize,
    overscan: Overscan,
    tv_centre: TvCentre,
    tv_aperture_rows: Option<usize>,
) -> anyhow::Result<()> {
    if crate::envcfg::flag("COPPERLINE_SHOT_RAW") {
        return screenshot::save(
            path,
            &present_fb[..src_rows * src_width],
            src_width as u32,
            src_rows as u32,
        );
    }

    if let Some(aperture_rows) = tv_aperture_rows {
        if overscan == Overscan::Tv && src_width == FB_WIDTH {
            // Both standards' apertures fill the same 4:3 glass, so the
            // saved picture keeps one shape: the captured aperture's
            // columns resample onto the glass width, exactly like the
            // live window (`tv_glass_sample`), and a 60 Hz crop's rows
            // scale onto the 50 Hz aperture's native row count. The
            // H/V-centre nudge follows the knob into captures too; glass
            // it exposes past the captured raster saves black, exactly
            // as the window shows it.
            let (source_x_offset, source_y_offset) = tv_centre_source_offset(tv_centre);
            let black = rgba(0, 0, 0);
            let mut glass = vec![0u32; FB_WIDTH * TV_GLASS_PRESENT_ROWS];
            for out_y in 0..TV_GLASS_PRESENT_ROWS {
                let crop_y =
                    screenshot::scaled_source_row(out_y, aperture_rows, TV_GLASS_PRESENT_ROWS);
                let src_y = (TV_PRESENT_SOURCE_Y + crop_y).min(src_rows.saturating_sub(1)) as i32
                    + source_y_offset;
                let dst = &mut glass[out_y * FB_WIDTH..(out_y + 1) * FB_WIDTH];
                if !(0..src_rows as i32).contains(&src_y) {
                    dst.fill(black);
                    continue;
                }
                let src_y = src_y as usize;
                let row = &present_fb[src_y * FB_WIDTH..(src_y + 1) * FB_WIDTH];
                for (out_x, px) in dst.iter_mut().enumerate() {
                    *px = tv_glass_sample(row, out_x, source_x_offset);
                }
            }
            return screenshot::save(path, &glass, FB_WIDTH as u32, TV_GLASS_PRESENT_ROWS as u32);
        }
    }

    // A double-width (35 ns) canvas saves at double height too, keeping the
    // 4:3 glass shape at the higher resolution.
    let out_rows = present_height() * src_width / FB_WIDTH;
    screenshot::save_scaled_y(
        path,
        present_fb,
        src_width as u32,
        src_rows as u32,
        out_rows as u32,
    )
}

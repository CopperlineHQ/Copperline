// SPDX-License-Identifier: GPL-3.0-or-later

//! Presentation path: render-worker output to presentation buffer, GPU
//! texture scaling, present-frame copies (window and TV aperture), the
//! TV overscan mask, recentring, and PNG capture. Split out of
//! `window.rs` for size; same module family, full access to the
//! parent's private items.

use super::*;

// The pure post-render helpers live in `video/present_common.rs` so headless
// consumers can present frames without the winit frontend; re-exported here so
// the rest of the window module family keeps its unqualified names.
pub(super) use crate::video::present_common::*;

pub(super) fn render_job_to_presentation(
    job: RenderJob,
    fb: &mut [u32],
    deinterlacer: &mut Deinterlacer,
) -> RenderWorkerResult {
    let RenderJob {
        generation,
        input,
        h_shift,
        overscan,
        mut presentation_fb,
    } = job;
    let render_result = bitplane::render_from_input(&input, fb);
    let geometry = input.geometry();
    let visible_start_vpos = input.visible_start_vpos();
    let field_rows =
        post_process_rendered_field(fb, geometry, visible_start_vpos, h_shift, overscan);
    let base = input.render_base();
    deinterlacer.push_field(
        fb,
        field_rows,
        base.bplcon0 & 0x0004 != 0,
        base.long_field,
        !geometry.programmable,
    );
    let present_rows = deinterlacer.output_rows();
    let active = present_rows * FB_WIDTH;
    presentation_fb.resize(active, 0);
    presentation_fb.copy_from_slice(&deinterlacer.output()[..active]);
    RenderWorkerResult {
        generation,
        emulated_frame: input.emulated_frames(),
        timing: render_result.timing,
        presentation_fb,
        present_rows,
        standard_tv_aperture: uses_standard_pal_tv_aperture(geometry, present_rows, &base),
        input,
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

/// React to a host DPI scale-factor change for one window's pixel surface.
///
/// pixels' `window_pos_to_pixel` maps a host click into texture space using
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

pub(super) fn build_pixels_for_window(
    window: Arc<Window>,
    texture_scale: usize,
    vsync: bool,
) -> std::result::Result<Pixels<'static>, pixels::Error> {
    let inner = window.inner_size();
    let surface = SurfaceTexture::new(inner.width.max(1), inner.height.max(1), window);
    let builder = PixelsBuilder::new(
        texture_width(texture_scale) as u32,
        texture_height(texture_scale) as u32,
        surface,
    )
    .enable_vsync(vsync);
    let builder = if cfg!(target_os = "linux") {
        builder.wgpu_backend(
            pixels::wgpu::Backends::from_env().unwrap_or(pixels::wgpu::Backends::VULKAN),
        )
    } else {
        builder
    };
    let mut pixels = builder.build()?;
    pixels.set_scaling_mode(ScalingMode::Fill);
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

pub(super) fn cursor_texture_position(
    pixels: &Pixels<'_>,
    position: winit::dpi::PhysicalPosition<f64>,
    texture_scale: usize,
) -> Option<(i32, i32)> {
    let (x, y) = pixels
        .window_pos_to_pixel((position.x as f32, position.y as f32))
        .ok()?;
    Some(((x / texture_scale) as i32, (y / texture_scale) as i32))
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
    frame: &mut [u8],
    texture_scale: usize,
) {
    debug_assert!(src_fb.len() >= src_rows * FB_WIDTH);
    debug_assert_eq!(
        frame.len(),
        texture_width(texture_scale) * texture_height(texture_scale) * 4
    );
    let dst_stride = texture_width(texture_scale) * 4;
    let out_rows = present_height() * texture_scale;
    // The 570 woven scanlines map onto the 537-row 4:3 presentation (times
    // the HiDPI texture scale). Select whole source rows instead of blending
    // adjacent Amiga scanlines; normal presentation should not synthesize
    // intermediate colours from line-to-line dithering.
    for y in 0..out_rows {
        let src_y = screenshot::scaled_source_row(y, src_rows, out_rows);
        let row = &src_fb[src_y * FB_WIDTH..(src_y + 1) * FB_WIDTH];

        let dst_off = y * dst_stride;
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
    frame: &mut [u8],
    texture_scale: usize,
    overscan: Overscan,
    standard_tv_aperture: bool,
) {
    if overscan == Overscan::Tv && standard_tv_aperture {
        copy_tv_aperture_to_window(src_fb, src_rows, frame, texture_scale);
    } else {
        copy_present_frame(src_fb, src_rows, frame, texture_scale);
    }
}

pub(super) fn copy_tv_aperture_to_window(
    src_fb: &[u32],
    src_rows: usize,
    frame: &mut [u8],
    texture_scale: usize,
) {
    debug_assert!(src_fb.len() >= src_rows * FB_WIDTH);
    debug_assert_eq!(
        frame.len(),
        texture_width(texture_scale) * texture_height(texture_scale) * 4
    );
    let dst_stride = texture_width(texture_scale) * 4;
    let present_rows = present_height();
    let out_rows = present_rows * texture_scale;
    // The aperture reaches a few columns past the framebuffer's right edge;
    // that uncaptured margin is bezel and must stay black rather than
    // replicate the edge column (which carries picture when a display
    // fetches into the deepest overscan).
    let black_px = rgba(0, 0, 0);
    let black = black_px.to_le_bytes();
    for y in 0..out_rows {
        let Some(crop_y) = tv_aperture_source_row(y, present_rows, texture_scale) else {
            let dst = &mut frame[y * dst_stride..(y + 1) * dst_stride];
            for px in dst.chunks_exact_mut(4) {
                px.copy_from_slice(&black);
            }
            continue;
        };
        let src_y = (TV_PAL_PRESENT_SOURCE_Y + crop_y).min(src_rows - 1);
        let row = &src_fb[src_y * FB_WIDTH..(src_y + 1) * FB_WIDTH];
        let dst_off = y * dst_stride;
        match texture_scale {
            1 => {
                let dst = &mut frame[dst_off..dst_off + dst_stride];
                for x in 0..FB_WIDTH {
                    let crop_x = x
                        .saturating_sub(TV_PAL_LIVE_PAD_X)
                        .min(TV_PAL_PRESENT_WIDTH - 1);
                    let src_x = TV_PAL_PRESENT_SOURCE_X + crop_x;
                    let pixel = if src_x < FB_WIDTH {
                        row[src_x]
                    } else {
                        black_px
                    };
                    dst[x * 4..x * 4 + 4].copy_from_slice(&pixel.to_le_bytes());
                }
            }
            2 => {
                for x in 0..FB_WIDTH {
                    let crop_x = x
                        .saturating_sub(TV_PAL_LIVE_PAD_X)
                        .min(TV_PAL_PRESENT_WIDTH - 1);
                    let src_x = TV_PAL_PRESENT_SOURCE_X + crop_x;
                    let pixel = if src_x < FB_WIDTH {
                        row[src_x]
                    } else {
                        black_px
                    };
                    let pair = pixel as u64 | ((pixel as u64) << 32);
                    unsafe {
                        (frame.as_mut_ptr().add(dst_off + x * 8) as *mut u64).write_unaligned(pair);
                    }
                }
            }
            _ => {
                let dst = &mut frame[dst_off..dst_off + dst_stride];
                for x in 0..FB_WIDTH * texture_scale {
                    let out_x = x / texture_scale;
                    let crop_x = out_x
                        .saturating_sub(TV_PAL_LIVE_PAD_X)
                        .min(TV_PAL_PRESENT_WIDTH - 1);
                    let src_x = TV_PAL_PRESENT_SOURCE_X + crop_x;
                    let pixel = if src_x < FB_WIDTH {
                        row[src_x]
                    } else {
                        black_px
                    };
                    dst[x * 4..x * 4 + 4].copy_from_slice(&pixel.to_le_bytes());
                }
            }
        }
    }
}

/// Map an output texture row to a row of the 540-line TV aperture crop,
/// or None for rows that fall on the black bezel of the square-pixel
/// presentation. The square canvas (570 rows) is taller than the
/// aperture, so the crop is centred between black bands -- the vertical
/// counterpart of the TV_PAL_LIVE_PAD_X side bands -- and its rows map
/// 1:1. The default 4:3 canvas (537 rows) is shorter than the aperture
/// and keeps mapping all 540 rows onto the output, as before.
pub(super) fn tv_aperture_source_row(
    y: usize,
    present_rows: usize,
    texture_scale: usize,
) -> Option<usize> {
    let out_rows = present_rows * texture_scale;
    let pad_rows = present_rows.saturating_sub(TV_PAL_PRESENT_HEIGHT) / 2 * texture_scale;
    let content_rows = out_rows - 2 * pad_rows;
    if y < pad_rows || y >= pad_rows + content_rows {
        return None;
    }
    Some(screenshot::scaled_source_row(
        y - pad_rows,
        TV_PAL_PRESENT_HEIGHT,
        content_rows,
    ))
}

/// Paint a TV-style test pattern into the presentation framebuffer,
/// shown while the machine is powered off. SMPTE-style colour bars over
/// a grayscale step wedge: instantly readable as "no signal", and handy
/// for setting up video capture levels before the machine boots.
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
    let bars_h = FB_HEIGHT * 4 / 5;
    for y in 0..FB_HEIGHT {
        let row = &mut fb[y * FB_WIDTH..(y + 1) * FB_WIDTH];
        if y < bars_h {
            for (x, px) in row.iter_mut().enumerate() {
                *px = BARS[x * BARS.len() / FB_WIDTH];
            }
        } else {
            for (x, px) in row.iter_mut().enumerate() {
                let level = (x * STEPS / FB_WIDTH) as u32 * 255 / (STEPS as u32 - 1);
                *px = rgba(level, level, level);
            }
        }
    }
    draw_test_screen_logo(fb, bars_h);
}

pub(super) fn draw_test_screen_logo(fb: &mut [u32], bars_h: usize) {
    let Some(image) = copperline_logo_image() else {
        return;
    };
    let x = FB_WIDTH.saturating_sub(image.width) / 2;
    let y = bars_h.saturating_sub(image.height) / 2;
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
    overscan: Overscan,
    standard_tv_aperture: bool,
) -> anyhow::Result<()> {
    if crate::envcfg::flag("COPPERLINE_SHOT_RAW") {
        return screenshot::save(
            path,
            &present_fb[..src_rows * FB_WIDTH],
            FB_WIDTH as u32,
            src_rows as u32,
        );
    }

    if overscan == Overscan::Tv && standard_tv_aperture {
        return screenshot::save_cropped_black_padded(
            path,
            present_fb,
            FB_WIDTH,
            src_rows,
            TV_PAL_PRESENT_SOURCE_X,
            TV_PAL_PRESENT_SOURCE_Y,
            TV_PAL_PRESENT_WIDTH,
            TV_PAL_PRESENT_HEIGHT,
        );
    }

    screenshot::save_scaled_y(
        path,
        present_fb,
        FB_WIDTH as u32,
        src_rows as u32,
        present_height() as u32,
    )
}

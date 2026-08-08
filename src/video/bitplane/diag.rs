// SPDX-License-Identifier: GPL-3.0-or-later

//! COPPERLINE_* frame/sprite/pixel diagnostic logging for the bitplane
//! renderer. Split out of `bitplane.rs` for size; same module family,
//! full access to the parent's private items.

use super::*;

/// Debug helper: when `COPPERLINE_DBG_FRAMESTATE` is set, log the per-frame display
/// snapshot the renderer starts from (DMA enable, scroll, window, modulos,
/// bitplane pointers, the active palette, and a sprite summary) once per
/// rendered frame, optionally bounded by `COPPERLINE_DBG_AFTER` / `COPPERLINE_DBG_UNTIL`
/// emulated seconds. This watches how display state evolves across a frame
/// boundary where content unexpectedly appears or vanishes. All values are read
/// from the renderer's own frame snapshot, so they match what is drawn.
pub(super) fn maybe_log_frame_state(
    emulated_seconds: f64,
    emulated_frames: u64,
    geometry: FrameGeometry,
    captured_sprite_lines: &[CapturedSpriteLine],
    sprite_dma_observed: bool,
    control: &ControlState,
    state: &RenderState,
    bplpt: &[u32; 8],
    visible_line0: i32,
) {
    if !crate::envcfg::flag("COPPERLINE_DBG_FRAMESTATE") {
        return;
    }
    let secs = emulated_seconds;
    let after = env_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0);
    let until = env_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY);
    if secs < after || secs >= until {
        return;
    }
    log::info!(
        "framestate secs={secs:.4} frame={} vline0={visible_line0} dmacon={:#06X} \
         bplcon0={:#06X} bplcon1={:#06X} bplcon2={:#06X} bplcon3={:#06X} \
         bplcon4={:#06X} diwstrt={:#06X} diwstop={:#06X} \
         ddfstrt={:#06X} ddfstop={:#06X} fmode={:#06X} bpl1mod={} bpl2mod={} bplpt={:08X?}",
        emulated_frames,
        control.dmacon,
        control.bplcon0,
        control.bplcon1,
        control.bplcon2,
        control.bplcon3,
        control.bplcon4,
        control.diwstrt,
        control.diwstop,
        control.ddfstrt,
        control.ddfstop,
        control.fmode,
        control.bpl1mod,
        control.bpl2mod,
        bplpt,
    );
    log::info!(
        "  geometry: programmable={} visible_start={} visible_lines={} line_cck={} lace={}",
        geometry.programmable,
        geometry.visible_start_vpos,
        geometry.visible_lines,
        geometry.line_cck,
        geometry.lace,
    );
    let pal: Vec<String> = (0..16)
        .map(|i| format!("{:03x}", state.palette[i]))
        .collect();
    log::info!("  pal0-15=[{}]", pal.join(" "));
    if crate::envcfg::flag("COPPERLINE_DBG_FRAMESTATE_FULLPAL") {
        for row in 0..16 {
            let entries: Vec<String> = (row * 16..row * 16 + 16)
                .map(|i| format!("{:06x}", state.palette.rgb24(i) & 0x00FF_FFFF))
                .collect();
            log::info!(
                "  pal{:3}-{:3}=[{}]",
                row * 16,
                row * 16 + 15,
                entries.join(" ")
            );
        }
    }
    let spr_lines = captured_sprite_lines;
    let mut per_sprite = [0u32; 8];
    let (mut ymin, mut ymax) = (i32::MAX, i32::MIN);
    for l in spr_lines {
        if l.sprite < 8 {
            per_sprite[l.sprite] += 1;
        }
        ymin = ymin.min(l.beam_y);
        ymax = ymax.max(l.beam_y);
    }
    log::info!(
        "  sprites: total={} dma_observed={} per_sprite={per_sprite:?} ybeam=[{},{}]",
        spr_lines.len(),
        sprite_dma_observed,
        ymin,
        ymax,
    );
    log::info!(
        "  sprpos={:04X?} sprctl={:04X?} sprarmed={:?}",
        state.sprpos,
        state.sprctl,
        state.spr_armed,
    );
    // The hardware-true latch view (spr_hw_*, fed by DMA fetches as well as
    // CPU/Copper writes) decides the DMA-idle latched redisplay. The labels
    // are the field names, so a line from a log grep straight back to here.
    log::info!(
        "  spr_hw_pos={:04X?} spr_hw_ctl={:04X?} spr_hw_data={:04X?} spr_hw_datb={:04X?} spr_hw_armed={:?}",
        state.spr_hw_pos,
        state.spr_hw_ctl,
        state.spr_hw_data,
        state.spr_hw_datb,
        state.spr_hw_armed,
    );
}

#[derive(Clone, Copy)]
pub(super) struct ManualSpriteDiagSpec {
    pub(super) want_all: bool,
    pub(super) beam_y: Option<i32>,
    pub(super) after: f64,
    pub(super) until: f64,
}

#[derive(Clone, Copy)]
pub(super) struct SpritePixelDiagSpec {
    pub(super) beam_y: i32,
    pub(super) step: usize,
    pub(super) after: f64,
    pub(super) until: f64,
}

#[derive(Clone, Copy)]
pub(super) struct PixelDiagSpec {
    pub(super) beam_y: i32,
    pub(super) x_start: usize,
    pub(super) x_stop: usize,
    pub(super) step: usize,
    pub(super) after: f64,
    pub(super) until: f64,
}

pub(super) fn manual_sprite_diag_spec() -> Option<ManualSpriteDiagSpec> {
    static SPEC: OnceLock<Option<ManualSpriteDiagSpec>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_MANUAL_SPRITES")?;
        let raw = raw.trim();
        let (want_all, beam_y) = if raw.eq_ignore_ascii_case("all") {
            (true, None)
        } else {
            (false, Some(raw.parse::<i32>().ok()?))
        };
        Some(ManualSpriteDiagSpec {
            want_all,
            beam_y,
            after: env_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0),
            until: env_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY),
        })
    })
}

pub(super) fn sprite_pixel_diag_spec() -> Option<SpritePixelDiagSpec> {
    static SPEC: OnceLock<Option<SpritePixelDiagSpec>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_SPRITE_PIXELS")?;
        let raw = raw.trim();
        let (beam_y, step) = if let Some((beam_y, step)) = raw.split_once(',') {
            (
                beam_y.trim().parse::<i32>().ok()?,
                step.trim().parse::<usize>().ok()?.max(1),
            )
        } else {
            (raw.parse::<i32>().ok()?, 32)
        };
        Some(SpritePixelDiagSpec {
            beam_y,
            step,
            after: env_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0),
            until: env_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY),
        })
    })
}

pub(super) fn parse_pixel_diag_spec(raw: &str) -> Option<PixelDiagSpec> {
    let parts: Vec<_> = raw.split(',').map(str::trim).collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let beam_y = parts[0].parse::<i32>().ok()?;
    let x_start = parts[1].parse::<usize>().ok()?;
    let x_stop = parts[2].parse::<usize>().ok()?;
    let step = parts
        .get(3)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    Some(PixelDiagSpec {
        beam_y,
        x_start: x_start.min(x_stop),
        x_stop: x_start.max(x_stop),
        step,
        after: env_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0),
        until: env_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY),
    })
}

pub(super) fn ham_pixel_diag_spec() -> Option<PixelDiagSpec> {
    static SPEC: OnceLock<Option<PixelDiagSpec>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_HAM_PIXELS")?;
        parse_pixel_diag_spec(&raw)
    })
}

pub(super) fn manual_bpl_pixel_diag_spec() -> Option<PixelDiagSpec> {
    static SPEC: OnceLock<Option<PixelDiagSpec>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_MANUAL_BPL_PIXELS")?;
        parse_pixel_diag_spec(&raw)
    })
}

pub(super) fn frame_pixel_diag_spec() -> Option<PixelDiagSpec> {
    static SPEC: OnceLock<Option<PixelDiagSpec>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_FRAME_PIXELS")?;
        parse_pixel_diag_spec(&raw)
    })
}

pub(super) fn maybe_log_frame_pixel_samples(
    label: &str,
    emulated_seconds: f64,
    emulated_frames: u64,
    fb: &[u32],
    visible_line0: i32,
) {
    let Some(spec) = frame_pixel_diag_spec()
        .filter(|spec| emulated_seconds >= spec.after && emulated_seconds < spec.until)
    else {
        return;
    };
    let row = spec.beam_y - visible_line0;
    if !(0..FB_HEIGHT as i32).contains(&row) {
        return;
    }
    let row = row as usize;
    let start = spec.x_start.min(FB_WIDTH);
    let stop = spec.x_stop.min(FB_WIDTH);
    for x in start..stop {
        if !(x - start).is_multiple_of(spec.step) {
            continue;
        }
        let color = fb[row * FB_WIDTH + x] & 0x00FF_FFFF;
        log::info!(
            "frame-pixel {label} secs={emulated_seconds:.4} frame={emulated_frames} y={} x={x} rgba={:#010X} rgb={:#08X}",
            spec.beam_y,
            fb[row * FB_WIDTH + x],
            color,
        );
    }
}

pub(super) fn maybe_log_manual_sprite_intervals(
    emulated_seconds: f64,
    emulated_frames: u64,
    state: &RenderState,
    events: &[BeamRegisterWrite],
    held: &[Option<HeldSpriteLine>; 8],
    lines: &[Vec<SpriteLine>],
) {
    let Some(spec) = manual_sprite_diag_spec() else {
        return;
    };
    let secs = emulated_seconds;
    if secs < spec.after || secs >= spec.until {
        return;
    }

    let held_summary: Vec<_> = held
        .iter()
        .map(|line| {
            line.map(|line| {
                (
                    line.vstart,
                    line.vstop,
                    line.line.hstart,
                    line.line.width_words,
                    line.line.attached,
                    line.line.data,
                    line.line.data_ext,
                    line.line.datb,
                    line.line.datb_ext,
                )
            })
        })
        .collect();
    let sprpt_align: Vec<_> = state.sprpt.iter().map(|ptr| ptr & 7).collect();
    let event_count = events
        .iter()
        .filter(|event| {
            matches!(
                event.offset & 0x01FE,
                0x096 | 0x106 | 0x10C | 0x140..=0x17E | 0x180..=0x1BE | 0x1FC
            )
        })
        .count();
    log::info!(
        "manual-sprite intervals secs={secs:.4} frame={} events={} sprpt={:08X?} sprpt_align={sprpt_align:?} held={held_summary:?}",
        emulated_frames,
        event_count,
        state.sprpt,
    );

    for event in events.iter().filter(|event| {
        matches!(
            event.offset & 0x01FE,
            0x096 | 0x106 | 0x10C | 0x140..=0x17E | 0x180..=0x1BE | 0x1FC
        )
    }) {
        if !spec.want_all
            && spec
                .beam_y
                .is_some_and(|beam_y| event.vpos as i32 != beam_y)
        {
            continue;
        }
        let off = event.offset & 0x01FE;
        let beam_x = beam_to_framebuffer_x_unclamped(event.hpos);
        let color_x = color_write_framebuffer_x(event.hpos);
        let source = manual_sprite_source_name(event.source);
        match off {
            0x096 => log::info!(
                "manual-sprite event source={source} y={} h={} beam_x={} DMACON={:#06X}",
                event.vpos,
                event.hpos,
                beam_x,
                event.value
            ),
            0x106 => log::info!(
                "manual-sprite event source={source} y={} h={} beam_x={} color_x={} BPLCON3={:#06X}",
                event.vpos,
                event.hpos,
                beam_x,
                color_x,
                event.value
            ),
            0x10C => log::info!(
                "manual-sprite event source={source} y={} h={} beam_x={} color_x={} BPLCON4={:#06X}",
                event.vpos,
                event.hpos,
                beam_x,
                color_x,
                event.value
            ),
            0x1FC => log::info!(
                "manual-sprite event source={source} y={} h={} beam_x={} FMODE={:#06X}",
                event.vpos,
                event.hpos,
                beam_x,
                event.value
            ),
            0x180..=0x1BE => log::info!(
                "manual-sprite event source={source} y={} h={} color_x={} COLOR{}={:#06X}",
                event.vpos,
                event.hpos,
                color_x,
                (off - 0x180) / 2,
                event.value
            ),
            0x140..=0x17E => {
                let sprite = ((off - 0x140) / 8) as usize;
                log::info!(
                    "manual-sprite event source={source} y={} h={} beam_x={} s{} reg={:#05X} val={:#06X}",
                    event.vpos,
                    event.hpos,
                    beam_x,
                    sprite,
                    off,
                    event.value
                );
            }
            _ => {}
        }
    }

    for (sprite, sprite_lines) in lines.iter().enumerate() {
        for line in sprite_lines {
            if !spec.want_all && spec.beam_y.is_some_and(|beam_y| line.beam_y != beam_y) {
                continue;
            }
            log::info!(
                "manual-sprite line y={} s{} x={}..{} hstart={} hsub={} words={} att={} A={:04X} {:04X?} B={:04X} {:04X?}",
                line.beam_y,
                sprite,
                line.x_start,
                line.x_stop,
                line.hstart,
                u8::from(line.hsub_70ns),
                line.width_words,
                u8::from(line.attached),
                line.data,
                line.data_ext,
                line.datb,
                line.datb_ext
            );
        }
    }
}

fn manual_sprite_source_name(source: BeamWriteSource) -> &'static str {
    match source {
        BeamWriteSource::Cpu => "cpu",
        BeamWriteSource::CpuCopperIrq => "cpu_copper_irq",
        BeamWriteSource::Copper => "copper",
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn maybe_log_sprite_pixel_samples(
    emulated_seconds: f64,
    emulated_frames: u64,
    state: &RenderState,
    fb: &[u32],
    captured_sprite_lines: &[CapturedSpriteLine],
    sprite_dma_observed: bool,
    manual_sprite_lines: &[Vec<SpriteLine>],
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    sprite_display_enable_x_by_y: &[Option<usize>],
    playfield_mask: &[u8],
    visible_line0: i32,
) {
    let Some(spec) = sprite_pixel_diag_spec() else {
        return;
    };
    let secs = emulated_seconds;
    if secs < spec.after || secs >= spec.until {
        return;
    }
    let y = spec.beam_y - visible_line0;
    if y < 0 || y >= base_controls.len() as i32 {
        return;
    }
    let y = y as usize;
    let use_captured_sprite_dma = sprite_dma_observed
        || state.dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN);
    let sprite_lines: [Vec<SpriteLine>; 8] = std::array::from_fn(|sprite| {
        collect_sprite_lines(
            sprite,
            state,
            captured_sprite_lines,
            use_captured_sprite_dma,
            Some(manual_sprite_lines),
        )
    });
    log::info!(
        "sprite-pixel samples secs={secs:.4} frame={} y={} step={}",
        emulated_frames,
        spec.beam_y,
        spec.step
    );
    for x in (0..FB_WIDTH).step_by(spec.step) {
        let control = control_at_x(base_controls[y], &control_segments[y], x);
        let palette = palette_at_x(base_palettes[y], &palette_segments[y], x);
        let fb_idx = y * FB_WIDTH + x;
        let pf_mask = playfield_mask[fb_idx];
        let final_rgb = rgba8_to_rgb24(fb[fb_idx]);
        let display_enable_x = sprite_display_enable_x_for_y(sprite_display_enable_x_by_y, y);
        for pair in 0..4 {
            let even_sprite = pair * 2;
            let odd_sprite = even_sprite + 1;
            let even_lines = &sprite_lines[even_sprite];
            let odd_lines = &sprite_lines[odd_sprite];
            let attached = sprite_pair_attach_active_for_beam(even_lines, odd_lines, spec.beam_y);
            if attached {
                let even_idx = sprite_lines_pixel_bits_at(
                    even_lines,
                    spec.beam_y,
                    y,
                    x as i32,
                    base_controls,
                    control_segments,
                );
                let odd_idx = sprite_lines_pixel_bits_at(
                    odd_lines,
                    spec.beam_y,
                    y,
                    x as i32,
                    base_controls,
                    control_segments,
                );
                let idx = even_idx | (odd_idx << 2);
                if idx == 0 {
                    continue;
                }
                let color_idx = sprite_color_entry(control, even_sprite, idx, true);
                let priority = sprite_has_priority(even_sprite, pf_mask, control);
                let display = sprite_pixel_inside_display_window(
                    control,
                    y,
                    x,
                    visible_line0,
                    display_enable_x,
                );
                log::info!(
                    "sprite-pixel y={} x={} pair{} att idx={:#04X} color={} rgb={:#08X} final={:#08X} pf_mask={:#04X} priority={} display={} enable_x={:?} DIW={:#06X}/{:#06X} BPLCON2={:#06X} BPLCON3={:#06X} BPLCON4={:#06X}",
                    spec.beam_y,
                    x,
                    pair,
                    idx,
                    color_idx,
                    palette.rgb24(color_idx) & 0x00FF_FFFF,
                    final_rgb,
                    pf_mask,
                    priority,
                    display,
                    display_enable_x,
                    control.diwstrt,
                    control.diwstop,
                    control.bplcon2,
                    control.bplcon3,
                    control.bplcon4
                );
            } else {
                for sprite in [even_sprite, odd_sprite] {
                    let idx = sprite_lines_pixel_bits_at(
                        &sprite_lines[sprite],
                        spec.beam_y,
                        y,
                        x as i32,
                        base_controls,
                        control_segments,
                    );
                    if idx == 0 {
                        continue;
                    }
                    let color_idx = sprite_color_entry(control, sprite, idx, false);
                    let priority = sprite_has_priority(sprite, pf_mask, control);
                    let display = sprite_pixel_inside_display_window(
                        control,
                        y,
                        x,
                        visible_line0,
                        display_enable_x,
                    );
                    log::info!(
                        "sprite-pixel y={} x={} s{} idx={:#04X} color={} rgb={:#08X} final={:#08X} pf_mask={:#04X} priority={} display={} enable_x={:?} DIW={:#06X}/{:#06X} BPLCON2={:#06X} BPLCON3={:#06X} BPLCON4={:#06X}",
                        spec.beam_y,
                        x,
                        sprite,
                        idx,
                        color_idx,
                        palette.rgb24(color_idx) & 0x00FF_FFFF,
                        final_rgb,
                        pf_mask,
                        priority,
                        display,
                        display_enable_x,
                        control.diwstrt,
                        control.diwstop,
                        control.bplcon2,
                        control.bplcon3,
                        control.bplcon4
                    );
                }
            }
        }
    }
}

pub(super) fn env_f64(var: &str) -> Option<f64> {
    crate::envcfg::var(var).and_then(|s| s.trim().parse::<f64>().ok())
}

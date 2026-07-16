// SPDX-License-Identifier: GPL-3.0-or-later

//! Status-bar drawing and layout: drive/CD/LED/volume/pause/power/reboot
//! controls, their hit rectangles, and the glyph rasterisers. Split out
//! of `window.rs` for size; same module family, full access to the
//! parent's private items.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_status_bar(frame: &mut [u8], view: &StatusBarView, texture_scale: usize) {
    let status = view.status;
    let layout = bar_layout(&view.media);
    let hover = view.hover;
    fill_rect(
        frame,
        scale_rect(status_bar_rect(), texture_scale),
        STATUS_BG,
        texture_scale,
    );
    draw_hline(
        frame,
        present_height() * texture_scale,
        STATUS_TOP,
        texture_scale,
    );
    draw_hline(
        frame,
        window_present_height() * texture_scale - 1,
        STATUS_BOTTOM,
        texture_scale,
    );
    let rows = led_rows(&status, view.powered_on);
    for (row, spec) in rows.iter().enumerate() {
        draw_text(
            frame,
            STATUS_LABEL_X * texture_scale,
            (present_height() + led_row_label_y(row, rows.len())) * texture_scale,
            spec.label,
            STATUS_TEXT,
            texture_scale,
        );
        draw_led(
            frame,
            scale_rect(led_row_rect(row, rows.len()), texture_scale),
            spec.on,
            spec.on_color,
            spec.off_color,
            spec.highlight_on,
            spec.highlight_off,
            texture_scale,
        );
    }
    draw_fdd_track_counter(frame, status.fdd_track, texture_scale);
    for idx in 0..4 {
        let drive = view.media.drives[idx];
        if let Some(rect) = layout.drive_load[idx] {
            draw_disk_button(
                frame,
                scale_rect(rect, texture_scale),
                idx,
                hover == Some(BarControl::DriveLoad(idx)),
                texture_scale,
            );
        }
        if let Some(rect) = layout.drive_swap[idx] {
            draw_swap_button(
                frame,
                scale_rect(rect, texture_scale),
                drive.multi,
                hover == Some(BarControl::DriveSwap(idx)),
                texture_scale,
            );
        }
        if let Some(rect) = layout.drive_eject[idx] {
            draw_eject_button(
                frame,
                scale_rect(rect, texture_scale),
                drive.inserted,
                hover == Some(BarControl::DriveEject(idx)),
                texture_scale,
            );
        }
    }
    if let Some(rect) = layout.cd_load {
        draw_cd_button(
            frame,
            scale_rect(rect, texture_scale),
            hover == Some(BarControl::CdLoad),
            texture_scale,
        );
    }
    if let Some(rect) = layout.cd_eject {
        draw_eject_button(
            frame,
            scale_rect(rect, texture_scale),
            view.media.cd == Some(true),
            hover == Some(BarControl::CdEject),
            texture_scale,
        );
    }
    draw_joystick_button(
        frame,
        scale_rect(joystick_toggle_rect(), texture_scale),
        view.joystick_input_mode,
        hover == Some(BarControl::Joystick),
        texture_scale,
    );
    if view.control_connected {
        // A remote control-protocol client is attached; tag the bar so a
        // machine that pauses or steps "by itself" is explicable.
        draw_text(
            frame,
            (JOY_TOGGLE_X.saturating_sub(44)) * texture_scale,
            (present_height() + STATUS_CONTROL_Y + 2) * texture_scale,
            "CCP",
            STATUS_TEXT,
            texture_scale,
        );
    }
    draw_volume_control(frame, status.output_volume_percent, texture_scale);
    draw_menu_button(
        frame,
        scale_rect(menu_button_rect(), texture_scale),
        hover == Some(BarControl::Menu),
        texture_scale,
    );
    draw_shot_button(
        frame,
        scale_rect(shot_button_rect(), texture_scale),
        hover == Some(BarControl::Screenshot),
        texture_scale,
    );
    draw_pause_button(
        frame,
        scale_rect(pause_button_rect(), texture_scale),
        view.paused,
        hover == Some(BarControl::Pause),
        texture_scale,
    );
    draw_power_button(
        frame,
        scale_rect(power_button_rect(), texture_scale),
        view.powered_on,
        hover == Some(BarControl::Power),
        texture_scale,
    );
    draw_reboot_button(
        frame,
        scale_rect(reboot_button_rect(), texture_scale),
        hover == Some(BarControl::Reboot),
        texture_scale,
    );
}

/// Per-drive status feeding the media controls in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct DriveBar {
    /// Drive is wired up this session; unconnected drives get no controls.
    pub(super) connected: bool,
    /// A disk is currently inserted (enables the eject button).
    pub(super) inserted: bool,
    /// More than one image is queued for this drive (enables swap).
    pub(super) multi: bool,
}

/// Removable-media status for the bar: the floppy drives plus the CD
/// drive (None on machines without one, Some(disc inserted) otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MediaBar {
    pub(super) drives: [DriveBar; 4],
    pub(super) cd: Option<bool>,
}

/// Everything draw_status_bar needs for one frame.
pub(super) struct StatusBarView {
    pub(super) status: FrontPanelStatus,
    pub(super) powered_on: bool,
    pub(super) paused: bool,
    pub(super) media: MediaBar,
    /// Active host joystick source, shown by the status-bar toggle icon.
    pub(super) joystick_input_mode: JoystickInputMode,
    pub(super) hover: Option<BarControl>,
    /// A control-protocol client is attached (--control-gui).
    pub(super) control_connected: bool,
}

/// A clickable status-bar control, used for hit-testing and hover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BarControl {
    Power,
    Pause,
    Reboot,
    Screenshot,
    Menu,
    Joystick,
    Volume,
    DriveLoad(usize),
    DriveSwap(usize),
    DriveEject(usize),
    CdLoad,
    CdEject,
}

/// Computed positions of the variable (media) part of the status bar.
/// The fixed controls (volume, screenshot, pause, power, reboot) keep
/// their own rect functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BarLayout {
    pub(super) drive_load: [Option<Rect>; 4],
    pub(super) drive_swap: [Option<Rect>; 4],
    pub(super) drive_eject: [Option<Rect>; 4],
    pub(super) cd_load: Option<Rect>,
    pub(super) cd_eject: Option<Rect>,
}

/// Lay out the media controls left to right after the track counter.
/// One or two drives sit in a single full-height row; three or four
/// stack two-up in shorter rows, so even the worst case (four drives
/// plus CD) keeps the counter and ends clear of the volume control.
pub(super) fn bar_layout(media: &MediaBar) -> BarLayout {
    let mut layout = BarLayout {
        drive_load: [None; 4],
        drive_swap: [None; 4],
        drive_eject: [None; 4],
        cd_load: None,
        cd_eject: None,
    };
    // At most 4 drives, so track membership/position without allocating a
    // Vec on every call (this runs on every mouse-move and frame redraw).
    let connected_count = (0..4).filter(|&idx| media.drives[idx].connected).count();
    let stacked = connected_count > 2;

    let cluster = |x: usize, y: usize, h: usize| {
        let button = |x: usize, w: usize| Rect { x, y, w, h };
        (
            button(x, MEDIA_LOAD_W),
            button(x + MEDIA_LOAD_W + MEDIA_INNER_GAP, MEDIA_SMALL_W),
            button(
                x + MEDIA_LOAD_W + 2 * MEDIA_INNER_GAP + MEDIA_SMALL_W,
                MEDIA_SMALL_W,
            ),
        )
    };

    let mut drives_end_x = MEDIA_CLUSTER_X;
    let mut pos = 0usize;
    for idx in 0..4 {
        if !media.drives[idx].connected {
            continue;
        }
        let (x, y, h) = if stacked {
            // Row-major two-column grid: DF0 DF1 over DF2 DF3.
            let col = pos % 2;
            let row = pos / 2;
            (
                MEDIA_CLUSTER_X + col * (MEDIA_CLUSTER_W + MEDIA_CLUSTER_GAP),
                present_height() + MEDIA_STACKED_ROW0_Y + row * MEDIA_STACKED_PITCH,
                MEDIA_STACKED_H,
            )
        } else {
            (
                MEDIA_CLUSTER_X + pos * (MEDIA_CLUSTER_W + MEDIA_CLUSTER_GAP),
                present_height() + STATUS_CONTROL_Y,
                STATUS_CONTROL_H,
            )
        };
        let (load, swap, eject) = cluster(x, y, h);
        layout.drive_load[idx] = Some(load);
        layout.drive_swap[idx] = Some(swap);
        layout.drive_eject[idx] = Some(eject);
        drives_end_x = drives_end_x.max(x + MEDIA_CLUSTER_W);
        pos += 1;
    }

    if media.cd.is_some() {
        let x = if connected_count == 0 {
            MEDIA_CLUSTER_X
        } else {
            drives_end_x + MEDIA_CD_GAP
        };
        // The CD cluster is load plus eject only; eject takes the slot a
        // drive cluster gives to swap.
        let (load, eject, _) = cluster(x, present_height() + STATUS_CONTROL_Y, STATUS_CONTROL_H);
        layout.cd_load = Some(load);
        layout.cd_eject = Some(eject);
    }
    layout
}

/// Map a cursor position to the status-bar control under it.
pub(super) fn control_at(pos: (i32, i32), layout: &BarLayout) -> Option<BarControl> {
    for idx in 0..4 {
        if layout.drive_load[idx].is_some_and(|r| r.contains(pos)) {
            return Some(BarControl::DriveLoad(idx));
        }
        if layout.drive_swap[idx].is_some_and(|r| r.contains(pos)) {
            return Some(BarControl::DriveSwap(idx));
        }
        if layout.drive_eject[idx].is_some_and(|r| r.contains(pos)) {
            return Some(BarControl::DriveEject(idx));
        }
    }
    if layout.cd_load.is_some_and(|r| r.contains(pos)) {
        return Some(BarControl::CdLoad);
    }
    if layout.cd_eject.is_some_and(|r| r.contains(pos)) {
        return Some(BarControl::CdEject);
    }
    if shot_button_rect().contains(pos) {
        return Some(BarControl::Screenshot);
    }
    if menu_button_rect().contains(pos) {
        return Some(BarControl::Menu);
    }
    if pause_button_rect().contains(pos) {
        return Some(BarControl::Pause);
    }
    if power_button_rect().contains(pos) {
        return Some(BarControl::Power);
    }
    if reboot_button_rect().contains(pos) {
        return Some(BarControl::Reboot);
    }
    if joystick_toggle_rect().contains(pos) {
        return Some(BarControl::Joystick);
    }
    if volume_control_hit_rect().contains(pos) {
        return Some(BarControl::Volume);
    }
    None
}

pub(super) fn status_bar_rect() -> Rect {
    Rect {
        x: 0,
        y: present_height(),
        w: FB_WIDTH,
        h: STATUS_BAR_HEIGHT,
    }
}

/// One LED row of the front-panel block (label plus LED palette).
pub(super) struct LedRowSpec {
    pub(super) label: &'static str,
    pub(super) on: bool,
    pub(super) on_color: u32,
    pub(super) off_color: u32,
    pub(super) highlight_on: u32,
    pub(super) highlight_off: u32,
}

/// The LED rows present this session: PWR and FDD always, HDD on IDE
/// machines, CD on CDTV/CD32.
pub(super) fn led_rows(status: &FrontPanelStatus, powered_on: bool) -> Vec<LedRowSpec> {
    let mut rows = vec![
        LedRowSpec {
            label: "PWR",
            on: powered_on && status.power_led_on,
            on_color: POWER_LED_ON,
            off_color: POWER_LED_OFF,
            highlight_on: rgba(255, 91, 82),
            highlight_off: rgba(90, 27, 24),
        },
        LedRowSpec {
            label: "FDD",
            on: status.fdd_led_on,
            on_color: FDD_LED_ON,
            off_color: FDD_LED_OFF,
            highlight_on: rgba(255, 190, 70),
            highlight_off: rgba(100, 58, 18),
        },
    ];
    if let Some(on) = status.hdd_led {
        rows.push(LedRowSpec {
            label: "HDD",
            on,
            on_color: HDD_LED_ON,
            off_color: HDD_LED_OFF,
            highlight_on: rgba(120, 255, 150),
            highlight_off: rgba(26, 88, 40),
        });
    }
    if let Some(on) = status.cd_led {
        rows.push(LedRowSpec {
            label: "CD",
            on,
            on_color: CD_LED_ON,
            off_color: CD_LED_OFF,
            highlight_on: rgba(140, 214, 255),
            highlight_off: rgba(32, 74, 104),
        });
    }
    rows
}

/// Label y (bar-local) for LED row `row` of `count`. Up to three rows
/// use the classic spacing; four rows pack tighter to stay inside the
/// bar.
pub(super) fn led_row_label_y(row: usize, count: usize) -> usize {
    if count <= 3 {
        LED_ROW_START_Y + row * LED_ROW_PITCH
    } else {
        LED_ROW_START_Y_TIGHT + row * LED_ROW_PITCH_TIGHT
    }
}

pub(super) fn led_row_rect(row: usize, count: usize) -> Rect {
    Rect {
        x: STATUS_LED_X,
        y: present_height() + led_row_label_y(row, count) + STATUS_LED_Y_OFFSET,
        w: STATUS_LED_W,
        h: STATUS_LED_H,
    }
}

pub(super) fn fdd_track_counter_rect() -> Rect {
    Rect {
        x: 132,
        y: present_height() + STATUS_CONTROL_Y,
        w: 58,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn fdd_track_digit_rect(index: usize) -> Rect {
    let display = fdd_track_counter_rect();
    Rect {
        x: display.x + 5 + index * 17,
        y: display.y + 3,
        w: 12,
        h: 16,
    }
}

pub(super) fn shot_button_rect() -> Rect {
    Rect {
        x: SHOT_BUTTON_X,
        y: present_height() + STATUS_CONTROL_Y,
        w: SHOT_BUTTON_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn menu_button_rect() -> Rect {
    Rect {
        x: ui::MENU_BUTTON_X,
        y: present_height() + STATUS_CONTROL_Y,
        w: ui::MENU_BUTTON_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn volume_control_hit_rect() -> Rect {
    Rect {
        x: VOLUME_SLIDER_X - 8,
        y: present_height() + STATUS_CONTROL_Y,
        w: VOLUME_SLIDER_W + 16,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn joystick_toggle_rect() -> Rect {
    Rect {
        x: JOY_TOGGLE_X,
        y: present_height() + STATUS_CONTROL_Y,
        w: JOY_TOGGLE_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn volume_slider_track_rect() -> Rect {
    Rect {
        x: VOLUME_SLIDER_X,
        y: present_height() + VOLUME_SLIDER_Y,
        w: VOLUME_SLIDER_W,
        h: VOLUME_SLIDER_H,
    }
}

pub(super) fn volume_slider_knob_rect(percent: u8) -> Rect {
    let track = volume_slider_track_rect();
    let range = track.w.saturating_sub(1).max(1);
    let center = track.x + range * usize::from(percent.min(100)) / 100;
    Rect {
        x: center.saturating_sub(VOLUME_KNOB_W / 2),
        y: present_height() + STATUS_CONTROL_Y + (STATUS_CONTROL_H - VOLUME_KNOB_H) / 2,
        w: VOLUME_KNOB_W,
        h: VOLUME_KNOB_H,
    }
}

pub(super) fn reboot_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 58,
        y: present_height() + STATUS_CONTROL_Y,
        w: 42,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn power_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 108,
        y: present_height() + STATUS_CONTROL_Y,
        w: 42,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn pause_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 158,
        y: present_height() + STATUS_CONTROL_Y,
        w: 42,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn bar_hover_changed(
    layout: &BarLayout,
    previous: Option<(i32, i32)>,
    current: Option<(i32, i32)>,
) -> bool {
    previous.and_then(|pos| control_at(pos, layout))
        != current.and_then(|pos| control_at(pos, layout))
}

pub(super) fn draw_fdd_track_counter(frame: &mut [u8], track: Option<u8>, texture_scale: usize) {
    let rect = scale_rect(fdd_track_counter_rect(), texture_scale);
    fill_rect(frame, rect, LED_BEZEL_DARK, texture_scale);
    draw_rect_bevel(frame, rect, LED_BEZEL_LIGHT, STATUS_BOTTOM, texture_scale);
    let inset = 2 * texture_scale;
    fill_rect(
        frame,
        Rect {
            x: rect.x + inset,
            y: rect.y + inset,
            w: rect.w.saturating_sub(inset * 2),
            h: rect.h.saturating_sub(inset * 2),
        },
        TRACK_DISPLAY_BG,
        texture_scale,
    );

    let digits = track.map_or(*b"---", |track| {
        [
            b'0' + track / 100,
            b'0' + (track / 10) % 10,
            b'0' + track % 10,
        ]
    });
    for (idx, ch) in digits.into_iter().enumerate() {
        draw_seven_segment_digit(
            frame,
            scale_rect(fdd_track_digit_rect(idx), texture_scale),
            ch as char,
            texture_scale,
        );
    }
}

pub(super) fn draw_volume_control(frame: &mut [u8], percent: u8, texture_scale: usize) {
    let percent = percent.min(100);
    draw_speaker_glyph(frame, texture_scale);

    let rect = scale_rect(volume_slider_track_rect(), texture_scale);
    fill_rect(frame, rect, LED_BEZEL_DARK, texture_scale);
    draw_rect_bevel(frame, rect, LED_BEZEL_LIGHT, STATUS_BOTTOM, texture_scale);

    let inset = 2 * texture_scale;
    let inner = Rect {
        x: rect.x + inset,
        y: rect.y + inset,
        w: rect.w.saturating_sub(inset * 2),
        h: rect.h.saturating_sub(inset * 2),
    };
    fill_rect(frame, inner, TRACK_DISPLAY_BG, texture_scale);

    let fill_w = inner.w * usize::from(percent) / 100;
    if fill_w != 0 {
        let filled = Rect {
            x: inner.x,
            y: inner.y,
            w: fill_w,
            h: inner.h,
        };
        fill_rect(frame, filled, VOLUME_FILL, texture_scale);
        draw_hline_span(
            frame,
            filled.y,
            filled.x,
            filled.x + filled.w,
            VOLUME_FILL_HIGHLIGHT,
            texture_scale,
        );
    }

    let knob = scale_rect(volume_slider_knob_rect(percent), texture_scale);
    fill_rect(frame, knob, BUTTON_FACE, texture_scale);
    draw_rect_bevel(
        frame,
        knob,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
}

pub(super) fn draw_button_base(frame: &mut [u8], rect: Rect, hover: bool, texture_scale: usize) {
    let face = if hover {
        BUTTON_FACE_HOVER
    } else {
        BUTTON_FACE
    };
    fill_rect(frame, rect, face, texture_scale);
    draw_rect_bevel(
        frame,
        rect,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
}

pub(super) fn draw_disk_button(
    frame: &mut [u8],
    rect: Rect,
    drive_idx: usize,
    hover: bool,
    texture_scale: usize,
) {
    draw_button_base(frame, rect, hover, texture_scale);
    draw_disk_glyph(frame, rect, drive_idx, texture_scale);
}

/// Swap button: two opposed horizontal arrows (cycle to the next queued
/// disk). Drawn dim when there is nothing to swap to.
pub(super) fn draw_swap_button(
    frame: &mut [u8],
    rect: Rect,
    enabled: bool,
    hover: bool,
    texture_scale: usize,
) {
    draw_button_base(frame, rect, hover && enabled, texture_scale);
    let color = if enabled {
        BUTTON_GLYPH
    } else {
        BUTTON_GLYPH_DISABLED
    };
    let s = texture_scale;
    // Glyph coordinates are designed for a full-height (22) button;
    // recentre vertically for the shorter stacked buttons.
    let dy = glyph_dy(rect, s);
    let fx = rect.x as f32;
    let fy = rect.y as f32 + dy as f32 * s as f32;
    let fs = s as f32;
    let uy = |v: i32| (rect.y as i32 + (v + dy) * s as i32) as usize;
    // Top arrow pointing right.
    fill_rect(
        frame,
        Rect {
            x: rect.x + 2 * s,
            y: uy(7),
            w: 8 * s,
            h: 2 * s,
        },
        color,
        texture_scale,
    );
    fill_triangle(
        frame,
        [
            (fx + 10.0 * fs, fy + 5.0 * fs),
            (fx + 10.0 * fs, fy + 11.0 * fs),
            (fx + 13.5 * fs, fy + 8.0 * fs),
        ],
        color,
        texture_scale,
    );
    // Bottom arrow pointing left.
    fill_rect(
        frame,
        Rect {
            x: rect.x + 6 * s,
            y: uy(13),
            w: 8 * s,
            h: 2 * s,
        },
        color,
        texture_scale,
    );
    fill_triangle(
        frame,
        [
            (fx + 6.0 * fs, fy + 11.0 * fs),
            (fx + 6.0 * fs, fy + 17.0 * fs),
            (fx + 2.5 * fs, fy + 14.0 * fs),
        ],
        color,
        texture_scale,
    );
}

/// Vertical recentring (in unscaled pixels) for glyph art designed for a
/// full-height control drawn in a shorter (stacked) button.
pub(super) fn glyph_dy(rect: Rect, texture_scale: usize) -> i32 {
    ((rect.h / texture_scale) as i32 - STATUS_CONTROL_H as i32) / 2
}

/// Eject button: up triangle over a bar. Drawn dim when no media is in.
pub(super) fn draw_eject_button(
    frame: &mut [u8],
    rect: Rect,
    enabled: bool,
    hover: bool,
    texture_scale: usize,
) {
    draw_button_base(frame, rect, hover && enabled, texture_scale);
    let color = if enabled {
        BUTTON_GLYPH
    } else {
        BUTTON_GLYPH_DISABLED
    };
    let s = texture_scale;
    let dy = glyph_dy(rect, s);
    let fx = rect.x as f32;
    let fy = rect.y as f32 + dy as f32 * s as f32;
    let fs = s as f32;
    fill_triangle(
        frame,
        [
            (fx + 8.0 * fs, fy + 5.0 * fs),
            (fx + 2.5 * fs, fy + 12.0 * fs),
            (fx + 13.5 * fs, fy + 12.0 * fs),
        ],
        color,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: rect.x + 3 * s,
            y: (rect.y as i32 + (14 + dy) * s as i32) as usize,
            w: 10 * s,
            h: 2 * s,
        },
        color,
        texture_scale,
    );
}

/// CD load/swap button: a compact disc.
pub(super) fn draw_cd_button(frame: &mut [u8], rect: Rect, hover: bool, texture_scale: usize) {
    draw_button_base(frame, rect, hover, texture_scale);
    let s = texture_scale;
    // Disc centre and radii in unscaled button-local pixels.
    let cx = (rect.x + 11 * s) as f32;
    let cy = rect.y as f32 + rect.h as f32 / 2.0;
    let fs = s as f32;
    for py in rect.y..rect.y + rect.h {
        for px in rect.x..rect.x + rect.w {
            let dx = (px as f32 + 0.5 - cx) / fs;
            let dy = (py as f32 + 0.5 - cy) / fs;
            let r2 = dx * dx + dy * dy;
            let color = if r2 <= 2.2 {
                CD_HOLE
            } else if r2 <= 6.2 {
                CD_HUB
            } else if r2 <= 64.0 {
                // A sheen wedge across the upper-left of the data area.
                if r2 >= 30.0 && dx + dy < -3.0 {
                    CD_SHEEN
                } else {
                    CD_BODY
                }
            } else {
                continue;
            };
            put_pixel(frame, px, py, color, texture_scale);
        }
    }
}

/// Menu button: three stacked bars (opens the pop-up menu).
pub(super) fn draw_menu_button(frame: &mut [u8], rect: Rect, hover: bool, texture_scale: usize) {
    draw_button_base(frame, rect, hover, texture_scale);
    let s = texture_scale;
    for row in 0..3 {
        fill_rect(
            frame,
            Rect {
                x: rect.x + 4 * s,
                y: rect.y + (6 + row * 4) * s,
                w: 14 * s,
                h: 2 * s,
            },
            BUTTON_GLYPH,
            texture_scale,
        );
    }
}

/// Screenshot button: a small camera.
pub(super) fn draw_shot_button(frame: &mut [u8], rect: Rect, hover: bool, texture_scale: usize) {
    draw_button_base(frame, rect, hover, texture_scale);
    let s = texture_scale;
    // Viewfinder bump, then the body, then the lens.
    fill_rect(
        frame,
        Rect {
            x: rect.x + 8 * s,
            y: rect.y + 5 * s,
            w: 6 * s,
            h: 3 * s,
        },
        CAMERA_BODY,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: rect.x + 3 * s,
            y: rect.y + 7 * s,
            w: 16 * s,
            h: 10 * s,
        },
        CAMERA_BODY,
        texture_scale,
    );
    let cx = (rect.x + 11 * s) as f32;
    let cy = (rect.y + 12 * s) as f32;
    let fs = s as f32;
    for py in rect.y + 7 * s..rect.y + 17 * s {
        for px in rect.x + 5 * s..rect.x + 17 * s {
            let dx = (px as f32 + 0.5 - cx) / fs;
            let dy = (py as f32 + 0.5 - cy) / fs;
            let r2 = dx * dx + dy * dy;
            let color = if r2 <= 5.5 {
                CAMERA_LENS
            } else if r2 <= 12.5 {
                BUTTON_GLYPH
            } else {
                continue;
            };
            put_pixel(frame, px, py, color, texture_scale);
        }
    }
}

/// Speaker glyph labelling the volume slider: a driver box, a cone, and
/// two sound arcs.
pub(super) fn draw_speaker_glyph(frame: &mut [u8], texture_scale: usize) {
    let s = texture_scale;
    let x = VOLUME_GLYPH_X * s;
    let y = (present_height() + STATUS_CONTROL_Y) * s;
    let fs = s as f32;
    fill_rect(
        frame,
        Rect {
            x: x + s,
            y: y + 9 * s,
            w: 3 * s,
            h: 5 * s,
        },
        STATUS_TEXT,
        texture_scale,
    );
    fill_triangle(
        frame,
        [
            (x as f32 + 4.0 * fs, y as f32 + 11.5 * fs),
            (x as f32 + 8.0 * fs, y as f32 + 5.5 * fs),
            (x as f32 + 8.0 * fs, y as f32 + 17.5 * fs),
        ],
        STATUS_TEXT,
        texture_scale,
    );
    draw_vline_span(
        frame,
        x + 10 * s,
        y + 9 * s,
        y + 14 * s,
        STATUS_TEXT,
        texture_scale,
    );
    draw_vline_span(
        frame,
        x + 12 * s,
        y + 6 * s,
        y + 17 * s,
        STATUS_TEXT,
        texture_scale,
    );
}

pub(super) fn draw_disk_glyph(
    frame: &mut [u8],
    rect: Rect,
    drive_idx: usize,
    texture_scale: usize,
) {
    let s = texture_scale;
    // Centre the 16px disk body vertically (full-height buttons give the
    // original 3px margin; stacked buttons less).
    let body_margin_y = (rect.h / s).saturating_sub(16) / 2;
    let body = Rect {
        x: rect.x + 3 * s,
        y: rect.y + body_margin_y * s,
        w: 16 * s,
        h: 16 * s,
    };
    fill_rect(frame, body, DISK_BODY_SHADOW, texture_scale);
    fill_rect(
        frame,
        Rect {
            x: body.x + s,
            y: body.y + s,
            w: body.w.saturating_sub(2 * s),
            h: body.h.saturating_sub(2 * s),
        },
        DISK_BODY,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + s,
            y: body.y + s,
            w: body.w.saturating_sub(2 * s),
            h: s,
        },
        DISK_BODY_HIGHLIGHT,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + s,
            y: body.y + s,
            w: s,
            h: body.h.saturating_sub(2 * s),
        },
        DISK_BODY_HIGHLIGHT,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 5 * s,
            y: body.y + 2 * s,
            w: 8 * s,
            h: 5 * s,
        },
        DISK_SHUTTER,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 6 * s,
            y: body.y + 3 * s,
            w: 5 * s,
            h: s,
        },
        DISK_LABEL,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 5 * s,
            y: body.y + 6 * s,
            w: 8 * s,
            h: s,
        },
        DISK_SHUTTER_DARK,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 3 * s,
            y: body.y + 9 * s,
            w: 10 * s,
            h: 6 * s,
        },
        DISK_LABEL,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 4 * s,
            y: body.y + 11 * s,
            w: 5 * s,
            h: s,
        },
        DISK_LABEL_LINE,
        texture_scale,
    );
    fill_rect(
        frame,
        Rect {
            x: body.x + 4 * s,
            y: body.y + 13 * s,
            w: 4 * s,
            h: s,
        },
        DISK_LABEL_LINE,
        texture_scale,
    );
    // The drive number, written on the right of the disk label.
    draw_tiny_digit(
        frame,
        body.x + 9 * s,
        body.y + 9 * s,
        drive_idx as u8,
        DISK_BODY_SHADOW,
        texture_scale,
    );
}

/// 3x5 pixel digits 0-3 for the drive number on the disk-button label.
pub(super) fn draw_tiny_digit(
    frame: &mut [u8],
    x: usize,
    y: usize,
    digit: u8,
    color: u32,
    texture_scale: usize,
) {
    const GLYPHS: [[u8; 5]; 4] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b011, 0b001, 0b111],
    ];
    let Some(rows) = GLYPHS.get(usize::from(digit)) else {
        return;
    };
    let s = texture_scale;
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..3 {
            if bits & (0b100 >> col) != 0 {
                fill_rect(
                    frame,
                    Rect {
                        x: x + col * s,
                        y: y + row * s,
                        w: s,
                        h: s,
                    },
                    color,
                    texture_scale,
                );
            }
        }
    }
}

pub(super) fn draw_seven_segment_digit(
    frame: &mut [u8],
    rect: Rect,
    ch: char,
    texture_scale: usize,
) {
    const SEG_A: u8 = 1 << 0;
    const SEG_B: u8 = 1 << 1;
    const SEG_C: u8 = 1 << 2;
    const SEG_D: u8 = 1 << 3;
    const SEG_E: u8 = 1 << 4;
    const SEG_F: u8 = 1 << 5;
    const SEG_G: u8 = 1 << 6;

    let mask = match ch {
        '0' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F,
        '1' => SEG_B | SEG_C,
        '2' => SEG_A | SEG_B | SEG_D | SEG_E | SEG_G,
        '3' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_G,
        '4' => SEG_B | SEG_C | SEG_F | SEG_G,
        '5' => SEG_A | SEG_C | SEG_D | SEG_F | SEG_G,
        '6' => SEG_A | SEG_C | SEG_D | SEG_E | SEG_F | SEG_G,
        '7' => SEG_A | SEG_B | SEG_C,
        '8' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_E | SEG_F | SEG_G,
        '9' => SEG_A | SEG_B | SEG_C | SEG_D | SEG_F | SEG_G,
        '-' => SEG_G,
        _ => 0,
    };
    let thickness = 2 * texture_scale;
    let short = 5 * texture_scale;
    let horizontal = 8 * texture_scale;

    let segments = [
        (
            SEG_A,
            Rect {
                x: rect.x + thickness,
                y: rect.y,
                w: horizontal,
                h: thickness,
            },
        ),
        (
            SEG_B,
            Rect {
                x: rect.x + rect.w - thickness,
                y: rect.y + thickness,
                w: thickness,
                h: short,
            },
        ),
        (
            SEG_C,
            Rect {
                x: rect.x + rect.w - thickness,
                y: rect.y + rect.h - thickness - short,
                w: thickness,
                h: short,
            },
        ),
        (
            SEG_D,
            Rect {
                x: rect.x + thickness,
                y: rect.y + rect.h - thickness,
                w: horizontal,
                h: thickness,
            },
        ),
        (
            SEG_E,
            Rect {
                x: rect.x,
                y: rect.y + rect.h - thickness - short,
                w: thickness,
                h: short,
            },
        ),
        (
            SEG_F,
            Rect {
                x: rect.x,
                y: rect.y + thickness,
                w: thickness,
                h: short,
            },
        ),
        (
            SEG_G,
            Rect {
                x: rect.x + thickness,
                y: rect.y + rect.h / 2 - thickness / 2,
                w: horizontal,
                h: thickness,
            },
        ),
    ];

    for (segment, segment_rect) in segments {
        let lit = mask & segment != 0;
        fill_rect(
            frame,
            segment_rect,
            if lit {
                TRACK_SEGMENT_ON
            } else {
                TRACK_SEGMENT_OFF
            },
            texture_scale,
        );
        if lit {
            draw_hline_span(
                frame,
                segment_rect.y,
                segment_rect.x,
                segment_rect.x + segment_rect.w,
                TRACK_SEGMENT_HIGHLIGHT,
                texture_scale,
            );
        }
    }
}

pub(super) fn draw_led(
    frame: &mut [u8],
    rect: Rect,
    on: bool,
    on_color: u32,
    off_color: u32,
    on_highlight: u32,
    off_highlight: u32,
    texture_scale: usize,
) {
    fill_rect(frame, rect, LED_BEZEL_DARK, texture_scale);
    draw_rect_bevel(frame, rect, LED_BEZEL_LIGHT, STATUS_BOTTOM, texture_scale);
    let inset = 2 * texture_scale;
    let inner = Rect {
        x: rect.x + inset,
        y: rect.y + inset,
        w: rect.w.saturating_sub(inset * 2),
        h: rect.h.saturating_sub(inset * 2),
    };
    fill_rect(
        frame,
        inner,
        if on { on_color } else { off_color },
        texture_scale,
    );
    for dy in 0..texture_scale {
        draw_hline_span(
            frame,
            inner.y + dy,
            inner.x,
            inner.x + inner.w,
            if on { on_highlight } else { off_highlight },
            texture_scale,
        );
    }
}

pub(super) fn draw_reboot_button(frame: &mut [u8], rect: Rect, hover: bool, texture_scale: usize) {
    fill_rect(
        frame,
        rect,
        if hover {
            BUTTON_FACE_HOVER
        } else {
            BUTTON_FACE
        },
        texture_scale,
    );
    draw_rect_bevel(
        frame,
        rect,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    draw_reset_glyph(frame, cx, cy, texture_scale);
}

pub(super) fn draw_power_button(
    frame: &mut [u8],
    rect: Rect,
    powered_on: bool,
    hover: bool,
    texture_scale: usize,
) {
    fill_rect(
        frame,
        rect,
        if hover {
            BUTTON_FACE_HOVER
        } else {
            BUTTON_FACE
        },
        texture_scale,
    );
    draw_rect_bevel(
        frame,
        rect,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    let color = if powered_on {
        POWER_GLYPH_ON
    } else {
        POWER_GLYPH_OFF
    };
    draw_power_glyph(frame, cx, cy, color, texture_scale);
}

pub(super) fn draw_pause_button(
    frame: &mut [u8],
    rect: Rect,
    paused: bool,
    hover: bool,
    texture_scale: usize,
) {
    fill_rect(
        frame,
        rect,
        if hover {
            BUTTON_FACE_HOVER
        } else {
            BUTTON_FACE
        },
        texture_scale,
    );
    draw_rect_bevel(
        frame,
        rect,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    // Show the action the button performs: a play triangle while paused
    // (click to resume), the twin pause bars while running.
    if paused {
        draw_play_glyph(frame, cx, cy, BUTTON_GLYPH, texture_scale);
    } else {
        draw_pause_glyph(frame, cx, cy, BUTTON_GLYPH, texture_scale);
    }
}

/// Joystick input-source toggle: shows the device currently driving the
/// emulated port-2 joystick (a gamepad in `Gamepad` mode, a keyboard in
/// `Keyboard` mode). Clicking it flips between the two, so the active source is
/// always visible rather than hidden behind a key combination.
pub(super) fn draw_joystick_button(
    frame: &mut [u8],
    rect: Rect,
    mode: JoystickInputMode,
    hover: bool,
    texture_scale: usize,
) {
    draw_button_base(frame, rect, hover, texture_scale);
    match mode {
        JoystickInputMode::Gamepad => draw_gamepad_glyph(frame, rect, texture_scale),
        JoystickInputMode::Keyboard => draw_keyboard_glyph(frame, rect, texture_scale),
    }
}

/// A small gamepad: a rounded green body with a recessed d-pad on the left and
/// two action buttons on the right.
pub(super) fn draw_gamepad_glyph(frame: &mut [u8], rect: Rect, texture_scale: usize) {
    let s = texture_scale;
    let mut cell = |x: usize, y: usize, w: usize, h: usize, color: u32| {
        fill_rect(
            frame,
            Rect {
                x: rect.x + x * s,
                y: rect.y + y * s,
                w: w * s,
                h: h * s,
            },
            color,
            texture_scale,
        );
    };
    // Body and the two grip bumps.
    cell(4, 8, 14, 8, BUTTON_GLYPH);
    cell(3, 13, 3, 3, BUTTON_GLYPH);
    cell(16, 13, 3, 3, BUTTON_GLYPH);
    // D-pad cross, cut into the body on the left.
    cell(7, 9, 2, 5, BUTTON_EDGE_DARK);
    cell(5, 11, 6, 2, BUTTON_EDGE_DARK);
    // Two action buttons on the right.
    cell(13, 10, 2, 2, BUTTON_EDGE_DARK);
    cell(15, 12, 2, 2, BUTTON_EDGE_DARK);
}

/// A small keyboard: a recessed dark case holding two rows of green keys and a
/// space bar.
pub(super) fn draw_keyboard_glyph(frame: &mut [u8], rect: Rect, texture_scale: usize) {
    let s = texture_scale;
    let mut cell = |x: usize, y: usize, w: usize, h: usize, color: u32| {
        fill_rect(
            frame,
            Rect {
                x: rect.x + x * s,
                y: rect.y + y * s,
                w: w * s,
                h: h * s,
            },
            color,
            texture_scale,
        );
    };
    // Case.
    cell(3, 6, 16, 11, BUTTON_EDGE_DARK);
    // Two rows of keys.
    for &kx in &[5, 8, 11, 14] {
        cell(kx, 8, 2, 2, BUTTON_GLYPH);
        cell(kx, 11, 2, 2, BUTTON_GLYPH);
    }
    // Space bar.
    cell(7, 14, 8, 2, BUTTON_GLYPH);
}

/// The pause symbol: two short vertical bars flanking the centre.
pub(super) fn draw_pause_glyph(
    frame: &mut [u8],
    cx: usize,
    cy: usize,
    color: u32,
    texture_scale: usize,
) {
    let bar_w = 2 * texture_scale;
    let bar_h = 11 * texture_scale;
    let gap = 3 * texture_scale;
    let top = cy.saturating_sub(bar_h / 2);
    let left = cx.saturating_sub(gap / 2 + bar_w);
    let right = cx + gap / 2;
    for x in [left, right] {
        fill_rect(
            frame,
            Rect {
                x,
                y: top,
                w: bar_w,
                h: bar_h,
            },
            color,
            texture_scale,
        );
    }
}

/// The play symbol: a right-pointing filled triangle.
pub(super) fn draw_play_glyph(
    frame: &mut [u8],
    cx: usize,
    cy: usize,
    color: u32,
    texture_scale: usize,
) {
    let s = texture_scale as f32;
    let half_h = 6.0 * s;
    let width = 11.0 * s;
    let left = cx as f32 - width / 2.0 + 1.0;
    let cyf = cy as f32 + 0.5;
    fill_triangle(
        frame,
        [
            (left, cyf - half_h),
            (left, cyf + half_h),
            (left + width, cyf),
        ],
        color,
        texture_scale,
    );
}

/// The IEC power symbol: a near-closed ring broken at the top, with a
/// vertical bar dropping through the gap toward the centre.
pub(super) fn draw_power_glyph(
    frame: &mut [u8],
    cx: usize,
    cy: usize,
    color: u32,
    texture_scale: usize,
) {
    let scale = texture_scale as f32;
    let ccx = cx as f32 + 0.5;
    let ccy = cy as f32 + 0.5 + 0.5 * scale;
    let radius = 5.5 * scale;
    let stroke = 1.35 * scale;

    // Ring, swept clockwise from just right of top all the way around to
    // just left of top, leaving a gap centred on 12 o'clock.
    let gap = 0.6_f32;
    let top = -std::f32::consts::FRAC_PI_2;
    let start = top + gap;
    let end = top + std::f32::consts::TAU - gap;
    let steps = 32;
    let mut prev = (ccx + radius * start.cos(), ccy + radius * start.sin());
    for step in 1..=steps {
        let t = start + (end - start) * step as f32 / steps as f32;
        let next = (ccx + radius * t.cos(), ccy + radius * t.sin());
        draw_thick_line(
            frame,
            prev.0,
            prev.1,
            next.0,
            next.1,
            stroke,
            color,
            texture_scale,
        );
        prev = next;
    }

    // Vertical bar from above the ring down to its centre.
    draw_thick_line(
        frame,
        ccx,
        ccy - radius - 1.5 * scale,
        ccx,
        ccy - 0.5 * scale,
        stroke,
        color,
        texture_scale,
    );
}

pub(super) fn draw_reset_glyph(frame: &mut [u8], cx: usize, cy: usize, texture_scale: usize) {
    let svg_scale = 0.17 * texture_scale as f32;
    let ox = cx as f32 - 8.5 * texture_scale as f32;
    let oy = cy as f32 - 8.5 * texture_scale as f32;
    let map = |x: f32, y: f32| (ox + x * svg_scale, oy + y * svg_scale);
    let stroke = 1.35 * texture_scale as f32;

    let (x0, y0) = map(50.0, 10.0);
    let (x1, y1) = map(50.0, 45.0);
    draw_thick_line(frame, x0, y0, x1, y1, stroke, BUTTON_GLYPH, texture_scale);

    draw_cubic(
        frame,
        [(20.0, 29.0), (4.0, 52.0), (15.0, 90.0), (50.0, 90.0)],
        svg_scale,
        ox,
        oy,
        stroke,
        BUTTON_GLYPH,
        texture_scale,
    );
    draw_cubic(
        frame,
        [(50.0, 90.0), (85.0, 90.0), (100.0, 47.0), (74.0, 20.0)],
        svg_scale,
        ox,
        oy,
        stroke,
        BUTTON_GLYPH,
        texture_scale,
    );

    let arrow = [map(2.0, 21.0), map(31.0, 19.0), map(33.0, 48.0)];
    fill_triangle(frame, arrow, BUTTON_GLYPH, texture_scale);
}

pub(super) fn draw_cubic(
    frame: &mut [u8],
    p: [(f32, f32); 4],
    scale: f32,
    ox: f32,
    oy: f32,
    radius: f32,
    color: u32,
    texture_scale: usize,
) {
    let mut prev = (ox + p[0].0 * scale, oy + p[0].1 * scale);
    for step in 1..=18 {
        let t = step as f32 / 18.0;
        let mt = 1.0 - t;
        let x = mt * mt * mt * p[0].0
            + 3.0 * mt * mt * t * p[1].0
            + 3.0 * mt * t * t * p[2].0
            + t * t * t * p[3].0;
        let y = mt * mt * mt * p[0].1
            + 3.0 * mt * mt * t * p[1].1
            + 3.0 * mt * t * t * p[2].1
            + t * t * t * p[3].1;
        let next = (ox + x * scale, oy + y * scale);
        draw_thick_line(
            frame,
            prev.0,
            prev.1,
            next.0,
            next.1,
            radius,
            color,
            texture_scale,
        );
        prev = next;
    }
}

pub(super) fn draw_thick_line(
    frame: &mut [u8],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
    color: u32,
    texture_scale: usize,
) {
    let min_x = (x0.min(x1) - radius - 1.0).floor().max(0.0) as usize;
    let max_x = (x0.max(x1) + radius + 1.0)
        .ceil()
        .min((texture_width(texture_scale) - 1) as f32) as usize;
    let min_y = (y0.min(y1) - radius - 1.0).floor().max(0.0) as usize;
    let max_y = (y0.max(y1) + radius + 1.0)
        .ceil()
        .min((texture_height(texture_scale) - 1) as f32) as usize;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = if len2 == 0.0 {
                0.0
            } else {
                (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
            };
            let nearest_x = x0 + t * dx;
            let nearest_y = y0 + t * dy;
            let dist_x = px - nearest_x;
            let dist_y = py - nearest_y;
            let dist = (dist_x * dist_x + dist_y * dist_y).sqrt();
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend_pixel(frame, x, y, color, coverage, texture_scale);
            }
        }
    }
}

pub(super) fn fill_triangle(
    frame: &mut [u8],
    p: [(f32, f32); 3],
    color: u32,
    texture_scale: usize,
) {
    let min_x = p
        .iter()
        .map(|(x, _)| *x)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = p
        .iter()
        .map(|(x, _)| *x)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((texture_width(texture_scale) - 1) as f32) as usize;
    let min_y = p
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_y = p
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((texture_height(texture_scale) - 1) as f32) as usize;
    let area = edge(p[0], p[1], p[2]);
    if area == 0.0 {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let mut hits = 0;
            for sy in 0..3 {
                for sx in 0..3 {
                    let point = (
                        x as f32 + (sx as f32 + 0.5) / 3.0,
                        y as f32 + (sy as f32 + 0.5) / 3.0,
                    );
                    let w0 = edge(p[1], p[2], point);
                    let w1 = edge(p[2], p[0], point);
                    let w2 = edge(p[0], p[1], point);
                    if (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                        || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0)
                    {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                blend_pixel(frame, x, y, color, hits as f32 / 9.0, texture_scale);
            }
        }
    }
}

pub(super) fn edge(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (c.0 - a.0) * (b.1 - a.1) - (c.1 - a.1) * (b.0 - a.0)
}

/// Draw a transient overlay message near the bottom-left of the display
/// region: a translucent panel with the text (plus a 1px drop shadow for
/// legibility over arbitrary video). Operates on the presentation
/// texture, so it is never captured in screenshots.
/// Persistent "(*) REC" badge in the display's top-right corner while a
/// video recording runs. Like the OSD it is drawn into the presentation
/// texture after the frame is captured, so it is never recorded.
pub(super) fn draw_record_badge(frame: &mut [u8], texture_scale: usize) {
    let s = texture_scale;
    let px = 2 * s;
    let pad = 4 * s;
    let margin = 8 * s;
    let dot_d = 8 * s;
    let gap = 4 * s;

    let text = "REC";
    let text_w = font::text_width(text, px);
    let text_h = font::text_height(px);
    let box_w = dot_d + gap + text_w + 2 * pad;
    let box_h = text_h + 2 * pad;
    let box_x = (FB_WIDTH * s).saturating_sub(margin + box_w);
    let box_y = margin;

    fill_rect_blend(
        frame,
        Rect {
            x: box_x,
            y: box_y,
            w: box_w,
            h: box_h,
        },
        OSD_BG,
        0.68,
        s,
    );
    // Red record dot, centred on the text line.
    let cx = (box_x + pad + dot_d / 2) as f32;
    let cy = (box_y + box_h / 2) as f32;
    let radius = dot_d as f32 / 2.0;
    for y in box_y..box_y + box_h {
        for x in box_x + pad..box_x + pad + dot_d {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= radius * radius {
                put_pixel(frame, x, y, RECORD_DOT, s);
            }
        }
    }
    let text_x = box_x + pad + dot_d + gap;
    let text_y = box_y + pad;
    font::draw_text(
        frame,
        texture_width(s),
        texture_height(s),
        text_x + s,
        text_y + s,
        text,
        OSD_SHADOW,
        px,
    );
    font::draw_text(
        frame,
        texture_width(s),
        texture_height(s),
        text_x,
        text_y,
        text,
        OSD_TEXT,
        px,
    );
}

pub(super) fn draw_osd(frame: &mut [u8], text: &str, texture_scale: usize) {
    let s = texture_scale;
    let px = 2 * s; // font pixel -> device pixels
    let pad = 4 * s;
    let margin = 8 * s;
    let fw = texture_width(s);
    let display_h = present_height() * s;

    let text_w = font::text_width(text, px).min(fw.saturating_sub(2 * (margin + pad)));
    let text_h = font::text_height(px);
    let box_w = (text_w + 2 * pad).min(fw.saturating_sub(2 * margin));
    let box_h = text_h + 2 * pad;
    let box_x = margin;
    let box_y = display_h.saturating_sub(margin + box_h);

    fill_rect_blend(
        frame,
        Rect {
            x: box_x,
            y: box_y,
            w: box_w,
            h: box_h,
        },
        OSD_BG,
        0.68,
        s,
    );
    let text_x = box_x + pad;
    let text_y = box_y + pad;
    font::draw_text(
        frame,
        fw,
        texture_height(s),
        text_x + s,
        text_y + s,
        text,
        OSD_SHADOW,
        px,
    );
    font::draw_text(
        frame,
        fw,
        texture_height(s),
        text_x,
        text_y,
        text,
        OSD_TEXT,
        px,
    );
}

/// Fill `rect` by alpha-blending `color` over the existing texture
/// contents. Used for the semi-transparent overlay panel.
pub(in crate::video) fn fill_rect_blend(
    frame: &mut [u8],
    rect: Rect,
    color: u32,
    alpha: f32,
    texture_scale: usize,
) {
    let x1 = (rect.x + rect.w).min(texture_width(texture_scale));
    let y1 = (rect.y + rect.h).min(texture_height(texture_scale));
    for y in rect.y.min(texture_height(texture_scale))..y1 {
        for x in rect.x.min(texture_width(texture_scale))..x1 {
            blend_pixel(frame, x, y, color, alpha, texture_scale);
        }
    }
}

pub(super) fn draw_text(
    frame: &mut [u8],
    x: usize,
    y: usize,
    text: &str,
    color: u32,
    texture_scale: usize,
) {
    let mut cursor = x;
    for ch in text.chars() {
        if let Some(rows) = glyph(ch) {
            draw_glyph(frame, cursor, y, rows, color, texture_scale);
            cursor += 12 * texture_scale;
        } else {
            cursor += 6 * texture_scale;
        }
    }
}

pub(super) fn draw_glyph(
    frame: &mut [u8],
    x: usize,
    y: usize,
    rows: [u8; 5],
    color: u32,
    texture_scale: usize,
) {
    let block = 2 * texture_scale;
    for (row_idx, row) in rows.iter().enumerate() {
        for col in 0..5 {
            if row & (1 << (4 - col)) == 0 {
                continue;
            }
            let px = x + col * block;
            let py = y + row_idx * block;
            fill_rect(
                frame,
                Rect {
                    x: px,
                    y: py,
                    w: block,
                    h: block,
                },
                color,
                texture_scale,
            );
        }
    }
}

pub(super) fn glyph(ch: char) -> Option<[u8; 5]> {
    match ch {
        'C' => Some([0b01110, 0b10000, 0b10000, 0b10000, 0b01110]),
        'D' => Some([0b11100, 0b10010, 0b10010, 0b10010, 0b11100]),
        'F' => Some([0b11110, 0b10000, 0b11100, 0b10000, 0b10000]),
        'H' => Some([0b10010, 0b10010, 0b11110, 0b10010, 0b10010]),
        'L' => Some([0b10000, 0b10000, 0b10000, 0b10000, 0b11110]),
        'O' => Some([0b01110, 0b10001, 0b10001, 0b10001, 0b01110]),
        'P' => Some([0b11110, 0b10010, 0b11110, 0b10000, 0b10000]),
        'R' => Some([0b11110, 0b10010, 0b11110, 0b10100, 0b10010]),
        'V' => Some([0b10001, 0b10001, 0b01010, 0b01010, 0b00100]),
        'W' => Some([0b10001, 0b10001, 0b10101, 0b10101, 0b01010]),
        _ => None,
    }
}

pub(in crate::video) fn draw_rect_bevel(
    frame: &mut [u8],
    rect: Rect,
    light: u32,
    dark: u32,
    texture_scale: usize,
) {
    for inset in 0..texture_scale {
        draw_hline_span(
            frame,
            rect.y + inset,
            rect.x,
            rect.x + rect.w,
            light,
            texture_scale,
        );
        draw_vline_span(
            frame,
            rect.x + inset,
            rect.y,
            rect.y + rect.h,
            light,
            texture_scale,
        );
        draw_hline_span(
            frame,
            rect.y + rect.h - 1 - inset,
            rect.x,
            rect.x + rect.w,
            dark,
            texture_scale,
        );
        draw_vline_span(
            frame,
            rect.x + rect.w - 1 - inset,
            rect.y,
            rect.y + rect.h,
            dark,
            texture_scale,
        );
    }
}

pub(super) fn draw_hline(frame: &mut [u8], y: usize, color: u32, texture_scale: usize) {
    draw_hline_span(
        frame,
        y,
        0,
        texture_width(texture_scale),
        color,
        texture_scale,
    );
}

pub(super) fn draw_hline_span(
    frame: &mut [u8],
    y: usize,
    x0: usize,
    x1: usize,
    color: u32,
    texture_scale: usize,
) {
    if y >= texture_height(texture_scale) {
        return;
    }
    for x in x0.min(texture_width(texture_scale))..x1.min(texture_width(texture_scale)) {
        put_pixel(frame, x, y, color, texture_scale);
    }
}

pub(super) fn draw_vline_span(
    frame: &mut [u8],
    x: usize,
    y0: usize,
    y1: usize,
    color: u32,
    texture_scale: usize,
) {
    if x >= texture_width(texture_scale) {
        return;
    }
    for y in y0.min(texture_height(texture_scale))..y1.min(texture_height(texture_scale)) {
        put_pixel(frame, x, y, color, texture_scale);
    }
}

pub(in crate::video) fn fill_rect(frame: &mut [u8], rect: Rect, color: u32, texture_scale: usize) {
    let x1 = (rect.x + rect.w).min(texture_width(texture_scale));
    let y1 = (rect.y + rect.h).min(texture_height(texture_scale));
    for y in rect.y.min(texture_height(texture_scale))..y1 {
        for x in rect.x.min(texture_width(texture_scale))..x1 {
            put_pixel(frame, x, y, color, texture_scale);
        }
    }
}

pub(super) fn put_pixel(frame: &mut [u8], x: usize, y: usize, color: u32, texture_scale: usize) {
    if x >= texture_width(texture_scale) || y >= texture_height(texture_scale) {
        return;
    }
    let off = (y * texture_width(texture_scale) + x) * 4;
    frame[off..off + 4].copy_from_slice(&color.to_le_bytes());
}

pub(super) fn blend_pixel(
    frame: &mut [u8],
    x: usize,
    y: usize,
    color: u32,
    alpha: f32,
    texture_scale: usize,
) {
    if alpha >= 1.0 {
        put_pixel(frame, x, y, color, texture_scale);
        return;
    }
    if x >= texture_width(texture_scale) || y >= texture_height(texture_scale) {
        return;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let off = (y * texture_width(texture_scale) + x) * 4;
    let src = color.to_le_bytes();
    for chan in 0..3 {
        let dst = frame[off + chan] as f32;
        let src = src[chan] as f32;
        frame[off + chan] = (dst + (src - dst) * alpha).round() as u8;
    }
    frame[off + 3] = 0xFF;
}

// SPDX-License-Identifier: GPL-3.0-or-later

//! Status-bar drawing and layout: drive/CD/LED/volume/pause/power/reboot
//! controls, their hit rectangles, and the glyph rasterisers. Split out
//! of `window.rs` for size; same module family, full access to the
//! parent's private items.

use super::*;

// Where the focus stands on the bar while it is being drawn, and how
// far through its breath it is. The bar's buttons light for the focus
// exactly as they light under the pointer, in the focus's own blue.
thread_local! {
    static NAV_LIGHT: std::cell::Cell<(Option<BarControl>, f32)> =
        const { std::cell::Cell::new((None, 0.0)) };
    /// Whether the marker is up on a panel rather than here. The
    /// keyboard is in charge either way, so the pointer lights nothing
    /// in the bar while it is.
    static NAV_ELSEWHERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Say where the focus is on the bar, for the drawing about to happen.
pub(in crate::video) fn set_nav_light(target: Option<BarControl>, mix: f32, elsewhere: bool) {
    NAV_ELSEWHERE.with(|flag| flag.set(elsewhere));
    NAV_LIGHT.with(|light| light.set((target, mix.clamp(0.0, 1.0))));
}

/// How lit a control is: all the way under the pointer, and as far as
/// the breath has come when the focus is on it. Negative says the
/// focus has it, so one number carries both.
fn lit(hover: Option<BarControl>, control: BarControl) -> f32 {
    // The focus is asked first: a control the mouse is resting on
    // still breathes when the keyboard walks onto it.
    let focused = NAV_LIGHT.with(|light| {
        let (target, mix) = light.get();
        if target == Some(control) {
            mix
        } else {
            0.0
        }
    });
    if focused != 0.0 {
        return -focused;
    }
    // And while the marker is up at all, the keyboard is in charge: a
    // hand left resting on the mouse would otherwise mark a second
    // control wherever it happens to sit. Moving the mouse puts the
    // marker away, and the pointer has the bar back.
    if NAV_LIGHT.with(|light| light.get().0.is_some()) || NAV_ELSEWHERE.with(std::cell::Cell::get) {
        return 0.0;
    }
    if hover == Some(control) {
        1.0
    } else {
        0.0
    }
}

use crate::video::ui::light_face;

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
        status_bar_top() * texture_scale,
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
            (status_bar_top() + led_row_label_y(row, rows.len())) * texture_scale,
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
    let counters = track_counter_layout(&view.media);
    if let Some(counter) = counters.fdd {
        draw_track_counter(
            frame,
            counter,
            status.fdd_track,
            TrackPalette::Fdd,
            texture_scale,
        );
    }
    if let Some(counter) = counters.cd {
        draw_track_counter(
            frame,
            counter,
            status.cd_track,
            TrackPalette::Cd,
            texture_scale,
        );
    }
    for idx in 0..4 {
        let drive = view.media.drives[idx];
        if let Some(rect) = layout.drive_load[idx] {
            draw_disk_button(
                frame,
                scale_rect(rect, texture_scale),
                idx,
                lit(hover, BarControl::DriveLoad(idx)),
                texture_scale,
            );
        }
        if let Some(rect) = layout.drive_swap[idx] {
            draw_swap_button(
                frame,
                scale_rect(rect, texture_scale),
                drive.multi && !drive.bridged,
                lit(hover, BarControl::DriveSwap(idx)),
                texture_scale,
            );
        }
        if let Some(rect) = layout.drive_eject[idx] {
            draw_eject_button(
                frame,
                scale_rect(rect, texture_scale),
                // Greyed on a bridged drive: the disk is in a real drive and
                // comes out by hand, not from here.
                drive.inserted && !drive.bridged,
                lit(hover, BarControl::DriveEject(idx)),
                texture_scale,
            );
        }
    }
    if let Some(rect) = layout.cd_load {
        draw_cd_button(
            frame,
            scale_rect(rect, texture_scale),
            lit(hover, BarControl::CdLoad),
            texture_scale,
        );
    }
    if let Some(rect) = layout.cd_eject {
        draw_eject_button(
            frame,
            scale_rect(rect, texture_scale),
            view.media.cd == Some(true),
            lit(hover, BarControl::CdEject),
            texture_scale,
        );
    }
    draw_joystick_button(
        frame,
        scale_rect(joystick_toggle_rect(), texture_scale),
        view.joystick_input_mode,
        lit(hover, BarControl::Joystick),
        texture_scale,
    );
    draw_keyboard_button(
        frame,
        scale_rect(keyboard_toggle_rect(), texture_scale),
        view.keyboard_panel_shown,
        lit(hover, BarControl::Keyboard),
        texture_scale,
    );
    if view.control_connected {
        // A remote control-protocol client is attached; tag the bar so a
        // machine that pauses or steps "by itself" is explicable.
        draw_text(
            frame,
            (KBD_TOGGLE_X.saturating_sub(44)) * texture_scale,
            (status_bar_top() + STATUS_CONTROL_Y + 2) * texture_scale,
            "CCP",
            STATUS_TEXT,
            texture_scale,
        );
    }
    draw_volume_control(
        frame,
        status.output_volume_percent,
        lit(hover, BarControl::Volume),
        texture_scale,
    );
    draw_menu_button(
        frame,
        scale_rect(menu_button_rect(), texture_scale),
        lit(hover, BarControl::Menu),
        texture_scale,
    );
    draw_shot_button(
        frame,
        scale_rect(shot_button_rect(), texture_scale),
        lit(hover, BarControl::Screenshot),
        texture_scale,
    );
    draw_pause_button(
        frame,
        scale_rect(pause_button_rect(), texture_scale),
        view.paused,
        lit(hover, BarControl::Pause),
        texture_scale,
    );
    draw_power_button(
        frame,
        scale_rect(power_button_rect(), texture_scale),
        view.powered_on,
        lit(hover, BarControl::Power),
        texture_scale,
    );
    draw_reboot_button(
        frame,
        scale_rect(reboot_button_rect(), texture_scale),
        lit(hover, BarControl::Reboot),
        texture_scale,
    );
}

/// Per-drive status feeding the media controls in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::video) struct DriveBar {
    /// Drive is wired up this session; unconnected drives get no controls.
    pub(in crate::video) connected: bool,
    /// A disk is currently inserted (enables the eject button).
    pub(in crate::video) inserted: bool,
    /// More than one image is queued for this drive (enables swap).
    pub(in crate::video) multi: bool,
    /// Backed by a real drive on a bridge. The buttons still draw, so the
    /// drive is visibly there and numbered, but there is no media for the
    /// emulator to load, swap, or eject -- the disk is in someone's hand.
    pub(in crate::video) bridged: bool,
}

/// Removable-media status for the bar: the floppy drives plus the CD
/// drive (None on machines without one, Some(disc inserted) otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) struct MediaBar {
    pub(in crate::video) drives: [DriveBar; 4],
    pub(in crate::video) cd: Option<bool>,
}

/// Everything draw_status_bar needs for one frame.
pub(super) struct StatusBarView {
    pub(super) status: FrontPanelStatus,
    pub(super) powered_on: bool,
    pub(super) paused: bool,
    pub(super) media: MediaBar,
    /// Active host joystick source, shown by the status-bar toggle icon.
    pub(super) joystick_input_mode: JoystickInputMode,
    /// Whether the on-screen keyboard is up, so its toggle can show it.
    pub(super) keyboard_panel_shown: bool,
    pub(super) hover: Option<BarControl>,
    /// A control-protocol client is attached (--control-gui).
    pub(super) control_connected: bool,
}

/// A clickable status-bar control, used for hit-testing and hover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) enum BarControl {
    Power,
    Pause,
    Reboot,
    Screenshot,
    Menu,
    Joystick,
    /// Show or hide the on-screen Amiga keyboard.
    Keyboard,
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
pub(in crate::video) struct BarLayout {
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
pub(in crate::video) fn bar_layout(media: &MediaBar) -> BarLayout {
    let mut layout = BarLayout {
        drive_load: [None; 4],
        drive_swap: [None; 4],
        drive_eject: [None; 4],
        cd_load: None,
        cd_eject: None,
    };
    // Pixel aspect is process-global but a layout must use one coherent
    // origin even if a test changes it concurrently. Live callers are on the
    // main thread; caching it also avoids re-reading the atomic for every
    // connected drive.
    let bar_top = status_bar_top();
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
                bar_top + MEDIA_STACKED_ROW0_Y + row * MEDIA_STACKED_PITCH,
                MEDIA_STACKED_H,
            )
        } else {
            (
                MEDIA_CLUSTER_X + pos * (MEDIA_CLUSTER_W + MEDIA_CLUSTER_GAP),
                bar_top + STATUS_CONTROL_Y,
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
        let (load, eject, _) = cluster(x, bar_top + STATUS_CONTROL_Y, STATUS_CONTROL_H);
        layout.cd_load = Some(load);
        layout.cd_eject = Some(eject);
    }
    layout
}

/// Map a cursor position to the status-bar control under it.
pub(in crate::video) fn control_at(pos: (i32, i32), layout: &BarLayout) -> Option<BarControl> {
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
    if keyboard_toggle_rect().contains(pos) {
        return Some(BarControl::Keyboard);
    }
    if volume_control_hit_rect().contains(pos) {
        return Some(BarControl::Volume);
    }
    None
}

pub(in crate::video) fn status_bar_rect() -> Rect {
    Rect {
        x: 0,
        y: status_bar_top(),
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
            // Lit whenever powered, like a real Amiga: full brightness
            // while the guest holds /LED engaged, dimmed -- never off --
            // once it releases it, as on A500 rev 6 and later boards.
            label: "PWR",
            on: powered_on,
            on_color: if status.power_led_bright {
                POWER_LED_BRIGHT
            } else {
                POWER_LED_DIM
            },
            off_color: POWER_LED_OFF,
            highlight_on: if status.power_led_bright {
                rgba(255, 120, 108)
            } else {
                rgba(196, 62, 54)
            },
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
        y: status_bar_top() + led_row_label_y(row, count) + STATUS_LED_Y_OFFSET,
        w: STATUS_LED_W,
        h: STATUS_LED_H,
    }
}

pub(super) fn fdd_track_counter_rect() -> Rect {
    Rect {
        x: 132,
        y: status_bar_top() + STATUS_CONTROL_Y,
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

/// One digital track display. A single removable-media type gets the
/// original full-size bay; when both floppy and CD drives are present the
/// same fixed bay holds two shallow, vertically stacked displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrackCounterSpec {
    pub(super) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrackCounterLayout {
    pub(super) fdd: Option<TrackCounterSpec>,
    pub(super) cd: Option<TrackCounterSpec>,
}

pub(super) fn track_counter_layout(media: &MediaBar) -> TrackCounterLayout {
    let has_fdd = media.drives.iter().any(|drive| drive.connected);
    let has_cd = media.cd.is_some();
    let full = || TrackCounterSpec {
        rect: fdd_track_counter_rect(),
    };
    let stacked = |y| TrackCounterSpec {
        rect: Rect {
            x: fdd_track_counter_rect().x,
            y: status_bar_top() + y,
            w: fdd_track_counter_rect().w,
            h: 20,
        },
    };
    match (has_fdd, has_cd) {
        (false, false) => TrackCounterLayout {
            fdd: None,
            cd: None,
        },
        (true, false) => TrackCounterLayout {
            fdd: Some(full()),
            cd: None,
        },
        (false, true) => TrackCounterLayout {
            fdd: None,
            cd: Some(full()),
        },
        (true, true) => TrackCounterLayout {
            fdd: Some(stacked(1)),
            cd: Some(stacked(23)),
        },
    }
}

pub(super) fn track_counter_digit_rect(counter: TrackCounterSpec, index: usize) -> Rect {
    if counter.rect == fdd_track_counter_rect() {
        return fdd_track_digit_rect(index);
    }
    Rect {
        x: counter.rect.x + 5 + index * 17,
        y: counter.rect.y + (counter.rect.h.saturating_sub(16)) / 2,
        w: 12,
        h: 16,
    }
}

pub(super) fn shot_button_rect() -> Rect {
    Rect {
        x: SHOT_BUTTON_X,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: SHOT_BUTTON_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn menu_button_rect() -> Rect {
    Rect {
        x: ui::MENU_BUTTON_X,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: ui::MENU_BUTTON_W,
        h: STATUS_CONTROL_H,
    }
}

pub(in crate::video) fn volume_control_hit_rect() -> Rect {
    Rect {
        x: VOLUME_SLIDER_X - 8,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: VOLUME_SLIDER_W + 16,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn joystick_toggle_rect() -> Rect {
    Rect {
        x: JOY_TOGGLE_X,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: JOY_TOGGLE_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn keyboard_toggle_rect() -> Rect {
    Rect {
        x: KBD_TOGGLE_X,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: KBD_TOGGLE_W,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn volume_slider_track_rect() -> Rect {
    Rect {
        x: VOLUME_SLIDER_X,
        y: status_bar_top() + VOLUME_SLIDER_Y,
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
        y: status_bar_top() + STATUS_CONTROL_Y + (STATUS_CONTROL_H - VOLUME_KNOB_H) / 2,
        w: VOLUME_KNOB_W,
        h: VOLUME_KNOB_H,
    }
}

pub(super) fn reboot_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 58,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: 42,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn power_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 108,
        y: status_bar_top() + STATUS_CONTROL_Y,
        w: 42,
        h: STATUS_CONTROL_H,
    }
}

pub(super) fn pause_button_rect() -> Rect {
    Rect {
        x: FB_WIDTH - 158,
        y: status_bar_top() + STATUS_CONTROL_Y,
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

#[derive(Debug, Clone, Copy)]
enum TrackPalette {
    Fdd,
    Cd,
}

impl TrackPalette {
    fn colors(self) -> (u32, u32, u32) {
        match self {
            Self::Fdd => (TRACK_SEGMENT_ON, TRACK_SEGMENT_OFF, TRACK_SEGMENT_HIGHLIGHT),
            Self::Cd => (
                CD_TRACK_SEGMENT_ON,
                CD_TRACK_SEGMENT_OFF,
                CD_TRACK_SEGMENT_HIGHLIGHT,
            ),
        }
    }
}

fn draw_track_counter(
    frame: &mut [u8],
    counter: TrackCounterSpec,
    track: Option<u8>,
    palette: TrackPalette,
    texture_scale: usize,
) {
    let rect = scale_rect(counter.rect, texture_scale);
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
            scale_rect(track_counter_digit_rect(counter, idx), texture_scale),
            ch as char,
            palette.colors(),
            texture_scale,
        );
    }
}

pub(super) fn draw_volume_control(
    frame: &mut [u8],
    percent: u8,
    hovered: f32,
    texture_scale: usize,
) {
    let percent = percent.min(100);
    draw_speaker_glyph(frame, texture_scale);

    let rect = scale_rect(volume_slider_track_rect(), texture_scale);
    // The slider lights under the pointer as the buttons beside it do:
    // it is as clickable as they are, and looked as though it were not.
    // What lights is the track itself -- the rectangle the hand takes
    // hold of -- and not the speaker beside it.
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

    // The knob is what lights: it is the thing the hand takes hold of,
    // and the track it runs in keeps its own colours. Standing open for
    // changing it holds the blue steady instead of breathing.
    let knob = scale_rect(volume_slider_knob_rect(percent), texture_scale);
    fill_rect(
        frame,
        knob,
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hovered),
        texture_scale,
    );
    draw_rect_bevel(
        frame,
        knob,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
}

pub(super) fn draw_button_base(frame: &mut [u8], rect: Rect, hover: f32, texture_scale: usize) {
    let face = light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover);
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
    hover: f32,
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
    hover: f32,
    texture_scale: usize,
) {
    draw_button_base(
        frame,
        rect,
        if enabled { hover } else { 0.0 },
        texture_scale,
    );
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
    hover: f32,
    texture_scale: usize,
) {
    draw_button_base(
        frame,
        rect,
        if enabled { hover } else { 0.0 },
        texture_scale,
    );
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
pub(super) fn draw_cd_button(frame: &mut [u8], rect: Rect, hover: f32, texture_scale: usize) {
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
pub(super) fn draw_menu_button(frame: &mut [u8], rect: Rect, hover: f32, texture_scale: usize) {
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
pub(super) fn draw_shot_button(frame: &mut [u8], rect: Rect, hover: f32, texture_scale: usize) {
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
    let y = (status_bar_top() + STATUS_CONTROL_Y) * s;
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
    colors: (u32, u32, u32),
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
    // Stop the horizontal segments before the vertical pair. A fixed 8px
    // span was correct for the original 12px digit, but overlapped the
    // right-hand strokes when the counter used narrower digits, filling the
    // upper- and lower-right corners.
    let horizontal = rect.w.saturating_sub(2 * thickness);
    let (segment_on, segment_off, segment_highlight) = colors;

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
            if lit { segment_on } else { segment_off },
            texture_scale,
        );
        if lit {
            draw_hline_span(
                frame,
                segment_rect.y,
                segment_rect.x,
                segment_rect.x + segment_rect.w,
                segment_highlight,
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

pub(super) fn draw_reboot_button(frame: &mut [u8], rect: Rect, hover: f32, texture_scale: usize) {
    fill_rect(
        frame,
        rect,
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover),
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
    hover: f32,
    texture_scale: usize,
) {
    fill_rect(
        frame,
        rect,
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover),
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
    hover: f32,
    texture_scale: usize,
) {
    fill_rect(
        frame,
        rect,
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover),
        texture_scale,
    );
    // Paused, the moulding turns over -- the MT-32 panel's pressed
    // effect: the light that caught its top and left falls onto the
    // bottom and right instead, so the button reads as held down for
    // as long as the machine is.
    let (near, far) = if paused {
        (BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT)
    } else {
        (BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK)
    };
    draw_rect_bevel(frame, rect, near, far, texture_scale);
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

/// Joystick input-source toggle: shows the host source currently driving the
/// emulated joystick port (a gamepad in `Gamepad` mode, a keyboard in
/// `Keyboard` mode; with joysticks in both ports the mode picks which source
/// gets the lower-numbered port). Clicking it flips between the two, so the
/// active source is always visible rather than hidden behind a key
/// combination.
pub(super) fn draw_joystick_button(
    frame: &mut [u8],
    rect: Rect,
    mode: JoystickInputMode,
    hover: f32,
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

/// The on-screen keyboard's toggle: the same little keyboard the joystick
/// toggle wears in its key-mapping mode, lit while the strip is up and dark
/// while it is away, so the button says which it is without a caption.
pub(super) fn draw_keyboard_button(
    frame: &mut [u8],
    rect: Rect,
    shown: bool,
    hover: f32,
    texture_scale: usize,
) {
    draw_button_base(frame, rect, hover, texture_scale);
    let keys = if shown {
        BUTTON_GLYPH
    } else {
        BUTTON_GLYPH_DISABLED
    };
    draw_keyboard_glyph_in(frame, rect, keys, texture_scale);
}

/// A small keyboard: a recessed dark case holding two rows of green keys and a
/// space bar.
pub(super) fn draw_keyboard_glyph(frame: &mut [u8], rect: Rect, texture_scale: usize) {
    draw_keyboard_glyph_in(frame, rect, BUTTON_GLYPH, texture_scale);
}

/// The same keyboard with its keys in a chosen colour.
fn draw_keyboard_glyph_in(frame: &mut [u8], rect: Rect, keys: u32, texture_scale: usize) {
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
        cell(kx, 8, 2, 2, keys);
        cell(kx, 11, 2, 2, keys);
    }
    // Space bar.
    cell(7, 14, 8, 2, keys);
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
    draw_power_glyph_sized(frame, cx, cy, 5.5, color, texture_scale);
}

/// The same mark at a chosen radius, for panels with less room than the
/// status bar. `radius` is in unscaled pixels; the stroke follows it so the
/// ring keeps its proportions.
pub(super) fn draw_power_glyph_sized(
    frame: &mut [u8],
    cx: usize,
    cy: usize,
    radius_px: f32,
    color: u32,
    texture_scale: usize,
) {
    let scale = texture_scale as f32;
    let ccx = cx as f32 + 0.5;
    let ccy = cy as f32 + 0.5 + 0.5 * scale;
    let radius = radius_px * scale;
    let stroke = (radius_px / 4.07).max(0.75) * scale;

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

/// The reboot symbol: a near-full ring broken at the upper left with a bold
/// arrowhead pointing counter-clockwise.
pub(super) fn draw_reset_glyph(frame: &mut [u8], cx: usize, cy: usize, texture_scale: usize) {
    let scale = texture_scale as f32;
    let ccx = cx as f32 + 0.5;
    let ccy = cy as f32 + 0.5;
    let radius = 5.5 * scale;
    let stroke = 1.35 * scale;

    let start = 165.0_f32.to_radians();
    let sweep = 260.0_f32.to_radians();
    let steps = 28;
    let ang = |t: f32| start - sweep * t;
    let mut prev = {
        let a = ang(0.0);
        (ccx + radius * a.cos(), ccy + radius * a.sin())
    };
    for step in 1..=steps {
        let a = ang(step as f32 / steps as f32);
        let next = (ccx + radius * a.cos(), ccy + radius * a.sin());
        draw_thick_line(
            frame,
            prev.0,
            prev.1,
            next.0,
            next.1,
            stroke,
            RESET_GLYPH,
            texture_scale,
        );
        prev = next;
    }

    // Arrowhead anchored to the arc end: base centred on the ring path and
    // perpendicular to the tangent, tip continuing the direction of travel.
    // The forward half of the stroke's rounded end cap falls inside the
    // triangle and the rear half coincides with the final arc segment's own
    // stroke, so the glyph reads as one arc ending in an arrowhead.
    let end = ang(1.0);
    let ex = ccx + radius * end.cos();
    let ey = ccy + radius * end.sin();
    let (tx, ty) = (end.sin(), -end.cos());
    let (nx, ny) = (end.cos(), end.sin());
    let half_w = 2.4 * scale;
    let len = 3.6 * scale;
    let arrow = [
        (ex + half_w * nx, ey + half_w * ny),
        (ex - half_w * nx, ey - half_w * ny),
        (ex + len * tx, ey + len * ty),
    ];
    fill_triangle(frame, arrow, RESET_GLYPH, texture_scale);
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
/// `corner_inset` is the same figure the other two overlays take: the
/// badge shares the performance readout's corner and loses it the same way.
/// `anchor` is the visible display region in canvas pixels `(x, y, w,
/// h)` -- the whole display classically, the crop rect under autocrop --
/// so the badge stays in the corner the viewer actually sees.
pub(super) fn draw_record_badge(
    frame: &mut [u8],
    texture_scale: usize,
    corner_inset: usize,
    anchor: (usize, usize, usize, usize),
) {
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
    let box_x = ((anchor.0 + anchor.2) * s).saturating_sub(margin + corner_inset + box_w);
    let box_y = anchor.1 * s + margin + corner_inset;

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

/// Right-aligned performance readout in the top-right of the display
/// (Cmd/Alt+P, `[display] perf_overlay`): one line per data point, same
/// font size as the menus (it follows the Menu Size setting, so it stays
/// small at the default 1x rather than taking the larger OSD size).
/// Painted into the presentation texture only, like the record badge, so
/// captures never include it; while a recording badge is up the block
/// starts below it instead of fighting for the corner.
/// The guest's fn-88 debug overlay (crate::uaelib): rects and text in a
/// 768x576 PAL hires space, mapped proportionally onto the display
/// sub-rect the viewer sees (`anchor`, canvas pixels) -- the way
/// Bartman's WinUAE fork stretches its overlay buffer over the picture.
/// Painted into the presentation texture only, after the frame capture
/// copy, so it can never appear in screenshots, dumps or recordings.
/// Cached rasterization of the guest's fn-88 overlay. The display list
/// is replayed into an anchor-sized RGBA buffer only when the list or
/// the destination geometry changes; every redraw then pays one bounded
/// alpha-tested composite of the anchor rect, so a large legal list (or
/// one the guest has stopped touching) cannot stall the window by being
/// replayed at full cost on every frame. Drawing into the local buffer
/// also clips everything -- text glyphs included -- to the anchor, so a
/// cropped sub-rect presentation never leaks overlay pixels into the
/// letterbox or host chrome.
#[derive(Default)]
pub(super) struct GuestOverlayCache {
    key: u64,
    w: usize,
    h: usize,
    pixels: Vec<u8>,
}

/// Rasterization work budget, in filled pixels, per list change: enough
/// for many full-anchor fills, far short of the hundreds of millions of
/// writes a maximal hostile list could otherwise demand in one pass.
/// Commands past the budget are dropped from the tail, matching the
/// trap's own drop-newest rule at the list cap.
const OVERLAY_RASTER_BUDGET_FILLS: usize = 32;

pub(super) fn draw_guest_overlay(
    frame: &mut [u8],
    cache: &mut GuestOverlayCache,
    cmds: &[crate::uaelib::OverlayCmd],
    texture_scale: usize,
    anchor: (usize, usize, usize, usize),
) {
    let s = texture_scale;
    let (ax, ay, aw, ah) = anchor;
    let (x0, y0, w, h) = (ax * s, ay * s, aw * s, ah * s);
    if w == 0 || h == 0 {
        return;
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (s, anchor).hash(&mut hasher);
        cmds.hash(&mut hasher);
        hasher.finish()
    };
    if cache.key != key || cache.w != w || cache.h != h {
        rasterize_guest_overlay(cache, cmds, s, w, h);
        cache.key = key;
    }
    // Composite: drawn pixels carry alpha 0xFF, untouched ones 0; only
    // the drawn ones reach the presentation.
    let (tex_w, tex_h) = (texture_width(s), texture_height(s));
    for row in 0..h {
        let fy = y0 + row;
        if fy >= tex_h {
            break;
        }
        for col in 0..w {
            let fx = x0 + col;
            if fx >= tex_w {
                break;
            }
            let src = (row * w + col) * 4;
            if cache.pixels[src + 3] == 0 {
                continue;
            }
            let dst = (fy * tex_w + fx) * 4;
            frame[dst..dst + 4].copy_from_slice(&cache.pixels[src..src + 4]);
        }
    }
}

fn rasterize_guest_overlay(
    cache: &mut GuestOverlayCache,
    cmds: &[crate::uaelib::OverlayCmd],
    texture_scale: usize,
    w: usize,
    h: usize,
) {
    use crate::uaelib::{OverlayCmd, OVERLAY_HEIGHT, OVERLAY_WIDTH};
    cache.w = w;
    cache.h = h;
    cache.pixels.clear();
    cache.pixels.resize(w * h * 4, 0);
    let map_x = |v: u16| usize::from(v) * w / OVERLAY_WIDTH as usize;
    let map_y = |v: u16| usize::from(v) * h / OVERLAY_HEIGHT as usize;
    // 0x00RRGGBB -> the texture's memory order (R low byte, alpha high).
    let guest_colour =
        |c: u32| 0xFF00_0000 | ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF);
    let mut budget = OVERLAY_RASTER_BUDGET_FILLS.saturating_mul(w * h);
    let fill = |buf: &mut [u8],
                budget: &mut usize,
                x: usize,
                y: usize,
                fw: usize,
                fh: usize,
                colour: u32| {
        let x1 = (x + fw).min(w);
        let y1 = (y + fh).min(h);
        let (x, y) = (x.min(w), y.min(h));
        let area = x1.saturating_sub(x) * y1.saturating_sub(y);
        if area > *budget {
            *budget = 0;
            return;
        }
        *budget -= area;
        let bytes = colour.to_le_bytes();
        for yy in y..y1 {
            for xx in x..x1 {
                let off = (yy * w + xx) * 4;
                buf[off..off + 4].copy_from_slice(&bytes);
            }
        }
    };
    let thickness = texture_scale.max(1);
    // Text keeps whole glyph blocks at the nearest integer scale from
    // the overlay space (rounded, floored at 1: flooring rendered the
    // text half-sized against its rectangles on HiDPI windows, where
    // the ratio sits just under the next whole scale).
    let px = ((w as f32 / OVERLAY_WIDTH as f32).round() as usize)
        .min((h as f32 / OVERLAY_HEIGHT as f32).round() as usize)
        .max(1);
    for cmd in cmds {
        if budget == 0 {
            log::warn!("guest overlay: rasterization budget exhausted; dropping trailing commands");
            break;
        }
        match cmd {
            OverlayCmd::FilledRect { l, t, r, b, colour } => {
                let (x, y) = (map_x(*l), map_y(*t));
                fill(
                    &mut cache.pixels,
                    &mut budget,
                    x,
                    y,
                    map_x(*r).saturating_sub(x),
                    map_y(*b).saturating_sub(y),
                    guest_colour(*colour),
                );
            }
            OverlayCmd::Rect { l, t, r, b, colour } => {
                let (x, y) = (map_x(*l), map_y(*t));
                let (rw, rh) = (map_x(*r).saturating_sub(x), map_y(*b).saturating_sub(y));
                let colour = guest_colour(*colour);
                fill(
                    &mut cache.pixels,
                    &mut budget,
                    x,
                    y,
                    rw,
                    thickness.min(rh),
                    colour,
                );
                fill(
                    &mut cache.pixels,
                    &mut budget,
                    x,
                    (y + rh).saturating_sub(thickness).max(y),
                    rw,
                    thickness.min(rh),
                    colour,
                );
                fill(
                    &mut cache.pixels,
                    &mut budget,
                    x,
                    y,
                    thickness.min(rw),
                    rh,
                    colour,
                );
                fill(
                    &mut cache.pixels,
                    &mut budget,
                    (x + rw).saturating_sub(thickness).max(x),
                    y,
                    thickness.min(rw),
                    rh,
                    colour,
                );
            }
            OverlayCmd::Text { l, t, text, colour } => {
                // Glyph work is bounded like the fills, by the text's
                // covered area.
                let area = text.chars().count() * 8 * 8 * px * px;
                if area > budget {
                    budget = 0;
                    continue;
                }
                budget -= area;
                // Drawn into the anchor-sized buffer, so glyphs clip to
                // the anchor exactly like the rectangles.
                font::draw_text(
                    &mut cache.pixels,
                    w,
                    h,
                    map_x(*l),
                    map_y(*t),
                    text,
                    guest_colour(*colour),
                    px,
                );
            }
        }
    }
}

pub(super) fn draw_perf_overlay(
    frame: &mut [u8],
    lines: &[String],
    texture_scale: usize,
    below_record_badge: bool,
    corner_inset: usize,
    anchor: (usize, usize, usize, usize),
) {
    let s = texture_scale;
    let px = crate::video::menu_scale().factor() * s;
    let pad = 4 * s;
    let margin = 8 * s;
    let line_gap = 2 * s;

    let text_h = font::text_height(px);
    let text_w = lines
        .iter()
        .map(|line| font::text_width(line, px))
        .max()
        .unwrap_or(0);
    if text_w == 0 {
        return;
    }
    let box_w = text_w + 2 * pad;
    let box_h = lines.len() * text_h + lines.len().saturating_sub(1) * line_gap + 2 * pad;
    // Away from the top-right corner on both axes, for the same reason and
    // by the same figure as the OSD leaves the bottom-left one.
    let box_x = ((anchor.0 + anchor.2) * s).saturating_sub(margin + corner_inset + box_w);
    let record_badge_h = font::text_height(2 * s) + 2 * (4 * s);
    let box_y = anchor.1 * s
        + corner_inset
        + if below_record_badge {
            margin + record_badge_h + 4 * s
        } else {
            margin
        };

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
    let fw = texture_width(s);
    let fh = texture_height(s);
    let mut y = box_y + pad;
    for line in lines {
        let x = box_x + pad + text_w - font::text_width(line, px);
        font::draw_text(frame, fw, fh, x + s, y + s, line, OSD_SHADOW, px);
        font::draw_text(frame, fw, fh, x, y, line, OSD_TEXT, px);
        y += text_h + line_gap;
    }
}

/// `corner_inset` is how far a drawn monitor front and a bowed preset cut
/// into the picture's corners (0 when neither is drawn). The message sits
/// in the bottom-left one and comes away from it diagonally -- in from the
/// left and up from the bottom by the same amount -- because that is the
/// direction the corner is missing in, and it is the placement the figure
/// was solved for. Moving on one axis alone would need a different figure
/// and a much longer run.
pub(super) fn draw_osd(
    frame: &mut [u8],
    text: &str,
    warning: bool,
    texture_scale: usize,
    corner_inset: usize,
    anchor: (usize, usize, usize, usize),
) {
    let s = texture_scale;
    let px = 2 * s; // font pixel -> device pixels
    let pad = 4 * s;
    let margin = 8 * s;
    let fw = texture_width(s);
    // The bottom-left of the visible display region (`anchor`, in canvas
    // pixels: the whole display classically, the crop rect under
    // autocrop): above the MT-32 panel as well as the status bar, since
    // the message belongs over the picture, not the instrument under it.
    let anchor_right = ((anchor.0 + anchor.2) * s).min(fw);
    let display_h = (anchor.1 + anchor.3) * s;

    let text_w = font::text_width(text, px)
        .min(anchor_right.saturating_sub(anchor.0 * s + 2 * margin + corner_inset + 2 * pad));
    let text_h = font::text_height(px);
    let box_h = text_h + 2 * pad;
    let box_x = anchor.0 * s + margin + corner_inset;
    // What is left of the width once the box has come in off the corner.
    let box_w = (text_w + 2 * pad).min(anchor_right.saturating_sub(box_x + margin));
    let box_y = display_h.saturating_sub(margin + box_h + corner_inset);

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
        if warning { OSD_TEXT_WARNING } else { OSD_TEXT },
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

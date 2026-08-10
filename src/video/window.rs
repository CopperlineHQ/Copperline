// SPDX-License-Identifier: GPL-3.0-or-later

//! winit + pixels integration. The emulator core runs synchronously on the
//! main thread inside `about_to_wait`; by default a worker renders the
//! completed frame while the main thread advances the next frame. winit and
//! wgpu presentation stay on the main thread.

use super::deinterlace::{Deinterlacer, OUT_HEIGHT};
use super::launcher::{LauncherField, LauncherState, MachineSetup, StatusMessage};
use super::ui::{self, Panel, UiControl, UiState};
use super::{
    bitplane, font, present_height, FB_HEIGHT, FB_PIXELS, FB_WIDTH, HOST_SHORTCUT_MODIFIER_LABEL,
    MAX_CANVAS_PIXELS, MAX_VISIBLE_LINES, PRESENT_HEIGHT_SQUARE,
};
use crate::audio::{AudioSink, CpalSink};
use crate::bus::{BeamWriteSource, FrontPanelStatus, PortDevice, VideoRenderFrameTiming};
use crate::config::{
    BezelStyle, Config, DisplayScaling, Overscan, PixelAspect, RawConfig, WarpSpeed,
};
use crate::emulator::Emulator;
use crate::heatmap;
use crate::keymap;
use crate::screenshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButtonKind {
    Left,
    Right,
    Middle,
}

/// A port-2 joystick/CD32-pad control scripted with `--joy-after`. Red
/// and Blue are the pad's fire/second buttons (a plain joystick's fire
/// is Red); the other five only exist in the CD32 pad's serial report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoyButtonKind {
    Up,
    Down,
    Left,
    Right,
    Red,
    Blue,
    Green,
    Yellow,
    Play,
    Rewind,
    Forward,
}

impl JoyButtonKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "red" | "fire" | "button1" => Self::Red,
            "blue" | "button2" => Self::Blue,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "play" | "pause" => Self::Play,
            "rwd" | "rewind" | "reverse" => Self::Rewind,
            "ffw" | "forward" => Self::Forward,
            _ => return None,
        })
    }
}

// The host input source for the emulated port-2 joystick/CD32 pad is a
// configurable value, so it lives with the other config enums; re-exported
// here because the window/menu/ui code refers to it as
// `crate::video::window::JoystickInputMode`.
pub use crate::config::JoystickInputMode;

/// Where each host input source lands this quantum; see
/// [`host_routing_for`]. Ports are 0-based.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostRouting {
    /// Port the host mouse drives: the lowest-numbered mouse port.
    pub(crate) mouse: Option<usize>,
    /// Port the physical gamepad drives (joystick/CD32 devices only).
    pub(crate) gamepad: Option<usize>,
    /// Port keyboard mapping 0 (cursor keys) drives; the device there may
    /// be a joystick/pad or a mouse (keyboard mouse emulation).
    pub(crate) keyboard: Option<usize>,
    /// Port keyboard mapping 1 (numpad) drives, as the gamepad port's
    /// stand-in in a two-controller setup.
    pub(crate) keyboard2: Option<usize>,
}

/// The host input sources' port assignment for a device wiring and
/// joystick-input mode. Pure, and shared by the live input pump and the
/// launcher's Input-tab summary, so what the GUI promises is exactly what
/// the pump does. The rules are documented on [`App::host_routing`].
pub(crate) fn host_routing_for(devices: [PortDevice; 2], mode: JoystickInputMode) -> HostRouting {
    let mouse = devices.iter().position(|&d| d == PortDevice::Mouse);
    let mut remaining = (0..2).filter(|&p| {
        Some(p) != mouse
            && matches!(
                devices[p],
                PortDevice::Mouse | PortDevice::Joystick | PortDevice::Cd32Pad
            )
    });
    let first = remaining.next();
    let second = remaining.next();
    let (gamepad, keyboard, keyboard2) = match (first, second, mode) {
        (None, _, _) => (None, None, None),
        (Some(p), None, JoystickInputMode::Gamepad) => {
            if devices[p] == PortDevice::Mouse {
                (None, None, None)
            } else {
                (Some(p), None, None)
            }
        }
        (Some(p), None, JoystickInputMode::Keyboard) => (None, Some(p), None),
        // Two leftover ports are always joysticks/pads: a second mouse
        // would itself have been claimed as the mouse port.
        (Some(p), Some(q), JoystickInputMode::Gamepad) => (Some(p), Some(q), Some(p)),
        (Some(p), Some(q), JoystickInputMode::Keyboard) => (Some(q), Some(p), Some(q)),
    };
    HostRouting {
        mouse,
        gamepad,
        keyboard,
        keyboard2,
    }
}

/// Quadrature counter steps per scheduler quantum (~one frame) while a
/// keyboard-mouse direction key is held: ~150 counts/second at PAL frame
/// rate, a comfortable Workbench pointer speed.
const KEYBOARD_MOUSE_COUNTS_PER_QUANTUM: i32 = 3;

/// One port's controls currently held by `--joy-after` scripting.
#[derive(Debug, Default, Clone, Copy)]
struct AutoJoyHeld {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    red: bool,
    blue: bool,
    green: bool,
    yellow: bool,
    play: bool,
    rwd: bool,
    ffw: bool,
}

impl AutoJoyHeld {
    fn set(&mut self, button: JoyButtonKind, held: bool) {
        match button {
            JoyButtonKind::Up => self.up = held,
            JoyButtonKind::Down => self.down = held,
            JoyButtonKind::Left => self.left = held,
            JoyButtonKind::Right => self.right = held,
            JoyButtonKind::Red => self.red = held,
            JoyButtonKind::Blue => self.blue = held,
            JoyButtonKind::Green => self.green = held,
            JoyButtonKind::Yellow => self.yellow = held,
            JoyButtonKind::Play => self.play = held,
            JoyButtonKind::Rewind => self.rwd = held,
            JoyButtonKind::Forward => self.ffw = held,
        }
    }
}

pub const DEFAULT_KEY_HOLD_MS: u32 = 100;
/// Emulated-frame gap between reverse-debug snapshots when the ring is
/// auto-armed by opening the debugger window. Larger than the headless
/// default to keep the per-snapshot serialize off the interactive path.
const DEBUGGER_REVERSE_INTERVAL_FRAMES: u64 = 10;
const MAX_TEXTURE_SCALE: usize = 2;
/// Cap on the integer-scaling supersample factor, which follows the window
/// fit rather than the host DPI (see `plan_present_scaling`). Bounds the
/// backing texture and the per-frame present copy on very large displays;
/// at 4x the canvas the picture is already 2864 physical pixels wide, and
/// beyond it PixelPerfect's own whole multiples of the capped texture keep
/// the fit integer.
const MAX_INTEGER_TEXTURE_SCALE: usize = 4;
const STATUS_BAR_HEIGHT: usize = 44;
/// Logical window height: the presentation canvas for the active pixel aspect,
/// plus the status bar below it unless it is hidden (in which case the display
/// scales to fill the whole window).
fn window_present_height() -> usize {
    present_height() + mt32_panel_height() + keyboard_panel_height() + status_bar_height()
}

/// Scanlines the CRT pass draws across the display rect: the emulated field
/// lines the present copy actually puts on screen, rescaled when that copy
/// letterboxes them inside the rect.
///
/// `tv_aperture_rows` mirrors `copy_window_present_frame`'s own branch
/// condition: Some when that path shows a TV-aperture crop instead of the
/// whole woven buffer, carrying the crop's row count, so the line count
/// comes from the aperture, not from `present_rows` -- 270 lines, not 285,
/// in the default 50 Hz TV-overscan presentation, 214 on a 60 Hz scan. The
/// square-pixel canvas is taller than the aperture and pads it with bezel
/// rows (`tv_aperture_source_row`), so the count scales back up by the
/// rect/content ratio to keep the pitch right across the whole viewport.
fn crt_scanline_count(
    present_rows: usize,
    present_h: usize,
    tv_aperture_rows: Option<usize>,
) -> f32 {
    let (woven_rows, content_rows) = if let Some(aperture_rows) = tv_aperture_rows {
        let pad = if present_h == PRESENT_HEIGHT_SQUARE {
            present_h.saturating_sub(aperture_rows) / 2
        } else {
            0
        };
        (aperture_rows, present_h - 2 * pad)
    } else {
        (present_rows, present_h)
    };
    // Two woven rows per emulated field line. The pass never runs on a
    // programmable scan, whose fields are not woven at all.
    let lines = (woven_rows / 2).max(1);
    if content_rows == 0 {
        return lines as f32;
    }
    (lines * present_h) as f32 / content_rows as f32
}

/// The status bar's height, or 0 while it is hidden.
fn status_bar_height() -> usize {
    if super::status_bar_hidden() {
        0
    } else {
        STATUS_BAR_HEIGHT
    }
}

/// Where the status bar starts: below the display and below whichever
/// strips are up. The bar sits at the very bottom either way; the strips
/// take the room immediately above it.
fn status_bar_top() -> usize {
    keyboard_panel_top() + keyboard_panel_height()
}

/// The MT-32 panel's height, or 0 while it is not shown. It sits between the
/// display and the status bar, the way the real unit sits under the monitor.
fn mt32_panel_height() -> usize {
    #[cfg(feature = "mt32")]
    if super::mt32_panel_shown() {
        return mt32panel::MT32_PANEL_HEIGHT;
    }
    0
}

/// Where the on-screen keyboard starts: under the display and under the
/// MT-32's panel, the way a keyboard sits below whatever is on the desk.
fn keyboard_panel_top() -> usize {
    present_height() + mt32_panel_height()
}

/// The on-screen keyboard's height, or 0 while it is not shown.
fn keyboard_panel_height() -> usize {
    if super::keyboard_panel_shown() {
        kbdpanel::KBD_PANEL_HEIGHT
    } else {
        0
    }
}
const STATUS_LABEL_X: usize = 18;
const STATUS_LED_X: usize = 58;
const STATUS_LED_Y_OFFSET: usize = 1;
const STATUS_LED_W: usize = 58;
const STATUS_LED_H: usize = 9;
// LED rows (PWR/FDD always; HDD and CD when the machine has them) are
// spaced like the original three fixed rows up to three rows, and packed
// tighter when a machine shows all four.
const LED_ROW_START_Y: usize = 4;
const LED_ROW_PITCH: usize = 14;
const LED_ROW_START_Y_TIGHT: usize = 1;
const LED_ROW_PITCH_TIGHT: usize = 11;
const STATUS_CONTROL_H: usize = 22;
const STATUS_CONTROL_Y: usize = (STATUS_BAR_HEIGHT - STATUS_CONTROL_H) / 2;
const VOLUME_STEP_PERCENT: i16 = 5;
/// Per-press step of the live sampler input-gain control, in decibels; the
/// range is the sampler's [`crate::sampler::MIN_SAMPLER_GAIN_DB`]..
/// [`crate::sampler::MAX_SAMPLER_GAIN_DB`].
const SAMPLER_GAIN_STEP_DB: f32 = 3.0;

/// Label a sampler gain in decibels for the OSD/menu, e.g. `0 dB`, `+6 dB`.
fn sampler_gain_osd(gain_db: f32) -> String {
    if gain_db.abs() < 0.05 {
        "0 dB".to_string()
    } else {
        format!("{gain_db:+.0} dB")
    }
}
// Media (floppy/CD) button clusters. Each connected drive gets a wide
// load button plus narrow swap and eject buttons; a CD machine gets a
// load and an eject button after the drives.
const MEDIA_CLUSTER_X: usize = 198;
// With three or four drives the clusters stack two-up in slightly
// shorter rows, so the bar never has to shed the track counter.
const MEDIA_STACKED_H: usize = 19;
const MEDIA_STACKED_ROW0_Y: usize = 2;
const MEDIA_STACKED_PITCH: usize = 21;
const MEDIA_CLUSTER_GAP: usize = 6;
const MEDIA_CD_GAP: usize = 12;
const MEDIA_LOAD_W: usize = 22;
const MEDIA_SMALL_W: usize = 16;
const MEDIA_INNER_GAP: usize = 2;
const MEDIA_CLUSTER_W: usize = MEDIA_LOAD_W + 2 * (MEDIA_INNER_GAP + MEDIA_SMALL_W);
// Screenshot button, menu button, and volume control, pinned on the
// right ahead of the pause/power/reboot block. The menu button anchor
// lives in `ui` so the pop-up menu can align with it.
const SHOT_BUTTON_X: usize = FB_WIDTH - 190;
const SHOT_BUTTON_W: usize = 22;
const VOLUME_SLIDER_X: usize = ui::MENU_BUTTON_X - 10 - VOLUME_SLIDER_W;
const VOLUME_SLIDER_Y: usize = STATUS_CONTROL_Y + 7;
// The slider is as wide as the slot between the media controls and the menu
// button leaves once the two icon toggles below have taken their 24 pixels
// each: the bar has one free run of x, and every control on it competes for
// the same worst case (four floppies plus a CD, ending at x=372).
const VOLUME_SLIDER_W: usize = 48;
const VOLUME_SLIDER_H: usize = 8;
const VOLUME_KNOB_W: usize = 8;
const VOLUME_KNOB_H: usize = 16;
const VOLUME_GLYPH_X: usize = VOLUME_SLIDER_X - 16;
// Joystick input-source and on-screen-keyboard toggles: compact icon buttons
// just left of the volume glyph, in the otherwise-free slot before the
// right-hand control cluster. The widest media layout (four floppies plus a
// CD) ends at x=372, so the pair of 22px buttons here clears both the media
// controls and the speaker glyph; this is verified by
// `joystick_toggle_clears_worst_case_media` and
// `keyboard_toggle_clears_worst_case_media`.
const JOY_TOGGLE_W: usize = 22;
const JOY_TOGGLE_X: usize = VOLUME_GLYPH_X - 2 - JOY_TOGGLE_W;
const KBD_TOGGLE_W: usize = 22;
const KBD_TOGGLE_X: usize = JOY_TOGGLE_X - 2 - KBD_TOGGLE_W;
// The standard-window and TV-aperture constants live in
// `video/present_common.rs` with the presentation helpers they anchor
// (re-exported through `use present::*` below). Both the live window and
// the PNG paths present the captured aperture -- every glass pixel
// derives from real framebuffer pixels, the standard window and the
// visible raster both exactly centred. On the 4:3 canvas the aperture
// resamples onto the full glass width; the square-pixel canvas keeps
// unit columns and centres the aperture between these black side pads
// instead.
const TV_LIVE_PAD_X: usize = (FB_WIDTH - TV_CAPTURED_WIDTH) / 2;
// Symmetric pads are what centres the square-pixel raster; a change to
// the captured aperture's width that breaks this must rethink the live
// layout.
const _: () = assert!(TV_LIVE_PAD_X * 2 + TV_CAPTURED_WIDTH == FB_WIDTH);
pub(super) const STATUS_BG: u32 = rgba(28, 28, 26);
pub(super) const STATUS_TOP: u32 = rgba(78, 76, 70);
const STATUS_BOTTOM: u32 = rgba(12, 12, 11);
const LED_BEZEL_DARK: u32 = rgba(8, 8, 7);
const LED_BEZEL_LIGHT: u32 = rgba(78, 76, 68);
// The power LED is lit whenever the machine is powered, driven by CIA-A's
// /LED line the way it drives the LED on an A500 rev 6 or later board:
// POWER_LED_BRIGHT while the guest holds /LED engaged (Paula's filter on),
// falling to the clearly dimmer -- but still lit -- POWER_LED_DIM once it
// releases the line. Earlier boards extinguished the LED instead; the
// panel models the common two-level behaviour. POWER_LED_OFF is the
// unpowered bezel.
const POWER_LED_BRIGHT: u32 = rgba(255, 38, 28);
const POWER_LED_DIM: u32 = rgba(150, 24, 18);
const POWER_LED_OFF: u32 = rgba(66, 12, 10);
const FDD_LED_ON: u32 = rgba(236, 142, 28);
const FDD_LED_OFF: u32 = rgba(72, 38, 10);
const HDD_LED_ON: u32 = rgba(44, 200, 80);
const HDD_LED_OFF: u32 = rgba(14, 56, 24);
const CD_LED_ON: u32 = rgba(64, 170, 234);
const CD_LED_OFF: u32 = rgba(16, 46, 70);
const TRACK_DISPLAY_BG: u32 = rgba(6, 8, 6);
const TRACK_SEGMENT_ON: u32 = rgba(27, 220, 71);
const TRACK_SEGMENT_OFF: u32 = rgba(11, 45, 19);
const TRACK_SEGMENT_HIGHLIGHT: u32 = rgba(119, 255, 141);
pub(super) const BUTTON_FACE: u32 = rgba(46, 46, 43);
pub(super) const BUTTON_FACE_HOVER: u32 = rgba(62, 62, 58);
pub(super) const BUTTON_EDGE_LIGHT: u32 = rgba(118, 116, 106);
pub(super) const BUTTON_EDGE_DARK: u32 = rgba(13, 13, 12);
const BUTTON_GLYPH: u32 = rgba(0, 174, 0);
/// Glyph colour for visible-but-inactive controls (eject with no disk,
/// swap with no other disk queued).
const BUTTON_GLYPH_DISABLED: u32 = rgba(96, 94, 86);
const POWER_GLYPH_ON: u32 = rgba(0, 174, 0);
const POWER_GLYPH_OFF: u32 = rgba(150, 36, 30);
const RESET_GLYPH: u32 = rgba(250, 200, 40);
const DISK_BODY: u32 = rgba(28, 82, 184);
const DISK_BODY_HIGHLIGHT: u32 = rgba(74, 139, 238);
const DISK_BODY_SHADOW: u32 = rgba(8, 26, 84);
const DISK_SHUTTER: u32 = rgba(184, 191, 196);
const DISK_SHUTTER_DARK: u32 = rgba(83, 91, 98);
const DISK_LABEL: u32 = rgba(238, 240, 232);
const DISK_LABEL_LINE: u32 = rgba(130, 139, 150);
const CD_BODY: u32 = rgba(186, 193, 202);
const CD_SHEEN: u32 = rgba(240, 244, 250);
const CD_HUB: u32 = rgba(120, 124, 130);
const CD_HOLE: u32 = rgba(24, 24, 26);
const CAMERA_BODY: u32 = rgba(190, 188, 178);
const CAMERA_LENS: u32 = rgba(20, 22, 24);
pub(super) const STATUS_TEXT: u32 = rgba(174, 170, 154);
const VOLUME_FILL: u32 = rgba(44, 178, 94);
const VOLUME_FILL_HIGHLIGHT: u32 = rgba(128, 244, 150);
const WINDOW_TITLE: &str = concat!("Copperline ", env!("COPPERLINE_DISPLAY_VERSION"));
const COPPERLINE_LOGO_PNG: &[u8] = include_bytes!("../../assets/brand/copperline-logo.png");
const COPPERLINE_ICON_PNG: &[u8] = include_bytes!("../../assets/brand/copperline-icon.png");
const MOUSE_MOTION_SCALE: f64 = 1.0;

/// Which front `Cmd/Alt+M` should switch back on for a starting style: the
/// style itself when one is chosen, and otherwise the default front, so the
/// shortcut has something to turn on from a session that starts with the
/// bezel off.
fn last_bezel_style(style: BezelStyle) -> BezelStyle {
    if style.is_on() {
        style
    } else {
        BezelStyle::Model1084
    }
}

/// Whether a window's logical inner size equals the presentation canvas
/// (FB_WIDTH x `canvas_height`) within a small rounding tolerance -- i.e. the
/// user has not manually resized it.
fn logical_size_is_canvas(logical_w: f64, logical_h: f64, canvas_height: usize) -> bool {
    (logical_w - FB_WIDTH as f64).abs() < 2.0 && (logical_h - canvas_height as f64).abs() < 2.0
}

const CANVAS_SNAP_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a resize still belongs to the presentation canvas. The first
/// event after an asynchronous `request_inner_size` is the platform's answer
/// even when a window-manager limit clamps it far away from the requested
/// dimensions. A platform may ignore the request entirely, so the ownership
/// expires rather than swallowing a user resize much later.
fn resize_is_canvas_owned(
    snap_request_deadline: &mut Option<Instant>,
    now: Instant,
    logical_w: f64,
    logical_h: f64,
    canvas_height: usize,
) -> bool {
    snap_request_deadline
        .take()
        .is_some_and(|deadline| now <= deadline)
        || logical_size_is_canvas(logical_w, logical_h, canvas_height)
}

/// Host mouse speed multiplier for a 0-100 sensitivity. Exponential so 50 is
/// exactly 1:1 (2^0), 0 is a quarter speed (2^-2) and 100 quadruple (2^2),
/// with perceptually even steps between.
fn mouse_sensitivity_factor(sensitivity: u8) -> f64 {
    2.0_f64.powf((f64::from(sensitivity.min(100)) - 50.0) / 25.0)
}
/// How long a transient on-screen overlay message (screenshot saved,
/// disk swapped) stays visible.
const OSD_DURATION: std::time::Duration = std::time::Duration::from_millis(2500);
/// On-screen overlay colours (packed R,G,B,A in memory order).
const OSD_TEXT: u32 = rgba(236, 236, 232);
/// Amber, for a message about something that did not go as asked.
const OSD_TEXT_WARNING: u32 = rgba(248, 205, 78);
const OSD_SHADOW: u32 = rgba(0, 0, 0);
const OSD_BG: u32 = rgba(10, 10, 12);
const RECORD_DOT: u32 = rgba(229, 56, 48);
const AMIGA_RAWKEY_LEFT_SHIFT: u8 = 0x60;
const AMIGA_RAWKEY_RIGHT_SHIFT: u8 = 0x61;
const AMIGA_RAWKEY_LEFT_ALT: u8 = 0x64;
const AMIGA_RAWKEY_RIGHT_ALT: u8 = 0x65;

/// The quick-save slot a number-row key selects: `1`..`9` are slots 1-9 and
/// `0` is slot 10, so the ten slots sit under the row in printed order.
/// `None` for every other key.
fn save_slot_for_key(code: KeyCode) -> Option<usize> {
    Some(match code {
        KeyCode::Digit1 => 1,
        KeyCode::Digit2 => 2,
        KeyCode::Digit3 => 3,
        KeyCode::Digit4 => 4,
        KeyCode::Digit5 => 5,
        KeyCode::Digit6 => 6,
        KeyCode::Digit7 => 7,
        KeyCode::Digit8 => 8,
        KeyCode::Digit9 => 9,
        KeyCode::Digit0 => crate::savestate::SLOT_COUNT,
        _ => return None,
    })
}

fn host_shortcut_modifier_pressed(modifiers: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.alt_key()
    }
}

/// Display name for a GDB-style register index (see `debug_set_register`):
/// D0-D7, A0-A7, SR, PC.
fn gdb_reg_label(reg: usize) -> String {
    match reg {
        0..=7 => format!("D{reg}"),
        8..=15 => format!("A{}", reg - 8),
        16 => "SR".to_string(),
        17 => "PC".to_string(),
        _ => format!("r{reg}"),
    }
}

fn window_title_mouse_captured() -> String {
    format!("{WINDOW_TITLE} - Mouse captured ({HOST_SHORTCUT_MODIFIER_LABEL}+G releases)")
}

/// A transient on-screen overlay message drawn over the display (but not
/// captured in screenshots, since it is painted into the presentation
/// texture, never into the emulated framebuffer `fb`).
struct Osd {
    text: String,
    expires_at: Instant,
    /// Drawn in amber rather than white: something did not go as asked.
    warning: bool,
}

/// How often the performance overlay resamples its counters. Twice a
/// second keeps the numbers readable; per-frame updates flicker.
const PERF_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Live readout state behind the performance overlay (Cmd/Alt+P,
/// `[display] perf_overlay`): the formatted lines drawn each frame plus
/// the counter baseline the next sample's deltas are taken against.
#[derive(Default)]
struct PerfOverlay {
    lines: Vec<String>,
    /// Bumped whenever `lines` changes so `MainRedrawState` repaints.
    revision: u64,
    baseline: Option<PerfBaseline>,
}

/// One sample of the cumulative counters the overlay derives rates from.
struct PerfBaseline {
    at: Instant,
    /// Whether the machine was advancing when this sample was taken. Rates
    /// across a pause/resume boundary would mix running and idle time, so a
    /// flip publishes the idle readout and re-baselines instead.
    running: bool,
    emulated_frames: u64,
    emulated_seconds: f64,
    busy: std::time::Duration,
    audio_underrun_frames: u64,
}

/// The numbers behind one refresh of the performance overlay, derived from
/// counter deltas by `perf_readout` and formatted by `perf_overlay_lines`.
/// Kept as plain values so both steps are testable.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PerfReadout {
    /// Emulated video frames retired per host second.
    fps: f64,
    /// Emulated seconds advanced per host second (1.0 = locked to real time).
    speed: f64,
    /// Host milliseconds of emulation work per emulated frame (pacing
    /// sleeps excluded).
    emu_frame_ms: f64,
    /// Share of host wall time spent emulating, in percent. In paced mode
    /// this equals `emu_frame_ms` over the frame period (20.0 ms PAL,
    /// 16.7 ms NTSC): the share of the host frame budget used.
    host_percent: f64,
    /// Live audio output lead in milliseconds (the underrun cushion).
    audio_lead_ms: f64,
    /// Audio underrun frames per host second.
    audio_underruns_per_s: f64,
    /// Pacer catch-up events since the last guest reset: the machine fell
    /// hopelessly behind real time and dropped emulated time. Frames are
    /// never skipped in paced mode; this counts the only case where time is.
    pacer_slips: u32,
}

fn perf_readout(
    base: &PerfBaseline,
    current: &PerfBaseline,
    audio_lead_ms: f64,
    pacer_slips: u32,
) -> PerfReadout {
    let dt = current.at.duration_since(base.at).as_secs_f64();
    if dt <= 0.0 {
        return PerfReadout {
            audio_lead_ms,
            pacer_slips,
            ..Default::default()
        };
    }
    // Counters that moved backwards (a guest reset cleared the stats, a
    // timeline jump rewound the machine) saturate to an empty window; the
    // next sample is taken against the fresh values.
    let frames = current.emulated_frames.saturating_sub(base.emulated_frames) as f64;
    let busy = current.busy.saturating_sub(base.busy).as_secs_f64();
    let emulated = (current.emulated_seconds - base.emulated_seconds).max(0.0);
    let underruns = current
        .audio_underrun_frames
        .saturating_sub(base.audio_underrun_frames) as f64;
    PerfReadout {
        fps: frames / dt,
        speed: emulated / dt,
        emu_frame_ms: if frames > 0.0 {
            busy * 1000.0 / frames
        } else {
            0.0
        },
        host_percent: busy / dt * 100.0,
        audio_lead_ms,
        audio_underruns_per_s: underruns / dt,
        pacer_slips,
    }
}

/// One line per data point, top to bottom as drawn.
fn perf_overlay_lines(r: &PerfReadout) -> Vec<String> {
    vec![
        format!("{:.1} fps", r.fps),
        format!("x{:.2}", r.speed),
        format!("emu {:.1} ms", r.emu_frame_ms),
        format!("host {:.0}%", r.host_percent),
        format!("audio {:.0} ms", r.audio_lead_ms),
        format!("xrun {:.0}", r.audio_underruns_per_s),
        format!("slip {}", r.pacer_slips),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyPressSpec {
    pub secs: f32,
    pub rawkey: u8,
    pub hold_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDumpSpec {
    pub dir: PathBuf,
    pub start_secs: f32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskInsertSpec {
    pub secs: f32,
    pub drive_idx: usize,
    pub path: PathBuf,
    pub write_protected: bool,
}

use anyhow::{anyhow, Context, Result};
use log::{error, info, warn};
use pixels::{Pixels, PixelsBuilder, ScalingMode, SurfaceTexture};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{
    mpsc::{self, Receiver, SyncSender, TryRecvError},
    Arc, OnceLock,
};
use std::thread::JoinHandle;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, RawKeyEvent,
    WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Icon, Window, WindowAttributes, WindowId};

fn rawkey_index(rawkey: u8) -> usize {
    (rawkey & 0x7F) as usize
}

/// Where a rawkey transition came from.
///
/// Two things can have a key down at once -- a finger on the host keyboard
/// and a click on the on-screen keyboard -- and each keeps its own held
/// table so its own repeats and stale releases are dropped. What the
/// machine is told is the *aggregate*: the key is down for it while either
/// source holds it, and only a change in that aggregate is enqueued,
/// recorded, or noted for replay. Without that, pressing a cap the host is
/// already holding would be swallowed as a duplicate while its release
/// still went through, cutting the host's key short -- and the strip would
/// go on drawing a latch the keyboard MCU never heard about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeySource {
    /// The host keyboard, both the focused and raw-device paths.
    Host,
    /// The on-screen keyboard strip.
    Panel,
}

fn rawkey_is_held(held_rawkeys: &[bool; 128], rawkey: u8) -> bool {
    held_rawkeys[rawkey_index(rawkey)]
}

fn rawkey_transition_is_duplicate(held_rawkeys: &[bool; 128], rawkey: u8, pressed: bool) -> bool {
    rawkey_is_held(held_rawkeys, rawkey) == pressed
}

fn repeated_main_key_should_drop(
    held_rawkeys: &[bool; 128],
    code: KeyCode,
    state: ElementState,
    repeat: bool,
    ui_accepts_repeat: bool,
) -> bool {
    if !repeat || state != ElementState::Pressed || ui_accepts_repeat {
        return false;
    }
    match host_to_amiga_rawkey(code) {
        Some(rawkey) => rawkey_is_held(held_rawkeys, rawkey),
        None => true,
    }
}

fn ui_needs_continuous_redraw(running: bool, active: bool) -> bool {
    running && active
}

fn raw_device_qualifier_rawkey(code: KeyCode) -> Option<u8> {
    match code {
        KeyCode::ShiftLeft => Some(AMIGA_RAWKEY_LEFT_SHIFT),
        KeyCode::ShiftRight => Some(AMIGA_RAWKEY_RIGHT_SHIFT),
        KeyCode::AltLeft => Some(AMIGA_RAWKEY_LEFT_ALT),
        KeyCode::AltRight => Some(AMIGA_RAWKEY_RIGHT_ALT),
        _ => None,
    }
}

fn raw_device_qualifier_family_held(held_rawkeys: &[bool; 128], left: u8, right: u8) -> bool {
    rawkey_is_held(held_rawkeys, left) || rawkey_is_held(held_rawkeys, right)
}

/// Every heat-map toucher, in [`heatmap::Toucher`] code order. The Memory
/// tab's census lists all of them, including the ones holding nothing, so
/// the column doubles as the map's legend and its rows never move.
const HEAT_TOUCHERS: [heatmap::Toucher; 8] = [
    heatmap::Toucher::CpuRead,
    heatmap::Toucher::CpuWrite,
    heatmap::Toucher::Blitter,
    heatmap::Toucher::Copper,
    heatmap::Toucher::Disk,
    heatmap::Toucher::Bitplane,
    heatmap::Toucher::Sprite,
    heatmap::Toucher::Audio,
];

/// The Memory tab's window presets: one per fitted RAM bank, then the
/// whole 24-bit space.
///
/// They come from the machine's decoded bank map rather than a fixed list
/// of addresses because that map is what the fitted machine actually has:
/// a Zorro board sits wherever autoconfig placed it, and the motherboard,
/// CPU-slot and Zorro III banks a 32-bit CPU sees live above the 24-bit
/// space entirely -- which is exactly why the heat map's window is
/// movable. A fixed list would either name banks this machine does not
/// have or fail to reach the ones it does.
fn analyzer_heat_presets(bus: &crate::bus::Bus) -> Vec<ui::HeatPreset> {
    let mb_base = bus.mem.mb_ram_base() as u32;
    let mut presets: Vec<ui::HeatPreset> = bus
        .writable_ram_regions()
        .into_iter()
        .map(|(base, len)| {
            let label = if base == crate::memory::CHIP_RAM_BASE as u32 {
                "Chip"
            } else if base == crate::memory::SLOW_RAM_BASE as u32 {
                "Slow"
            } else if base == mb_base {
                "MB"
            } else if base == crate::memory::ACCEL_RAM_BASE as u32 {
                "CPU"
            } else if base < 0x0100_0000 {
                // What is left is a RAM board: one autoconfigured into the
                // Zorro II space, or a Zorro III board above it.
                "Z2"
            } else {
                "Z3"
            };
            ui::HeatPreset {
                label: label.to_string(),
                base,
                // The fitted bank length, so a preset's window covers the
                // RAM that exists rather than the select window it decodes
                // in (a smaller bank repeats inside its window).
                span: len,
            }
        })
        .collect();
    // Two boards of the same kind would otherwise offer two buttons with
    // the same name; the base address tells them apart.
    let labels: Vec<String> = presets.iter().map(|preset| preset.label.clone()).collect();
    for (index, preset) in presets.iter_mut().enumerate() {
        let label = preset.label.clone();
        if labels
            .iter()
            .enumerate()
            .any(|(other, other_label)| other != index && *other_label == label)
        {
            preset.label = format!("{label} ${:X}", preset.base);
        }
    }
    presets.push(ui::HeatPreset {
        label: "24-bit".to_string(),
        base: 0,
        span: heatmap::DEFAULT_SPAN,
    });
    presets
}

/// The window the Memory tab arms when nothing has armed the map yet: the
/// chip RAM bank. It is the bank every chip-bus engine works out of and
/// usually the smallest fitted one, so its cells cover the fewest bytes
/// each -- the most legible default. A machine with no chip bank at all
/// falls back to the 24-bit overview.
fn analyzer_default_heat_window(bus: &crate::bus::Bus) -> (u32, u32) {
    bus.writable_ram_regions()
        .into_iter()
        .find(|(base, _)| *base == crate::memory::CHIP_RAM_BASE as u32)
        .unwrap_or((crate::memory::CHIP_RAM_BASE as u32, heatmap::DEFAULT_SPAN))
}

pub struct App {
    emu: Emulator,
    fb: Vec<u32>,
    /// Merges rendered fields into the double-height presentation
    /// buffer that the window texture, screenshots, and frame dumps
    /// read (see [`deinterlace`](super::deinterlace)).
    deinterlacer: Deinterlacer,
    /// The machine's resolved deinterlace and phosphor settings. Carried
    /// in every render job so the worker's own deinterlacer follows
    /// them; also applied to `deinterlacer` for the synchronous fallback
    /// path.
    deinterlace: bool,
    phosphor: f32,
    /// Active presentation buffer, already deinterlaced/line-doubled and
    /// post-processed. The first `present_rows * FB_WIDTH` pixels are valid.
    present_fb: Vec<u32>,
    present_rows: usize,
    /// Pixels per `present_fb` row: FB_WIDTH classically, twice that for
    /// a 35 ns super-hi-res canvas.
    present_width: usize,
    /// TV-aperture crop rows for the presented frame when it is a standard
    /// 15 kHz scan with the standard horizontal window (None otherwise);
    /// applied by the present copy under `Overscan::Tv`.
    present_tv_aperture_rows: Option<usize>,
    /// Aperture/recentring decisions latched across border-only frames:
    /// the blank frames a screen change emits keep the previous
    /// presentation geometry instead of snapping to the full framebuffer,
    /// so the picture does not jump at every Kickstart mode change.
    presentation_latch: PresentationLatch,
    /// Whether the presented frame came from a programmable (multisync) scan
    /// rather than a woven 15 kHz one. Those fields reach the presentation
    /// buffer at their native height, so neither the CRT pass nor its
    /// two-rows-per-line count applies to them.
    present_programmable: bool,
    /// Scratch for composing an RTG board frame (Z3660 scanout); reused
    /// across frames to avoid a per-frame allocation.
    rtg_fb: Vec<u32>,
    /// Native (width, height) of the RTG frame in `rtg_fb` when the last
    /// presented frame was RTG; `None` when the chipset drives the display.
    /// The draw path uploads `rtg_fb` to the RTG texture when set.
    rtg_present_dims: Option<(u32, u32)>,
    render: Option<Render>,
    debugger_tool_window: Option<ToolWindow>,
    frame_analyzer_tool_window: Option<ToolWindow>,
    console_tool_window: Option<ToolWindow>,
    /// When the frame loop last requested a paced tool window repaint
    /// (see TOOL_REDRAW_INTERVAL).
    last_tool_redraw: Instant,
    debugger_panel: Option<ui::DebuggerPanel>,
    frame_analyzer_panel: Option<ui::FrameAnalyzerPanel>,
    /// The debugger console: a GDB-flavoured command line in its own tool
    /// window, so it can sit beside the debugger and Frame Analyzer.
    console_panel: Option<ui::ConsolePanel>,
    /// Beam-space render of the analyzer trace's frame for the picture
    /// underlay: unlike `fb`, no presentation recentring or TV masking is
    /// applied, so its pixels line up with the DMA trace's beam grid.
    /// Shared with the per-redraw view via Rc to avoid copying the frame.
    analyzer_underlay_fb: std::rc::Rc<Vec<u32>>,
    /// Rows valid in `analyzer_underlay_fb` (the traced frame's scan height).
    analyzer_underlay_rows: usize,
    /// Pixels per `analyzer_underlay_fb` row (the frame's canvas width).
    analyzer_underlay_width: usize,
    /// Emulated frame `analyzer_underlay_fb` was rendered for.
    analyzer_underlay_frame: Option<u64>,
    /// Recycled snapshot buffers for the underlay's side-effect-free render.
    analyzer_underlay_input: Option<bitplane::RenderInput>,
    /// Tracks whether the Frame Analyzer armed the bus heat map, so closing
    /// it releases only a map it owns (a map armed over the control
    /// protocol is left alone).
    heatmap_armed_by_panel: bool,
    render_worker: Option<RenderWorker>,
    render_recycle_fb: Vec<u32>,
    /// Spent frame snapshot handed back by the render worker; reused by the
    /// next `RenderInput::refill_from_bus` to avoid re-allocating its
    /// buffers (the chip-RAM copy alone is up to 2 MiB) every frame.
    render_recycle_input: Option<bitplane::RenderInput>,
    cpu_halted: bool,
    /// Host-level power state. When false the emulator does not step;
    /// the machine sits powered off until the status-bar power button
    /// is clicked. Distinct from the emulated (CIA-driven) power LED.
    powered_on: bool,
    /// Host-level pause state. When true the emulator does not step but
    /// stays powered on, so the last rendered frame is held on screen and
    /// emulation resumes from the same point when unpaused.
    paused: bool,
    /// Scheduled --screenshot-after captures, earliest deadline first.
    /// The flag repeats, so a run can bracket several moments; the run
    /// ends once the last of them has been saved.
    auto_shot: Vec<(f32, PathBuf)>,
    pending_auto_shot: Vec<(f32, PathBuf)>,
    /// Scheduled --save-state-after captures, earliest deadline first:
    /// write a save state once emulated time reaches each deadline, then
    /// keep running. Repeats like --screenshot-after.
    auto_save_state: Vec<(f32, PathBuf)>,
    pending_auto_save_state: Vec<(f32, PathBuf)>,
    frame_dump: Option<FrameDumpState>,
    pending_frame_dump: Option<FrameDumpSpec>,
    auto_keys: Vec<ScheduledKey>,
    pending_auto_keys: Vec<KeyPressSpec>,
    /// Scheduled mouse-button press/release events from --click-after.
    /// `Press` and `Release` deadlines per requested click.
    auto_clicks: Vec<ScheduledClick>,
    pending_auto_clicks: Vec<(f32, MouseButtonKind, u32, u8)>,
    /// Scheduled joystick/CD32-pad events from --joy-after, plus the
    /// controls currently held per port. An `auto_joy_engaged` entry stays
    /// true once any scripted joy event has fired on that port so the state
    /// keeps overriding the (absent) physical pad, including the final
    /// release.
    auto_joys: Vec<ScheduledJoy>,
    pending_auto_joys: Vec<(f32, JoyButtonKind, u32, u8)>,
    auto_joy_held: [AutoJoyHeld; 2],
    auto_joy_engaged: [bool; 2],
    /// The windowed control-protocol server (`--control-gui`), attached
    /// after construction via [`App::attach_control`]; its commands are
    /// drained at the top of `about_to_wait` (see window/control.rs).
    #[cfg(feature = "control")]
    control: Option<control::ControlState>,
    /// Scheduled relative port-1 mouse motions from --mouse-after,
    /// one-shot per entry; (at_emulated_secs, dx, dy).
    auto_mouse: Vec<(f64, i32, i32, u8)>,
    pending_auto_mouse: Vec<(f32, i32, i32, u8)>,
    /// `--mouse-to-after` requests waiting for their timestamp, and the
    /// one servo currently steering the pointer. Only one runs at a time:
    /// two servos fighting over the same quadrature counters would each
    /// mis-measure the other's motion as its own.
    auto_mouse_to: Vec<(f64, i32, i32, u8)>,
    pending_auto_mouse_to: Vec<(f32, i32, i32, u8)>,
    active_mouse_to: Option<crate::pointer::PointerServo>,
    /// Scheduled analogue pot positions from --pot-after (one-shot each).
    auto_pots: Vec<(f64, u8, u8, u8)>,
    pending_auto_pots: Vec<(f32, u8, u8, u8)>,
    auto_disk_inserts: Vec<ScheduledDiskInsert>,
    pending_auto_disk_inserts: Vec<DiskInsertSpec>,
    /// Scheduled CD swaps from --insert-cd-after (one-shot each);
    /// (at_emulated_secs, image path).
    auto_cd_inserts: Vec<(f64, PathBuf)>,
    pending_auto_cd_inserts: Vec<(f32, PathBuf)>,
    /// Live-input recorder: logs every input event that reaches the
    /// emulated machine and writes a --script-replayable file on stop.
    /// None while not recording.
    input_recorder: Option<crate::inputrec::InputRecorder>,
    /// --record-input destination: when set, the recorder runs for the
    /// whole session and the script is written here on exit (the Drop
    /// impl catches every exit path, including the headless captures).
    record_input_path: Option<PathBuf>,
    modifiers: ModifiersState,
    /// Rawkeys the host keyboard is holding down (both the focused
    /// `KeyboardInput` path and the raw-device qualifier path feed it).
    /// One of the two sources behind [`App::amiga_rawkey_held`]; see
    /// [`KeySource`] for why the machine is told about the aggregate
    /// rather than about either source on its own.
    held_rawkeys: [bool; 128],
    /// Rawkeys the on-screen keyboard is holding down: the cap under the
    /// mouse and every latched qualifier. The other source.
    panel_held_rawkeys: [bool; 128],
    /// Physical state of the qualifier keys as the raw-device listener
    /// sees them, which is not a source of its own: it is what stops a
    /// winit `ModifiersState` update from releasing a qualifier the
    /// hardware still has down (see `update_host_modifiers`).
    raw_device_held_rawkeys: [bool; 128],
    main_window_focused: bool,
    /// Whether the user has sized the main window themselves, in which case
    /// the canvas reflows into it instead of the window snapping to the
    /// canvas. Tracked from the resizes that arrive rather than measured
    /// from the current size so a snap the platform clamped or rounded does
    /// not read as the user's own drag and disable future snaps.
    window_manually_sized: bool,
    /// Deadline for the asynchronous response to the last canvas snap, so a
    /// platform-clamped result is not counted as the user's resize. Bounded
    /// because a window manager may ignore the request entirely.
    snap_request_deadline: Option<Instant>,
    /// A canvas change that could not size the window because it was
    /// fullscreen, waiting for the window to come back.
    snap_when_windowed: bool,
    cursor_pos: Option<(i32, i32)>,
    last_display_cursor_pos: Option<(i32, i32)>,
    /// Most recent raw host cursor position (physical pixels) from the last
    /// CursorMoved. Kept only for the COPPERLINE_DIAG_CURSOR click trace, which
    /// needs the un-mapped coordinate alongside the mapped pixel.
    last_cursor_phys: Option<winit::dpi::PhysicalPosition<f64>>,
    volume_dragging: bool,
    /// True while the frame analyzer selector is following a held left
    /// mouse button.
    analyzer_dragging: bool,
    mouse_captured: bool,
    /// Set when a menu, overlay panel, or tool window took the mouse away
    /// from a capture that was live, so closing the last of them can hand
    /// it back. The Cmd/Alt+G toggle clears it: the capture is then off
    /// because the operator asked for it, not because the UI borrowed the
    /// cursor. Focus loss deliberately does not clear it -- opening a tool
    /// window unfocuses the main window as a matter of course, and that
    /// must not count as the operator letting the capture go.
    capture_suspended_by_ui: bool,
    mouse_delta_remainder: (f64, f64),
    last_rendered_emulated_frame: Option<u64>,
    last_submitted_render_frame: Option<u64>,
    render_generation: u64,
    last_fdd_track: Option<u8>,
    /// Last status/UI state paired with a requested main-window redraw.
    /// When both this and the exact presentation pixels are unchanged, the
    /// existing GPU texture can be held instead of uploaded and presented at
    /// the emulated field rate.
    last_main_redraw_state: Option<MainRedrawState>,
    /// A newly processed frame changed the active presentation buffer. Kept
    /// separate from the render methods' boolean ("a frame was processed") so
    /// recordings still receive exact duplicate frames.
    main_presentation_dirty: bool,
    /// Transient on-screen overlay message (screenshot saved, disk
    /// swapped), or None when nothing is being shown.
    osd: Option<Osd>,
    /// True while a file drag hovers over the main window; draws the
    /// drop-hint overlay. winit sends no HoveredFileCancelled after a
    /// successful drop, so DroppedFile clears this too.
    drop_hover: bool,
    /// Files from DroppedFile events, coalesced in about_to_wait: winit
    /// delivers one event per file, and a multi-file drop must act once.
    pending_dropped_files: Vec<PathBuf>,
    /// A disk image being written by the launcher's workshop. A large,
    /// fully-allocated image takes long enough that writing it on this
    /// thread would look like a hang, so it runs on a worker and the loop
    /// stays awake to collect it.
    image_job: Option<ImageJob>,
    /// Per-drive disk-swap playlists: the ordered image paths the user can
    /// cycle through for each drive with the disk-swap shortcut. Lets a
    /// multi-disk demo run on a single drive.
    disk_playlists: [Vec<PathBuf>; 4],
    /// Write-protect flag applied to disks swapped in from each playlist.
    disk_write_protected: [bool; 4],
    /// Index of the currently inserted disk within each drive's playlist.
    disk_playlist_index: [usize; 4],
    /// Whether to horizontally recentre a standard (non-overscan) display for
    /// full-overscan presentation. On by default; set COPPERLINE_HCENTER=0 to
    /// disable. TV presentation keeps the framebuffer's fixed source origin.
    hcenter: bool,
    /// Presentation-level overscan handling ([display] overscan): Tv masks
    /// the deep-overscan margins with black like a CRT bezel.
    overscan: Overscan,
    /// Window shader pass in effect ([display] shader). Presentation only:
    /// screenshots, frame dumps and recordings never go through it.
    crt_shader_kind: crate::config::ShaderKind,
    /// Source file behind `ShaderKind::Custom`, kept so the menu can cycle
    /// back to a user shader (and re-read an edited one) after leaving it.
    custom_shader_path: Option<std::path::PathBuf>,
    /// How strongly the shader pass is mixed in, 0.0 to 1.0 ([display]
    /// shader_strength).
    shader_strength: f32,
    /// Which monitor front the bezel pass draws, if any ([display] bezel,
    /// Video Settings). Presentation only, like the shader pass: captures
    /// never include it.
    bezel: BezelStyle,
    /// The style `Cmd/Alt+M` switches back on, so the shortcut is an
    /// on-off for whichever front was chosen rather than a third way of
    /// choosing one. Never [`BezelStyle::None`]: turning the bezel off
    /// leaves this pointing at what was on.
    bezel_last: BezelStyle,
    /// Performance overlay in effect ([display] perf_overlay, Cmd/Alt+P):
    /// a live emulation-performance readout in the top-right of the
    /// display. Presentation only, like the OSD: captures never include it.
    perf_overlay: bool,
    /// The overlay's formatted lines and sampling baseline.
    perf: PerfOverlay,
    /// Screen tint in effect ([display] tint). Presentation only, applied
    /// to the chipset display region of the window frame: captures and
    /// RTG board scanout are never tinted.
    tint: crate::config::Tint,
    /// Luma-indexed colour table for `tint`; `None` when the tint is off,
    /// which skips the pass entirely.
    tint_lut: Option<Box<[u32; 256]>>,
    /// Open the window fullscreen when it is first created ([display]
    /// full_screen). Applied once in `resumed`; the runtime toggle takes over
    /// after that.
    start_fullscreen: bool,
    /// Host USB gamepad reader (pure-Rust, no SDL2), mapped to the emulated
    /// port-2 digital joystick via a per-pad calibration. A no-op when no
    /// input backend is available (e.g. headless CI) or the pad is not yet
    /// calibrated.
    gamepad: crate::gamepad::GamepadReader,
    /// Host source policy for the emulated port-2 joystick/CD32 pad.
    joystick_input_mode: JoystickInputMode,
    /// Host mouse sensitivity, 0-100 ([input] mouse_sensitivity), and the speed
    /// multiplier derived from it. A host-input scale only: it multiplies the
    /// live host mouse delta, never scripted --mouse-after input or the core.
    mouse_sensitivity: u8,
    mouse_sensitivity_factor: f64,
    /// When the host mouse is grabbed ([input] mouse_capture): on a display
    /// click, automatically whenever the window holds the focus, or only on
    /// the Cmd/Alt+G shortcut.
    mouse_capture: crate::config::MouseCapture,
    /// Whether the "press Cmd/Alt+G to release" hint has been shown for an
    /// automatic capture yet. Auto mode grabs on every focus gain, and a
    /// message on each one would be noise; the operator only needs telling
    /// how to get the cursor back the first time.
    auto_capture_hint_shown: bool,
    /// Whether the serial port is bridged to MIDI, so the runtime menu offers
    /// the device items. Fixed for the machine's life.
    serial_is_midi: bool,
    /// Host audio output selection for machines started from this session:
    /// system default, a named device (from `[audio] output_device` /
    /// `--audio-device`), or Disabled (no sound, GUI-only). A session-level
    /// setting: the config-screen launcher rebuilds the machine config from its
    /// own fields, so this is held here rather than read back from that config.
    audio_output: crate::audio::AudioOutput,
    /// The session's `[emulation] realtime_priority` request (config value, before
    /// the env override). Re-fed to `priority::requested` whenever the audio sink
    /// is rebuilt live (device switch, disconnect recovery, post-load install) so
    /// the new stream/callback thread keeps the same scheduling as the first sink.
    realtime_priority: bool,
    /// Parallel-port sampler request for this session (from `[parallel]` /
    /// `--parallel sampler` or the launcher). Re-applied whenever a machine
    /// session starts, and edited live from the runtime menu / gain shortcut.
    sampler: crate::sampler::SamplerRequest,
    /// The live cpal capture stream feeding the attached sampler. The stream is
    /// `!Send`, so it is kept here on the main thread while its `Send` read-port
    /// sits in the bus; `None` when no sampler is attached.
    sampler_stream: Option<cpal::Stream>,
    /// Output frame-skip level for warp/turbo mode: how many emulated frames
    /// are retired per presented frame while warp is engaged. Presentation is
    /// vsync-gated, so this is what decouples warp speed from the host monitor
    /// refresh rate. Adjustable from the Emulator menu and the keyboard.
    warp_speed: WarpSpeed,
    /// Rewind capture settings from `[emulation]`, kept so the Rewind menu
    /// item can re-arm the ring with the configured budget after it is
    /// toggled off. `rewind_armed` tracks the user's intent independently of
    /// `Emulator::time_travel_enabled`, which the debugger also arms.
    rewind_budget_mb: usize,
    rewind_interval_frames: u64,
    rewind_armed: bool,
    /// The MT-32's front panel: what it is showing and what it believes
    /// each value to be. The synth has no panel of its own -- on the
    /// hardware this is firmware -- so it is kept here.
    #[cfg(feature = "mt32")]
    mt32_panel: mt32panel::Mt32Panel,
    /// The on-screen Amiga keyboard: which cap the mouse is holding, which
    /// qualifiers are latched, and which legends the caps wear. Whether the
    /// strip is up at all is `video::keyboard_panel_shown`, because the
    /// canvas height is derived from it.
    kbd_panel: kbdpanel::KbdPanelState,
    /// Mapped host keys currently held for keyboard joystick emulation.
    keyboard_joy_held: [keymap::HeldKeys; keymap::MAPPING_COUNT],
    /// Host-key to controller-control bindings, loaded from the per-user
    /// `keymap.toml` (defaults when there is none) and editable from the
    /// Input Mapping panel.
    keymap: keymap::KeyMap,
    /// Autofire rate in Hz for the fire button on both ports, 0 = off. A
    /// host input policy, not machine state: it gates a *held* fire button
    /// into a pulse train, so nothing changes unless the user holds fire.
    autofire_hz: u8,
    /// Pop-up menu and main-window overlay state. Debugger and frame
    /// analyzer panes live in separate tool-window state so they can be
    /// open at the same time.
    ui: UiState,
    /// Emulated-machine summary lines for the About window.
    about_machine_lines: Vec<String>,
    /// Raw config of the running (or last-applied) machine, so the "Machine
    /// Configuration..." menu item reopens the launcher showing the current
    /// settings.
    machine_config: RawConfig,
    /// Host pause state before the debugger forced a pause, restored when
    /// the debugger window closes (unless Run was used inside it).
    paused_before_debugger: bool,
    /// Host pause state before the frame analyzer forced a pause, restored
    /// when the analyzer pane closes unless Run was used inside it.
    paused_before_analyzer: bool,
    /// Host pause state before the console forced a pause, restored when
    /// the console closes unless run/pause was used inside it.
    paused_before_console: bool,
    /// The reason for the last interactive breakpoint/watchpoint stop,
    /// shown on the debugger's Break tab until execution resumes.
    last_debug_stop: Option<String>,
    /// A CPU double-fault halt has been reported (reset when the CPU
    /// leaves the halted state, e.g. by reset or a state load).
    reported_double_fault: bool,
    /// The console's running memory hunt (HUNT delta search), if any.
    hunt: Option<console::HuntState>,
    /// Active video+audio capture (shortcut or the menu's Record Video item),
    /// or None when not recording. Frames and the matching mixer audio are
    /// appended on emulated-frame boundaries, so captures stay in sync
    /// even under warp or host stutter.
    recorder: Option<crate::recorder::VideoRecorder>,
    /// Scratch presentation-scaled framebuffer for the recorder (same
    /// vertical resample as screenshots).
    record_fb: Vec<u32>,
    /// Scratch for narrowing a 35 ns-canvas presentation to the recorder's
    /// fixed FB_WIDTH frame.
    record_scratch_fb: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledClick {
    press_at_emulated_secs: f64,
    release_at_emulated_secs: f64,
    button: MouseButtonKind,
    /// 0-based controller port the click lands on.
    port: u8,
    pressed: bool,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledKey {
    press_at_emulated_secs: f64,
    release_at_emulated_secs: f64,
    rawkey: u8,
    pressed: bool,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledJoy {
    press_at_emulated_secs: f64,
    release_at_emulated_secs: f64,
    button: JoyButtonKind,
    /// 0-based controller port the control belongs to.
    port: u8,
    pressed: bool,
}

#[derive(Debug, Clone)]
struct ScheduledDiskInsert {
    insert_at_emulated_secs: f64,
    drive_idx: usize,
    path: PathBuf,
    write_protected: bool,
}

#[derive(Debug, Clone)]
struct FrameDumpState {
    start_secs: f32,
    dir: PathBuf,
    count: u32,
    dumped: u32,
    last_saved_emulated_frame: Option<u64>,
}

struct Render {
    window: Arc<Window>,
    pixels: Pixels<'static>,
    texture_scale: usize,
    /// Native-resolution RTG display texture, drawn over the UI buffer in
    /// the `pixels` render pass (see [`rtg_texture`]). Present whenever the
    /// window is (its pipeline uses the same GPU device as `pixels`).
    rtg_texture: rtg_texture::RtgTexture,
    /// The optional CRT/scanline pass drawn over the display region in the
    /// same `pixels` render pass (see [`crt_shader`]). Built with the
    /// window, whatever preset is selected: the pass is skipped per frame,
    /// not per session, so the menu can turn it on live.
    crt_shader: crt_shader::CrtShader,
    /// The optional monitor-bezel pass drawn under the CRT pass (see
    /// [`bezel`]). Built with the window whatever the setting, like the
    /// CRT pass, so switching style or turning it off works live; each
    /// style's shader is compiled the first time that style is drawn.
    bezel_shader: bezel::BezelShader,
    /// True while the host window is minimized (Windows delivers a 0x0
    /// Resized). Presenting while minimized deadlocks on Windows: DWM stops
    /// consuming swapchain frames, so once the in-flight buffers fill,
    /// pixels.render() blocks the main thread, the message pump dies, and
    /// the window can never be restored (which is what would unblock the
    /// present). Skip all rendering until a nonzero resize restores it.
    minimized: bool,
    /// The physical surface size `pixels` was last configured with, so a
    /// redraw can tell that the host window has outgrown it (see
    /// `resync_surface_size`).
    surface_size: (u32, u32),
}

impl Render {
    /// Resize the presentation surface, recording the size it was configured
    /// with. Every resize goes through here (the first configure is
    /// `build_pixels_for_window`'s, whose size this struct is built with):
    /// `pixels` reconfigures its swapchain from its own copy of this size and
    /// nothing else can correct it, so the record must never lag behind what
    /// `pixels` holds.
    fn resize_surface(&mut self, size: PhysicalSize<u32>) -> Result<(), pixels::TextureError> {
        let (width, height) = (size.width.max(1), size.height.max(1));
        self.pixels.resize_surface(width, height)?;
        self.surface_size = (width, height);
        Ok(())
    }
}

struct ToolWindow {
    window: Arc<Window>,
    pixels: Pixels<'static>,
    texture_scale: usize,
    cursor_pos: Option<(i32, i32)>,
    /// Same Windows minimized-present deadlock hazard as Render::minimized.
    minimized: bool,
    /// Same configured-surface-size record as Render::surface_size.
    surface_size: (u32, u32),
}

impl ToolWindow {
    /// Tool-window counterpart of `Render::resize_surface`.
    fn resize_surface(&mut self, size: PhysicalSize<u32>) -> Result<(), pixels::TextureError> {
        let (width, height) = (size.width.max(1), size.height.max(1));
        self.pixels.resize_surface(width, height)?;
        self.surface_size = (width, height);
        Ok(())
    }
}

/// Frame-loop repaints of the tool windows (debugger, frame analyzer) are
/// paced to this wall-clock interval (20 Hz). Each repaint costs a full
/// panel raster plus a whole-texture GPU upload on the emulation thread, so
/// repainting at the 50 Hz emulated frame rate can push the loop past its
/// frame budget and underrun the audio ring. Interactive updates (hover,
/// clicks, stepping, debug stops) request immediate redraws and are not
/// paced.
const TOOL_REDRAW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPanelKind {
    Debugger,
    FrameAnalyzer,
    Console,
}

impl ToolPanelKind {
    /// Every kind of tool window. The lifecycle passes and, above all,
    /// `request_redraw` iterate this rather than naming windows one by one:
    /// a window left out of the redraw only shows what it drew last, and the
    /// panels that pause the machine have nothing else to repaint them.
    const ALL: [Self; 3] = [Self::Debugger, Self::FrameAnalyzer, Self::Console];
}

struct RenderJob {
    generation: u64,
    input: bitplane::RenderInput,
    h_shift: usize,
    overscan: Overscan,
    /// Deinterlacing and phosphor persistence for this frame. They travel
    /// per job (like `h_shift`/`overscan`) so the worker's deinterlacer
    /// always follows the App's current settings; a value captured at
    /// worker spawn would go stale when the launcher starts a machine
    /// with a different config.
    deinterlace: bool,
    phosphor: f32,
    presentation_fb: Vec<u32>,
}

struct RenderWorkerResult {
    generation: u64,
    emulated_frame: u64,
    timing: VideoRenderFrameTiming,
    /// The worker proved this frame's complete render/presentation inputs
    /// identical to the previous progressive frame. `presentation_fb` is then
    /// merely the unused recycle buffer from the job; the main thread keeps
    /// presenting its current buffer without copying it.
    reused_previous: bool,
    presentation_fb: Vec<u32>,
    present_rows: usize,
    present_width: usize,
    /// The frame's aperture classification; the App resolves it through
    /// its `PresentationLatch` when the result lands, so border-only
    /// frames keep the previous geometry.
    tv_aperture: TvApertureFrame,
    programmable: bool,
    /// The job's frame snapshot, handed back for buffer reuse.
    input: bitplane::RenderInput,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MainRedrawState {
    status: FrontPanelStatus,
    media: MediaBar,
    powered_on: bool,
    paused: bool,
    joystick_input_mode: JoystickInputMode,
    control_connected: bool,
    recording: bool,
    input_recording: bool,
    warp: bool,
    /// The performance overlay's line revision (0 while hidden), so a
    /// resample repaints an otherwise static frame.
    perf_revision: u64,
    /// The MT-32 panel's face -- its display line and lamp -- folded to a
    /// fingerprint, so a program writing to the LCD repaints an otherwise
    /// static frame. Zero while the panel is hidden.
    #[cfg(feature = "mt32")]
    mt32_face: u64,
}

struct RenderWorker {
    job_tx: Option<SyncSender<RenderJob>>,
    result_rx: Receiver<RenderWorkerResult>,
    handle: Option<JoinHandle<()>>,
}

impl RenderWorker {
    fn new() -> Self {
        let (job_tx, job_rx) = mpsc::sync_channel::<RenderJob>(1);
        let (result_tx, result_rx) = mpsc::channel::<RenderWorkerResult>();
        let handle = std::thread::Builder::new()
            .name("copperline-render".to_string())
            .spawn(move || {
                let mut fb = vec![0u32; MAX_CANVAS_PIXELS];
                let mut deinterlacer = Deinterlacer::new();
                let mut repeated_frame_cache = RepeatedPresentationCache::default();
                let mut last_generation = None;
                while let Ok(job) = job_rx.recv() {
                    // A generation bump marks a presentation discontinuity
                    // (machine swap, reset, state load): nothing from the
                    // previous stream may weave or glow into this frame.
                    if last_generation != Some(job.generation) {
                        deinterlacer.reset_history();
                        repeated_frame_cache.clear();
                        last_generation = Some(job.generation);
                    }
                    let result = render_job_to_presentation(
                        job,
                        &mut fb,
                        &mut deinterlacer,
                        &mut repeated_frame_cache,
                    );
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn render worker");
        Self {
            job_tx: Some(job_tx),
            result_rx,
            handle: Some(handle),
        }
    }

    /// On failure (worker thread gone) the whole job is handed back so the
    /// caller can recycle its presentation buffer and frame snapshot.
    #[allow(clippy::result_large_err)]
    fn send(&self, job: RenderJob) -> std::result::Result<(), RenderJob> {
        match self
            .job_tx
            .as_ref()
            .expect("render worker sender missing")
            .send(job)
        {
            Ok(()) => Ok(()),
            Err(err) => Err(err.0),
        }
    }

    fn try_recv(&self) -> std::result::Result<RenderWorkerResult, TryRecvError> {
        self.result_rx.try_recv()
    }

    fn recv(&self) -> std::result::Result<RenderWorkerResult, mpsc::RecvError> {
        self.result_rx.recv()
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.job_tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Which of the two Create Image pages asked for a file, and what it asked
/// for. The choice is made while the launcher state is still borrowed, and
/// acted on after the save dialog has come back.
enum ImageToMake {
    Floppy(crate::diskimage::FloppySpec),
    Hard(crate::diskimage::HardSpec),
}

impl ImageToMake {
    /// Whether the file will be left with holes in it. A floppy is written
    /// whole -- it is under two megabytes -- so it never is.
    fn is_sparse(&self) -> bool {
        match self {
            ImageToMake::Floppy(_) => false,
            ImageToMake::Hard(spec) => spec.sparse,
        }
    }

    /// How much room the finished file will take on the host. For a hard
    /// drive that is the geometry's own size, which a hand-set geometry
    /// decides rather than the size box.
    fn bytes_on_disk(&self) -> u64 {
        match self {
            ImageToMake::Floppy(spec) => crate::diskimage::floppy_bytes(spec),
            ImageToMake::Hard(spec) => spec
                .geometry
                .unwrap_or_else(|| crate::diskimage::Geometry::for_size(spec.bytes))
                .bytes(),
        }
    }
}

/// An image being written on a worker thread, and what to call it when it
/// lands.
struct ImageJob {
    rx: std::sync::mpsc::Receiver<std::io::Result<crate::diskimage::Created>>,
    path: PathBuf,
    /// The file's own name, for the line that reports it landing.
    name: String,
}

/// Bytes free on the filesystem a not-yet-created file would land on.
///
/// The file itself does not exist, so the question is asked of the
/// directory holding it -- and of *that* directory, not of Copperline's
/// own: saving onto a second drive is measured against the second drive.
/// `None` when the host will not say, in which case there is nothing to
/// warn about and the write simply goes ahead.
fn free_space_for_new_file(path: &std::path::Path) -> Option<u64> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty())?;
    crate::filesys::host_fs_usage(dir).map(|(_, avail)| avail)
}

impl App {
    pub fn new(
        emu: Emulator,
        power_on: bool,
        screenshot_after: Vec<(f32, PathBuf)>,
        save_state_after: Vec<(f32, PathBuf)>,
        frame_dump: Option<FrameDumpSpec>,
        press_after: Vec<KeyPressSpec>,
        click_after: Vec<(f32, MouseButtonKind, u32, u8)>,
        joy_after: Vec<(f32, JoyButtonKind, u32, u8)>,
        mouse_after: Vec<(f32, i32, i32, u8)>,
        mouse_to_after: Vec<(f32, i32, i32, u8)>,
        pot_after: Vec<(f32, u8, u8, u8)>,
        disk_insert_after: Vec<DiskInsertSpec>,
        cd_insert_after: Vec<(f32, PathBuf)>,
        record_input: Option<PathBuf>,
        disk_playlists: [Vec<PathBuf>; 4],
        disk_write_protected: [bool; 4],
        overscan: Overscan,
        deinterlace: bool,
        phosphor: f32,
        shader: crate::config::ShaderMode,
        shader_strength: f32,
        bezel: BezelStyle,
        perf_overlay: bool,
        tint: crate::config::Tint,
        start_fullscreen: bool,
        hide_status_bar: bool,
        warp_speed: WarpSpeed,
        joystick_input_mode: JoystickInputMode,
        mouse_sensitivity: u8,
        mouse_capture: crate::config::MouseCapture,
        about_machine_lines: Vec<String>,
        machine_config: RawConfig,
        // Effective live-audio state for this machine: for a real machine the
        // caller's --audio/--noaudio-resolved value; for the config-screen
        // placeholder the config intent (so a state loaded over it gets sound).
        audio_output_enabled: bool,
        // Parallel-port sampler request (disabled for the config-screen
        // placeholder; run_machine re-derives it from the launcher's config).
        sampler: crate::sampler::SamplerRequest,
    ) -> Self {
        // The status-bar visibility is a process-global read from deep in the
        // presentation code; seed it before the window is built so the initial
        // window size accounts for it.
        super::set_status_bar_hidden(hide_status_bar);
        // Headless capture runs drive themselves off emulated time, so a
        // powered-off start would simply hang. Force power on for those.
        let powered_on = power_on
            || !screenshot_after.is_empty()
            || !save_state_after.is_empty()
            || frame_dump.is_some();
        let render_worker = threaded_render_enabled().then(|| {
            info!("threaded render pipeline enabled");
            RenderWorker::new()
        });
        // MIDI needs a &mut to probe the sink; rebind so the parameter is not
        // needlessly `mut` in a build without the feature.
        #[cfg(feature = "midi")]
        let (serial_is_midi, emu) = {
            let mut emu = emu;
            let is_midi = emu.bus_mut().midi_serial_mut().is_some();
            (is_midi, emu)
        };
        #[cfg(not(feature = "midi"))]
        let serial_is_midi = false;
        // `audio_output_enabled` is the effective state the caller resolved
        // (--audio/--noaudio applied over output_enabled for a real machine, or
        // the config intent for the silent config-screen placeholder), so the
        // menu label matches what is actually running. from_config treats a
        // blank device name as the default.
        let audio_output = crate::audio::AudioOutput::from_config(
            audio_output_enabled,
            machine_config.audio.output_device.as_deref(),
        );
        // Config's realtime-priority request, re-fed to priority::requested when
        // the audio sink is rebuilt live so those streams keep the same setting.
        let realtime_priority = machine_config.emulation.realtime_priority.unwrap_or(false);
        let rewind_budget_mb = machine_config
            .emulation
            .rewind_budget_mb
            .unwrap_or(crate::config::REWIND_DEFAULT_BUDGET_MB)
            .max(1);
        let rewind_interval_frames = machine_config
            .emulation
            .rewind_interval_frames
            .unwrap_or(crate::config::REWIND_DEFAULT_INTERVAL_FRAMES)
            .max(1);
        let rewind_armed = machine_config.emulation.rewind.unwrap_or(false);
        let autofire_hz = machine_config
            .input
            .autofire_hz
            .unwrap_or(0)
            .min(crate::config::AUTOFIRE_MAX_HZ);
        let mut app = Self {
            emu,
            serial_is_midi,
            audio_output,
            realtime_priority,
            sampler,
            sampler_stream: None,
            fb: vec![0u32; MAX_CANVAS_PIXELS],
            deinterlacer: Deinterlacer::with_settings(deinterlace, phosphor),
            deinterlace,
            phosphor,
            present_fb: vec![0u32; FB_WIDTH * OUT_HEIGHT],
            present_rows: OUT_HEIGHT,
            present_width: FB_WIDTH,
            rtg_fb: Vec::new(),
            rtg_present_dims: None,
            present_tv_aperture_rows: Some(TV_PAL_PRESENT_HEIGHT),
            presentation_latch: PresentationLatch::default(),
            present_programmable: false,
            render: None,
            debugger_tool_window: None,
            frame_analyzer_tool_window: None,
            console_tool_window: None,
            last_tool_redraw: Instant::now(),
            debugger_panel: None,
            frame_analyzer_panel: None,
            console_panel: None,
            analyzer_underlay_fb: std::rc::Rc::new(Vec::new()),
            analyzer_underlay_rows: 0,
            analyzer_underlay_width: FB_WIDTH,
            analyzer_underlay_frame: None,
            analyzer_underlay_input: None,
            heatmap_armed_by_panel: false,
            render_worker,
            render_recycle_fb: Vec::new(),
            render_recycle_input: None,
            cpu_halted: false,
            powered_on,
            paused: false,
            auto_shot: Vec::new(),
            pending_auto_shot: screenshot_after,
            auto_save_state: Vec::new(),
            pending_auto_save_state: save_state_after,
            frame_dump: None,
            pending_frame_dump: frame_dump,
            auto_keys: Vec::new(),
            pending_auto_keys: press_after,
            auto_clicks: Vec::new(),
            pending_auto_clicks: click_after,
            auto_joys: Vec::new(),
            pending_auto_joys: joy_after,
            auto_joy_held: [AutoJoyHeld::default(); 2],
            auto_joy_engaged: [false; 2],
            #[cfg(feature = "control")]
            control: None,
            auto_mouse: Vec::new(),
            pending_auto_mouse: mouse_after,
            auto_mouse_to: Vec::new(),
            pending_auto_mouse_to: mouse_to_after,
            active_mouse_to: None,
            auto_pots: Vec::new(),
            pending_auto_pots: pot_after,
            auto_disk_inserts: Vec::new(),
            pending_auto_disk_inserts: disk_insert_after,
            auto_cd_inserts: Vec::new(),
            pending_auto_cd_inserts: cd_insert_after,
            input_recorder: record_input
                .is_some()
                .then(|| crate::inputrec::InputRecorder::new(0.0)),
            record_input_path: record_input,
            modifiers: ModifiersState::empty(),
            held_rawkeys: [false; 128],
            panel_held_rawkeys: [false; 128],
            raw_device_held_rawkeys: [false; 128],
            main_window_focused: false,
            window_manually_sized: false,
            snap_request_deadline: None,
            snap_when_windowed: false,
            cursor_pos: None,
            last_display_cursor_pos: None,
            last_cursor_phys: None,
            volume_dragging: false,
            analyzer_dragging: false,
            mouse_captured: false,
            capture_suspended_by_ui: false,
            mouse_delta_remainder: (0.0, 0.0),
            last_rendered_emulated_frame: None,
            last_submitted_render_frame: None,
            render_generation: 0,
            last_fdd_track: None,
            last_main_redraw_state: None,
            main_presentation_dirty: true,
            osd: None,
            drop_hover: false,
            pending_dropped_files: Vec::new(),
            image_job: None,
            disk_playlists,
            disk_write_protected,
            disk_playlist_index: [0; 4],
            hcenter: hcenter_enabled(),
            overscan,
            crt_shader_kind: shader.kind(),
            custom_shader_path: match &shader {
                crate::config::ShaderMode::Custom(path) => Some(path.clone()),
                _ => None,
            },
            shader_strength,
            bezel,
            bezel_last: last_bezel_style(bezel),
            perf_overlay,
            perf: PerfOverlay::default(),
            tint,
            tint_lut: tint_lut(tint),
            start_fullscreen,
            gamepad: crate::gamepad::GamepadReader::new(),
            joystick_input_mode,
            mouse_sensitivity,
            mouse_sensitivity_factor: mouse_sensitivity_factor(mouse_sensitivity),
            mouse_capture,
            auto_capture_hint_shown: false,
            warp_speed,
            rewind_budget_mb,
            rewind_interval_frames,
            rewind_armed,
            #[cfg(feature = "mt32")]
            mt32_panel: mt32panel::Mt32Panel::default(),
            kbd_panel: kbdpanel::KbdPanelState::default(),
            keyboard_joy_held: [keymap::HeldKeys::default(); keymap::MAPPING_COUNT],
            keymap: keymap::KeyMap::load(),
            autofire_hz,
            ui: UiState::default(),
            about_machine_lines,
            machine_config,
            paused_before_debugger: false,
            paused_before_analyzer: false,
            paused_before_console: false,
            last_debug_stop: None,
            reported_double_fault: false,
            hunt: None,
            recorder: None,
            record_fb: Vec::new(),
            record_scratch_fb: Vec::new(),
        };
        // Attach the sampler now for a directly-booted machine; the config-screen
        // placeholder passes a disabled request and attaches on Run instead.
        app.attach_session_sampler();
        if app.rewind_armed {
            app.arm_rewind_ring();
        }
        app
    }

    /// Start recording rewind history with the configured budget/interval.
    /// The debugger arms the same ring on its own terms; this re-arms it so
    /// the rewind hotkey gets the interval the user asked for.
    fn arm_rewind_ring(&mut self) {
        self.emu
            .enable_time_travel(self.rewind_budget_mb, self.rewind_interval_frames);
        // Take the first anchor now rather than at the end of the next frame,
        // so rewind can reach back to the moment recording was switched on.
        // Callers are always between frames (App::new before the first one,
        // the menu/hotkey handlers between two), which is where the renderer's
        // capture buffers are consistent enough to serialize.
        if let Err(e) = self.emu.debug_ensure_time_travel_anchor() {
            warn!("rewind: could not take the initial snapshot: {e:#}");
        }
        info!(
            "rewind: recording history ({} MiB budget, one snapshot every {} frames)",
            self.rewind_budget_mb, self.rewind_interval_frames
        );
    }

    /// Menu/hotkey toggle for rewind capture. Turning it off releases the
    /// retained snapshots, which is the point: the ring is the memory cost.
    fn toggle_rewind(&mut self) {
        self.rewind_armed = !self.rewind_armed;
        if self.rewind_armed {
            self.arm_rewind_ring();
            self.show_osd("Rewind recording on");
        } else {
            // Leave the ring alone if the debugger armed it for its own
            // reverse controls; only the user's rewind recording stops.
            if !self.debugger_wants_time_travel() {
                self.emu.disable_time_travel();
            }
            self.show_osd("Rewind recording off");
        }
    }

    /// Whether a debugger-family window is open and relying on the snapshot
    /// ring for its reverse controls.
    fn debugger_wants_time_travel(&self) -> bool {
        self.debugger_panel.is_some() || self.console_panel.is_some()
    }

    /// Rewind the machine one capture point. Unlike the debugger's reverse
    /// controls this leaves the run state alone: rewinding a running machine
    /// keeps it running, from the earlier point.
    fn rewind_one_step(&mut self) {
        use crate::timetravel::ReverseOutcome;
        if !self.emu.time_travel_enabled() {
            self.show_osd("Rewind is off (Emulator menu > Rewind)");
            return;
        }
        match self.emu.tt_rewind_step() {
            Ok(ReverseOutcome::Found(_)) => {
                let secs = self.emu.rewind_history_seconds().unwrap_or(0.0);
                self.show_osd(format!("Rewind ({secs:.0}s of history left)"));
            }
            Ok(ReverseOutcome::NotFound | ReverseOutcome::BeyondHistory) => {
                self.show_osd("Rewind: start of recorded history")
            }
            Err(e) => {
                error!("rewind step halted: {e:?}");
                self.show_osd("Rewind failed (see log)");
            }
        }
        // The restore rewrote the whole machine, including the renderer's
        // capture buffers; repaint from the restored frame rather than
        // leaving the pre-rewind image on screen.
        self.finish_render_for_current_frame();
    }

    /// Build the parallel-port sampler for the current [`self.sampler`] request
    /// and attach it to the live machine, replacing any previous one. The cpal
    /// capture stream is kept here on the main thread (it is `!Send`); its `Send`
    /// read-port goes into the bus. A disabled request detaches the port. A
    /// device-open failure logs and leaves the port empty rather than aborting.
    fn attach_session_sampler(&mut self) {
        // Drop any prior stream first so the old capture device is released
        // before a new one opens.
        self.sampler_stream = None;
        if !self.sampler.enabled {
            return;
        }
        match crate::sampler::CpalSampler::open(
            self.sampler.input_device.as_deref(),
            self.sampler.gain_db,
        ) {
            Ok((stream, port)) => {
                info!(
                    "parallel: sampler attached (input {:?})",
                    port.device_label()
                );
                self.emu.bus_mut().attach_parallel_port(Box::new(port));
                self.sampler_stream = Some(stream);
            }
            Err(e) => warn!("parallel: sampler failed to attach: {e}"),
        }
    }

    /// The port live host-mouse input drives: the lowest-numbered port with
    /// a mouse plugged in. With no mouse on either port, live mouse input is
    /// dropped.
    fn mouse_port(&self) -> Option<usize> {
        self.emu
            .bus()
            .input
            .ports
            .iter()
            .position(|p| p.device == PortDevice::Mouse)
    }

    /// Which port each host input source drives this quantum. The host
    /// mouse claims the lowest-numbered mouse port; the ports left over
    /// that a host source can drive (joysticks, CD32 pads, and a second
    /// mouse) are assigned by count:
    ///
    /// - One port: the [`JoystickInputMode`] picks its source. `Gamepad`
    ///   leaves the keyboard passing through to the Amiga -- and cannot
    ///   drive a second mouse, which is then undriven until the mode is
    ///   flipped to `Keyboard`.
    /// - Two ports (a two-controller setup): the gamepad -- backed by the
    ///   numpad keyboard mapping whenever no physical pad is present --
    ///   and the cursor-key mapping drive one each, the mode picking
    ///   which source pair gets the lower-numbered port.
    ///
    /// The cursor-key mapping drives whatever device its port carries:
    /// direction lines on a joystick/pad, pointer motion and buttons on a
    /// mouse.
    fn host_routing(&self) -> HostRouting {
        let input = &self.emu.bus().input;
        host_routing_for(
            [input.ports[0].device, input.ports[1].device],
            self.joystick_input_mode,
        )
    }

    /// Poll the host input sources and drive the emulated port(s). Called
    /// once per scheduler quantum. Scripted --joy-after state beats the
    /// keyboard mapping on a shared port and asserts alone on ports no
    /// host source drives; a present physical pad beats the scripted
    /// state on its port, as it always has.
    fn pump_joystick_input(&mut self) {
        let r = self.host_routing();
        if let Some(port) = r.gamepad {
            match self.gamepad.poll() {
                Some(state) => self.apply_joystick_state(port, state),
                // No physical pad but --joy-after scripting has fired: keep
                // asserting the scripted state so it survives this release
                // path and drives the upcoming scheduler quantum.
                None if self.auto_joy_engaged[port] => self.apply_auto_joy_state(port),
                // No pad in a two-controller setup: the numpad keyboard
                // mapping stands in for it.
                None if r.keyboard2 == Some(port) => {
                    self.apply_joystick_state(port, self.keyboard_joystick_state(1))
                }
                // Pad gone/uncalibrated: release the port so nothing sticks.
                None => self.release_joystick_lines(port),
            }
        }
        if let Some(port) = r.keyboard {
            if self.emu.bus().input.device(port) == PortDevice::Mouse {
                self.apply_keyboard_mouse_state(port);
            } else if self.auto_joy_engaged[port] {
                self.apply_auto_joy_state(port);
            } else {
                self.apply_joystick_state(port, self.keyboard_joystick_state(0));
            }
        }
        // Scripted joy state on ports no host source drives asserts
        // independently.
        for port in 0..2 {
            if Some(port) != r.gamepad && Some(port) != r.keyboard && self.auto_joy_engaged[port] {
                self.apply_auto_joy_state(port);
            }
        }
    }

    /// Whether a keyboard mapping owns its keys right now: mapping 0
    /// (cursor keys) when a port routes to the keyboard source, mapping 1
    /// (numpad) when it is the gamepad port's stand-in.
    fn keyboard_mapping_active(&self, mapping: usize) -> bool {
        let r = self.host_routing();
        if mapping == 0 {
            r.keyboard.is_some()
        } else {
            r.keyboard2.is_some()
        }
    }

    /// Drive a mouse port from the cursor-key mapping: held direction
    /// keys become steady pointer motion, the fire keys the left button,
    /// X the right, D the middle.
    fn apply_keyboard_mouse_state(&mut self, port: usize) {
        let state = self.keyboard_joystick_state(0);
        let dx =
            KEYBOARD_MOUSE_COUNTS_PER_QUANTUM * (i32::from(state.right) - i32::from(state.left));
        let dy = KEYBOARD_MOUSE_COUNTS_PER_QUANTUM * (i32::from(state.down) - i32::from(state.up));
        if dx != 0 || dy != 0 {
            self.apply_scripted_mouse_delta(port as u8, dx, dy);
        }
        let input = &mut self.emu.bus_mut().input;
        input.set_mouse_button(port, 0, state.fire);
        input.set_mouse_button(port, 1, state.button2);
        input.set_mouse_button(port, 2, state.green);
    }

    /// The controller state keyboard mapping `index` is producing right now.
    fn keyboard_joystick_state(&self, index: usize) -> crate::gamepad::JoystickState {
        self.keymap
            .mapping(index)
            .joystick_state(&self.keyboard_joy_held[index])
    }

    fn apply_joystick_state(&mut self, port: usize, mut state: crate::gamepad::JoystickState) {
        // Autofire gates a *held* fire button into a pulse train. It is a host
        // input convenience applied before the port sees anything, so the
        // emulated machine reads ordinary presses and releases on /FIRx --
        // nothing downstream knows autofire exists. Scripted --joy-after input
        // deliberately bypasses this (see apply_auto_joy_state): a recorded or
        // scripted run must replay the events it was given, verbatim.
        if state.fire
            && !crate::config::autofire_asserted(
                self.autofire_hz,
                self.emu.bus().emulated_seconds(),
            )
        {
            state.fire = false;
        }
        let input = &mut self.emu.bus_mut().input;
        input.set_joystick(
            port,
            state.up,
            state.down,
            state.left,
            state.right,
            state.fire,
            state.button2,
        );
        input.set_cd32_buttons(
            port,
            state.play,
            state.rwd,
            state.ffw,
            state.green,
            state.yellow,
        );
    }

    /// Release every control on a joystick port. A no-op unless a
    /// joystick/pad is engaged there, so a mouse sharing the line fields is
    /// never clobbered.
    fn release_joystick_lines(&mut self, port: usize) {
        let input = &mut self.emu.bus_mut().input;
        if matches!(
            input.device(port),
            PortDevice::Joystick | PortDevice::Cd32Pad
        ) {
            input.set_joystick(port, false, false, false, false, false, false);
            input.set_cd32_buttons(port, false, false, false, false, false);
        }
    }

    /// Hot-plug a controller device into a port, as if swapping the
    /// physical plug: the old device's lines release, the quadrature
    /// counters hold, and any stale scripted --joy-after ownership of the
    /// port is dropped so it cannot re-engage the old device kind on the
    /// next quantum. Not journaled for reverse replay -- like a media
    /// change, the plugged device is host state.
    fn hot_plug_port_device(&mut self, port: usize, device: PortDevice) {
        self.auto_joy_engaged[port] = false;
        self.auto_joy_held[port] = AutoJoyHeld::default();
        self.emu.bus_mut().input.set_port_device(port, device);
    }

    fn cycle_joystick_input_mode(&mut self) {
        self.set_joystick_input_mode(self.joystick_input_mode.next());
    }

    /// Current MIDI input/output device names for the runtime menu (empty when
    /// the serial port is not in MIDI mode).
    #[cfg(feature = "midi")]
    fn midi_menu_labels(&mut self) -> (String, String) {
        match self.emu.bus_mut().midi_serial_mut() {
            Some(sink) => (sink.input_label(), sink.output_label()),
            None => (String::new(), String::new()),
        }
    }

    #[cfg(not(feature = "midi"))]
    fn midi_menu_labels(&mut self) -> (String, String) {
        (String::new(), String::new())
    }

    /// Step the live audio output through "Default", the host devices, then
    /// "Disabled", rebuilding the sink so the change takes effect at once.
    /// "Disabled" swaps in a null sink -- live audio off, exactly like
    /// `--noaudio`. Freshly re-reads the device list so a just-connected device
    /// appears.
    fn cycle_audio_output(&mut self) {
        let devices = crate::audio::picker_output_devices();
        self.audio_output = self.audio_output.cycle(&devices, true);
        let realtime = crate::priority::requested(self.realtime_priority);
        match crate::audio::open_output_sink(realtime, &self.audio_output) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio = sink;
                self.sync_live_audio_suspension();
            }
            Err(e) => {
                warn!("audio: could not open the selected device; keeping silence: {e:#}");
                self.emu.bus_mut().paula.audio = Box::new(crate::audio::NullSink);
            }
        }
        self.show_osd(format!("Audio output: {}", self.audio_output.label()));
    }

    /// Cycle Paula's analogue filter override: Auto (guest-driven) -> On -> Off.
    /// Applies live and updates the PWR LED brightness on the next redraw.
    fn cycle_audio_filter(&mut self) {
        use crate::config::AudioFilterMode;
        let next = match self.emu.bus().paula.led_filter_mode() {
            AudioFilterMode::Auto => AudioFilterMode::On,
            AudioFilterMode::On => AudioFilterMode::Off,
            AudioFilterMode::Off => AudioFilterMode::Auto,
        };
        self.emu.bus_mut().paula.set_led_filter_mode(next);
        let label = match next {
            AudioFilterMode::Auto => "Auto",
            AudioFilterMode::On => "Enabled",
            AudioFilterMode::Off => "Disabled",
        };
        self.show_osd(format!("Audio filter: {label}"));
        self.request_redraw();
    }

    /// Raise (`forward`) or lower the live sampler input gain by one
    /// [`SAMPLER_GAIN_STEP_DB`] step, clamped to the sampler's dB range,
    /// rebuilding the capture so the new preamp level takes effect at once. A
    /// no-op when no sampler is attached. Bound to the runtime menu and the
    /// gain shortcut.
    fn step_sampler_gain(&mut self, forward: bool) {
        if !self.sampler.enabled {
            return;
        }
        let delta = if forward {
            SAMPLER_GAIN_STEP_DB
        } else {
            -SAMPLER_GAIN_STEP_DB
        };
        let gain_db = (self.sampler.gain_db + delta).clamp(
            crate::sampler::MIN_SAMPLER_GAIN_DB,
            crate::sampler::MAX_SAMPLER_GAIN_DB,
        );
        if gain_db == self.sampler.gain_db {
            return;
        }
        self.sampler.gain_db = gain_db;
        self.attach_session_sampler();
        self.show_osd(format!("Sampler gain: {}", sampler_gain_osd(gain_db)));
    }

    fn set_joystick_input_mode(&mut self, mode: JoystickInputMode) {
        if self.joystick_input_mode == mode {
            return;
        }
        self.joystick_input_mode = mode;
        if !matches!(self.ui.panel, Some(Panel::Calibration(_))) {
            self.pump_joystick_input();
        }
        info!("joystick input mode: {}", mode.label());
        let routing = self.host_routing();
        let osd = match (mode, routing.keyboard, routing.gamepad) {
            (JoystickInputMode::Keyboard, Some(port), _) => {
                format!("Joystick input: keyboard (port {})", port + 1)
            }
            (JoystickInputMode::Gamepad, _, Some(port)) => {
                format!("Joystick input: gamepad (port {})", port + 1)
            }
            _ => format!("Joystick input: {}", mode.label()),
        };
        self.show_osd(osd);
    }

    /// Open the Input Mapping panel on a working copy of the live map, so
    /// closing without saving changes nothing.
    fn open_input_mapping(&mut self) {
        self.ui.panel = Some(Panel::InputMap(Box::new(ui::InputMapPanel::new(
            self.keymap.clone(),
        ))));
        self.request_redraw();
    }

    fn input_map_panel_mut(&mut self) -> Option<&mut ui::InputMapPanel> {
        match self.ui.panel.as_mut() {
            Some(Panel::InputMap(panel)) => Some(panel),
            _ => None,
        }
    }

    fn input_map_select_mapping(&mut self, set: usize) {
        if let Some(panel) = self.input_map_panel_mut() {
            panel.mapping = set.min(keymap::MAPPING_COUNT - 1);
            // An armed row belongs to the mapping it was armed on.
            panel.capturing = None;
            panel.message = "Click Set, then press the key to bind.".to_string();
        }
        self.request_redraw();
    }

    fn input_map_arm_capture(&mut self, index: usize) {
        if let Some(panel) = self.input_map_panel_mut() {
            let Some(control) = keymap::CONTROLS.get(index).copied() else {
                return;
            };
            panel.capturing = Some(control);
            panel.message = format!("Press a key for {}... (Esc cancels)", control.label());
        }
        self.request_redraw();
    }

    fn input_map_clear(&mut self, index: usize) {
        if let Some(panel) = self.input_map_panel_mut() {
            let Some(control) = keymap::CONTROLS.get(index).copied() else {
                return;
            };
            let mapping = panel.mapping;
            panel.map.mapping_mut(mapping).clear(control);
            panel.capturing = None;
            panel.message = format!("{} unbound.", control.label());
        }
        self.request_redraw();
    }

    fn input_map_defaults(&mut self) {
        if let Some(panel) = self.input_map_panel_mut() {
            panel.map = keymap::KeyMap::default();
            panel.capturing = None;
            panel.message = "Restored the built-in bindings (not saved yet).".to_string();
        }
        self.request_redraw();
    }

    /// Commit the edited map: apply it to the live session and persist it.
    /// Any keys held under the old map are released first, so a binding that
    /// just moved cannot leave a controller line stuck asserted.
    fn input_map_save(&mut self) {
        let Some(map) = self.input_map_panel_mut().map(|panel| panel.map.clone()) else {
            return;
        };
        self.keyboard_joy_held = [keymap::HeldKeys::default(); keymap::MAPPING_COUNT];
        self.keymap = map;
        match self.keymap.save() {
            Ok(()) => self.show_osd("Input mapping saved"),
            Err(e) => {
                warn!("saving the keyboard map failed: {e:#}");
                self.show_osd("Input mapping applied (not saved; see log)");
            }
        }
        self.pump_joystick_input();
        self.close_panel();
    }

    /// Feed a key press to an armed Input Mapping row. Returns true when the
    /// panel consumed it, which also keeps the key out of the emulated
    /// machine while the panel is open.
    fn input_map_handle_key(&mut self, code: KeyCode) -> bool {
        let armed = matches!(
            self.ui.panel.as_ref(),
            Some(Panel::InputMap(panel)) if panel.capturing.is_some()
        );
        if !armed {
            return false;
        }
        if code == KeyCode::Escape {
            if let Some(panel) = self.input_map_panel_mut() {
                panel.capturing = None;
                panel.message = "Binding cancelled.".to_string();
            }
            self.request_redraw();
            return true;
        }
        if let Some(panel) = self.input_map_panel_mut() {
            panel.capture_key(code);
        }
        self.request_redraw();
        true
    }

    /// Consume a mapped host key as joystick input when keyboard joystick
    /// emulation is active. Releases for previously consumed mapped keys
    /// are also swallowed, even if a gamepad has taken over meanwhile.
    fn handle_keyboard_joystick_key(&mut self, code: KeyCode, pressed: bool) -> bool {
        let Some((mapping, _control)) = self.keymap.lookup(code) else {
            return false;
        };
        let active = self.keyboard_mapping_active(mapping);
        let was_held = self.keyboard_joy_held[mapping].is_set(code);
        if !active && !was_held {
            return false;
        }
        self.keyboard_joy_held[mapping].set(code, pressed);
        if active {
            // Re-run the input pump so the transition lands this quantum
            // on whatever port and device the mapping drives.
            self.pump_joystick_input();
        }
        true
    }

    /// Drive a port's emulated joystick/CD32 pad from the --joy-after
    /// held-control set.
    fn apply_auto_joy_state(&mut self, port: usize) {
        let held = self.auto_joy_held[port];
        let input = &mut self.emu.bus_mut().input;
        input.set_joystick(
            port, held.up, held.down, held.left, held.right, held.red, held.blue,
        );
        input.set_cd32_buttons(port, held.play, held.rwd, held.ffw, held.green, held.yellow);
        // Reverse-debug: note the held state so replay can reproduce it.
        self.emu
            .tt_note_input(crate::inputsched::ReplayAction::Joy {
                port: port as u8,
                state: crate::inputsched::JoyState {
                    up: held.up,
                    down: held.down,
                    left: held.left,
                    right: held.right,
                    red: held.red,
                    blue: held.blue,
                    play: held.play,
                    rwd: held.rwd,
                    ffw: held.ffw,
                    green: held.green,
                    yellow: held.yellow,
                },
            });
    }

    pub fn run(self) -> Result<()> {
        let event_loop = EventLoop::new().map_err(|e| anyhow!("EventLoop::new: {e}"))?;
        event_loop.set_control_flow(ControlFlow::Poll);
        let mut app = self;
        // Start the control server's socket threads with a wake that
        // kicks the loop out of ControlFlow::Wait, so a command arriving
        // while the machine is paused is serviced promptly.
        #[cfg(feature = "control")]
        if let Some(ctl) = app.control.as_mut() {
            let proxy = event_loop.create_proxy();
            ctl.handle.start(Box::new(move || {
                let _ = proxy.send_event(());
            }));
        }
        event_loop
            .run_app(&mut app)
            .map_err(|e| anyhow!("event loop: {e}"))?;
        Ok(())
    }
}

/// The event-loop-free half of the session driver: arming and firing the
/// scheduled capture/input flags is shared verbatim between the windowed
/// event loop (about_to_wait) and the windowless capture loop below, so a
/// capture run produces byte-identical output either way.
impl App {
    /// Drive a scheduled capture run (--screenshot-after / --dump-frames)
    /// to completion without a host window or event loop. Scheduled input
    /// and captures fire on emulated time exactly as in the windowed loop,
    /// and the run ends when the first capture completes, matching the
    /// windowed exit. Never touching winit means no display-server
    /// connection is made, so capture runs work over SSH and in sandboxes
    /// without window-server access.
    pub fn run_headless(mut self) -> Result<()> {
        self.arm_scheduled_events();
        if self.auto_shot.is_empty() && self.frame_dump.is_none() {
            return Err(anyhow!(
                "windowless capture run needs --screenshot-after or --dump-frames"
            ));
        }
        loop {
            // The windowed loop parks a halted machine so the user can
            // inspect it; with no window the captures can never fire, so
            // surface the halt as the run's failure.
            if let Err(e) = self.emu.step_frame() {
                return Err(anyhow!(
                    "emulator halted before the scheduled captures completed: {e:#}"
                ));
            }
            // Audio may be live (--screenshot-after without --noaudio):
            // recover a lost output device exactly as the windowed loop does.
            self.recover_audio_if_device_lost();
            self.render_emulated_frame_if_needed();
            if self.dump_frame_if_due() {
                return Ok(());
            }
            self.fire_scheduled_events();
            self.fire_auto_save_state();
            if self.fire_auto_shot() {
                return Ok(());
            }
        }
    }

    /// Arm every scheduled capture and input flag: pending (parse-time)
    /// entries become live ones gated on emulated time. Scheduled events
    /// are gated on emulated time (like disk inserts and the
    /// auto-screenshot): headless runs are unthrottled, so wall-clock
    /// scheduling would fire at the wrong emulated point or never fire at
    /// all before the run exits.
    fn arm_scheduled_events(&mut self) {
        for (secs, path) in std::mem::take(&mut self.pending_auto_shot) {
            info!(
                "auto-screenshot armed: will save {} after {:.1}s emulated time",
                path.display(),
                secs
            );
            self.auto_shot.push((secs.max(0.0), path));
        }
        // Deadline order, not command-line order, so the run ends on the
        // latest capture however the flags were written. The sort is
        // stable, so captures sharing a deadline keep their given order.
        self.auto_shot.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        for (secs, path) in std::mem::take(&mut self.pending_auto_save_state) {
            info!(
                "auto-save-state armed: will save {} after {:.1}s emulated time",
                path.display(),
                secs
            );
            self.auto_save_state.push((secs.max(0.0), path));
        }
        self.auto_save_state
            .sort_by(|(a, _), (b, _)| a.total_cmp(b));
        if let Some(spec) = self.pending_frame_dump.take() {
            info!(
                "frame dump armed: will save {} frames to {} after {:.1}s emulated time",
                spec.count,
                spec.dir.display(),
                spec.start_secs
            );
            self.frame_dump = Some(FrameDumpState {
                start_secs: spec.start_secs.max(0.0),
                dir: spec.dir,
                count: spec.count,
                dumped: 0,
                last_saved_emulated_frame: None,
            });
        }
        // Scheduled keys/clicks are gated on emulated time (like disk
        // inserts and the auto-screenshot): headless runs are unthrottled,
        // so wall-clock scheduling would fire at the wrong emulated point
        // or never fire at all before the run exits.
        for key in self.pending_auto_keys.drain(..) {
            let press_at = key.secs.max(0.0) as f64;
            let release_at = press_at + key.hold_ms as f64 / 1000.0;
            info!(
                "auto-key armed: rawkey {:#04X} press at {:.1}s emulated, hold {}ms",
                key.rawkey, key.secs, key.hold_ms
            );
            self.auto_keys.push(ScheduledKey {
                press_at_emulated_secs: press_at,
                release_at_emulated_secs: release_at,
                rawkey: key.rawkey,
                pressed: false,
            });
        }
        for (secs, button, dur_ms, port) in self.pending_auto_clicks.drain(..) {
            let press_at = secs.max(0.0) as f64;
            let release_at = press_at + dur_ms as f64 / 1000.0;
            info!(
                "auto-click armed: {:?} press at {:.1}s emulated, hold {}ms, port {}",
                button,
                secs,
                dur_ms,
                port + 1
            );
            self.auto_clicks.push(ScheduledClick {
                press_at_emulated_secs: press_at,
                release_at_emulated_secs: release_at,
                button,
                port,
                pressed: false,
            });
        }
        for (secs, dx, dy, port) in self.pending_auto_mouse.drain(..) {
            self.auto_mouse.push((secs.max(0.0) as f64, dx, dy, port));
        }
        for (secs, x, y, port) in self.pending_auto_mouse_to.drain(..) {
            self.auto_mouse_to.push((secs.max(0.0) as f64, x, y, port));
        }
        if !self.auto_mouse_to.is_empty() {
            // Earliest first: the firing pass takes one at a time, and a
            // servo holds the pointer until it lands or gives up.
            self.auto_mouse_to
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            info!(
                "auto-mouse-to armed: {} scheduled pointer targets",
                self.auto_mouse_to.len()
            );
        }
        if !self.auto_mouse.is_empty() {
            info!(
                "auto-mouse armed: {} scheduled motions",
                self.auto_mouse.len()
            );
        }
        for (secs, x, y, port) in self.pending_auto_pots.drain(..) {
            self.auto_pots.push((secs.max(0.0) as f64, x, y, port));
        }
        if !self.auto_pots.is_empty() {
            info!(
                "auto-pot armed: {} scheduled positions",
                self.auto_pots.len()
            );
        }
        for (secs, button, dur_ms, port) in self.pending_auto_joys.drain(..) {
            let press_at = secs.max(0.0) as f64;
            let release_at = press_at + dur_ms as f64 / 1000.0;
            info!(
                "auto-joy armed: {:?} press at {:.1}s emulated, hold {}ms, port {}",
                button,
                secs,
                dur_ms,
                port + 1
            );
            self.auto_joys.push(ScheduledJoy {
                press_at_emulated_secs: press_at,
                release_at_emulated_secs: release_at,
                button,
                port,
                pressed: false,
            });
        }
        for insert in self.pending_auto_disk_inserts.drain(..) {
            let insert_at_emulated_secs = insert.secs.max(0.0) as f64;
            info!(
                "auto-disk armed: df{} insert {} at {:.1}s emulated time",
                insert.drive_idx,
                insert.path.display(),
                insert.secs
            );
            self.auto_disk_inserts.push(ScheduledDiskInsert {
                insert_at_emulated_secs,
                drive_idx: insert.drive_idx,
                path: insert.path,
                write_protected: insert.write_protected,
            });
        }
        for (secs, path) in self.pending_auto_cd_inserts.drain(..) {
            info!(
                "auto-cd armed: insert {} at {secs:.1}s emulated time",
                path.display()
            );
            self.auto_cd_inserts.push((secs.max(0.0) as f64, path));
        }
    }

    /// Fire any scheduled key/click/joy/mouse/pot/disk/CD events whose
    /// emulated timestamps have passed, then let the input recorder
    /// observe the resulting machine-visible input state once for this
    /// quantum. Runs after step_frame so events land at frame boundaries.
    fn fire_scheduled_events(&mut self) {
        // Fire any scheduled key/click/disk events on emulated time
        // (mirroring the auto-screenshot below): headless runs are
        // unthrottled, so wall-clock gating would land the events at the
        // wrong emulated point or after the run already exited.
        let emu_secs = self.emu.bus().emulated_seconds();
        let mut key_events = Vec::new();
        self.auto_keys.retain_mut(|key| {
            if !key.pressed && emu_secs >= key.press_at_emulated_secs {
                info!("auto-key pressing: rawkey {:#04X}", key.rawkey);
                key_events.push((key.rawkey, true));
                key.pressed = true;
            }
            if key.pressed && emu_secs >= key.release_at_emulated_secs {
                info!("auto-key releasing: rawkey {:#04X}", key.rawkey);
                key_events.push((key.rawkey, false));
                false
            } else {
                true
            }
        });
        for (rawkey, pressed) in key_events {
            self.handle_amiga_key_event(rawkey, pressed);
        }
        // Fire any scheduled --click-after events: transition the
        // corresponding button to pressed at press_at, released at
        // release_at, then drop the entry.
        self.auto_clicks.retain_mut(|c| {
            if !c.pressed && emu_secs >= c.press_at_emulated_secs {
                info!("auto-click pressing: {:?} (port {})", c.button, c.port + 1);
                set_mouse_button(&mut self.emu, c.port, c.button, true);
                c.pressed = true;
            }
            if c.pressed && emu_secs >= c.release_at_emulated_secs {
                info!("auto-click releasing: {:?}", c.button);
                set_mouse_button(&mut self.emu, c.port, c.button, false);
                return false;
            }
            true
        });
        // Fire any scheduled --joy-after events into the named port's
        // joystick/CD32-pad state, then assert the held sets (input polling
        // re-applies them every quantum while scripting is engaged).
        let mut joy_changed = [false; 2];
        let held = &mut self.auto_joy_held;
        self.auto_joys.retain_mut(|j| {
            let port = usize::from(j.port != 0);
            if !j.pressed && emu_secs >= j.press_at_emulated_secs {
                info!("auto-joy pressing: {:?} (port {})", j.button, port + 1);
                held[port].set(j.button, true);
                j.pressed = true;
                joy_changed[port] = true;
            }
            if j.pressed && emu_secs >= j.release_at_emulated_secs {
                info!("auto-joy releasing: {:?}", j.button);
                held[port].set(j.button, false);
                joy_changed[port] = true;
                return false;
            }
            true
        });
        for port in 0..2 {
            if joy_changed[port] {
                self.auto_joy_engaged[port] = true;
                self.apply_auto_joy_state(port);
            }
        }
        // Fire any scheduled --mouse-after relative motions (one-shot
        // each); these land on the named port's quadrature counters,
        // whatever device is configured there (the lines are the lines).
        // Held back while a --mouse-to-after servo is steering: the servo
        // measures the pointer's response to its own counts to learn the
        // guest's acceleration, so motion from another source in the same
        // frame is attributed to it and corrupts the estimate. The
        // deferral is bounded by the servo's own frame budget.
        //
        // The servo is advanced first so a target coming due this frame
        // takes ownership before the deltas are considered, and so the
        // frame it finishes on releases them again immediately.
        // Only while the machine is actually advancing. fire_scheduled_events
        // runs on every event-loop pass, including while paused or powered
        // off; polling the servo there would compare the same frame to
        // itself, inject counts the guest never gets to act on, and then
        // call a reachable target stuck -- with the phantom deltas landing
        // on unpause.
        if self.powered_on && !self.paused && !self.cpu_halted {
            self.advance_scripted_pointer_targets(emu_secs);
        }
        let mut mouse_deltas = Vec::new();
        if self.active_mouse_to.is_none() {
            self.auto_mouse.retain(|&(at, dx, dy, port)| {
                if emu_secs >= at {
                    mouse_deltas.push((dx, dy, port));
                    false
                } else {
                    true
                }
            });
        }
        for (dx, dy, port) in mouse_deltas {
            self.apply_scripted_mouse_delta(port, dx, dy);
        }
        // Fire any scheduled --pot-after analogue positions (one-shot
        // each).
        let mut pot_sets = Vec::new();
        self.auto_pots.retain(|&(at, x, y, port)| {
            if emu_secs >= at {
                pot_sets.push((x, y, port));
                false
            } else {
                true
            }
        });
        for (x, y, port) in pot_sets {
            info!("auto-pot: position ({x}, {y}) on port {}", port + 1);
            self.emu.bus_mut().input.set_analogue(port as usize, x, y);
            self.emu
                .tt_note_input(crate::inputsched::ReplayAction::Pot { port, x, y });
        }
        let mut disk_inserts = Vec::new();
        self.auto_disk_inserts.retain(|insert| {
            if emu_secs >= insert.insert_at_emulated_secs {
                disk_inserts.push(insert.clone());
                false
            } else {
                true
            }
        });
        for insert in disk_inserts {
            self.insert_disk_image(insert.drive_idx, insert.path, insert.write_protected);
        }
        // Fire any scheduled --insert-cd-after swaps (one-shot each).
        let mut cd_inserts = Vec::new();
        self.auto_cd_inserts.retain(|(at, path)| {
            if emu_secs >= *at {
                cd_inserts.push(path.clone());
                false
            } else {
                true
            }
        });
        for path in cd_inserts {
            if self.emu.bus().cd_drive_present() {
                self.insert_cd_image_from_path(&path);
            } else {
                warn!(
                    "--insert-cd-after {}: no CD drive on this machine",
                    path.display()
                );
            }
        }
        // Input recording: with every input source for this quantum
        // applied (live, gamepad, and the scheduled events above), diff
        // the machine-visible input state once at this quantum's emulated
        // timestamp. Skipped while the core is not advancing so paused
        // wall-clock time records nothing.
        if self.powered_on && !self.cpu_halted && !self.paused {
            if let Some(rec) = self.input_recorder.as_mut() {
                rec.observe(&self.emu.bus().input, emu_secs);
            }
        }
    }

    /// Scheduled --save-state-after capture. Runs after step_frame for
    /// the quantum, so the machine is at the frame-boundary quiescent
    /// point save states require. Unlike the auto-screenshot this does not
    /// end the run: a state save is a capture along the way, not the end
    /// of a verification run.
    fn fire_auto_save_state(&mut self) {
        if self.auto_save_state.is_empty() {
            return;
        }
        let now = self.emu.bus().emulated_seconds();
        // Deadline-ordered, so everything still pending starts at the first
        // entry that is not due yet.
        let due = self
            .auto_save_state
            .iter()
            .take_while(|(secs, _)| now >= *secs as f64)
            .count();
        for (_, path) in self.auto_save_state.drain(..due).collect::<Vec<_>>() {
            match self.emu.save_state(&path) {
                Ok(()) => info!("auto-save-state saved: {}", path.display()),
                Err(e) => warn!("auto-save-state failed ({}): {e:#}", path.display()),
            }
        }
    }

    /// Fire every scheduled --screenshot-after capture whose emulated
    /// timestamp has passed. Returns true when the last one has been saved
    /// and the run is complete (both loops exit on it); captures stay armed
    /// while the target frame is still being rendered, and a run with more
    /// captures still pending keeps going.
    fn fire_auto_shot(&mut self) -> bool {
        if self.auto_shot.is_empty() {
            return false;
        }
        let now = self.emu.bus().emulated_seconds();
        // Deadline-ordered, so everything still pending starts at the first
        // entry that is not due yet.
        let due = self
            .auto_shot
            .iter()
            .take_while(|(secs, _)| now >= *secs as f64)
            .count();
        if due == 0 {
            return false;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        self.finish_render_for_current_frame();
        if self.last_rendered_emulated_frame != Some(emulated_frame) {
            return false;
        }
        // Captures that came due together all describe this frame, so they
        // all get it rather than drifting a frame apart.
        for (_, path) in self.auto_shot.drain(..due).collect::<Vec<_>>() {
            self.save_screenshot(&path);
        }
        if !self.auto_shot.is_empty() {
            return false;
        }
        self.emu.report_stats();
        self.emu.bus().poll_stats.dump_top("at screenshot");
        // Evaluate an untargeted reverse watchpoint at run end.
        if let Err(e) = self.emu.tt_finalize_reverse_watch() {
            warn!("reverse watchpoint evaluation failed: {e:#}");
        }
        true
    }
}

impl Drop for App {
    /// Flush a whole-run `--record-input` recording on any exit path
    /// (auto-screenshot exit, window close, shortcut quit). The interactive
    /// recording toggle writes its file when stopped, so by the time the app
    /// drops there is nothing left for it here.
    fn drop(&mut self) {
        let (Some(rec), Some(path)) = (self.input_recorder.take(), self.record_input_path.take())
        else {
            return;
        };
        let events = rec.events_recorded();
        match std::fs::write(&path, rec.finish()) {
            Ok(()) => info!(
                "input recording saved: {} ({events} events)",
                path.display()
            ),
            Err(e) => warn!("input recording save failed ({}): {e:#}", path.display()),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.render.is_some() {
            return;
        }
        // Keep the internal overscan field buffer, but present it with
        // the configured pixel aspect: a standard 4:3 Amiga display by
        // default, or square pixels ([display] pixel_aspect = "square").
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        // Headless capture (screenshot / frame dump) renders into the
        // framebuffer for the saved PNG but has no interactive viewer, so
        // create the window hidden: it avoids flashing an empty window on
        // screen and removes the vsync present gate, letting the run
        // advance as fast as the host allows. Emulated state is identical.
        let headless_capture =
            !self.pending_auto_shot.is_empty() || self.pending_frame_dump.is_some();
        // Start fullscreen only for an interactive window ([display] full_screen
        // / --full-screen); a headless capture window stays hidden and windowed.
        let fullscreen =
            (self.start_fullscreen && !headless_capture).then(|| Fullscreen::Borderless(None));
        let attrs = WindowAttributes::default()
            .with_title(WINDOW_TITLE)
            .with_window_icon(copperline_window_icon())
            .with_visible(!headless_capture)
            .with_fullscreen(fullscreen)
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(
                FB_WIDTH as f64 / 2.0,
                window_present_height() as f64 / 2.0,
            ));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                error!("create_window failed: {e}");
                event_loop.exit();
                return;
            }
        };
        // winit's with_window_icon above does nothing for the macOS dock; set
        // the application icon explicitly now that NSApplication exists.
        #[cfg(target_os = "macos")]
        set_macos_dock_icon();
        let inner = window.inner_size();
        let (texture_scale, scaling_mode) = plan_present_scaling(
            integer_scaling_requested(),
            window.scale_factor(),
            (inner.width.max(1), inner.height.max(1)),
        );
        // On Linux, restrict wgpu to the Vulkan backend. wgpu's GL fallback
        // initializes its EGL instance without a display handle (pixels uses
        // InstanceDescriptor::new_without_display_handle), so EGL drops to the
        // Mesa "surfaceless" platform, which is not compatible with an on-screen
        // window surface -- adapter selection then fails with "No suitable
        // wgpu::Adapter found" on any machine that lacks a hardware Vulkan
        // driver. Vulkan does not need the display handle at instance creation,
        // so it works; GPUs without a hardware Vulkan driver (pre-Skylake Intel
        // and other pre-2015 parts) can fall back to the software lavapipe ICD.
        // An explicit WGPU_BACKEND override is still honoured for debugging.
        // Other platforms keep wgpu's default backend set (Metal on macOS,
        // DX12/Vulkan on Windows). cfg!() (not #[cfg]) keeps the Linux branch
        // type-checked on every host.
        let pixels =
            match build_pixels_for_window(window.clone(), texture_scale, true, scaling_mode) {
                Ok(p) => p,
                Err(e) => {
                    error!("pixels init failed: {e}");
                    if cfg!(target_os = "linux") {
                        error!(
                            "Copperline requires a Vulkan driver on Linux. Update your GPU \
                         drivers, or install a software Vulkan ICD (lavapipe): \
                         'vulkan-swrast' on Arch, 'mesa-vulkan-drivers' on Debian/Ubuntu, \
                         'mesa-vulkan-drivers' (or 'vulkan-loader') on Fedora."
                        );
                    }
                    event_loop.exit();
                    return;
                }
            };
        info!(
            "window + pixels surface ready ({}x{}, texture {}x{})",
            inner.width,
            inner.height,
            texture_width(texture_scale),
            texture_height(texture_scale)
        );
        let rtg_texture =
            rtg_texture::RtgTexture::new(pixels.device(), pixels.render_texture_format());
        let mut crt_shader =
            crt_shader::CrtShader::new(pixels.device(), pixels.render_texture_format());
        let bezel_shader = bezel::BezelShader::new(pixels.device(), pixels.render_texture_format());
        // A user shader can only be compiled once the device exists (too
        // early for `reload_custom_shader`, which needs the built `Render`).
        // A bad one drops back to no shader rather than failing the session:
        // the log takes naga's whole multi-line diagnostic, the overlay
        // below only its first line.
        let shader_error = match (self.crt_shader_kind, self.custom_shader_path.as_deref()) {
            (crate::config::ShaderKind::Custom, Some(path)) => crt_shader
                .load_custom(pixels.device(), pixels.render_texture_format(), path)
                .err()
                .map(|msg| {
                    error!("[display] shader: {msg}");
                    msg.lines().next().unwrap_or_default().to_string()
                }),
            _ => None,
        };
        if shader_error.is_some() {
            self.crt_shader_kind = crate::config::ShaderKind::None;
        }
        self.render = Some(Render {
            window,
            pixels,
            texture_scale,
            rtg_texture,
            crt_shader,
            bezel_shader,
            minimized: false,
            surface_size: (inner.width.max(1), inner.height.max(1)),
        });
        // After the window exists, so the overlay has somewhere to be drawn.
        if let Some(msg) = shader_error {
            self.show_osd(format!("CRT shader: off (custom failed: {msg})"));
        }
        // Paint at least once so the status bar (and power button) is
        // visible immediately, even when the machine starts powered off
        // and no emulated frame is being produced yet. A powered-off
        // start shows the test screen rather than a black display.
        if !self.powered_on {
            paint_test_screen(&mut self.fb);
            self.deinterlacer
                .push_field(&self.fb, FB_HEIGHT, FB_WIDTH, false, true, true);
            self.refresh_present_from_deinterlacer();
        }
        self.request_redraw();
        self.arm_scheduled_events();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if let Some(kind) = self.tool_window_kind(window_id) {
            self.handle_tool_window_event(event_loop, kind, event);
            return;
        }
        if self
            .render
            .as_ref()
            .is_some_and(|render| render.window.id() != window_id)
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(code),
                        repeat,
                        text,
                        ..
                    },
                ..
            } => {
                if self.should_drop_repeated_main_key(code, state, repeat) {
                    return;
                }
                // An open menu takes the keyboard first: while it is up the
                // cursor keys walk it rather than reaching the Amiga.
                if self.ui.menu_open
                    && !self.ui.menu_rows.is_empty()
                    && state == ElementState::Pressed
                    && self.handle_menu_key(code, event_loop)
                {
                    return;
                }
                match (code, state) {
                    (KeyCode::KeyQ, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        event_loop.exit()
                    }
                    (KeyCode::KeyS, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        self.save_state_interactive()
                    }
                    (KeyCode::KeyL, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        self.load_state_from_dialog(Some(event_loop))
                    }
                    (KeyCode::KeyS, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.take_screenshot()
                    }
                    (KeyCode::KeyD, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.cycle_disk()
                    }
                    (KeyCode::KeyG, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        // Capturing the mouse under an open menu/panel would
                        // hide the cursor the panel needs.
                        if !self.modal_ui_active() {
                            self.toggle_mouse_capture()
                        }
                    }
                    (KeyCode::KeyE, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_menu();
                    }
                    (KeyCode::KeyB, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_debugger();
                        self.ensure_tool_windows_for_open_panels(event_loop);
                    }
                    (KeyCode::KeyK, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_console();
                        self.ensure_tool_windows_for_open_panels(event_loop);
                    }
                    (KeyCode::KeyJ, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.cycle_joystick_input_mode()
                    }
                    (KeyCode::KeyM, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_bezel()
                    }
                    (KeyCode::KeyP, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_perf_overlay()
                    }
                    (KeyCode::KeyF, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        self.toggle_status_bar()
                    }
                    (KeyCode::KeyF, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_fullscreen()
                    }
                    (KeyCode::KeyR, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        self.toggle_input_recording()
                    }
                    (KeyCode::KeyR, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_recording()
                    }
                    (KeyCode::KeyW, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        self.cycle_warp_speed()
                    }
                    (KeyCode::KeyZ, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        // Rewind acts on the running machine, so ignore it
                        // while a menu or panel has the foreground.
                        if !self.modal_ui_active() {
                            self.rewind_one_step()
                        }
                    }
                    (KeyCode::KeyW, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_warp()
                    }
                    (KeyCode::KeyA, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        // Cycle the live audio output (Default -> devices ->
                        // Disabled), same as the menu's Audio Out item. Ignored
                        // while a menu/panel is open, so it acts only on the
                        // running machine, not the config-screen placeholder.
                        if !self.modal_ui_active() {
                            self.cycle_audio_output()
                        }
                    }
                    (KeyCode::KeyA, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        // Cycle Paula's analogue filter (auto -> on -> off),
                        // the counterpart to Cmd/Alt+Shift+A. Ignored while a
                        // menu or panel is open.
                        if !self.modal_ui_active() {
                            self.cycle_audio_filter()
                        }
                    }
                    // Quick save/load slots on the number row: the modifier
                    // alone saves, adding Shift loads. Matched on the
                    // physical key so the mapping holds on non-QWERTY
                    // layouts, and `0` is the tenth slot as it sits.
                    (code, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && save_slot_for_key(code).is_some()
                            && !self.modal_ui_active() =>
                    {
                        let slot = save_slot_for_key(code).expect("guarded above");
                        if self.modifiers.shift_key() {
                            self.quick_load_state(slot, Some(event_loop));
                        } else {
                            self.quick_save_state(slot);
                        }
                    }
                    (KeyCode::Equal, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        // Raise the sampler input gain (Shift+= is "+"). A no-op
                        // unless a sampler is attached; ignored under a menu/panel.
                        if !self.modal_ui_active() {
                            self.step_sampler_gain(true)
                        }
                    }
                    (KeyCode::Minus, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        // Lower the sampler input gain (Shift+- is "_").
                        if !self.modal_ui_active() {
                            self.step_sampler_gain(false)
                        }
                    }
                    (KeyCode::Period, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        // Raise the host mouse sensitivity (Shift+. is ">").
                        if !self.modal_ui_active() {
                            self.step_mouse_sensitivity(true)
                        }
                    }
                    (KeyCode::Comma, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers)
                            && self.modifiers.shift_key() =>
                    {
                        // Lower the host mouse sensitivity (Shift+, is "<").
                        if !self.modal_ui_active() {
                            self.step_mouse_sensitivity(false)
                        }
                    }
                    (other, state) => {
                        let pressed = state == ElementState::Pressed;
                        if pressed && self.ui_handle_key(other, text.as_deref()) {
                            return;
                        }
                        // Open panels are modal: key presses must not leak
                        // into the emulated machine. Releases still pass so
                        // a key held across opening a panel is not stuck.
                        if pressed && self.modal_ui_active() {
                            return;
                        }
                        if self.handle_keyboard_joystick_key(other, pressed) {
                            return;
                        }
                        if let Some(rawkey) = host_to_amiga_rawkey(other) {
                            self.handle_amiga_key_event(rawkey, pressed);
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.update_host_modifiers(modifiers.state());
            }
            WindowEvent::CursorMoved { position, .. } => {
                let previous_cursor_pos = self.cursor_pos;
                self.last_cursor_phys = Some(position);
                let pos = self
                    .render
                    .as_ref()
                    .and_then(|r| cursor_texture_position(&r.pixels, position, r.texture_scale));
                // A button held on the dial turns it by following the hand
                // round the face.
                #[cfg(feature = "mt32")]
                if self.mt32_panel.dial_held() {
                    if let Some(pos) = pos {
                        self.drag_mt32_dial(pos);
                    }
                }
                if self.mouse_captured {
                    self.cursor_pos = None;
                    self.last_display_cursor_pos = None;
                } else {
                    // While a menu/panel is open, the host cursor is
                    // operating the UI; don't feed its motion to the
                    // emulated mouse underneath.
                    if self.modal_ui_active() {
                        self.last_display_cursor_pos = None;
                    } else {
                        self.track_uncaptured_cursor_motion(pos);
                    }
                    self.cursor_pos = pos;
                    if self.volume_dragging {
                        if let Some(pos) = pos {
                            self.set_output_volume_from_pos(pos);
                        }
                    }
                }
                // The pointer moves the same cursor the keys do, so hovering
                // a row lights it and Return would take it.
                if self.follow_menu_hover() {
                    self.request_redraw();
                }
                let layout = bar_layout(&self.media_bar());
                // The MT-32's buttons light under the pointer as the bar's
                // do, so they need the same redraw when it crosses one.
                #[cfg(feature = "mt32")]
                let mt32_hover_changed =
                    mt32panel::shown_panel_rect(present_height()).is_some_and(|panel| {
                        mt32panel::hover_changed(panel, previous_cursor_pos, self.cursor_pos)
                    });
                #[cfg(not(feature = "mt32"))]
                let mt32_hover_changed = false;
                // The keycaps light under the pointer the same way.
                let kbd_hover_changed = kbdpanel::shown_panel_rect(keyboard_panel_top())
                    .is_some_and(|panel| {
                        kbdpanel::hover_changed(panel, previous_cursor_pos, self.cursor_pos)
                    });
                if bar_hover_changed(&layout, previous_cursor_pos, self.cursor_pos)
                    || mt32_hover_changed
                    || kbd_hover_changed
                    || self.main_ui_hover_changed(previous_cursor_pos, self.cursor_pos)
                {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let previous_cursor_pos = self.cursor_pos;
                self.cursor_pos = None;
                self.last_display_cursor_pos = None;
                self.volume_dragging = false;
                self.analyzer_dragging = false;
                let layout = bar_layout(&self.media_bar());
                if bar_hover_changed(&layout, previous_cursor_pos, self.cursor_pos) {
                    self.request_redraw();
                }
            }
            WindowEvent::HoveredFile(_) => {
                // One event per hovered file; flag once and redraw once.
                if !self.drop_hover {
                    self.drop_hover = true;
                    self.request_redraw();
                }
            }
            WindowEvent::HoveredFileCancelled => {
                self.drop_hover = false;
                self.request_redraw();
            }
            WindowEvent::DroppedFile(path) => {
                // No HoveredFileCancelled follows a successful drop, so the
                // hint is cleared here. One event arrives per dropped file;
                // they are coalesced into a single action in about_to_wait.
                self.drop_hover = false;
                self.pending_dropped_files.push(path);
                self.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                self.main_window_focused = focused;
                if focused {
                    // A capture a panel borrowed can only be repaid to a
                    // focused window, so a panel that closed while the focus
                    // was still elsewhere left the loan outstanding for this
                    // moment.
                    self.restore_mouse_capture_after_ui();
                    // In auto mode the grab follows the focus, so the
                    // window that has the keyboard also has the pointer and
                    // no host cursor is ever loose over the display. This is
                    // also the start-up grab: the first Focused(true) is the
                    // one that arrives when the window opens.
                    self.apply_auto_mouse_capture();
                } else {
                    self.volume_dragging = false;
                    self.analyzer_dragging = false;
                    self.set_mouse_captured(false);
                    // The button that was holding a keycap will lift over
                    // some other window, where no MouseInput reaches us.
                    self.release_keyboard_panel_key();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed {
                    self.log_cursor_diag(button);
                }
                if button == MouseButton::Left {
                    if pressed {
                        self.analyzer_dragging = false;
                    } else {
                        let was_volume_dragging = self.volume_dragging;
                        self.volume_dragging = false;
                        self.analyzer_dragging = false;
                        if was_volume_dragging {
                            return;
                        }
                    }
                }
                if pressed && !self.mouse_captured && self.modal_ui_active() {
                    if button == MouseButton::Left {
                        if let Some(control) =
                            self.cursor_pos.and_then(|p| self.main_ui_control_at(p))
                        {
                            self.activate_ui_control_with_event_loop(control, Some(event_loop));
                            self.ensure_tool_windows_for_open_panels(event_loop);
                            return;
                        }
                    }
                    if self.ui.menu_open {
                        // A click anywhere off the menu closes it, except on
                        // the menu button itself, whose own handler toggles.
                        if !self
                            .cursor_pos
                            .is_some_and(|p| menu_button_rect().contains(p))
                        {
                            self.close_menu();
                            self.request_redraw();
                        }
                    }
                    // Swallow display clicks under an open menu/panel so
                    // they neither capture the mouse nor reach the Amiga;
                    // status-bar controls below stay clickable.
                    if self.cursor_pos.is_some_and(cursor_in_display) {
                        return;
                    }
                }
                // The MT-32's panel sits above the status bar and takes its
                // own clicks, the way the bar does.
                #[cfg(feature = "mt32")]
                if !pressed {
                    self.mt32_panel.release_dial();
                    // The buttons are momentary: one lights while the mouse
                    // is down on it and comes back out when it lifts.
                    self.mt32_panel.release_press();
                    self.request_redraw();
                }
                #[cfg(feature = "mt32")]
                if pressed
                    && !self.mouse_captured
                    && matches!(button, MouseButton::Left | MouseButton::Right)
                {
                    if let (Some(pos), Some(panel)) = (
                        self.cursor_pos,
                        mt32panel::shown_panel_rect(present_height()),
                    ) {
                        if panel.contains(pos) {
                            if let Some(control) = mt32panel::control_at(panel, pos) {
                                let left = button == MouseButton::Left;
                                self.press_mt32_control(control, left, pos);
                            }
                            return;
                        }
                    }
                }
                // A cap on the on-screen keyboard is let go wherever the
                // pointer has got to: one mouse button holds one key, so
                // the lift ends it even if the hand slid off the strip.
                if !pressed && button == MouseButton::Left && self.release_keyboard_panel_key() {
                    return;
                }
                // The keyboard strip sits between the MT-32's panel and the
                // status bar and takes its own clicks, as they both do.
                if pressed && !self.mouse_captured && button == MouseButton::Left {
                    if let (Some(pos), Some(panel)) = (
                        self.cursor_pos,
                        kbdpanel::shown_panel_rect(keyboard_panel_top()),
                    ) {
                        if panel.contains(pos) {
                            if let Some(control) = kbdpanel::control_at(panel, pos) {
                                self.press_keyboard_panel_control(control);
                            }
                            return;
                        }
                    }
                }
                if pressed
                    && !self.mouse_captured
                    && self.cursor_pos.is_some_and(cursor_in_status_bar)
                {
                    if button == MouseButton::Left {
                        if let Some(pos) = self.cursor_pos {
                            let layout = bar_layout(&self.media_bar());
                            match control_at(pos, &layout) {
                                Some(BarControl::Volume) => {
                                    self.volume_dragging = true;
                                    self.set_output_volume_from_pos(pos);
                                }
                                Some(control) => self.activate_bar_control(control),
                                None => {}
                            }
                        }
                    }
                    return;
                }
                if pressed
                    && !self.mouse_captured
                    && self.mouse_capture != crate::config::MouseCapture::Manual
                    && self.cursor_pos.is_some_and(cursor_in_display)
                {
                    // With no mouse on either port there is nothing to
                    // drive: grabbing and hiding the host cursor would
                    // just trap it.
                    if self.mouse_port().is_some() {
                        self.set_mouse_captured(true);
                        // The click that takes the grab is a window-management
                        // action, not an Amiga click. Forwarding it as well
                        // lands a press on the guest immediately before the
                        // first intended one, and two presses that close
                        // together are what Intuition's double-click window is
                        // looking for -- so a single deliberate click on a
                        // gadget arrives as a double click. Swallow it, but
                        // only once the grab has actually taken: if it failed
                        // the mouse stays uncaptured and this click is the only
                        // thing the guest is going to get.
                        if self.mouse_captured {
                            return;
                        }
                    } else {
                        self.show_osd("No mouse on either port".to_string());
                    }
                }
                if let Some(port) = self.mouse_port() {
                    let input = &mut self.emu.bus_mut().input;
                    match button {
                        MouseButton::Left => input.set_mouse_button(port, 0, pressed),
                        MouseButton::Right => input.set_mouse_button(port, 1, pressed),
                        MouseButton::Middle => input.set_mouse_button(port, 2, pressed),
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // An open menu claims the wheel and does nothing with it,
                // rather than letting it reach the volume slider underneath.
                if self.ui.menu_open {
                } else if !self.mouse_captured
                    && self
                        .cursor_pos
                        .is_some_and(|pos| volume_control_hit_rect().contains(pos))
                {
                    if let Some(steps) = volume_scroll_steps(delta) {
                        self.adjust_output_volume(steps * VOLUME_STEP_PERCENT);
                    }
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The host DPI changed -- the GNOME/Wayland scale setting was
                // altered while running, or (the common case) the window was
                // dragged onto a monitor with a different scale factor. winit
                // resizes the surface via the Resized event that follows, but
                // the backing texture is sized FB_WIDTH x window height
                // times an integer supersample factor captured at window
                // creation. Left stale, cursor_texture_position maps clicks
                // against a texture extent that no longer matches the surface,
                // so a status-bar click is mis-classified as a display click
                // and grabs the mouse. Re-plan for the new scale (which also
                // re-fits integer scaling, whose factor tracks the surface
                // rather than the DPI); the Resized event that follows
                // recomputes the scaling matrix from it.
                if let Some(r) = self.render.as_mut() {
                    let surface = r.window.inner_size();
                    if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                        warn!("resize texture buffer for scale {scale_factor} failed: {e}");
                    }
                }
                self.request_redraw();
            }
            WindowEvent::Resized(size) => {
                self.note_window_resize(size);
                self.apply_surface_size(size);
            }
            WindowEvent::RedrawRequested => {
                self.resync_surface_size();
                if self.render.as_ref().is_some_and(|r| r.minimized) {
                    return;
                }
                let status = status_with_latched_fdd_track(
                    self.emu.bus().front_panel_status(),
                    &mut self.last_fdd_track,
                );
                let media = self.media_bar();
                let hover = self
                    .cursor_pos
                    .and_then(|pos| control_at(pos, &bar_layout(&media)));
                let control_connected = {
                    #[cfg(feature = "control")]
                    {
                        self.control.as_ref().is_some_and(|c| c.handle.connected())
                    }
                    #[cfg(not(feature = "control"))]
                    {
                        false
                    }
                };
                let view = StatusBarView {
                    status,
                    powered_on: self.powered_on,
                    paused: self.paused,
                    media,
                    joystick_input_mode: self.joystick_input_mode,
                    keyboard_panel_shown: super::keyboard_panel_shown(),
                    hover,
                    control_connected,
                };
                let osd = self.active_osd_text();
                let ui_hover = self.cursor_pos.and_then(|p| self.main_ui_control_at(p));
                let recording = self.recorder.is_some();
                // The MT-32's panel reads its display live off the engine, so
                // it is only gathered when the panel is actually up.
                #[cfg(feature = "mt32")]
                let mt32_panel = super::mt32_panel_shown()
                    .then(|| self.mt32_panel_view())
                    .flatten();
                // The Caps Lock lamp is the MCU's, so it is read fresh
                // every frame rather than mirrored from the clicks.
                let kbd_panel = super::keyboard_panel_shown().then(|| self.keyboard_panel_view());
                let ui_data = self.build_panel_view_data();
                if let Some(r) = self.render.as_mut() {
                    // RTG with a working GPU pipeline presents the native frame
                    // through its own texture in the GPU render pass below.
                    //
                    // Not while the UI is up, though: that pass overdraws the
                    // display region after the UI has been drawn into it, so
                    // an open menu or panel would be painted over and vanish.
                    // Fall back to the CPU present, which composites the UI on
                    // top as usual, at the cost of the FB_WIDTH downscale for
                    // as long as the overlay is open.
                    let rtg_gpu = self.rtg_present_dims.is_some() && !self.ui.active();
                    // The CRT pass re-draws the display rect from the same
                    // buffer, so it also re-draws whatever the UI composited
                    // into it -- through curvature and a phosphor mask, which
                    // is unreadable for menu and panel text. Suspend it while
                    // an overlay is open, as the RTG arm above does for its
                    // own reason.
                    //
                    // Off for two kinds of frame that have no 15 kHz line
                    // structure to reproduce: RTG board scanout (which reaches
                    // the surface through the RTG texture, not the buffer this
                    // pass samples) and programmable multisync scans (amifb's
                    // 31 kHz console, DblPAL, SHRES), whose fields are not
                    // woven, so the pass's two-rows-per-line assumption would
                    // not hold either.
                    //
                    // Interlaced content is drawn at field-line pitch: the
                    // gaps land every other emulated line over the woven
                    // frame, the look of a 15 kHz set showing an interlaced
                    // signal, rather than one gap per woven row.
                    let crt_active = self.crt_shader_kind != crate::config::ShaderKind::None
                        && !self.ui.active()
                        && self.rtg_present_dims.is_none()
                        && !self.present_programmable;
                    // The bezel is content-agnostic (a frame has no 15 kHz
                    // structure to get wrong), so unlike the CRT pass it
                    // stays on for programmable multisync scans. It shares
                    // the other two suspensions: an open overlay must not
                    // be overdrawn, and RTG scanout reaches the surface
                    // through its own texture, which this pass does not
                    // sample.
                    let bezel_active =
                        self.bezel.is_on() && !self.ui.active() && self.rtg_present_dims.is_none();
                    if let Some((w, h)) = self.rtg_present_dims.filter(|_| rtg_gpu) {
                        r.rtg_texture.upload(
                            r.pixels.device(),
                            r.pixels.queue(),
                            &self.rtg_fb,
                            w,
                            h,
                        );
                    }
                    let frame = r.pixels.frame_mut();
                    if rtg_gpu {
                        // The GPU pass overdraws the display region; black it
                        // out so nothing stale shows at the seams.
                        let rows = present_height() * r.texture_scale;
                        let stride = texture_width(r.texture_scale) * 4;
                        frame[..rows * stride].fill(0);
                    } else {
                        copy_window_present_frame(
                            &self.present_fb,
                            self.present_rows,
                            self.present_width,
                            frame,
                            r.texture_scale,
                            self.overscan,
                            // The TV aperture is a chipset crop rect. An RTG
                            // frame fills the buffer on its own terms, so
                            // applying it here would show a sub-rect of the
                            // board's screen.
                            self.present_tv_aperture_rows
                                .filter(|_| self.rtg_present_dims.is_none()),
                        );
                        // The tint models the monitor on the Amiga's video
                        // output, so RTG board scanout stays untinted here
                        // too, matching the GPU RTG path (which never sees
                        // this buffer).
                        if self.rtg_present_dims.is_none() {
                            if let Some(lut) = &self.tint_lut {
                                tint_display_rows(frame, r.texture_scale, lut);
                            }
                        }
                    }
                    #[cfg(feature = "mt32")]
                    if let Some(panel) = &mt32_panel {
                        mt32panel::draw(frame, panel, present_height(), r.texture_scale);
                    }
                    if let Some(panel) = &kbd_panel {
                        kbdpanel::draw(frame, panel, keyboard_panel_top(), r.texture_scale);
                    }
                    if !super::status_bar_hidden() {
                        draw_status_bar(frame, &view, r.texture_scale);
                    }
                    if recording {
                        // Painted into the presentation texture only, so
                        // the badge never appears in the recorded file.
                        draw_record_badge(frame, r.texture_scale);
                    }
                    if self.perf_overlay {
                        draw_perf_overlay(frame, &self.perf.lines, r.texture_scale, recording);
                    }
                    if let Some((text, warning)) = &osd {
                        draw_osd(frame, text, *warning, r.texture_scale);
                    }
                    ui::draw(frame, r.texture_scale, &self.ui, ui_hover, ui_data.as_ref());
                    // The drag hint sits on top of everything: the drop will
                    // land wherever the drag is released, panels or not. The
                    // launcher refuses drops, so no hint over it.
                    if self.drop_hover && !matches!(self.ui.panel, Some(Panel::Launcher(_))) {
                        ui::draw_drop_hint(frame, r.texture_scale);
                    }
                    let render_result = if rtg_gpu {
                        // Draw the UI buffer, then overdraw the display region
                        // with the native RTG texture (GPU-scaled). The display
                        // rect is the top present_height fraction of the buffer's
                        // letterboxed clip rect on the surface.
                        let rtg = &r.rtg_texture;
                        // The board frame is drawn straight to the surface,
                        // so integer scaling applies to it in its own native
                        // pixels rather than through the canvas texture the
                        // scaling renderer letterboxed above.
                        let integer_scaling = integer_scaling_requested();
                        r.pixels.render_with(|encoder, target, ctx| {
                            ctx.scaling_renderer.render(encoder, target);
                            let (cx, cy, cw, ch) = ctx.scaling_renderer.clip_rect();
                            let disp_h = ch as f32 * present_height() as f32
                                / window_present_height() as f32;
                            rtg.render(
                                &ctx.queue,
                                encoder,
                                target,
                                (cx as f32, cy as f32, cw as f32, disp_h),
                                integer_scaling,
                            );
                            Ok(())
                        })
                    } else if crt_active || bezel_active {
                        // Draw the composited buffer, then re-draw the display
                        // rect. Bezel alone: one pass draws the frame with the
                        // picture scaled into its opening. Preset alone: the
                        // pass covers the display rect. Both: the preset paints
                        // the picture into the opening first and the bezel
                        // frames it on top in frame-only mode -- the plastic
                        // overlaps the tube face, so the frame's rounded
                        // corners and chamfer clip the preset's square viewport
                        // rather than being buried under it. One CRT beam pass
                        // per emulated field line the copy above actually
                        // shows.
                        let scanlines = crt_scanline_count(
                            self.present_rows,
                            present_height(),
                            // The same branch copy_window_present_frame took.
                            self.present_tv_aperture_rows.filter(|_| {
                                self.overscan == Overscan::Tv
                                    && self.rtg_present_dims.is_none()
                                    && self.present_width == FB_WIDTH
                            }),
                        );
                        let kind = self.crt_shader_kind;
                        let strength = self.shader_strength;
                        let bezel_style = self.bezel;
                        // The closure is FnOnce and captures `r`, so the
                        // shaders have to be split out of it as separate
                        // borrows rather than reached through `r` inside.
                        let crt = &mut r.crt_shader;
                        let bezel_shader = &mut r.bezel_shader;
                        r.pixels.render_with(|encoder, target, ctx| {
                            ctx.scaling_renderer.render(encoder, target);
                            let (uniforms, viewport) = crt_shader::uniforms_for(
                                kind,
                                strength,
                                ctx.scaling_renderer.clip_rect(),
                                present_height(),
                                window_present_height(),
                                (ctx.texture_extent.width, ctx.texture_extent.height),
                                scanlines,
                            );
                            if bezel_active {
                                let opening = bezel::opening_rect(bezel_style, viewport);
                                if crt_active {
                                    crt.render(
                                        &ctx.device,
                                        &ctx.queue,
                                        &ctx.texture,
                                        encoder,
                                        target,
                                        opening,
                                        kind,
                                        uniforms.with_viewport(opening),
                                    );
                                }
                                bezel_shader.render(
                                    &ctx.device,
                                    &ctx.queue,
                                    &ctx.texture,
                                    encoder,
                                    target,
                                    viewport,
                                    bezel_style,
                                    bezel::uniforms_from(&uniforms, viewport, opening, crt_active),
                                );
                            } else {
                                crt.render(
                                    &ctx.device,
                                    &ctx.queue,
                                    &ctx.texture,
                                    encoder,
                                    target,
                                    viewport,
                                    kind,
                                    uniforms,
                                );
                            }
                            Ok(())
                        })
                    } else {
                        r.pixels.render()
                    };
                    if let Err(e) = render_result {
                        error!("pixels.render: {e}");
                    }
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        match event {
            DeviceEvent::MouseMotion { delta } => {
                if self.mouse_captured {
                    self.add_host_mouse_delta(delta.0, delta.1);
                }
            }
            DeviceEvent::Key(event) => self.handle_raw_device_key_event(event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Drain remote control commands first, before this pass's run
        // state is computed, so they land at a frame boundary. Sits
        // ahead of the render guard so tests can drive the drain on an
        // App that never opened a window.
        #[cfg(feature = "control")]
        {
            self.drain_control();
            if self.control_exit_requested() {
                event_loop.exit();
                return;
            }
        }
        if self.render.is_none() {
            return;
        }
        // Act on a completed drop before the OSD/control-flow computation
        // below, so a drop-raised OSD keeps the loop awake for its fade.
        if !self.pending_dropped_files.is_empty() {
            let files = std::mem::take(&mut self.pending_dropped_files);
            self.handle_dropped_files(files);
        }
        let running = self.powered_on && !self.cpu_halted && !self.paused;
        // While a transient overlay is up, keep the loop awake (and, when
        // the machine is paused/off, request repaints) so the message
        // fades on schedule instead of freezing on the last drawn frame.
        let osd_active = match &self.osd {
            Some(osd) if Instant::now() < osd.expires_at => true,
            Some(_) => {
                self.osd = None;
                self.request_redraw();
                false
            }
            None => false,
        };
        // The calibration panel polls raw gamepad events, so it needs the
        // loop awake even while the machine is paused or powered off.
        let calibrating = matches!(self.ui.panel, Some(Panel::Calibration(_)));
        // An image being written on a worker has to be collected, and the
        // launcher is up with the machine off -- nothing else would wake
        // the loop to notice it finished.
        self.poll_image_job();
        let writing_image = self.image_job.is_some();
        event_loop.set_control_flow(if running || osd_active || calibrating || writing_image {
            ControlFlow::Poll
        } else {
            ControlFlow::Wait
        });
        if writing_image && !running {
            // Nothing else paces the loop while the machine is off; check
            // back at a human rate rather than spinning a core on it.
            std::thread::sleep(std::time::Duration::from_millis(16));
            self.request_redraw();
        }
        if osd_active && !running {
            self.request_redraw();
        }
        if calibrating {
            // Feed raw pad input to the calibration session instead of the
            // emulated port-2 joystick, releasing anything already held.
            if let Some(Panel::Calibration(session)) = self.ui.panel.as_mut() {
                if self.gamepad.calibration_tick(session) {
                    self.request_redraw();
                }
            }
            for port in 0..2 {
                self.release_joystick_lines(port);
            }
            if !running {
                // Nothing paces the loop while the machine is not stepping;
                // don't busy-spin just to poll the pad.
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else {
            self.pump_joystick_input();
        }
        // Headless capture (screenshot/frame-dump) builds the framebuffer for
        // the saved PNG but presents nothing: it already runs unthrottled at one
        // frame per loop (request_redraw is skipped below), and every captured
        // frame must be rendered, so warp's output frame-skip burst must not
        // apply there.
        let headless_capture = !self.auto_shot.is_empty() || self.frame_dump.is_some();
        // Run one scheduler quantum. Rebuild the host framebuffer only
        // when Agnus has crossed into a new frame; the expensive renderer
        // reconstructs a completed hardware frame, not an instruction slice.
        if running {
            // If the live output device vanished (unplugged), reopen on the
            // current default so sound continues.
            self.recover_audio_if_device_lost();
            // Presentation is vsync-gated, so emulating exactly one frame per
            // presented frame would cap warp at the host monitor refresh rate
            // (about 1.2x for 50 Hz PAL on a 60 Hz display). In warp, retire
            // several frames per presented frame (output frame skip): only the
            // last frame of the burst is rendered and presented, so the
            // effective speed is the warp level times the refresh rate, host
            // CPU permitting. Real-time pacing and headless capture stay at one
            // frame per loop.
            let (frame_cap, time_budget) = self.warp_burst_plan(headless_capture);
            let burst_start = Instant::now();
            let mut frames_done = 0usize;
            loop {
                if let Err(e) = self.emu.step_frame() {
                    error!("emulator step halted: {e:?}");
                    self.cpu_halted = true;
                    self.sync_live_audio_suspension();
                    break;
                }
                #[cfg(feature = "control")]
                self.control_emit_events();
                frames_done += 1;
                // A breakpoint/watchpoint hit pauses the machine and brings
                // the debugger window up with the reason; end the burst so the
                // stop surfaces at the frame where it happened.
                if self.surface_debug_stop() {
                    break;
                }
                // A remote run_until frame/cck target completes the
                // pending resume and pauses at its boundary.
                #[cfg(feature = "control")]
                if self.control_run_target_reached() {
                    break;
                }
                if frames_done >= frame_cap {
                    break;
                }
                if let Some(budget) = time_budget {
                    if burst_start.elapsed() >= budget {
                        break;
                    }
                }
            }
            self.refresh_tool_windows_paced(event_loop);
            // A disk swapped by hand in a bridged bay is the one media change
            // no menu or drop initiated, so its message is raised here from
            // the drive's own report -- the same style an image insert or
            // eject shows. On a drive the configuration lets write, the tab
            // is the fact the user just changed disks to check, so it rides
            // along; a config-protected drive says nothing about it, since
            // nothing can be written either way.
            #[cfg(feature = "fluxbridge")]
            for (bay, present, tab) in self.emu.bus_mut().floppy.take_bridge_media_events() {
                self.show_osd(match (present, tab) {
                    (false, _) => format!("DF{bay}: disk ejected"),
                    (true, None) => format!("DF{bay}: disk inserted"),
                    (true, Some(true)) => format!("DF{bay}: disk inserted (write protected)"),
                    (true, Some(false)) => format!("DF{bay}: disk inserted (writable)"),
                });
            }
        }
        // Resample the performance overlay after the step so its revision
        // is current when the redraw decision below is taken.
        self.update_perf_overlay(running);
        #[cfg(feature = "mt32")]
        {
            self.repeat_mt32_dial();
            // A machine booted straight into an MT-32 has its port fitted
            // before there is a window to say anything on, so the fault is
            // picked up here instead. Taking it means this says it once.
            if self.serial_is_midi {
                self.report_mt32_fault();
            }
        }
        // While powered off, leave the parked test screen in place; the
        // emulator is not advancing, so there is no new frame to show.
        let mut rendered = self.powered_on && self.render_emulated_frame_if_needed();
        if self.recorder.is_some() && self.powered_on {
            rendered |= self.finish_render_for_current_frame();
        }
        self.capture_recorder_output(rendered);
        // Skipping request_redraw for headless capture avoids the vsync gate so
        // the run advances as fast as the host allows; emulated state is
        // identical either way. (`headless_capture` was resolved above, before
        // the step, to decide the warp burst.) When the exact presentation and
        // the window chrome are both unchanged, retain the existing GPU texture
        // instead of uploading/presenting it again at the field rate.
        if !headless_capture {
            let redraw_state = self.main_redraw_state();
            let chrome_changed = self.last_main_redraw_state != Some(redraw_state);
            if self.main_presentation_dirty
                || chrome_changed
                || ui_needs_continuous_redraw(running, self.ui.active())
                || self.drop_hover
                || osd_active
                || calibrating
            {
                self.last_main_redraw_state = Some(redraw_state);
                self.main_presentation_dirty = false;
                self.request_main_redraw();
            }
        }

        if self.dump_frame_if_due() {
            event_loop.exit();
            return;
        }

        self.fire_scheduled_events();
        self.fire_auto_save_state();
        if self.fire_auto_shot() {
            event_loop.exit();
        }
    }
}

/// The file name of a path for on-screen messages, falling back to the
/// full path when there is none.
fn display_file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What a file dropped on the window should be treated as. Extension-based:
/// only floppies get content-sniffed (by the insert path itself), and cue
/// sheets/hard disks/ROMs have no shared magic worth probing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DroppedMediaKind {
    /// Anything the floppy loader may accept (floppy::IMAGE_EXTENSIONS and
    /// unknown extensions): FloppyImage::from_bytes sniffs the content and
    /// rejects what it cannot read, surfacing a clean OSD failure.
    Floppy,
    /// A CD image (cue sheet, bare ISO, or CHD) for the CD drive.
    Cd,
    /// Hard disk images cannot be hot-attached; point at the config screen.
    HardDisk,
    /// Kickstart ROMs load from the config screen, not at runtime.
    Rom,
    /// A WHDLoad game package (.lha), or a .slave inside an extracted one:
    /// something to boot into (src/whdload.rs), not media to insert.
    WhdloadGame,
}

fn classify_dropped_media(path: &std::path::Path) -> DroppedMediaKind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("cue") | Some("iso") | Some("chd") => DroppedMediaKind::Cd,
        Some("hdf") | Some("hdz") | Some("img") => DroppedMediaKind::HardDisk,
        Some("rom") => DroppedMediaKind::Rom,
        Some("lha") | Some("slave") => DroppedMediaKind::WhdloadGame,
        _ => DroppedMediaKind::Floppy,
    }
}

/// A WHDLoad game path as the configuration stores it. Picking or dropping a
/// bare `.slave` means its directory (an already-extracted package), which is
/// what `whdload::prepare` mounts; an `.lha` archive or a directory is taken
/// as given.
fn whdload_game_config_path(path: PathBuf) -> PathBuf {
    let is_slave = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("slave"));
    if is_slave {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            return dir.to_path_buf();
        }
    }
    path
}

/// Shorten any filesystem path in a status message so its file name stays
/// visible: a long path keeps its final component behind a "..." prefix instead
/// of running past the panel. Windows and Unix paths both work.
///
/// `anyhow`'s alternate Display joins the error chain with `": "`, and a path
/// sits at the end of its segment (`reading ROM <path>`), so each segment is
/// clipped as one span. That keeps paths containing spaces intact instead of
/// splitting them into several fragments.
fn shorten_status_paths(msg: &str) -> String {
    // Enough for the file name plus a directory or two, leaving room for the
    // cause after it; the status line holds roughly eighty characters.
    const MAX_PATH_CHARS: usize = 28;
    msg.split(": ")
        .map(|segment| {
            let Some(sep) = segment.find(['/', '\\']) else {
                return segment.to_string();
            };
            // The path runs from the token holding the first separator (back up
            // to the preceding space) to the end of the segment.
            let start = segment[..sep].rfind(' ').map_or(0, |i| i + 1);
            let (prose, path) = segment.split_at(start);
            format!("{prose}{}", ui::clip_path_to_chars(path, MAX_PATH_CHARS))
        })
        .collect::<Vec<_>>()
        .join(": ")
}

/// A one-line, length-bounded form of an error for the configuration panel's
/// status line. `{:#}` walks the whole chain, so the cause is kept: showing
/// only the outermost context turned "reading ROM <path>" into what looked like
/// a progress message when the ROM was simply not there. Paths are shortened to
/// their file name and the first letter capitalised so it reads as a sentence
/// instead of trailing off past the edge of the panel.
fn short_status_error(err: &anyhow::Error) -> String {
    let msg = format!("{err:#}");
    let first_line = msg.lines().next().unwrap_or("").trim();
    let shortened = shorten_status_paths(first_line);
    let mut chars = shortened.chars();
    let sentence = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    sentence.chars().take(96).collect()
}

fn set_mouse_button(emu: &mut Emulator, port: u8, button: MouseButtonKind, pressed: bool) {
    let index = match button {
        MouseButtonKind::Left => 0,
        MouseButtonKind::Right => 1,
        MouseButtonKind::Middle => 2,
    };
    emu.bus_mut()
        .input
        .set_mouse_button(port as usize, index, pressed);
    // Reverse-debug: note the transition so replay can reproduce it.
    emu.tt_note_input(crate::inputsched::ReplayAction::MouseButton {
        port,
        index,
        pressed,
    });
}

impl Rect {
    pub(super) fn contains(self, pos: (i32, i32)) -> bool {
        let (x, y) = pos;
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.w) as i32
            && y < (self.y + self.h) as i32
    }
}

pub(super) use crate::video::present_common::rgba;

struct EmbeddedRgbaImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

fn copperline_logo_image() -> Option<&'static EmbeddedRgbaImage> {
    static LOGO: OnceLock<Option<EmbeddedRgbaImage>> = OnceLock::new();
    LOGO.get_or_init(|| match decode_embedded_png(COPPERLINE_LOGO_PNG) {
        Ok(image) => Some(image),
        Err(e) => {
            warn!("embedded Copperline logo decode failed: {e:#}");
            None
        }
    })
    .as_ref()
}

fn copperline_icon_image() -> Option<&'static EmbeddedRgbaImage> {
    static ICON: OnceLock<Option<EmbeddedRgbaImage>> = OnceLock::new();
    ICON.get_or_init(|| match decode_embedded_png(COPPERLINE_ICON_PNG) {
        Ok(image) => Some(image),
        Err(e) => {
            warn!("embedded Copperline icon decode failed: {e:#}");
            None
        }
    })
    .as_ref()
}

/// Set the macOS dock/application icon from the embedded PNG.
///
/// winit's `with_window_icon` is ignored on macOS (the title bar has no icon
/// and the dock icon comes from the app bundle or `NSApplication`), so a bare
/// `target/release/copperline` run otherwise shows the generic executable icon.
/// `NSImage` decodes the PNG itself, so we hand it the embedded bytes directly.
/// Runs once; repeated `resumed` events do not re-decode.
#[cfg(target_os = "macos")]
fn set_macos_dock_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    use std::sync::Once;

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // setApplicationIconImage must be touched on the main thread; the winit
        // event loop calls resumed there, but guard anyway.
        let Some(mtm) = MainThreadMarker::new() else {
            warn!("skipping macOS dock icon: not on the main thread");
            return;
        };
        let data = NSData::with_bytes(COPPERLINE_ICON_PNG);
        match NSImage::initWithData(NSImage::alloc(), &data) {
            Some(image) => {
                let app = NSApplication::sharedApplication(mtm);
                // SAFETY: FFI into AppKit; `image` is a valid NSImage and the
                // call only borrows it for the duration of the message send.
                unsafe { app.setApplicationIconImage(Some(&image)) };
            }
            None => warn!("macOS dock icon: NSImage rejected the embedded PNG"),
        }
    });
}

fn copperline_window_icon() -> Option<Icon> {
    let image = copperline_icon_image()?;
    match Icon::from_rgba(image.rgba.clone(), image.width as u32, image.height as u32) {
        Ok(icon) => Some(icon),
        Err(e) => {
            warn!("embedded Copperline icon rejected by window system: {e}");
            None
        }
    }
}

fn decode_embedded_png(bytes: &[u8]) -> Result<EmbeddedRgbaImage> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let width = info.width as usize;
    let height = info.height as usize;
    let src = &buf[..info.buffer_size()];
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => src.to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut out = Vec::with_capacity(width * height * 4);
            for px in src.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        (color, depth) => {
            anyhow::bail!("unsupported PNG format: {color:?} {depth:?}");
        }
    };
    if rgba.len() != width * height * 4 {
        anyhow::bail!(
            "decoded PNG size mismatch: got {} bytes, expected {}x{}x4",
            rgba.len(),
            width,
            height
        );
    }
    Ok(EmbeddedRgbaImage {
        width,
        height,
        rgba,
    })
}

impl App {
    fn toggle_mouse_capture(&mut self) {
        if !self.mouse_captured && self.mouse_port().is_none() {
            self.show_osd("No mouse on either port".to_string());
            return;
        }
        // An explicit toggle settles the question either way: whatever the
        // UI borrowed earlier is no longer owed back.
        self.capture_suspended_by_ui = false;
        self.set_mouse_captured(!self.mouse_captured);
    }

    /// Release the mouse on behalf of a panel or tool window that needs the
    /// host cursor, remembering a live capture so
    /// `restore_mouse_capture_after_ui` can hand it back when the last of
    /// them closes. Without that, opening the debugger over a captured
    /// session left the machine uncaptured for good -- most visible in
    /// fullscreen, where there is no desktop to reach for anyway.
    ///
    /// This covers the routes that can be taken *while* captured, which is
    /// the keyboard shortcuts. The menu and status bar are not among them:
    /// their click targets are refused while the mouse is captured, so
    /// reaching them means releasing it by hand first, and that explicit
    /// release is not something to undo afterwards.
    fn suspend_mouse_capture_for_ui(&mut self) {
        if self.mouse_captured {
            self.capture_suspended_by_ui = true;
            self.set_mouse_captured(false);
        }
    }

    /// Take the grab if `[input] mouse_capture = "auto"` and the moment is
    /// right for it: the window holds the focus, nothing modal wants the
    /// cursor, and there is a mouse on a port to drive.
    ///
    /// Deliberately driven by discrete events (focus gain, entering
    /// fullscreen, the last panel closing) rather than polled per frame --
    /// a poll would re-take the grab the instant the operator released it
    /// with the shortcut, leaving no way to get the cursor back at all.
    fn apply_auto_mouse_capture(&mut self) {
        if self.mouse_capture != crate::config::MouseCapture::Auto
            || self.mouse_captured
            || !self.main_window_focused
            || self.modal_ui_active()
            || self.mouse_port().is_none()
        {
            return;
        }
        self.set_mouse_captured(true);
        // Auto mode hides the host cursor without the operator having done
        // anything to ask for it, so say once how to get it back. Every
        // later focus gain re-grabs silently.
        if self.mouse_captured && !self.auto_capture_hint_shown {
            self.auto_capture_hint_shown = true;
            self.show_osd(format!(
                "Mouse captured ({HOST_SHORTCUT_MODIFIER_LABEL}+G releases)"
            ));
        }
    }

    /// Re-take a capture the UI borrowed, once no modal UI still wants the
    /// cursor. A no-op unless `suspend_mouse_capture_for_ui` recorded one,
    /// so a session that was never captured is never surprised by a grab.
    fn restore_mouse_capture_after_ui(&mut self) {
        if !self.capture_suspended_by_ui || self.modal_ui_active() {
            return;
        }
        // Same guard the click-to-capture path applies: with no mouse left
        // on either port there is nothing to drive, and grabbing would only
        // trap a hidden cursor. Cheap insurance against a port device that
        // changed while the panel was open -- and the loan is void, not
        // outstanding, because no later event can repay it.
        if self.mouse_port().is_none() {
            self.capture_suspended_by_ui = false;
            return;
        }
        // A grab wants the focus. Closing a tool window hands the focus back
        // to the main window, but the order of that against this call is the
        // window manager's business: attempted too early the grab fails, and
        // clearing the loan on a failed grab would lose the capture for good
        // -- the very thing this mechanism exists to prevent. Leave it
        // outstanding and let the Focused(true) that follows retry.
        if !self.main_window_focused {
            return;
        }
        self.set_mouse_captured(true);
        // Only a grab that actually took discharges the loan.
        if self.mouse_captured {
            self.capture_suspended_by_ui = false;
        }
    }

    /// COPPERLINE_DIAG_CURSOR: trace how the most recent click maps from host
    /// physical coordinates through the scaling renderer's clip rect into a
    /// texture/region hit. The tool for diagnosing mouse capture on DPI scale
    /// changes and mixed-scale monitors (see the ScaleFactorChanged handler):
    /// if a status-bar click logs region=display(->capture), the surface and
    /// texture extents have drifted out of agreement.
    fn log_cursor_diag(&self, button: MouseButton) {
        if !crate::envcfg::flag("COPPERLINE_DIAG_CURSOR") {
            return;
        }
        let Some(r) = self.render.as_ref() else {
            return;
        };
        let scale_factor = r.window.scale_factor();
        let inner = r.window.inner_size();
        let phys = self.last_cursor_phys;
        let context = r.pixels.context();
        let clip = context.scaling_renderer.clip_rect();
        let texture = (context.texture_extent.width, context.texture_extent.height);
        let pos = phys.and_then(|p| cursor_texture_position(&r.pixels, p, r.texture_scale));
        let region = match pos {
            Some(p) if cursor_in_status_bar(p) => "status_bar",
            Some(p) if cursor_in_display(p) => "display(->capture)",
            Some(_) => "other",
            None => "none",
        };
        info!(
            "[DIAG_CURSOR] button={button:?} phys={phys:?} scale_factor={scale_factor:.4} \
             inner={}x{} texture_scale={} clip_rect={clip:?} texture={}x{} mapped_pos={pos:?} \
             region={region} (present_h={} window_present_h={} fb_w={FB_WIDTH})",
            inner.width,
            inner.height,
            r.texture_scale,
            texture.0,
            texture.1,
            present_height(),
            window_present_height(),
        );
    }

    fn set_mouse_captured(&mut self, captured: bool) {
        if self.mouse_captured == captured {
            return;
        }
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        self.volume_dragging = false;
        self.analyzer_dragging = false;

        if captured {
            match window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|locked_err| {
                    window
                        .set_cursor_grab(CursorGrabMode::Confined)
                        .map_err(|confined_err| (locked_err, confined_err))
                }) {
                Ok(()) => {
                    self.mouse_captured = true;
                    self.cursor_pos = None;
                    self.last_display_cursor_pos = None;
                    self.mouse_delta_remainder = (0.0, 0.0);
                    window.set_cursor_visible(false);
                    window.set_title(&window_title_mouse_captured());
                    info!("mouse captured; press {HOST_SHORTCUT_MODIFIER_LABEL}+G to release");
                }
                Err((locked_err, confined_err)) => {
                    warn!("mouse capture failed (locked: {locked_err}; confined: {confined_err})")
                }
            }
        } else {
            if let Err(e) = window.set_cursor_grab(CursorGrabMode::None) {
                warn!("mouse release failed: {e}");
            }
            self.mouse_captured = false;
            self.cursor_pos = None;
            self.last_display_cursor_pos = None;
            self.mouse_delta_remainder = (0.0, 0.0);
            self.release_mouse_buttons();
            window.set_cursor_visible(true);
            window.set_title(WINDOW_TITLE);
            info!("mouse released");
        }
    }

    fn release_mouse_buttons(&mut self) {
        if let Some(port) = self.mouse_port() {
            let input = &mut self.emu.bus_mut().input;
            for index in 0..3 {
                input.set_mouse_button(port, index, false);
            }
        }
    }

    fn track_uncaptured_cursor_motion(&mut self, pos: Option<(i32, i32)>) {
        let Some(pos) = pos.filter(|p| cursor_in_display(*p)) else {
            self.last_display_cursor_pos = None;
            return;
        };
        if let Some(prev) = self.last_display_cursor_pos {
            let dx = pos.0 - prev.0;
            let dy = pos.1 - prev.1;
            if dx != 0 || dy != 0 {
                // Through the same scale the captured path uses, so the
                // sensitivity setting means something on both sides of a
                // grab instead of silently doing nothing until the mouse is
                // captured. The units still differ -- these are texture
                // pixels where a captured delta is a raw device count -- so
                // this equalises the operator's knob, not the underlying
                // ratio; at the default sensitivity the factor is 1.0 and
                // the long-standing 1:1 tracking is unchanged.
                self.add_host_mouse_delta(f64::from(dx), f64::from(dy));
            }
        }
        self.last_display_cursor_pos = Some(pos);
    }

    fn add_host_mouse_delta(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        // The sensitivity scale is applied to the live host mouse only, here --
        // scripted --mouse-after deltas go through apply_scripted_mouse_delta
        // and stay exact, so the core is deterministic regardless of it.
        let scale = MOUSE_MOTION_SCALE * self.mouse_sensitivity_factor;
        self.mouse_delta_remainder.0 += dx * scale;
        self.mouse_delta_remainder.1 += dy * scale;
        let ix = take_integral_mouse_delta(&mut self.mouse_delta_remainder.0);
        let iy = take_integral_mouse_delta(&mut self.mouse_delta_remainder.1);
        if ix != 0 || iy != 0 {
            self.add_mouse_delta_i32(ix, iy);
        }
    }

    /// Set the host mouse sensitivity (0-100), recomputing the speed factor.
    fn set_mouse_sensitivity(&mut self, sensitivity: u8) {
        self.mouse_sensitivity = sensitivity.min(100);
        self.mouse_sensitivity_factor = mouse_sensitivity_factor(self.mouse_sensitivity);
    }

    /// Nudge the mouse sensitivity by one, clamped to 0-100, with an on-screen
    /// readout. Bound to the Cmd/Alt+Shift+> / < shortcuts, which ramp while
    /// held via key repeat. A no-op when no port holds a mouse, since the scale
    /// would have nothing to act on.
    fn step_mouse_sensitivity(&mut self, up: bool) {
        if self.mouse_port().is_none() {
            return;
        }
        let next = if up {
            self.mouse_sensitivity.saturating_add(1)
        } else {
            self.mouse_sensitivity.saturating_sub(1)
        };
        self.set_mouse_sensitivity(next);
        self.show_osd(format!(
            "Mouse sensitivity: {}",
            crate::config::mouse_sensitivity_label(self.mouse_sensitivity)
        ));
    }

    fn add_mouse_delta_i32(&mut self, dx: i32, dy: i32) {
        let Some(port) = self.mouse_port() else {
            return;
        };
        self.apply_scripted_mouse_delta(port as u8, dx, dy);
    }

    /// Apply quadrature motion to an explicit port: scripted/CCP events
    /// drive the named port's counters whatever device is configured
    /// there, while live host-mouse motion goes through `mouse_port`.
    fn apply_scripted_mouse_delta(&mut self, port: u8, dx: i32, dy: i32) {
        self.emu
            .bus_mut()
            .input
            .add_mouse_delta(port as usize, dx, dy);
        // Reverse-debug: note the motion so replay can reproduce it.
        self.emu
            .tt_note_input(crate::inputsched::ReplayAction::MouseMove { port, dx, dy });
    }

    /// Arm one scripted pointer target directly, for tests that do not go
    /// through the CLI.
    #[cfg(test)]
    pub(super) fn arm_scripted_pointer_target(&mut self, secs: f64, x: i32, y: i32, port: u8) {
        self.auto_mouse_to.push((secs, x, y, port));
    }

    /// Whether a scripted pointer servo is currently steering.
    #[cfg(test)]
    pub(super) fn scripted_pointer_target_active(&self) -> bool {
        self.active_mouse_to.is_some()
    }

    /// Advance the scripted `--mouse-to-after` pointer targets: start the
    /// next one that is due when nothing is steering, then give the
    /// running servo this frame's correction.
    ///
    /// One correction per frame is the servo's whole contract -- it has
    /// to see what the previous delta did before choosing the next -- and
    /// this runs once per emulated frame, in the same pass the other
    /// scheduled input fires from.
    fn advance_scripted_pointer_targets(&mut self, emu_secs: f64) {
        if self.active_mouse_to.is_none() {
            if let Some(pos) = self
                .auto_mouse_to
                .iter()
                .position(|&(at, ..)| emu_secs >= at)
            {
                let (_, x, y, port) = self.auto_mouse_to.remove(pos);
                info!(
                    "auto-mouse-to: steering the pointer to ({x}, {y}) on port {}",
                    port + 1
                );
                self.active_mouse_to = Some(crate::pointer::PointerServo::new(
                    port,
                    (x, y),
                    crate::pointer::DEFAULT_TOLERANCE,
                    crate::pointer::DEFAULT_MAX_FRAMES,
                ));
            }
        }
        let Some(servo) = self.active_mouse_to.as_mut() else {
            return;
        };
        match servo.poll(self.emu.bus()) {
            crate::pointer::ServoStep::Move { port, dx, dy } => {
                self.apply_scripted_mouse_delta(port, dx, dy);
            }
            crate::pointer::ServoStep::Arrived { x, y, frames } => {
                info!("auto-mouse-to: pointer at ({x}, {y}) after {frames} frame(s)");
                self.active_mouse_to = None;
            }
            crate::pointer::ServoStep::Failed(why) => {
                warn!("auto-mouse-to: {why}");
                self.active_mouse_to = None;
            }
        }
    }

    fn set_output_volume_from_pos(&mut self, pos: (i32, i32)) {
        self.emu
            .bus_mut()
            .set_output_volume_percent(volume_percent_from_pos(pos));
        self.request_redraw();
    }

    fn adjust_output_volume(&mut self, delta: i16) {
        self.emu.bus_mut().adjust_output_volume_percent(delta);
        self.request_redraw();
    }

    /// Whether this drive is backed by a real one on a bridge.
    fn drive_is_bridged(&self, idx: usize) -> bool {
        #[cfg(feature = "fluxbridge")]
        {
            self.emu.bus().floppy.is_bridged(idx)
        }
        #[cfg(not(feature = "fluxbridge"))]
        {
            let _ = idx;
            false
        }
    }

    /// Removable-media status for the bar controls: which drives exist,
    /// what is inserted, and whether a CD drive is fitted this session.
    fn media_bar(&self) -> MediaBar {
        let bus = self.emu.bus();
        let drives = std::array::from_fn(|idx| DriveBar {
            connected: bus.floppy.drive_connected(idx),
            inserted: bus.floppy.disk_inserted(idx),
            multi: self.disk_playlists[idx].len() > 1,
            #[cfg(feature = "fluxbridge")]
            bridged: bus.floppy.is_bridged(idx),
            #[cfg(not(feature = "fluxbridge"))]
            bridged: false,
        });
        let cd = bus.cd_drive_present().then(|| bus.cd_disc_inserted());
        MediaBar { drives, cd }
    }

    fn main_redraw_state(&mut self) -> MainRedrawState {
        let mut status = self.emu.bus().front_panel_status();
        if status.fdd_track.is_none() {
            status.fdd_track = self.last_fdd_track;
        }
        let control_connected = {
            #[cfg(feature = "control")]
            {
                self.control.as_ref().is_some_and(|c| c.handle.connected())
            }
            #[cfg(not(feature = "control"))]
            {
                false
            }
        };
        #[cfg(feature = "mt32")]
        let mt32_face = if crate::video::mt32_panel_shown() {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.emu
                .bus_mut()
                .midi_serial_mut()
                .and_then(crate::midi::MidiSerialSink::mt32_mut)
                .map(|mt32| mt32.synth_mut().display_raw())
                .hash(&mut h);
            h.finish()
        } else {
            0
        };
        MainRedrawState {
            status,
            media: self.media_bar(),
            powered_on: self.powered_on,
            paused: self.paused,
            joystick_input_mode: self.joystick_input_mode,
            control_connected,
            recording: self.recorder.is_some(),
            input_recording: self.input_recorder.is_some(),
            warp: !self.emu.paced(),
            perf_revision: if self.perf_overlay {
                self.perf.revision
            } else {
                0
            },
            #[cfg(feature = "mt32")]
            mt32_face,
        }
    }

    fn main_ui_control_at(&self, pos: (i32, i32)) -> Option<UiControl> {
        if self.ui.panel.is_none() && self.tool_panel_open() && !self.ui.menu_open {
            return None;
        }
        self.ui.control_at(pos)
    }

    fn main_ui_hover_changed(
        &self,
        previous: Option<(i32, i32)>,
        current: Option<(i32, i32)>,
    ) -> bool {
        previous.and_then(|pos| self.main_ui_control_at(pos))
            != current.and_then(|pos| self.main_ui_control_at(pos))
    }

    fn tool_panel_open(&self) -> bool {
        self.debugger_panel.is_some() || self.frame_analyzer_panel.is_some()
    }

    fn modal_ui_active(&self) -> bool {
        self.ui.active() || self.tool_panel_open()
    }

    fn tool_window(&self, kind: ToolPanelKind) -> Option<&ToolWindow> {
        match kind {
            ToolPanelKind::Debugger => self.debugger_tool_window.as_ref(),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_tool_window.as_ref(),
            ToolPanelKind::Console => self.console_tool_window.as_ref(),
        }
    }

    fn tool_window_mut(&mut self, kind: ToolPanelKind) -> Option<&mut ToolWindow> {
        match kind {
            ToolPanelKind::Debugger => self.debugger_tool_window.as_mut(),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_tool_window.as_mut(),
            ToolPanelKind::Console => self.console_tool_window.as_mut(),
        }
    }

    fn tool_window_slot(&mut self, kind: ToolPanelKind) -> &mut Option<ToolWindow> {
        match kind {
            ToolPanelKind::Debugger => &mut self.debugger_tool_window,
            ToolPanelKind::FrameAnalyzer => &mut self.frame_analyzer_tool_window,
            ToolPanelKind::Console => &mut self.console_tool_window,
        }
    }

    fn tool_panel_for_kind(&self, kind: ToolPanelKind) -> Option<Panel> {
        match kind {
            ToolPanelKind::Debugger => self
                .debugger_panel
                .as_ref()
                .map(|panel| Panel::Debugger(panel.clone())),
            ToolPanelKind::FrameAnalyzer => self
                .frame_analyzer_panel
                .as_ref()
                .map(|panel| Panel::FrameAnalyzer(panel.clone())),
            ToolPanelKind::Console => self
                .console_panel
                .as_ref()
                .map(|panel| Panel::Console(panel.clone())),
        }
    }

    fn tool_panel_control_at(&self, kind: ToolPanelKind, pos: (i32, i32)) -> Option<UiControl> {
        self.tool_panel_for_kind(kind)
            .as_ref()
            .and_then(|panel| ui::panel_control_at(panel, pos))
    }

    fn tool_hover_changed(
        &self,
        kind: ToolPanelKind,
        previous: Option<(i32, i32)>,
        current: Option<(i32, i32)>,
    ) -> bool {
        previous.and_then(|pos| self.tool_panel_control_at(kind, pos))
            != current.and_then(|pos| self.tool_panel_control_at(kind, pos))
    }

    fn tool_window_kind(&self, window_id: WindowId) -> Option<ToolPanelKind> {
        ToolPanelKind::ALL.into_iter().find(|&kind| {
            self.tool_window(kind)
                .is_some_and(|tool| tool.window.id() == window_id)
        })
    }

    fn ui_key_accepts_repeat(&self, kind: Option<ToolPanelKind>, code: KeyCode) -> bool {
        match kind {
            Some(ToolPanelKind::FrameAnalyzer) => matches!(
                code,
                KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown
            ),
            // A command line wants held-key repeat for typing and editing.
            Some(ToolPanelKind::Console) => true,
            _ => false,
        }
    }

    fn should_drop_repeated_main_key(
        &self,
        code: KeyCode,
        state: ElementState,
        repeat: bool,
    ) -> bool {
        repeated_main_key_should_drop(
            &self.held_rawkeys,
            code,
            state,
            repeat,
            self.ui_key_accepts_repeat(None, code),
        )
    }

    fn handle_raw_device_key_event(&mut self, event: RawKeyEvent) {
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        let Some(rawkey) = raw_device_qualifier_rawkey(code) else {
            return;
        };

        let pressed = event.state == ElementState::Pressed;
        self.raw_device_held_rawkeys[rawkey_index(rawkey)] = pressed;
        if pressed && (!self.main_window_focused || self.modal_ui_active()) {
            return;
        }
        if self.handle_keyboard_joystick_key(code, pressed) {
            return;
        }
        self.handle_amiga_key_event(rawkey, pressed);
    }

    fn activate_analyzer_pick_at(&mut self, kind: ToolPanelKind, pos: (i32, i32)) -> bool {
        if kind != ToolPanelKind::FrameAnalyzer {
            return false;
        }
        let control = self.tool_panel_control_at(kind, pos);
        let Some(UiControl::AnalyzerPick { x, y, scanline }) = control else {
            return false;
        };
        self.frame_analyzer_select(x, y, scanline);
        self.request_redraw();
        true
    }

    fn handle_tool_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: ToolPanelKind,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.close_tool_panel(kind),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(code),
                        repeat,
                        text,
                        ..
                    },
                ..
            } => {
                if state != ElementState::Pressed
                    || (repeat && !self.ui_key_accepts_repeat(Some(kind), code))
                {
                    return;
                }
                if code == KeyCode::KeyQ && host_shortcut_modifier_pressed(self.modifiers) {
                    event_loop.exit();
                } else if kind == ToolPanelKind::Console
                    && self.console_handle_text_input(code, text.as_deref())
                {
                    // Paste or layout-aware typed text; editing and command
                    // keys fall through to the keycode handler below.
                } else if !self.ui_handle_tool_key(kind, code) {
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.update_host_modifiers(modifiers.state());
            }
            WindowEvent::CursorMoved { position, .. } => {
                let previous = self.tool_window(kind).and_then(|tool| tool.cursor_pos);
                let pos = self.tool_window(kind).and_then(|tool| {
                    cursor_texture_position(&tool.pixels, position, tool.texture_scale)
                });
                if let Some(tool) = self.tool_window_mut(kind) {
                    tool.cursor_pos = pos;
                }
                if kind == ToolPanelKind::FrameAnalyzer && self.analyzer_dragging {
                    if let Some(pos) = pos {
                        self.activate_analyzer_pick_at(kind, pos);
                    }
                }
                if self.tool_hover_changed(kind, previous, pos) {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let previous = self.tool_window(kind).and_then(|tool| tool.cursor_pos);
                if let Some(tool) = self.tool_window_mut(kind) {
                    tool.cursor_pos = None;
                }
                if kind == ToolPanelKind::FrameAnalyzer {
                    self.analyzer_dragging = false;
                }
                if self.tool_hover_changed(kind, previous, None) {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                if state != ElementState::Pressed {
                    if kind == ToolPanelKind::FrameAnalyzer {
                        self.analyzer_dragging = false;
                    }
                    return;
                }
                if kind == ToolPanelKind::FrameAnalyzer {
                    self.analyzer_dragging = false;
                }
                let control = self
                    .tool_window(kind)
                    .and_then(|tool| tool.cursor_pos)
                    .and_then(|pos| self.tool_panel_control_at(kind, pos));
                if let Some(control) = control {
                    if kind == ToolPanelKind::FrameAnalyzer {
                        self.analyzer_dragging = matches!(control, UiControl::AnalyzerPick { .. });
                    }
                    self.activate_tool_control(kind, control);
                    self.ensure_tool_windows_for_open_panels(event_loop);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Same stale-texture hazard as the main window (see the main
                // window's ScaleFactorChanged handler): rebuild the tool
                // window's texture for the new scale so its own hit-testing
                // stays aligned after a DPI change or monitor move.
                if let Some(tool) = self.tool_window_mut(kind) {
                    resync_render_scale(&mut tool.pixels, &mut tool.texture_scale, scale_factor);
                }
                self.request_redraw();
            }
            WindowEvent::Resized(size) => {
                self.apply_tool_surface_size(kind, size);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y as i32,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y / 12.0) as i32,
                };
                // Scroll the Memory tab's hex/bitmap view or the console's
                // scrollback: one display row per wheel notch, a chunk for
                // pixel-precise trackpads.
                if kind == ToolPanelKind::Debugger {
                    if self
                        .debugger_panel
                        .as_ref()
                        .is_some_and(|panel| panel.tab == ui::DebugTab::IoMap)
                    {
                        self.debugger_iomap_move(rows);
                    } else {
                        self.debugger_mem_scroll(rows);
                    }
                } else if kind == ToolPanelKind::Console {
                    if let Some(panel) = self.console_panel.as_mut() {
                        panel.scroll = panel
                            .scroll
                            .saturating_add_signed(-(rows as isize))
                            .min(ui::CONSOLE_SCROLLBACK_LINES);
                        self.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => self.draw_tool_window(kind),
            _ => {}
        }
    }

    fn draw_tool_window(&mut self, kind: ToolPanelKind) {
        let Some(panel) = self.tool_panel_for_kind(kind) else {
            *self.tool_window_slot(kind) = None;
            return;
        };
        self.resync_tool_surface_size(kind);
        if kind == ToolPanelKind::FrameAnalyzer {
            self.ensure_analyzer_underlay();
        }
        let ui_data = self.build_tool_panel_view_data(kind);
        let hover = self
            .tool_window(kind)
            .and_then(|tool| tool.cursor_pos)
            .and_then(|pos| ui::panel_control_at(&panel, pos));
        if let Some(tool) = self.tool_window_mut(kind) {
            if tool.minimized {
                return;
            }
            let frame = tool.pixels.frame_mut();
            frame.fill(0);
            ui::draw_panel_layer(frame, tool.texture_scale, &panel, hover, ui_data.as_ref());
            if let Err(e) = tool.pixels.render() {
                error!("tool pixels.render: {e}");
            }
        }
    }

    /// The menu as it stands for this machine, built when it is opened.
    ///
    /// Everything the tree needs is read here, once: the tree is then a plain
    /// value, so nothing it offers can shift under the pointer while it is
    /// up, and the drawing code never reaches into the machine.
    fn build_menu(&mut self, fullscreen: bool) -> Vec<crate::video::menu::MenuRow> {
        use crate::video::menu::{AudioOutputChoice, MenuState};

        let midi_active = self.serial_is_midi;
        let (midi_in, midi_out) = if midi_active {
            self.midi_menu_labels()
        } else {
            (String::new(), String::new())
        };
        #[cfg(feature = "midi")]
        let (midi_inputs, midi_outputs) = if midi_active {
            let ends = crate::midi::enumerate();
            (
                ends.inputs.into_iter().map(|e| e.name).collect::<Vec<_>>(),
                ends.outputs.into_iter().map(|e| e.name).collect::<Vec<_>>(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        #[cfg(not(feature = "midi"))]
        let (midi_inputs, midi_outputs): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());

        let sampler_active = self.sampler_stream.is_some();
        let sampler_inputs = if sampler_active {
            crate::sampler::picker_input_devices()
        } else {
            Vec::new()
        };

        let audio_output = match &self.audio_output {
            crate::audio::AudioOutput::Disabled => AudioOutputChoice::Disabled,
            crate::audio::AudioOutput::Device(name) => AudioOutputChoice::Named(name.clone()),
            crate::audio::AudioOutput::Default => AudioOutputChoice::Default,
        };

        // Whether an MT-32 can be picked, whether it is the one playing, and
        // whether its own MIDI OUT is wired back to the machine.
        #[cfg(feature = "mt32")]
        let (mt32_available, mt32_attached, mt32_input) = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .map_or((false, false, false), |sink| {
                (
                    sink.mt32_available(),
                    sink.mt32().is_some(),
                    sink.mt32_input(),
                )
            });
        #[cfg(not(feature = "mt32"))]
        let (mt32_available, mt32_attached, mt32_input) = (false, false, false);

        let save_slots = self.save_slot_stamps();
        let state = MenuState {
            fullscreen,
            status_bar_hidden: crate::video::status_bar_hidden(),
            bezel: self.bezel,
            perf_overlay: self.perf_overlay,
            warp: !self.emu.paced(),
            warp_speed: self.warp_speed,
            rewind: self.rewind_armed,
            recording: self.recorder.is_some(),
            input_recording: self.input_recorder.is_some(),
            autofire_hz: self.autofire_hz,
            joystick_input_mode: self.joystick_input_mode,
            port_devices: [
                self.emu.bus().input.device(0),
                self.emu.bus().input.device(1),
            ],
            pixel_aspect: crate::video::pixel_aspect(),
            scaling: crate::video::display_scaling(),
            shader: self.crt_shader_kind,
            custom_shader_available: self.custom_shader_path.is_some(),
            tint: self.tint,
            menu_scale: crate::video::menu_scale(),
            floppy_speed: self.emu.bus().floppy.speed_percent(),
            floppy_speed_applies: self.emu.bus().floppy.has_image_drive(),
            audio_filter: self.emu.bus().paula.led_filter_mode(),
            audio_output,
            audio_devices: &crate::audio::picker_output_devices(),
            midi_in: &midi_in,
            midi_out: &midi_out,
            midi_inputs: &midi_inputs,
            midi_outputs: &midi_outputs,
            mt32_available,
            mt32_attached,
            mt32_input,
            mt32_panel: crate::video::mt32_panel_shown(),
            keyboard_panel: crate::video::keyboard_panel_shown(),
            mt32_lcd: crate::video::mt32_lcd(),
            sampler_input: self.sampler.input_device.as_deref().unwrap_or(""),
            sampler_inputs: &sampler_inputs,
            sampler_gain: self.sampler.gain_db,
            save_slots: &save_slots,
        };
        crate::video::menu::build(&state)
    }

    /// When each numbered save slot was written, for the Quick Save/Load
    /// rows. A slot that cannot be read is treated as free: the menu is
    /// describing what is there, not diagnosing the disk.
    fn save_slot_stamps(&self) -> [Option<String>; crate::video::menu::SAVE_SLOTS] {
        std::array::from_fn(|i| {
            let path = crate::savestate::slot_path(i + 1)?;
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
            Some(crate::timestamp::readable(
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ))
        })
    }

    /// Act on the row the cursor is on: open a category, or run its action.
    ///
    /// Rows meant to be used more than once -- a toggle, a step -- leave the
    /// menu up and rebuild it, so what it shows keeps pace with what it just
    /// changed.
    fn activate_menu_row(&mut self, event_loop: Option<&ActiveEventLoop>) {
        let rows = std::mem::take(&mut self.ui.menu_rows);
        let Some(cursor) = self.ui.menu_nav.cursor() else {
            self.ui.menu_rows = rows;
            return;
        };
        let Some(row) = self.ui.menu_nav.current(&rows).get(cursor).cloned() else {
            self.ui.menu_rows = rows;
            return;
        };
        self.ui.menu_rows = rows;
        if !row.enabled {
            return;
        }
        if row.is_submenu() {
            let ui = &mut self.ui;
            ui.menu_nav.descend(&ui.menu_rows);
            self.request_redraw();
            return;
        }
        let Some(action) = row.menu_action().cloned() else {
            return;
        };
        let closes = row.closes_menu();
        if closes {
            self.close_menu();
        }
        self.run_menu_action(action, event_loop);
        if !closes {
            // The menu stays up, so rebuild it: a toggle that has just been
            // flipped should read as flipped.
            let fullscreen = self
                .render
                .as_ref()
                .is_some_and(|r| r.window.fullscreen().is_some());
            self.ui.menu_rows = self.build_menu(fullscreen);
        }
        self.request_redraw();
    }

    /// Carry out a menu action.
    ///
    /// Every row of the tree ends here, so the menu's shape and its effects
    /// stay separable: the tree says what is offered, this says what happens.
    fn run_menu_action(
        &mut self,
        action: crate::video::menu::MenuAction,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        use crate::video::menu::{AudioOutputChoice, MenuAction as A};
        match action {
            A::OpenMachineConfig => self.open_launcher(),
            A::OpenFrameAnalyzer => self.open_frame_analyzer(),
            A::OpenDebugger => self.open_debugger(),
            A::OpenConsole => self.open_console(),
            A::OpenInputMapping => self.open_input_mapping(),
            A::OpenCalibration => {
                self.ui.panel = Some(Panel::Calibration(crate::gamepad::CalibrationSession::new()));
            }
            A::OpenShortcuts => self.ui.panel = Some(Panel::Shortcuts),
            A::OpenAbout => self.ui.panel = Some(Panel::About),
            A::LoadRom => self.load_rom_from_dialog(),

            A::SetAudioOutput(choice) => {
                let want = match choice {
                    AudioOutputChoice::Default => crate::audio::AudioOutput::Default,
                    AudioOutputChoice::Named(name) => crate::audio::AudioOutput::Device(name),
                    AudioOutputChoice::Disabled => crate::audio::AudioOutput::Disabled,
                };
                self.audio_output = want;
                let realtime = crate::priority::requested(self.realtime_priority);
                match crate::audio::open_output_sink(realtime, &self.audio_output) {
                    Ok(sink) => {
                        self.emu.bus_mut().paula.audio = sink;
                        self.sync_live_audio_suspension();
                    }
                    Err(e) => {
                        warn!("audio: could not open the selected device; keeping silence: {e:#}");
                        self.emu.bus_mut().paula.audio = Box::new(crate::audio::NullSink);
                    }
                }
                self.show_osd(format!("Audio output: {}", self.audio_output.label()));
            }
            A::SetAudioFilter(mode) => {
                use crate::config::AudioFilterMode;
                self.emu.bus_mut().paula.set_led_filter_mode(mode);
                let label = match mode {
                    AudioFilterMode::Auto => "Auto",
                    AudioFilterMode::On => "Enabled",
                    AudioFilterMode::Off => "Disabled",
                };
                self.show_osd(format!("Audio filter: {label}"));
                self.request_redraw();
            }

            A::SetPixelAspect(aspect) => self.apply_pixel_aspect(aspect),
            A::SetDisplayScaling(scaling) => self.apply_display_scaling(scaling),
            A::SetShader(kind) => {
                use crate::config::ShaderKind;
                // A user shader is re-read from disk each time it is chosen,
                // so editing the file and picking it again shows the new
                // version; one that will not compile falls back to off and
                // says why.
                let mut failure = None;
                let mut applied = kind;
                if kind == ShaderKind::Custom {
                    if let Err(msg) = self.reload_custom_shader() {
                        failure = Some(msg);
                        applied = ShaderKind::None;
                    }
                }
                self.crt_shader_kind = applied;
                info!("crt shader: {}", applied.label());
                self.show_osd(match failure {
                    Some(msg) => {
                        format!("CRT shader: {} (custom failed: {msg})", applied.label())
                    }
                    None => format!("CRT shader: {}", applied.label()),
                });
                self.request_redraw();
            }
            A::SetMenuScale(scale) => {
                crate::video::set_menu_scale(scale);
                self.show_osd(format!("Menu size: {}", scale.menu_label()));
                self.request_redraw();
            }
            A::SetTint(tint) => {
                self.set_tint(tint);
                self.show_osd(format!("Screen tint: {}", tint.label()));
                self.request_redraw();
            }
            A::ToggleFullscreen => self.toggle_fullscreen(),
            A::ToggleStatusBar => self.toggle_status_bar(),
            A::SetBezel(style) => self.set_bezel(style),
            A::TogglePerfOverlay => self.toggle_perf_overlay(),

            A::SetPortDevice(port, device) => {
                self.hot_plug_port_device(port, device);
                self.show_osd(format!("Port {}: {}", port + 1, device.menu_label()));
            }
            A::SetJoystickInput(mode) => self.set_joystick_input_mode(mode),
            A::SetAutofire(hz) => {
                self.autofire_hz = hz;
                let label = crate::config::autofire_label(hz);
                info!("autofire: {label}");
                self.show_osd(format!("Autofire: {label}"));
                self.request_redraw();
            }
            A::ToggleKeyboardPanel => self.toggle_keyboard_panel(),

            #[cfg(feature = "midi")]
            A::SetMidiInput(name) => {
                let mut shown = "None".to_string();
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.set_input_endpoint(name.as_deref());
                    shown = sink.input_label();
                }
                self.show_osd(format!("MIDI input: {shown}"));
            }
            #[cfg(feature = "midi")]
            A::SetMidiOutput(name) => {
                let mut shown = "None".to_string();
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.set_output_endpoint(name.as_deref());
                    shown = sink.output_label();
                }
                // The device on the port changed, so the mixer has to ask it
                // for audio again -- an MT-32 just attached, or just left.
                self.emu.bus_mut().paula.rearm_synth_audio();
                #[cfg(feature = "mt32")]
                {
                    self.sync_mt32_panel();
                    self.report_mt32_fault();
                }
                self.show_osd(format!("MIDI output: {shown}"));
            }
            #[cfg(not(feature = "midi"))]
            A::SetMidiInput(_) | A::SetMidiOutput(_) => {}
            #[cfg(feature = "mt32")]
            A::ToggleMt32Panel => {
                let shown = !crate::video::mt32_panel_shown();
                self.set_mt32_panel_shown(shown);
                self.show_osd(if shown {
                    "MT-32: front panel shown"
                } else {
                    "MT-32: front panel hidden"
                });
            }
            #[cfg(not(feature = "mt32"))]
            A::ToggleMt32Panel => {}
            #[cfg(feature = "mt32")]
            A::SetMt32Lcd(style) => {
                crate::video::set_mt32_lcd(style);
                self.show_osd(format!("MT-32: {} display", style.menu_label()));
                self.request_redraw();
            }
            #[cfg(not(feature = "mt32"))]
            A::SetMt32Lcd(_) => {}
            A::SetSamplerInput(name) => {
                if self.sampler.enabled {
                    self.sampler.input_device = Some(name.clone());
                    self.attach_session_sampler();
                    self.show_osd(format!("Sampler input: {name}"));
                }
            }
            A::StepSamplerGain(dir) => self.step_sampler_gain(dir > 0),

            A::SetFloppySpeed(percent) => {
                self.emu.bus_mut().floppy.set_speed_percent(percent);
                let label = crate::floppy::speed_label(percent);
                info!("floppy speed: {label}");
                self.show_osd(format!("Floppy speed: {label}"));
                self.request_redraw();
            }
            A::ToggleRewind => self.toggle_rewind(),

            A::ToggleWarp => self.toggle_warp(),
            A::SetWarpLimit(limit) => {
                self.warp_speed = limit;
                let label = limit.label();
                info!("warp limit: {label}");
                if self.emu.paced() {
                    self.show_osd(format!("Warp limit: {label} (warp off)"));
                } else {
                    self.show_osd(format!("Warp limit: {label}"));
                }
                self.request_redraw();
            }

            A::ToggleRecord => self.toggle_recording(),
            A::ToggleRecordInput => self.toggle_input_recording(),

            A::SaveState => self.save_state_interactive(),
            A::LoadState => self.load_state_from_dialog(event_loop),
            A::QuickSave(slot) => self.quick_save_state(slot + 1),
            A::QuickLoad(slot) => self.quick_load_state(slot + 1, event_loop),
        }
    }

    /// Put the menu cursor under the pointer. Returns true when it moved.
    ///
    /// Hovering a row of a level closes anything opened deeper from it: the
    /// lit row and the open trail are then always the same thing, whether the
    /// pointer or the keys put it there.
    fn follow_menu_hover(&mut self) -> bool {
        if !self.ui.menu_open || self.ui.menu_rows.is_empty() {
            return false;
        }
        let Some(pos) = self.cursor_pos else {
            return false;
        };
        let pos = (pos.0.max(0) as usize, pos.1.max(0) as usize);
        let Some((depth, row)) = ui::menu_hit(&self.ui.menu_rows, &self.ui.menu_nav, pos) else {
            return false;
        };
        // The pointer is resting on the row this level is already open to.
        // Leave it alone: the pointer sits on that row for as long as it takes
        // to set off towards the level it opened, and rebuilding the path here
        // would take that level away before it could be reached.
        if self.ui.menu_nav.open_at(depth) == Some(row) {
            return false;
        }
        if self.ui.menu_nav.depth() == depth && self.ui.menu_nav.cursor() == Some(row) {
            return false;
        }
        let mut path = self.menu_path_to(depth);
        // A category opens as the pointer reaches it, so submenus are walked
        // into rather than clicked into. The cursor stays off the level that
        // opens until the pointer is actually over one of its rows; the
        // category itself stays lit as the way back.
        let opens = self
            .menu_row_at(depth, row)
            .is_some_and(|r| r.enabled && r.is_submenu());
        let cursor = if opens {
            path.push(row);
            None
        } else {
            Some(row)
        };
        self.ui.menu_nav.open_path(path, cursor);
        true
    }

    /// The open path down to `depth`, for a pointer that has landed on a level
    /// without stepping through its parents.
    fn menu_path_to(&self, depth: usize) -> Vec<usize> {
        (0..depth)
            .filter_map(|d| self.ui.menu_nav.open_at(d))
            .collect()
    }

    /// The row at `row` on level `depth` of the open menu.
    fn menu_row_at(&self, depth: usize, row: usize) -> Option<&crate::video::menu::MenuRow> {
        let levels = self.ui.menu_nav.levels(&self.ui.menu_rows);
        let level: &[crate::video::menu::MenuRow] = levels.get(depth)?;
        level.get(row)
    }

    /// Walk the open menu with the keyboard. Returns true when the key was
    /// the menu's, so the caller stops before the machine sees it.
    ///
    /// Up and down step the current level, right opens a category and left
    /// leaves one, Return picks, and Escape backs out a level at a time --
    /// closing the menu from the top, which is where Escape has nothing left
    /// to close.
    fn handle_menu_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) -> bool {
        let ui = &mut self.ui;
        match code {
            KeyCode::ArrowUp => ui.menu_nav.step(&ui.menu_rows, false),
            KeyCode::ArrowDown => ui.menu_nav.step(&ui.menu_rows, true),
            KeyCode::ArrowRight => {
                ui.menu_nav.descend(&ui.menu_rows);
            }
            KeyCode::ArrowLeft => {
                ui.menu_nav.ascend();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.activate_menu_row(Some(event_loop));
                self.ensure_tool_windows_for_open_panels(event_loop);
                return true;
            }
            KeyCode::Escape => {
                if !ui.menu_nav.ascend() {
                    self.close_menu();
                }
            }
            _ => return false,
        }
        self.request_redraw();
        true
    }

    /// Show or hide the MT-32's panel, resizing the presentation to match.
    ///
    /// The panel takes height from the canvas, and the draw helpers size
    /// themselves from the flag, so the two must move together: a taller
    /// canvas over an unchanged buffer indexes past the end of it. Every
    /// route in goes through here.
    #[cfg(feature = "mt32")]
    fn set_mt32_panel_shown(&mut self, shown: bool) {
        if shown == crate::video::mt32_panel_shown() {
            return;
        }
        // Decide before the flag flips whether the window still matches the
        // canvas, so a manual resize survives.
        let was_canvas_sized = self.window_is_canvas_sized();
        crate::video::set_mt32_panel_shown(shown);
        if self.resync_canvas_height() {
            self.follow_canvas_change(was_canvas_sized);
        } else {
            crate::video::set_mt32_panel_shown(!shown);
            let _ = self.resync_canvas_height();
        }
        self.request_redraw();
    }

    /// Show or hide the on-screen keyboard, resizing the presentation to
    /// match. Like the MT-32's panel, the strip takes height from the
    /// canvas and the draw helpers size themselves from the flag, so the
    /// two have to move together. Every route in goes through here.
    fn set_keyboard_panel_shown(&mut self, shown: bool) {
        if shown == crate::video::keyboard_panel_shown() {
            return;
        }
        if !shown {
            // The strip is going away with keys still down on it; hand
            // them back before it does, or the guest is left holding them.
            self.release_keyboard_panel_holds();
        }
        // Decide before the flag flips whether the window still matches the
        // canvas, so a manual resize survives.
        let was_canvas_sized = self.window_is_canvas_sized();
        crate::video::set_keyboard_panel_shown(shown);
        if self.resync_canvas_height() {
            self.follow_canvas_change(was_canvas_sized);
        } else {
            crate::video::set_keyboard_panel_shown(!shown);
            let _ = self.resync_canvas_height();
        }
        self.request_redraw();
    }

    fn toggle_keyboard_panel(&mut self) {
        let shown = !crate::video::keyboard_panel_shown();
        self.set_keyboard_panel_shown(shown);
        self.show_osd(if shown {
            "On-screen keyboard shown"
        } else {
            "On-screen keyboard hidden"
        });
    }

    /// A click on the on-screen keyboard. The strip works out what it
    /// means; what comes back is rawkey transitions for the machine.
    fn press_keyboard_panel_control(&mut self, control: kbdpanel::KbdControl) {
        let outcome = self.kbd_panel.press(control, Instant::now());
        let close = outcome.close;
        self.apply_keyboard_panel_outcome(outcome);
        if close {
            self.set_keyboard_panel_shown(false);
        }
        self.request_redraw();
    }

    /// The mouse button lifted. True when it was holding a keycap, in
    /// which case this click was the keyboard's and nobody else's.
    fn release_keyboard_panel_key(&mut self) -> bool {
        if !self.kbd_panel.holding_key() {
            return false;
        }
        let outcome = self.kbd_panel.release(Instant::now());
        self.apply_keyboard_panel_outcome(outcome);
        self.request_redraw();
        true
    }

    /// Hand the strip's key transitions to the machine. They go through
    /// the same door as a host keystroke -- recorded by `--record-input`
    /// and noted for replay exactly as a real one is -- but as their own
    /// source, so a cap and a host key can hold the same rawkey without
    /// either cutting the other short (see [`KeySource`]).
    fn apply_keyboard_panel_outcome(&mut self, outcome: kbdpanel::KbdOutcome) {
        for (rawkey, pressed) in outcome.keys {
            self.handle_amiga_key_event_from(KeySource::Panel, rawkey, pressed);
        }
    }

    /// Let go of everything the on-screen keyboard is holding, through the
    /// aggregate path, so the machine hears the releases and the drawn
    /// latches match what it believes.
    ///
    /// Called wherever the machine or its keyboard is about to be replaced
    /// or restarted: a latch is a host-side affordance and has no business
    /// outliving the machine it was latched against.
    fn release_keyboard_panel_holds(&mut self) {
        let outcome = self.kbd_panel.release_all();
        self.apply_keyboard_panel_outcome(outcome);
    }

    /// What the strip looks like this frame, with the Caps Lock lamp read
    /// off the keyboard MCU rather than mirrored from the clicks: a
    /// save-state load moves that lamp with no key pressed.
    fn keyboard_panel_view(&mut self) -> kbdpanel::KbdPanelView {
        let caps_lit = self.emu.bus().keyboard.caps_lock_led();
        let hover = self
            .cursor_pos
            .zip(kbdpanel::shown_panel_rect(keyboard_panel_top()))
            .and_then(|(pos, panel)| kbdpanel::control_at(panel, pos));
        self.kbd_panel.view(caps_lit, hover)
    }

    /// The synth the panel is driving, when one is fitted and switched on.
    #[cfg(feature = "mt32")]
    fn mt32_synth_mut(&mut self) -> Option<&mut crate::mt32::Mt32Synth> {
        Some(
            self.emu
                .bus_mut()
                .midi_serial_mut()?
                .mt32_mut()?
                .synth_mut(),
        )
    }

    /// A press on the MT-32's front panel. The panel decides what it means;
    /// anything it cannot reach itself comes back as an action.
    #[cfg(feature = "mt32")]
    fn press_mt32_control(&mut self, control: mt32panel::Mt32Control, left: bool, pos: (i32, i32)) {
        let Some(rect) = mt32panel::shown_panel_rect(present_height()) else {
            return;
        };
        // A unit that is switched off still takes note of what is held on
        // it, so the panel is told which it is.
        let powered = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        let mut panel = std::mem::take(&mut self.mt32_panel);
        let action = panel.press(control, left, pos, rect, powered, self.mt32_synth_mut());
        self.mt32_panel = panel;
        self.apply_mt32_action(action);
        self.serve_mt32_demo();
        self.request_redraw();
    }

    /// Carry out what the panel asked for.
    #[cfg(feature = "mt32")]
    fn apply_mt32_action(&mut self, action: mt32panel::PanelAction) {
        use mt32panel::PanelAction;
        match action {
            PanelAction::None => {}
            PanelAction::Say(text) => self.show_osd(text),
            PanelAction::Power(_) => self.toggle_mt32_power(),
            PanelAction::Recycle => {
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    // Off and on again is what a reset amounts to here: the
                    // engine comes back at its power-on defaults.
                    sink.set_mt32_power(false);
                    sink.set_mt32_power(true);
                }
                self.emu.bus_mut().paula.rearm_synth_audio();
                self.mt32_panel.reset();
                self.show_osd("MT-32: all reset");
            }
        }
    }

    /// The power switch.
    #[cfg(feature = "mt32")]
    fn toggle_mt32_power(&mut self) {
        let on = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
            sink.set_mt32_power(!on);
        }
        // A fresh synth has to be asked for audio again, and the panel
        // starts over: a unit just switched on is at its defaults, showing
        // its greeting.
        self.emu.bus_mut().paula.rearm_synth_audio();
        self.mt32_panel.reset();
        self.tell_panel_the_rom_version();
        self.serve_mt32_demo();
        let came_up = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        if !on && !came_up {
            // Asked to switch on and it did not: say why rather than
            // claiming it is running.
            self.report_mt32_fault();
            return;
        }
        self.show_osd(if on {
            "MT-32: power off"
        } else {
            "MT-32: power on"
        });
    }

    /// Follow the pointer while a button is held on the dial.
    #[cfg(feature = "mt32")]
    fn drag_mt32_dial(&mut self, pos: (i32, i32)) {
        let Some(rect) = mt32panel::shown_panel_rect(present_height()) else {
            return;
        };
        let mut panel = std::mem::take(&mut self.mt32_panel);
        panel.drag_dial(pos, rect, self.mt32_synth_mut());
        self.mt32_panel = panel;
        self.request_redraw();
    }

    /// Step the dial on while a button is held still on it.
    #[cfg(feature = "mt32")]
    fn repeat_mt32_dial(&mut self) {
        if !self.mt32_panel.dial_held() {
            return;
        }
        let mut panel = std::mem::take(&mut self.mt32_panel);
        let moved = panel.repeat_dial(self.mt32_synth_mut());
        self.mt32_panel = panel;
        if moved {
            self.request_redraw();
        }
    }

    /// Say why the MT-32 is not there, if it was asked for and could not be
    /// fitted. Said once: the fault is taken, not borrowed.
    #[cfg(feature = "mt32")]
    fn report_mt32_fault(&mut self) {
        let fault = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .and_then(crate::midi::MidiSerialSink::take_mt32_fault);
        if let Some(fault) = fault {
            self.warn_osd(format!("MT-32: {fault}"));
        }
    }

    /// Bring the panel into line with what is on the port.
    ///
    /// Unplugging the MT-32 takes the panel down with it -- a fascia with no
    /// instrument behind it is just a blank strip -- and plugging one in
    /// starts its panel from scratch, so the engine's power-up greeting is
    /// what shows.
    #[cfg(feature = "mt32")]
    fn sync_mt32_panel(&mut self) {
        // Selected, not powered: switching the unit off leaves its panel
        // where it is, which is the whole point of having a switch.
        let selected = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32_selected());
        self.mt32_panel.reset();
        self.tell_panel_the_rom_version();
        if !selected {
            self.set_mt32_panel_shown(false);
        }
    }

    /// Start whichever of the ROM's own songs the panel is asking for, and
    /// tell it what that one is called.
    #[cfg(feature = "mt32")]
    fn serve_mt32_demo(&mut self) {
        // A song that has run out hands on to the next, which is what makes
        // it a chain.
        if self.mt32_panel.chain_ran_out(
            self.emu
                .bus_mut()
                .midi_serial_mut()
                .is_some_and(|sink| sink.mt32_demo_playing()),
        ) {
            self.request_redraw();
        }
        let Some(want) = self.mt32_panel.demo_want() else {
            return;
        };
        let track = match want {
            mt32panel::DemoWant::Play(track) => Some(track),
            mt32panel::DemoWant::Stop => None,
        };
        let title = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .map(|sink| match track {
                Some(track) => sink
                    .play_mt32_demo(track)
                    // Only the later ROMs carry them; the earlier units had
                    // no demonstration to play.
                    .unwrap_or_else(|| "Needs v2.0x ROM".to_string()),
                None => {
                    sink.stop_mt32_demo();
                    String::new()
                }
            })
            .unwrap_or_default();
        self.mt32_panel.set_track_title(title);
    }

    /// Hand the panel what the control ROM calls itself, for its version
    /// screen. The engine keeps its copy of the image to itself, so this
    /// comes from the sink, which read it off disk when the pair was
    /// configured.
    #[cfg(feature = "mt32")]
    fn tell_panel_the_rom_version(&mut self) {
        if let Some(version) = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .and_then(|sink| sink.mt32_version().map(str::to_string))
        {
            self.mt32_panel.set_version(version);
        }
    }

    /// What the MT-32's panel should show, when one is attached.
    #[cfg(feature = "mt32")]
    fn mt32_panel_view(&mut self) -> Option<mt32panel::Mt32PanelView> {
        // A song that runs out hands on to the next, which only happens
        // while frames are being rendered -- so it is looked at here, as
        // the panel is drawn, rather than only when something is clicked.
        self.serve_mt32_demo();
        let sink = self.emu.bus_mut().midi_serial_mut()?;
        if !sink.mt32_selected() {
            return None;
        }
        // Switched off, the fascia is still there: dark display, no lamp.
        let (lcd, led) = sink
            .mt32_mut()
            .map_or_else(|| (String::new(), false), |mt32| mt32.synth_mut().display());
        let powered = sink.mt32().is_some();
        let hover = self
            .cursor_pos
            .zip(mt32panel::shown_panel_rect(present_height()))
            .and_then(|(pos, panel)| mt32panel::hover_at(panel, pos));
        Some(self.mt32_panel.view(lcd, led, powered, hover))
    }

    /// Re-plan the presentation after the canvas height changed, and resize
    /// every buffer that indexes by it. False when the texture could not be
    /// resized, in which case the caller has to put its flag back: the draw
    /// helpers size themselves from the flag, so a taller canvas over a
    /// shorter buffer would index past it.
    ///
    /// Shared by every strip that takes a slice of the canvas -- the MT-32
    /// panel and the on-screen keyboard -- since what has to follow the
    /// change is the same in each case.
    fn resync_canvas_height(&mut self) -> bool {
        if let Some(r) = self.render.as_mut() {
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                warn!("resize texture buffer for a canvas-height change failed: {e}");
                return false;
            }
        }
        // Tool windows draw through the same canvas height, so their buffers
        // follow too. Buffer only: their own window sizes are their business.
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for a canvas-height change failed: {e}");
                }
                tool.window.request_redraw();
            }
        }
        true
    }

    /// Close the menu and forget where it was open to.
    fn close_menu(&mut self) {
        self.ui.menu_open = false;
        self.ui.menu_rows = Vec::new();
        self.ui.menu_nav.reset();
    }

    /// Run the action behind a clicked status-bar control (volume is
    /// handled separately because it starts a drag).
    fn activate_bar_control(&mut self, control: BarControl) {
        match control {
            BarControl::Power => self.toggle_power(),
            BarControl::Pause => self.toggle_pause(),
            BarControl::Reboot => self.reset_emulator(true),
            BarControl::Screenshot => self.take_screenshot(),
            BarControl::Menu => self.toggle_menu(),
            // A bridged drive's media is a real disk in a real drive: it is
            // loaded, swapped, and ejected by hand. The buttons stay drawn so
            // the drive is visibly there and numbered, but they do nothing.
            BarControl::DriveLoad(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveSwap(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveEject(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveLoad(idx) => self.load_drive_disks_from_dialog(idx),
            BarControl::DriveSwap(idx) => self.swap_drive_disk(idx),
            BarControl::DriveEject(idx) => self.eject_drive_disk(idx),
            BarControl::CdLoad => self.load_cd_from_dialog(),
            BarControl::CdEject => self.eject_cd(),
            BarControl::Joystick => {
                self.cycle_joystick_input_mode();
                self.request_redraw();
            }
            BarControl::Keyboard => self.toggle_keyboard_panel(),
            BarControl::Volume => {}
        }
    }

    /// Run the action behind a clicked menu item or panel control.
    #[cfg(test)]
    fn activate_ui_control(&mut self, control: UiControl) {
        self.activate_ui_control_with_event_loop(control, None);
    }

    fn activate_ui_control_with_event_loop(
        &mut self,
        control: UiControl,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        match control {
            // A row of the tree menu: a category opens, a leaf acts. The
            // pointer can land on any level, so the path is set to where it
            // landed rather than stepped into.
            UiControl::MenuRow { depth, row } => {
                let path = self.menu_path_to(depth);
                self.ui.menu_nav.open_path(path, Some(row));
                self.activate_menu_row(event_loop);
            }
            UiControl::RemapSet(set) => self.input_map_select_mapping(set),
            UiControl::RemapBind(index) => self.input_map_arm_capture(index),
            UiControl::RemapClear(index) => self.input_map_clear(index),
            UiControl::RemapDefaults => self.input_map_defaults(),
            UiControl::RemapSave => self.input_map_save(),
            UiControl::PanelClose | UiControl::CalCancel => self.close_panel(),
            UiControl::PanelBody => {}
            UiControl::CalSkip => {
                if let Some(Panel::Calibration(session)) = self.ui.panel.as_mut() {
                    session.skip_current();
                }
            }
            UiControl::CalSave => self.save_calibration(),
            UiControl::DebugTab(tab) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.tab = tab;
                }
            }
            UiControl::DebugRun => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugStep => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugStepOver => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugStepOut => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugStepFrame => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugRunTo => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugRunLine => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugReverseStep => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugReverseFrame => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugReverseRun => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugMemPrev => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugMemNext => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugPoke => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugEntry => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.entry_active = true;
                }
            }
            UiControl::DebugBreakToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugWatchToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugRegToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugBeamToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugCatchToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugCopperBreakToggle => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugCopperStep => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugMemFind => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugMemSave => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugMemWriter => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugMemBits => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugPlaneToggle(_) => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugSpriteToggle(_) => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugBreaksClear => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugWaveArm => self.activate_tool_control(ToolPanelKind::Debugger, control),
            UiControl::DebugWaveStop => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::DebugAudioMute(_) => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            UiControl::AnalyzerRun => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            UiControl::AnalyzerFrame => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            UiControl::AnalyzerUnderlay => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            UiControl::AnalyzerScrub => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            UiControl::AnalyzerRunTo => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            UiControl::AnalyzerPick { x, y, scanline } => {
                self.frame_analyzer_select(x, y, scanline)
            }
            UiControl::LauncherModel(model) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.select_model(Some(model));
                    state.status = None;
                }
            }
            UiControl::LauncherTab(tab) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.tab = tab;
                    // A message reports what the page just did, so it belongs
                    // to that page: leaving clears it.
                    state.status = None;
                    // Opening the Host Disk page is the moment to look at the
                    // host's storage: a card pushed in since the launcher
                    // opened should be there when the page is.
                    if tab == crate::video::launcher::LauncherTab::HostDisk {
                        state.setup.refresh_host_disks();
                    }
                }
            }
            UiControl::LauncherCycle { field, forward } => {
                if let Some(state) = self.launcher_state_mut() {
                    // Reaching for another control ends the typing, the way
                    // Enter does: what is in the box counts.
                    state.edit_commit();
                    let refused = state.editing().is_some();
                    if LauncherState::is_workshop(field) {
                        state.workshop_cycle(field, forward);
                    } else {
                        state.setup.cycle(field, forward);
                    }
                    if !refused {
                        state.status = None;
                    }
                }
            }
            UiControl::LauncherToggle(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_commit();
                    let refused = state.editing().is_some();
                    if LauncherState::is_workshop(field) {
                        state.workshop_toggle_flip(field);
                    } else {
                        state.setup.toggle(field);
                    }
                    if !refused {
                        state.status = None;
                    }
                }
            }
            UiControl::LauncherClear(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.clear_path(field);
                    state.status = None;
                }
            }
            UiControl::LauncherDriveNameEdit(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.begin_edit_drive_name(field);
                }
            }
            UiControl::LauncherNewImageEdit(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.begin_edit_new_image(field);
                }
            }
            UiControl::LauncherNewImageCreate(field) => self.launcher_create_image(field),
            UiControl::LauncherFsFamily { field, family } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_commit();
                    state.workshop_set_fs_family(field, family);
                    state.status = None;
                }
            }
            UiControl::LauncherFsVariant { field, variant } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_commit();
                    state.workshop_set_fs_variant(field, variant);
                    state.status = None;
                }
            }
            UiControl::LauncherNewImageUnit => {
                if let Some(state) = self.launcher_state_mut() {
                    // The size in the box is in the unit being left, so it
                    // has to be taken before the unit changes under it.
                    state.edit_commit();
                    state.workshop.flip_size_unit();
                    state.status = None;
                }
            }
            UiControl::LauncherGeometryAuto | UiControl::LauncherGeometryCustom => {
                let by_hand = control == UiControl::LauncherGeometryCustom;
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_commit();
                    // Entering hand-set geometry starts from what Auto
                    // would have produced, so the figures are never blank
                    // and never disagree with the size.
                    if by_hand && !state.workshop.geometry_custom {
                        state.workshop.geometry_from_size();
                    }
                    state.workshop.geometry_custom = by_hand;
                    state.status = None;
                }
            }
            UiControl::LauncherDriveBootpriEdit(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.begin_edit_drive_bootpri(field);
                }
            }
            UiControl::LauncherDriveBridgeToggle(bay) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    let on = !state.setup.drive_bridged(bay);
                    state.setup.set_drive_bridged(bay, on);
                    state.status = None;
                }
            }
            UiControl::LauncherHostDiskSelect(index) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.select_host_disk(index);
                    // A refused tick reports itself on the status line, where
                    // every other warning is looked for.
                    state.status = state.setup.host_disk_warning().map(StatusMessage::err);
                }
            }
            UiControl::LauncherHostDiskWritable(index) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.toggle_host_disk_writable(index);
                    state.status = None;
                }
            }
            UiControl::LauncherHostDiskAttach(index) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.cycle_host_disk_attach(index, true);
                    state.status = None;
                }
            }
            UiControl::LauncherHostDiskUnmount(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    let attach = crate::video::launcher::MachineSetup::host_disk_attach_of(field);
                    let removed = attach.and_then(|a| state.setup.unmount_host_disk(a));
                    state.status = Some(match (removed, attach) {
                        (Some(device), Some(attach)) => {
                            // Off the machine and back to the host in one act.
                            // Mounting took the disk, so letting go has to
                            // close what that opened, or the host would be
                            // told it may have a disk this process still
                            // holds.
                            let released = crate::blockdev::release_device(&device);
                            log::info!("host disk: {device} taken off {}", attach.label());
                            StatusMessage::ok(if released {
                                format!("{device} released")
                            } else {
                                // The setting is gone either way, but a
                                // machine already running with this disk holds
                                // it in its own right, and saying "released"
                                // of a disk still locked is how somebody ends
                                // up wondering why they cannot have it back.
                                format!(
                                    "{device} released; a running machine keeps it until it stops"
                                )
                            })
                        }
                        _ => StatusMessage::err("Nothing to unmount"),
                    });
                }
            }
            UiControl::LauncherHostDiskUnmountSelected => {
                if let Some(state) = self.launcher_state_mut() {
                    let released = state.setup.unmount_selected_host_disks();
                    state.status = Some(if released.is_empty() {
                        StatusMessage::err("Nothing to unmount")
                    } else {
                        // Off the machine and back to the host in one act,
                        // exactly as the Storage rows' Unmount does it.
                        for device in &released {
                            crate::blockdev::release_device(device);
                            log::info!("host disk: {device} taken off the machine");
                        }
                        StatusMessage::ok(match released.as_slice() {
                            [device] => format!("{device} released"),
                            many => format!("{} released", many.join(", ")),
                        })
                    });
                }
            }
            UiControl::LauncherHostDiskScroll(delta) => {
                if let Some(state) = self.launcher_state_mut() {
                    state
                        .setup
                        .scroll_host_disks(delta, crate::video::ui::HOST_DISK_VISIBLE_ROWS);
                }
            }
            UiControl::LauncherHostDiskRefresh => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.refresh_host_disks();
                    let found = state.setup.host_disks().len();
                    state.status = Some(StatusMessage::ok(match found {
                        0 => "No supported disks found on the host system".to_string(),
                        1 => "1 disk found".to_string(),
                        n => format!("{n} disks found"),
                    }));
                }
            }
            UiControl::LauncherHostDiskMount => {
                if let Some(state) = self.launcher_state_mut() {
                    // Mounting rearranges the machine -- a disk takes a slot,
                    // and whatever image was in it goes. If the host then
                    // refuses the disk, none of that should have happened, so
                    // the whole setup is put back rather than unpicked
                    // step by step and one step forgotten.
                    let before = state.setup.clone();
                    let status = match state.setup.mount_host_disks() {
                        Ok(disks) => {
                            // The host gives the disk up here, not when the
                            // machine starts. A real disk needs permission on
                            // some hosts, and this is where somebody has just
                            // asked for one -- a dialog minutes later, behind
                            // a machine booting, belongs to nothing they did.
                            // Every disk the machine is set up to have, not
                            // only the ones just ticked: this says which disks
                            // are wanted, and anything held that is not on the
                            // list goes back to the host.
                            let asked: Vec<(String, Option<String>, bool, bool)> = state
                                .setup
                                .host_disks_attached()
                                .iter()
                                .map(|disk| {
                                    (
                                        disk.device.clone(),
                                        disk.fingerprint.clone(),
                                        disk.writable,
                                        disk.identity_confirmed,
                                    )
                                })
                                .collect();
                            let refused = match crate::blockdev::reserve_devices(&asked) {
                                Ok(()) => {
                                    for disk in &disks {
                                        log::info!(
                                            "host disk: {} attached to {} ({})",
                                            disk.device,
                                            disk.attach.label(),
                                            if disk.writable {
                                                "read/write"
                                            } else {
                                                "read only"
                                            }
                                        );
                                    }
                                    None
                                }
                                Err(error) => {
                                    // The outermost sentence only. These are
                                    // written to be read, and flattening the
                                    // chain onto one line ends in a raw OS
                                    // code with the useful half cut out of the
                                    // middle. The log keeps all of it.
                                    log::warn!("host disk: not attached: {error:#}");
                                    Some(error.to_string())
                                }
                            };
                            match refused {
                                Some(reason) => {
                                    // Nothing stays attached that the host did
                                    // not give up, including the disks taken
                                    // before the one that failed: a machine
                                    // must not start expecting a disk that was
                                    // refused here.
                                    for disk in &disks {
                                        crate::blockdev::release_device(&disk.device);
                                    }
                                    state.setup = before;
                                    log::warn!("host disk: not attached: {reason}");
                                    StatusMessage::err(reason)
                                }
                                None => {
                                    let places: Vec<_> = disks.iter().map(|d| d.attach).collect();
                                    let where_to =
                                        crate::config::HostDiskAttach::describe_all(&places);
                                    StatusMessage::ok(if disks.len() == 1 {
                                        format!("Host disk attached to {where_to}")
                                    } else {
                                        format!("Host disks attached to {where_to}")
                                    })
                                }
                            }
                        }
                        Err(reason) => {
                            log::warn!("host disk: not attached: {reason}");
                            StatusMessage::err(reason)
                        }
                    };
                    state.status = Some(status);
                }
            }
            UiControl::LauncherBridgeConfigure(bay) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.set_bridge_edit_drive(bay);
                    state.tab = crate::video::launcher::LauncherTab::FluxBridge;
                    state.status = None;
                }
            }
            UiControl::LauncherDriveBootToggle(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.toggle_drive_boot(field);
                    state.status = None;
                }
            }
            UiControl::LauncherZorroRemove(idx) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.remove_zorro(idx);
                    state.status = None;
                }
            }
            UiControl::LauncherBoardCycle {
                board,
                opt,
                forward,
            } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.zorro_option_cycle(board, opt, forward);
                    state.status = None;
                }
            }
            UiControl::LauncherBoardToggle { board, opt } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.zorro_option_toggle(board, opt);
                    state.status = None;
                }
            }
            UiControl::LauncherBoardClear { board, opt } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.edit_cancel();
                    state.setup.zorro_option_clear(board, opt);
                    state.status = None;
                }
            }
            UiControl::LauncherBoardEdit { board, opt } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.begin_edit_board(board, opt);
                }
            }
            UiControl::LauncherBoardBrowse { board, opt } => self.launcher_board_browse(board, opt),
            UiControl::LauncherDefaults => {
                if let Some(state) = self.launcher_state_mut() {
                    let model = state.setup.model();
                    state.setup = MachineSetup::default();
                    state.setup.select_model(model);
                    state.setup.refresh_host_devices();
                    state.status = Some(StatusMessage::ok("Reset to defaults"));
                }
            }
            UiControl::LauncherBrowse(field) => self.launcher_browse(field),
            UiControl::LauncherZorroAdd => self.launcher_add_zorro(),
            UiControl::LauncherLoad => self.launcher_load(),
            UiControl::LauncherSave => self.launcher_save(),
            UiControl::LauncherRun => self.launcher_run(),
            UiControl::DropDrive(drive_idx) => self.drop_chooser_route(drive_idx),
            UiControl::AnalyzerTab(_)
            | UiControl::AnalyzerHeatPreset(_)
            | UiControl::AnalyzerHeatPick { .. } => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
        }
        self.request_redraw();
    }

    fn activate_tool_control(&mut self, kind: ToolPanelKind, control: UiControl) {
        match (kind, control) {
            (ToolPanelKind::Debugger, UiControl::PanelClose) => self.close_tool_panel(kind),
            (ToolPanelKind::FrameAnalyzer, UiControl::PanelClose)
            | (ToolPanelKind::Console, UiControl::PanelClose) => self.close_tool_panel(kind),
            (ToolPanelKind::Console, UiControl::PanelBody) => {}
            (ToolPanelKind::Debugger, UiControl::PanelBody)
            | (ToolPanelKind::FrameAnalyzer, UiControl::PanelBody) => {}
            (ToolPanelKind::Debugger, UiControl::DebugTab(tab)) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.tab = tab;
                }
            }
            (ToolPanelKind::Debugger, UiControl::DebugRun) => self.debugger_toggle_run(),
            (ToolPanelKind::Debugger, UiControl::DebugStep) => self.debugger_step(),
            (ToolPanelKind::Debugger, UiControl::DebugStepOver) => self.debugger_step_over(),
            (ToolPanelKind::Debugger, UiControl::DebugStepOut) => self.debugger_step_out(),
            (ToolPanelKind::Debugger, UiControl::DebugStepFrame) => self.debugger_step_frame(),
            (ToolPanelKind::Debugger, UiControl::DebugRunTo) => self.debugger_run_to(),
            (ToolPanelKind::Debugger, UiControl::DebugRunLine) => self.debugger_run_to_line_end(),
            (ToolPanelKind::Debugger, UiControl::DebugReverseStep) => self.debugger_reverse_step(),
            (ToolPanelKind::Debugger, UiControl::DebugReverseFrame) => {
                self.debugger_reverse_frame()
            }
            (ToolPanelKind::Debugger, UiControl::DebugReverseRun) => {
                self.debugger_reverse_continue()
            }
            (ToolPanelKind::Debugger, UiControl::DebugMemPrev) => self.debugger_mem_page(-1),
            (ToolPanelKind::Debugger, UiControl::DebugMemNext) => self.debugger_mem_page(1),
            (ToolPanelKind::Debugger, UiControl::DebugPoke) => self.debugger_poke(),
            (ToolPanelKind::Debugger, UiControl::DebugEntry) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.entry_active = true;
                }
            }
            (ToolPanelKind::Debugger, UiControl::DebugBreakToggle) => {
                self.debugger_toggle_breakpoint()
            }
            (ToolPanelKind::Debugger, UiControl::DebugWatchToggle) => {
                self.debugger_toggle_watchpoint()
            }
            (ToolPanelKind::Debugger, UiControl::DebugRegToggle) => {
                self.debugger_toggle_reg_watch()
            }
            (ToolPanelKind::Debugger, UiControl::DebugBeamToggle) => {
                self.debugger_toggle_beam_trap()
            }
            (ToolPanelKind::Debugger, UiControl::DebugCatchToggle) => self.debugger_toggle_catch(),
            (ToolPanelKind::Debugger, UiControl::DebugCopperBreakToggle) => {
                self.debugger_toggle_copper_break()
            }
            (ToolPanelKind::Debugger, UiControl::DebugCopperStep) => self.debugger_step_copper(),
            (ToolPanelKind::Debugger, UiControl::DebugMemFind) => self.debugger_mem_find(),
            (ToolPanelKind::Debugger, UiControl::DebugMemSave) => self.debugger_mem_save_region(),
            (ToolPanelKind::Debugger, UiControl::DebugMemWriter) => self.debugger_mem_writer(),
            (ToolPanelKind::Debugger, UiControl::DebugMemBits) => self.debugger_mem_toggle_bits(),
            (ToolPanelKind::Debugger, UiControl::DebugPlaneToggle(plane)) => {
                self.debugger_toggle_plane(plane)
            }
            (ToolPanelKind::Debugger, UiControl::DebugSpriteToggle(sprite)) => {
                self.debugger_toggle_sprite(sprite)
            }
            (ToolPanelKind::Debugger, UiControl::DebugBreaksClear) => {
                self.emu.machine.ui_breaks_clear();
                self.last_debug_stop = None;
                self.show_osd("Cleared all breakpoints and watchpoints");
            }
            (ToolPanelKind::Debugger, UiControl::DebugWaveArm) => self.debugger_wave_arm(),
            (ToolPanelKind::Debugger, UiControl::DebugWaveStop) => self.debugger_wave_stop(),
            (ToolPanelKind::Debugger, UiControl::DebugAudioMute(idx)) => {
                let paula = &mut self.emu.bus_mut().paula;
                let (label, muted) = if idx < 4 {
                    paula.toggle_channel_muted(idx);
                    (format!("AUD{idx}"), paula.channel_muted(idx))
                } else {
                    paula.toggle_cd_muted();
                    ("CD audio".to_string(), paula.cd_muted())
                };
                self.show_osd(format!(
                    "{label} {}",
                    if muted { "muted" } else { "unmuted" }
                ));
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerRun) => {
                self.frame_analyzer_toggle_run()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerFrame) => {
                self.frame_analyzer_step_frame()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerPick { x, y, scanline }) => {
                self.frame_analyzer_select(x, y, scanline)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerUnderlay) => {
                self.frame_analyzer_toggle_underlay()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerScrub) => {
                self.frame_analyzer_toggle_scrub()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerRunTo) => {
                self.frame_analyzer_run_to_slot()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerTab(tab)) => {
                self.frame_analyzer_set_tab(tab)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerHeatPreset(index)) => {
                self.frame_analyzer_heat_preset(index)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerHeatPick { x, y }) => {
                self.frame_analyzer_heat_pick(x, y)
            }
            _ => {}
        }
        self.request_redraw();
    }

    /// Keys consumed by the open menu/panel (Escape, debugger hex entry).
    /// Returns true when the key was handled and must not reach the Amiga.
    fn ui_handle_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        if self.ui.active() {
            // An armed Input Mapping row eats the next key, including Escape
            // (which cancels the binding rather than closing the panel).
            if self.input_map_handle_key(code) {
                return true;
            }
            if code == KeyCode::Escape {
                // While typing into a plugin option, Escape cancels the edit
                // rather than closing the panel.
                if self.launcher_cancel_edit_if_active() {
                    return true;
                }
                if self.ui.menu_open {
                    self.ui.menu_open = false;
                    self.request_redraw();
                } else {
                    self.close_panel();
                }
                return true;
            }
            // Drop chooser: a digit picks the Nth listed drive (the button
            // labels carry the same numbers).
            if let Some(Panel::DropChooser(state)) = &self.ui.panel {
                let index = match code {
                    KeyCode::Digit1 => Some(0),
                    KeyCode::Digit2 => Some(1),
                    KeyCode::Digit3 => Some(2),
                    KeyCode::Digit4 => Some(3),
                    _ => None,
                };
                if let Some(index) = index {
                    if let Some(drive) = state.drives.get(index).map(|entry| entry.drive) {
                        self.drop_chooser_route(drive);
                    }
                    return true;
                }
            }
            // Route keys to a focused plugin-option text field, if any.
            if self.launcher_handle_edit_key(code, text) {
                return true;
            }
            return false;
        }
        self.default_tool_key_kind()
            .is_some_and(|kind| self.ui_handle_tool_key(kind, code))
    }

    /// Cancel an in-progress plugin-option text edit, if one is focused.
    fn launcher_cancel_edit_if_active(&mut self) -> bool {
        let cancelled = matches!(
            self.launcher_state_mut(),
            Some(state) if state.editing().is_some()
        );
        if cancelled {
            if let Some(state) = self.launcher_state_mut() {
                state.edit_cancel();
            }
            self.request_redraw();
        }
        cancelled
    }

    /// Feed a key to a focused plugin-option text field. Returns false (so the
    /// key falls through) when no field is being edited.
    fn launcher_handle_edit_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        let handled = {
            let Some(state) = self.launcher_state_mut() else {
                return false;
            };
            if state.editing().is_none() {
                return false;
            }
            match code {
                KeyCode::Backspace => state.edit_backspace(),
                KeyCode::Enter | KeyCode::NumpadEnter => state.edit_commit(),
                _ => {
                    // Prefer the layout- and shift-aware text the platform
                    // reports, so volume names can contain lowercase letters,
                    // underscores, and other printable characters (the
                    // keycode map is uppercase-only and lacks symbols). Fall
                    // back to it only when no text is delivered.
                    if let Some(t) = text.filter(|t| !t.is_empty()) {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            state.edit_push(ch);
                        }
                    } else if let Some(ch) = entry_char_for_key(code) {
                        state.edit_push(ch);
                    }
                }
            }
            true
        };
        if handled {
            self.request_redraw();
        }
        handled
    }

    fn default_tool_key_kind(&self) -> Option<ToolPanelKind> {
        if self.debugger_panel.is_some() {
            Some(ToolPanelKind::Debugger)
        } else if self.frame_analyzer_panel.is_some() {
            Some(ToolPanelKind::FrameAnalyzer)
        } else {
            None
        }
    }

    fn ui_handle_tool_key(&mut self, kind: ToolPanelKind, code: KeyCode) -> bool {
        if code == KeyCode::Escape {
            self.close_tool_panel(kind);
            return true;
        }
        match kind {
            ToolPanelKind::Debugger => self.ui_handle_debugger_key(code),
            ToolPanelKind::FrameAnalyzer => self.ui_handle_frame_analyzer_key(code),
            ToolPanelKind::Console => self.ui_handle_console_key(code),
        }
    }

    fn ui_handle_debugger_key(&mut self, code: KeyCode) -> bool {
        let Some(panel) = self.debugger_panel.as_mut() else {
            return false;
        };
        if panel.entry_active {
            if let Some(ch) = entry_char_for_key(code) {
                panel.push_entry_char(ch);
                self.request_redraw();
                return true;
            }
            match code {
                KeyCode::Backspace => {
                    panel.backspace_entry();
                    self.request_redraw();
                    return true;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    match panel.tab {
                        ui::DebugTab::Memory => {
                            if let Some(addr) = panel.entry_addr() {
                                panel.mem_addr = addr & !0xF;
                            }
                        }
                        // The IO Map takes an offset (96) or address (DFF096).
                        ui::DebugTab::IoMap => {
                            if let Some(addr) = panel.entry_addr() {
                                panel.iomap_sel = (addr as u16) & 0x1FE;
                            }
                        }
                        // On the CPU tab, Enter pins the disassembly to
                        // the typed address; an empty box follows the PC again.
                        ui::DebugTab::Cpu => panel.disasm_addr = panel.entry_addr(),
                        _ => {}
                    }
                    panel.entry_active = false;
                    self.request_redraw();
                    return true;
                }
                _ => {}
            }
        }
        if panel.entry_active {
            return false;
        }
        let control = match code {
            KeyCode::KeyS => Some(UiControl::DebugStep),
            KeyCode::KeyO => Some(UiControl::DebugStepOver),
            KeyCode::KeyU => Some(UiControl::DebugStepOut),
            KeyCode::KeyF => Some(UiControl::DebugStepFrame),
            KeyCode::KeyL => Some(UiControl::DebugRunLine),
            KeyCode::KeyC => Some(UiControl::DebugCopperStep),
            KeyCode::KeyR => Some(UiControl::DebugRun),
            _ => None,
        };
        if let Some(control) = control {
            self.activate_tool_control(ToolPanelKind::Debugger, control);
            return true;
        }
        // Memory tab: cursor/page keys scroll the hex or bitmap view.
        if self
            .debugger_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::DebugTab::Memory)
        {
            let rows = match code {
                KeyCode::ArrowUp => Some(-1),
                KeyCode::ArrowDown => Some(1),
                KeyCode::PageUp => Some(-16),
                KeyCode::PageDown => Some(16),
                _ => None,
            };
            if let Some(rows) = rows {
                self.debugger_mem_scroll(rows);
                return true;
            }
        }
        // IO Map tab: arrows move the register selection (left/right by
        // a display column), PageUp/Down by a page.
        if self
            .debugger_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::DebugTab::IoMap)
        {
            let delta = match code {
                KeyCode::ArrowUp => Some(-1i32),
                KeyCode::ArrowDown => Some(1),
                KeyCode::ArrowLeft => Some(-26),
                KeyCode::ArrowRight => Some(26),
                KeyCode::PageUp => Some(-78),
                KeyCode::PageDown => Some(78),
                _ => None,
            };
            if let Some(delta) = delta {
                self.debugger_iomap_move(delta);
                return true;
            }
        }
        false
    }

    /// Move the IO Map selection by `delta` registers, clamped to the
    /// custom bank.
    fn debugger_iomap_move(&mut self, delta: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            let idx = i32::from(panel.iomap_sel >> 1) + delta;
            panel.iomap_sel = (idx.clamp(0, 255) as u16) << 1;
            self.request_redraw();
        }
    }

    fn ui_handle_frame_analyzer_key(&mut self, code: KeyCode) -> bool {
        if self.frame_analyzer_panel.is_none() {
            return false;
        }
        let memory_tab = self
            .frame_analyzer_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::AnalyzerTab::Memory);
        let control = match code {
            KeyCode::KeyF => Some(UiControl::AnalyzerFrame),
            KeyCode::KeyR => Some(UiControl::AnalyzerRun),
            KeyCode::KeyU => Some(UiControl::AnalyzerUnderlay),
            KeyCode::KeyB => Some(UiControl::AnalyzerScrub),
            KeyCode::KeyT => Some(UiControl::AnalyzerRunTo),
            // One key flips between the two views of the traced machine.
            KeyCode::KeyM => Some(UiControl::AnalyzerTab(if memory_tab {
                ui::AnalyzerTab::Beam
            } else {
                ui::AnalyzerTab::Memory
            })),
            _ => None,
        };
        if let Some(control) = control {
            self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control);
            return true;
        }
        let delta = match code {
            KeyCode::ArrowLeft => Some((-1, 0)),
            KeyCode::ArrowRight => Some((1, 0)),
            KeyCode::ArrowUp => Some((0, -1)),
            KeyCode::ArrowDown => Some((0, 1)),
            _ => None,
        };
        if let Some((dx, dy)) = delta {
            // The arrows nudge whichever selection the visible tab has: a
            // beam slot on the Beam tab, a grid cell on the Memory tab.
            if memory_tab {
                self.frame_analyzer_move_heat_selection(dx, dy);
            } else {
                self.frame_analyzer_move_selection(dx, dy);
            }
            return true;
        }
        false
    }

    /// Console keyboard input: printable characters append to the command
    /// line; editing, history, and scrollback keys do the rest.
    fn ui_handle_console_key(&mut self, code: KeyCode) -> bool {
        if self.console_panel.is_none() {
            return false;
        }
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
            self.console_submit();
            self.request_redraw();
            return true;
        }
        let Some(panel) = self.console_panel.as_mut() else {
            return false;
        };
        if let Some(ch) = entry_char_for_key(code) {
            panel.push_input_char(ch);
            self.request_redraw();
            return true;
        }
        match code {
            KeyCode::Backspace => {
                panel.input.pop();
                panel.history_pos = None;
            }
            KeyCode::ArrowUp => panel.history_step(-1),
            KeyCode::ArrowDown => panel.history_step(1),
            KeyCode::PageUp => {
                panel.scroll = (panel.scroll + 10).min(ui::CONSOLE_SCROLLBACK_LINES);
            }
            KeyCode::PageDown => panel.scroll = panel.scroll.saturating_sub(10),
            _ => return false,
        }
        self.request_redraw();
        true
    }

    /// Open the debugger window (pausing the machine), or close it again
    /// if it is already open (the host shortcut toggle).
    fn toggle_debugger(&mut self) {
        if self.debugger_panel.is_some() {
            self.close_tool_panel(ToolPanelKind::Debugger);
        } else {
            self.ui.menu_open = false;
            self.open_debugger();
            self.request_redraw();
        }
    }

    fn open_debugger(&mut self) {
        if self.debugger_panel.is_none() {
            // The debugger shortcut can arrive while the mouse is captured;
            // release it so the window's controls are reachable, and note
            // it so closing the panel gives the capture back.
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_debugger = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            let mut panel = ui::DebuggerPanel::new();
            // Start the memory view at the current program counter's
            // neighbourhood; it is usually what you came to look at.
            panel.mem_addr = self.emu.machine.pc() & self.emu.machine.ui_addr_mask() & !0xF;
            self.debugger_panel = Some(panel);
            self.emu.machine.ui_set_pc_history_enabled(true);
            // Arm reverse debugging so the < Step / < Run controls work. A
            // conservative interval keeps the per-snapshot serialize off the
            // critical path; captures only accrue while the machine advances
            // (Run / Step Frame inside the debugger), not while paused.
            if !self.emu.time_travel_enabled() {
                self.emu.enable_time_travel(
                    crate::debugger::RR_DEFAULT_BUDGET_MB,
                    DEBUGGER_REVERSE_INTERVAL_FRAMES,
                );
            }
        }
    }

    /// Open the console window (pausing the machine), or close it again
    /// if it is already open (the host shortcut toggle).
    fn toggle_console(&mut self) {
        if self.console_panel.is_some() {
            self.close_tool_panel(ToolPanelKind::Console);
        } else {
            self.ui.menu_open = false;
            self.open_console();
            self.request_redraw();
        }
    }

    fn open_console(&mut self) {
        if self.console_panel.is_none() {
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_console = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            let mut panel = ui::ConsolePanel::default();
            panel.push_output("Copperline debugger console. Type HELP for commands.");
            self.console_panel = Some(panel);
            self.emu.machine.ui_set_pc_history_enabled(true);
            // Arm reverse debugging so the reverse commands work, exactly
            // like opening the debugger window does.
            if !self.emu.time_travel_enabled() {
                self.emu.enable_time_travel(
                    crate::debugger::RR_DEFAULT_BUDGET_MB,
                    DEBUGGER_REVERSE_INTERVAL_FRAMES,
                );
            }
        }
    }

    fn tool_window_title(kind: ToolPanelKind) -> &'static str {
        match kind {
            ToolPanelKind::Debugger => "Copperline Debugger",
            ToolPanelKind::FrameAnalyzer => "Copperline Frame Analyzer",
            ToolPanelKind::Console => "Copperline Console",
        }
    }

    fn tool_panel_is_open(&self, kind: ToolPanelKind) -> bool {
        match kind {
            ToolPanelKind::Debugger => self.debugger_panel.is_some(),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_panel.is_some(),
            ToolPanelKind::Console => self.console_panel.is_some(),
        }
    }

    fn ensure_tool_windows_for_open_panels(&mut self, event_loop: &ActiveEventLoop) {
        for kind in ToolPanelKind::ALL {
            self.ensure_tool_window_for_kind(event_loop, kind, true);
        }
    }

    /// Frame-loop variant of ensure_tool_windows_for_open_panels: still
    /// creates/destroys windows to match the open panels every call, but
    /// paces the repaint of existing windows to TOOL_REDRAW_INTERVAL.
    fn refresh_tool_windows_paced(&mut self, event_loop: &ActiveEventLoop) {
        let due = self.last_tool_redraw.elapsed() >= TOOL_REDRAW_INTERVAL;
        if due {
            self.last_tool_redraw = Instant::now();
        }
        for kind in ToolPanelKind::ALL {
            self.ensure_tool_window_for_kind(event_loop, kind, due);
        }
    }

    fn ensure_tool_window_for_kind(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: ToolPanelKind,
        redraw: bool,
    ) {
        if !self.tool_panel_is_open(kind) {
            *self.tool_window_slot(kind) = None;
            return;
        }
        let title = Self::tool_window_title(kind);
        if let Some(tool) = self.tool_window(kind) {
            tool.window.set_title(title);
            if redraw && !tool.minimized {
                tool.window.request_redraw();
            }
            return;
        }

        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_window_icon(copperline_window_icon())
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(
                FB_WIDTH as f64 / 2.0,
                window_present_height() as f64 / 2.0,
            ));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                warn!("create tool window failed: {e}");
                return;
            }
        };
        let texture_scale = texture_scale_for_window(&window);
        // No vsync for tool windows: pixels.render() runs on the emulation
        // thread, which already paces against the emulator window's vsynced
        // present. A second vsync gate per frame can push the loop past its
        // frame budget and underrun the audio ring.
        //
        // A tool window shows panel text, not the emulated picture, so it
        // always takes the aspect-preserving fit -- integer scaling is a
        // setting for the machine's display.
        let pixels = match build_pixels_for_window(
            window.clone(),
            texture_scale,
            false,
            ScalingMode::Fill,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("tool window pixels init failed: {e}");
                return;
            }
        };
        info!(
            "tool window ready: {title} (texture {}x{})",
            texture_width(texture_scale),
            texture_height(texture_scale)
        );
        let inner = window.inner_size();
        *self.tool_window_slot(kind) = Some(ToolWindow {
            window,
            pixels,
            texture_scale,
            cursor_pos: None,
            minimized: false,
            surface_size: (inner.width.max(1), inner.height.max(1)),
        });
        self.request_redraw();
    }

    fn open_frame_analyzer(&mut self) {
        if self.frame_analyzer_panel.is_none() {
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_analyzer = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            self.emu.bus_mut().set_frame_analyzer_enabled(true);
            self.frame_analyzer_panel = Some(ui::FrameAnalyzerPanel::new());
        }
    }

    fn frame_analyzer_toggle_run(&mut self) {
        self.paused = !self.paused;
        self.paused_before_analyzer = self.paused;
        self.sync_live_audio_suspension();
        if !self.paused {
            self.emu.bus_mut().set_frame_analyzer_enabled(true);
        }
    }

    fn frame_analyzer_step_frame(&mut self) {
        self.emu.bus_mut().set_frame_analyzer_enabled(true);
        self.debugger_step_frame();
    }

    /// Switch the analyzer to `tab`. Entering the Memory tab rebuilds the
    /// window presets from the machine's memory map and arms the heat map
    /// over chip RAM if nothing has armed it yet, so the tab always opens
    /// on a map that is recording. Leaving the tab deliberately does not
    /// disarm it: flipping tabs would otherwise wipe the recording.
    fn frame_analyzer_set_tab(&mut self, tab: ui::AnalyzerTab) {
        if self.frame_analyzer_panel.is_none() {
            return;
        }
        let presets = (tab == ui::AnalyzerTab::Memory).then(|| {
            if self.emu.bus().heat_map().is_none() {
                let window = analyzer_default_heat_window(self.emu.bus());
                self.emu.bus_mut().set_heat_map(Some(window));
                self.heatmap_armed_by_panel = true;
            }
            analyzer_heat_presets(self.emu.bus())
        });
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.tab = tab;
            if let Some(presets) = presets {
                panel.heat_presets = presets;
            }
        }
        self.request_redraw();
    }

    /// Point the heat map at preset `index`. An index past the end does
    /// nothing: a click can land after the preset list was rebuilt from a
    /// machine with fewer banks.
    fn frame_analyzer_heat_preset(&mut self, index: u8) {
        let Some(window) = self
            .frame_analyzer_panel
            .as_ref()
            .and_then(|panel| panel.heat_presets.get(usize::from(index)))
            .map(|preset| (preset.base, preset.span))
        else {
            return;
        };
        // Ownership follows the arming, not the window. Re-windowing an
        // already-armed map is shared control (the last window request
        // wins, and the map goes cold either way), but a map the control
        // protocol armed is not the pane's to release on close just
        // because a preset was clicked; only a click that arms an unarmed
        // map makes the pane the owner.
        if self.emu.bus().heat_map().is_none() {
            self.heatmap_armed_by_panel = true;
        }
        self.emu.bus_mut().set_heat_map(Some(window));
        self.request_redraw();
    }

    /// Pin grid cell (`x`, `y`) so the readout under the map names what
    /// last touched it.
    fn frame_analyzer_heat_pick(&mut self, x: u8, y: u8) {
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.heat_selected = Some(usize::from(y) * heatmap::GRID + usize::from(x));
        }
        self.request_redraw();
    }

    /// Move the Memory tab's pinned cell by one grid cell, clamped to the
    /// grid's edges. With nothing pinned the arrow starts from the centre
    /// cell, so the keyboard can reach the map without a click first.
    fn frame_analyzer_move_heat_selection(&mut self, dx: i16, dy: i16) {
        let grid = heatmap::GRID as i32;
        let Some(panel) = self.frame_analyzer_panel.as_mut() else {
            return;
        };
        let centre = heatmap::CELLS / 2 + heatmap::GRID / 2;
        let cell = panel
            .heat_selected
            .unwrap_or(centre)
            .min(heatmap::CELLS - 1);
        let x = (cell % heatmap::GRID) as i32 + i32::from(dx);
        let y = (cell / heatmap::GRID) as i32 + i32::from(dy);
        panel.heat_selected =
            Some(y.clamp(0, grid - 1) as usize * heatmap::GRID + x.clamp(0, grid - 1) as usize);
        self.request_redraw();
    }

    fn frame_analyzer_toggle_underlay(&mut self) {
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_underlay = !panel.show_underlay;
            // Dropping the underlay also ends a scrub riding on it.
            if !panel.show_underlay {
                panel.show_scrub = false;
            }
            self.request_redraw();
        }
    }

    fn frame_analyzer_toggle_scrub(&mut self) {
        // Snap inputs for enabling scrub: the traced frame's last slot and
        // the frame-start DIW top-left corner in (vpos, cck) beam units
        // (same decode as build_frame_analyzer_view's DIW overlay).
        let trace_end = self.emu.bus().frame_bus_trace().map(|trace| {
            (
                trace.rows.saturating_sub(1).min(u16::MAX as usize) as u16,
                trace.cols.saturating_sub(1).min(u16::MAX as usize) as u16,
            )
        });
        let base = self.emu.bus().frame_render_base();
        let diw_top_left = (!(base.diwstrt == 0 && base.diwstop == 0)).then(|| {
            (
                base.diwhigh.v_start(base.diwstrt),
                base.diwhigh.h_start(base.diwstrt) / 2,
            )
        });
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_scrub = !panel.show_scrub;
            // Enabling scrub with the selection at or before the display
            // window's top-left corner would ghost the whole picture (the
            // CRT has drawn none of it at that beam position), which reads
            // as the underlay switching off. Snap the selection to the end
            // of the traced frame instead: the picture starts fully drawn
            // and scrubbing backward peels it away.
            if panel.show_scrub {
                if let (Some((end_v, end_h)), Some(diw)) = (trace_end, diw_top_left) {
                    if (panel.selected_vpos, panel.selected_hpos) <= diw {
                        panel.selected_vpos = end_v;
                        panel.selected_hpos = end_h;
                    }
                }
            }
            self.request_redraw();
        }
    }

    /// Re-render the picture underlay when the analyzer's traced frame has
    /// changed. The render is a pure function of a `RenderInput` snapshot
    /// (`render_from_input`), so unlike the `bitplane::render` wrapper it
    /// never feeds collision bits or timing stats back into the machine:
    /// inspecting a frame cannot perturb the emulation. The result stays in
    /// beam coordinates (no presentation post-processing), matching the DMA
    /// trace's grid.
    fn ensure_analyzer_underlay(&mut self) {
        let want = self
            .frame_analyzer_panel
            .as_ref()
            .is_some_and(|panel| panel.underlay_active());
        if !want {
            return;
        }
        let Some(frame) = self.emu.bus().frame_bus_trace().map(|trace| trace.frame) else {
            return;
        };
        if self.analyzer_underlay_frame == Some(frame) && self.analyzer_underlay_rows != 0 {
            return;
        }
        match &mut self.analyzer_underlay_input {
            Some(input) => input.refill_from_bus(self.emu.bus()),
            slot @ None => *slot = Some(bitplane::RenderInput::from_bus(self.emu.bus())),
        }
        let input = self
            .analyzer_underlay_input
            .as_ref()
            .expect("underlay render input just filled");
        let underlay_width = FB_WIDTH * input.canvas_scale();
        let fb = std::rc::Rc::make_mut(&mut self.analyzer_underlay_fb);
        fb.resize(MAX_CANVAS_PIXELS, 0);
        fb.fill(0);
        let _ = bitplane::render_from_input(input, fb.as_mut_slice());
        self.analyzer_underlay_width = underlay_width;
        self.analyzer_underlay_rows = self
            .emu
            .bus()
            .frame_geometry()
            .visible_lines
            .min(MAX_VISIBLE_LINES);
        self.analyzer_underlay_frame = Some(frame);
    }

    fn frame_analyzer_select(&mut self, x: u16, y: u16, scanline: bool) {
        let Some(trace) = self.emu.bus().frame_bus_trace() else {
            return;
        };
        let hpos = (usize::from(x) * trace.cols / 1024).min(trace.cols.saturating_sub(1));
        let vpos = if scanline {
            self.frame_analyzer_panel
                .as_ref()
                .map(|panel| panel.selected_vpos as usize)
                .unwrap_or(trace.visible_start_vpos as usize)
        } else {
            (usize::from(y) * trace.rows / 1024).min(trace.rows.saturating_sub(1))
        };
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.selected_hpos = hpos.min(u16::MAX as usize) as u16;
            panel.selected_vpos = vpos.min(u16::MAX as usize) as u16;
        }
    }

    fn frame_analyzer_move_selection(&mut self, dhpos: i16, dvpos: i16) {
        let Some((max_hpos, max_vpos)) = self.emu.bus().frame_bus_trace().map(|trace| {
            (
                trace.cols.saturating_sub(1).min(u16::MAX as usize) as i32,
                trace.rows.saturating_sub(1).min(u16::MAX as usize) as i32,
            )
        }) else {
            return;
        };
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            let hpos =
                (i32::from(panel.selected_hpos) + i32::from(dhpos)).clamp(0, max_hpos) as u16;
            let vpos =
                (i32::from(panel.selected_vpos) + i32::from(dvpos)).clamp(0, max_vpos) as u16;
            if panel.selected_hpos != hpos || panel.selected_vpos != vpos {
                panel.selected_hpos = hpos;
                panel.selected_vpos = vpos;
                self.request_redraw();
            }
        }
    }

    /// Open or close the pop-up menu, from the hamburger button or the
    /// keyboard.
    ///
    /// Opening hands the mouse back: the menu is worked with the host
    /// pointer, and a captured one is inside the machine where it cannot
    /// reach it. Closing asks for the grab again, which auto mode takes and
    /// the other modes decline, exactly as closing a panel does.
    fn toggle_menu(&mut self) {
        self.ui.menu_open = !self.ui.menu_open;
        // Each open starts at the top of the list; a position left over from
        // the last time would be a small mystery.
        self.ui.menu_nav.reset();
        if self.ui.menu_open {
            self.set_mouse_captured(false);
            let fullscreen = self
                .render
                .as_ref()
                .is_some_and(|r| r.window.fullscreen().is_some());
            self.ui.menu_rows = self.build_menu(fullscreen);
        } else {
            self.ui.menu_rows = Vec::new();
            self.apply_auto_mouse_capture();
        }
        self.request_redraw();
    }

    /// Close the open main-window overlay panel.
    fn close_panel(&mut self) {
        self.analyzer_dragging = false;
        self.ui.panel = None;
        self.request_redraw();
    }

    /// Open the machine-configuration screen, seeded from the running (or
    /// last-applied) machine so it reflects the current settings.
    pub fn open_launcher(&mut self) {
        self.ui.menu_open = false;
        let mut state = LauncherState::from_raw(&self.machine_config);
        // A machine set up with a real disk names it on the Storage page, and
        // naming it properly means knowing what is on it. Looking is otherwise
        // put off until the Host Disk page opens, so a launcher that never
        // goes there never touches the host's disks -- but a configuration
        // already naming one has spent that cost, and without this the same
        // disk reads by its bare device name here and by its volume there.
        if !state.setup.host_disks_attached().is_empty() {
            state.setup.refresh_host_disks();
        }
        self.ui.panel = Some(Panel::Launcher(Box::new(state)));
        self.request_redraw();
    }

    fn launcher_state(&self) -> Option<&LauncherState> {
        match self.ui.panel.as_ref() {
            Some(Panel::Launcher(state)) => Some(state.as_ref()),
            _ => None,
        }
    }

    fn launcher_state_mut(&mut self) -> Option<&mut LauncherState> {
        match self.ui.panel.as_mut() {
            Some(Panel::Launcher(state)) => Some(state.as_mut()),
            _ => None,
        }
    }

    fn set_launcher_status(&mut self, status: StatusMessage) {
        if let Some(state) = self.launcher_state_mut() {
            state.status = Some(status);
        }
    }

    /// Open a native file dialog for a configuration-screen path field, seeded
    /// at the field's current directory, and store the picked path.
    fn launcher_browse(&mut self, field: LauncherField) {
        // Host FS mounts and the WHDLoad staging directories are a host
        // directory, not an image file, so they get a folder picker seeded
        // at the current directory itself.
        if field.is_filesys_dir_field() || field.is_whdload_dir_field() {
            self.launcher_browse_folder(field);
            return;
        }
        // The printer capture is a file we create/overwrite, not an existing
        // image to open, so it gets a save dialog seeded with a default name.
        if field == LauncherField::ParallelOutput {
            self.launcher_browse_save(field, "printer.txt");
            return;
        }
        let start_dir = self
            .launcher_state()
            .and_then(|s| s.setup.path(field))
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        self.suspend_live_audio_for_host_io();
        let mut dialog = rfd::FileDialog::new().set_title("Select file");
        dialog = match field {
            LauncherField::Rom
            | LauncherField::ExtendedRom
            | LauncherField::ScsiRom
            | LauncherField::ScsiRomOdd
            | LauncherField::Mt32ControlRom
            | LauncherField::Mt32PcmRom => {
                // Both cases spelled out: ROM dumps are as often shouted as
                // not, and some hosts match the filter case-sensitively.
                dialog.add_filter("ROM images", &["rom", "ROM", "bin", "BIN"])
            }
            LauncherField::Df0Image
            | LauncherField::Df1Image
            | LauncherField::Df2Image
            | LauncherField::Df3Image => {
                dialog.add_filter("Floppy images", crate::floppy::IMAGE_EXTENSIONS)
            }
            // Only formats CdImage::load takes: a cue sheet, a bare ISO,
            // or a CHD (a raw .bin is a cue sheet's payload, not loadable
            // alone).
            LauncherField::CdImage => dialog.add_filter("CD images", &["cue", "iso", "chd"]),
            // A WHDLoad package as distributed (.lha), or a bare .slave
            // picked inside an already-extracted one (stored as its
            // directory, which is what the stager mounts). Both cases
            // spelled out like the ROM filters.
            LauncherField::WhdloadGame => {
                dialog.add_filter("WHDLoad packages", &["lha", "LHA", "slave", "Slave"])
            }
            LauncherField::Cd32Nvram => dialog.add_filter("NVRAM images", &["bin", "nv", "sav"]),
            // SCSI units take hard disks or CD images (a cue/iso/chd
            // attaches a CD-ROM drive at that ID).
            LauncherField::ScsiUnit0
            | LauncherField::ScsiUnit1
            | LauncherField::ScsiUnit2
            | LauncherField::ScsiUnit3
            | LauncherField::ScsiUnit4
            | LauncherField::ScsiUnit5
            | LauncherField::ScsiUnit6 => dialog
                .add_filter("Hard disk images", &["hdf", "hdz", "img", "bin"])
                .add_filter("CD images", &["cue", "iso", "chd"]),
            _ => dialog.add_filter("Hard disk images", &["hdf", "hdz", "img", "bin"]),
        };
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let picked = dialog.pick_file();
        if let Some(mut path) = picked {
            if field == LauncherField::WhdloadGame {
                path = whdload_game_config_path(path);
            }
            if let Some(state) = self.launcher_state_mut() {
                // A pending volume-name edit (on this or another drive row)
                // would otherwise be left visually focused after the dialog.
                state.edit_cancel();
                state.setup.set_path(field, path);
                state.status = None;
            }
        }
        self.finish_host_io_pause();
    }

    /// Folder picker for a Host FS mount's directory field.
    fn launcher_browse_folder(&mut self, field: LauncherField) {
        let start_dir = self
            .launcher_state()
            .and_then(|s| s.setup.path(field))
            .map(|p| p.to_path_buf());
        self.suspend_live_audio_for_host_io();
        let mut dialog = rfd::FileDialog::new().set_title("Select host directory");
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let picked = dialog.pick_folder();
        if let Some(path) = picked {
            if let Some(state) = self.launcher_state_mut() {
                state.edit_cancel();
                state.setup.set_path(field, path);
                state.status = None;
            }
        }
        self.finish_host_io_pause();
    }

    /// Save-file picker for a path field that names a host file to create or
    /// overwrite (the printer capture), seeded with `default_name` so the dialog
    /// suggests a filename without the user typing one. An existing file can
    /// still be chosen.
    fn launcher_browse_save(&mut self, field: LauncherField, default_name: &str) {
        let current = self
            .launcher_state()
            .and_then(|s| s.setup.path(field))
            .map(|p| p.to_path_buf());
        self.suspend_live_audio_for_host_io();
        let mut dialog = rfd::FileDialog::new().set_title("Choose output file");
        // Seed with the existing path's directory and name, else the default.
        match current.as_ref().and_then(|p| p.parent()) {
            Some(dir) if !dir.as_os_str().is_empty() => dialog = dialog.set_directory(dir),
            _ => {}
        }
        let name = current
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or(default_name);
        dialog = dialog.set_file_name(name);
        let picked = dialog.save_file();
        if let Some(path) = picked {
            if let Some(state) = self.launcher_state_mut() {
                state.edit_cancel();
                state.setup.set_path(field, path);
                state.status = None;
            }
        }
        self.finish_host_io_pause();
    }

    /// Make a fresh disk image from what the Create Image page is showing.
    ///
    /// The file is chosen first: the save dialog is where a user cancels,
    /// and nothing is written until they have named somewhere to write it.
    fn launcher_create_image(&mut self, field: crate::video::launcher::LauncherField) {
        use crate::video::launcher::LauncherField as F;
        // Whatever is half-typed counts: pressing a button is as much an
        // end to typing as Enter is, and a size typed but not committed
        // would otherwise be silently thrown away.
        if let Some(state) = self.launcher_state_mut() {
            state.edit_commit();
            if state.editing().is_some() {
                // The commit was refused -- an invalid name -- and the
                // status line says so. Nothing further can be trusted.
                return;
            }
        }
        // The geometry editor's two buttons write no file: Save takes the
        // figures as they stand and returns, Auto fills them in from the
        // size so a hand-set geometry can be started over.
        if matches!(field, F::NewGeomSave | F::NewGeomAuto) {
            if let Some(state) = self.launcher_state_mut() {
                if field == F::NewGeomAuto {
                    state.workshop.geometry_from_size();
                } else {
                    state.tab = crate::video::launcher::LauncherTab::CreateHard;
                }
                state.status = None;
            }
            return;
        }
        if self.image_job.is_some() {
            // The status line already says which one, and starting a second
            // would leave the first writing with nothing watching for it.
            return;
        }
        let floppy = field == F::NewFloppyCreate;
        let Some(state) = self.launcher_state() else {
            return;
        };
        let suggested = state.workshop.suggested_name(floppy);
        let spec = if floppy {
            ImageToMake::Floppy(state.workshop.floppy_spec())
        } else {
            ImageToMake::Hard(state.workshop.hard_spec())
        };

        self.suspend_live_audio_for_host_io();
        let (kind, ext) = if floppy {
            ("Amiga floppy image", vec!["adf"])
        } else {
            // The same bytes either way: .hdf is what emulators look for,
            // .img what a card writer expects, so both are offered.
            ("Amiga hard disk image", vec!["hdf", "img"])
        };
        let picked = rfd::FileDialog::new()
            .set_title("Create disk image")
            .add_filter(kind, &ext)
            .set_file_name(&suggested)
            .save_file();
        self.finish_host_io_pause();

        let Some(path) = picked else { return };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let claimed = spec.bytes_on_disk();
        let size = crate::config::format_size(claimed as usize);

        // Only a fully-written image needs its room now; a sparse one takes
        // what it uses, and making a large one on a small drive is a
        // perfectly ordinary thing to do.
        if !spec.is_sparse() {
            if let Some(free) = free_space_for_new_file(&path) {
                if claimed > free {
                    let where_to = path.parent().unwrap_or(&path).display().to_string();
                    warn!(
                        "create disk image {}: needs {claimed} bytes, {free} free",
                        path.display()
                    );
                    self.set_launcher_status(crate::video::launcher::StatusMessage::err(format!(
                        "Not enough free space to create {name} ({size}) -- {} free on {where_to}",
                        crate::config::format_size(free as usize)
                    )));
                    return;
                }
            }
        }

        // Writing gigabytes takes as long as it takes, and doing it on this
        // thread would stop the loop servicing events -- which the host
        // reads as a hung application. It goes to a worker, the panel says
        // what it is waiting for, and `poll_image_job` collects the result.
        let (tx, rx) = std::sync::mpsc::channel();
        let job_path = path.clone();
        std::thread::spawn(move || {
            let made = match &spec {
                ImageToMake::Floppy(spec) => crate::diskimage::create_floppy(&job_path, spec),
                ImageToMake::Hard(spec) => crate::diskimage::create_hard(&job_path, spec),
            };
            let _ = tx.send(made);
        });
        self.image_job = Some(ImageJob {
            rx,
            path,
            name: name.clone(),
        });
        self.set_launcher_status(crate::video::launcher::StatusMessage::busy(format!(
            "Creating {name} ({size})..."
        )));
    }

    /// Collect a finished image write and report it, or leave the job
    /// running. Called once per pass while one is outstanding.
    fn poll_image_job(&mut self) {
        let Some(job) = &self.image_job else { return };
        let made = match job.rx.try_recv() {
            Ok(made) => made,
            // Still writing. A disconnected channel means the worker died
            // without sending, which nothing here does, but treating it as
            // "still going" would hang the status line forever.
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(std::io::Error::other(
                "the image writer stopped without saying why",
            )),
        };
        let job = self.image_job.take().expect("checked above");
        let status = match made {
            Ok(made) => {
                info!(
                    "created {} ({} bytes){}",
                    job.path.display(),
                    made.bytes,
                    match made.geometry {
                        Some(g) => format!(", {}/{}/{}", g.cylinders, g.surfaces, g.sectors),
                        None => String::new(),
                    }
                );
                crate::video::launcher::StatusMessage::ok(format!(
                    "Created {} ({})",
                    job.name,
                    crate::config::format_size(made.bytes as usize)
                ))
            }
            Err(e) => {
                warn!("create disk image {}: {e}", job.path.display());
                crate::video::launcher::StatusMessage::err(format!("Could not create: {e}"))
            }
        };
        self.set_launcher_status(status);
        self.request_redraw();
    }

    fn launcher_add_zorro(&mut self) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Add Zorro board metadata")
            .add_filter("Board metadata", &["toml"])
            .pick_file();
        if let Some(path) = picked {
            if let Some(state) = self.launcher_state_mut() {
                state.setup.add_zorro(path);
                state.status = None;
            }
        }
        self.finish_host_io_pause();
    }

    /// Pick a file for a plugin board's file-typed config option.
    fn launcher_board_browse(&mut self, board: usize, opt: usize) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Choose plugin file")
            .pick_file();
        if let Some(path) = picked {
            if let Some(state) = self.launcher_state_mut() {
                state.edit_cancel();
                state
                    .setup
                    .zorro_option_set(board, opt, path.to_string_lossy().into_owned());
                state.status = None;
            }
        }
        self.finish_host_io_pause();
    }

    fn launcher_load(&mut self) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load configuration")
            .add_filter("Copperline config", &["toml"])
            .pick_file();
        if let Some(path) = picked {
            match MachineSetup::load_from(&path) {
                Ok(setup) => {
                    if let Some(state) = self.launcher_state_mut() {
                        state.setup = setup;
                        // Re-read host device lists so the loaded setup's pickers
                        // are populated, not stuck on "Default"/"None".
                        state.setup.refresh_host_devices();
                        state.status = Some(StatusMessage::ok(format!(
                            "Loaded {}",
                            display_file_name(&path)
                        )));
                    }
                }
                Err(e) => {
                    warn!("config load failed ({}): {e:#}", path.display());
                    self.set_launcher_status(StatusMessage::err(format!(
                        "Load failed: {}",
                        short_status_error(&e)
                    )));
                }
            }
        }
        self.finish_host_io_pause();
    }

    fn launcher_save(&mut self) {
        // Capture a name/option typed but not yet committed with Enter.
        if let Some(state) = self.launcher_state_mut() {
            state.edit_commit();
        }
        let toml = match self.launcher_state().map(|s| s.setup.to_toml()) {
            Some(Ok(text)) => text,
            Some(Err(e)) => {
                self.set_launcher_status(StatusMessage::err(format!(
                    "Save failed: {}",
                    short_status_error(&e)
                )));
                return;
            }
            None => return,
        };
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Save configuration")
            .add_filter("Copperline config", &["toml"])
            .set_file_name("machine.toml")
            .save_file();
        if let Some(path) = picked {
            match std::fs::write(&path, toml) {
                Ok(()) => self.set_launcher_status(StatusMessage::ok(format!(
                    "Saved {}",
                    display_file_name(&path)
                ))),
                Err(e) => {
                    warn!("config save failed ({}): {e}", path.display());
                    self.set_launcher_status(StatusMessage::err("Save failed (see log)"));
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// Build and start the configured machine (the Run button). Validation,
    /// WHDLoad staging, AROS resolution, audio-device and
    /// machine-construction errors all stay in the panel as a status line;
    /// only success swaps the live machine.
    fn launcher_run(&mut self) {
        // Capture a name/option typed but not yet committed with Enter.
        if let Some(state) = self.launcher_state_mut() {
            state.edit_commit();
        }
        let raw = match self.launcher_state().map(|s| s.setup.to_raw()) {
            Some(raw) => raw,
            None => return,
        };
        if let Err(e) = self.stage_and_run(raw) {
            // The status line is one shortened sentence; the log keeps the
            // whole chain (which names the underlying cause).
            warn!("run failed: {e:#}");
            self.set_launcher_status(StatusMessage::err(short_status_error(&e)));
        }
    }

    /// Stage any configured WHDLoad game, validate the configuration, and
    /// boot it. `raw` is the user's own configuration and is what the
    /// session remembers; the WHDLoad derivation (machine profile, fast
    /// RAM, ROM, the two staged mounts -- whdload::apply_to_raw) happens on
    /// a copy, so it is rebuilt fresh on every boot and a later Save writes
    /// the setup, not the derivation.
    fn stage_and_run(&mut self, raw: RawConfig) -> Result<()> {
        let mut staged = raw.clone();
        let (game, opts) = crate::whdload::game_and_options(&staged);
        if let Some(game) = game {
            let prepared = crate::whdload::prepare(&game, &opts)?;
            crate::whdload::apply_to_raw(&mut staged, &prepared);
            info!(
                "whdload: booting {} ({}) from {}, saves persist in {}",
                prepared.slave_rel.display(),
                prepared.slave.name.as_deref().unwrap_or("unnamed slave"),
                game.display(),
                prepared.game_dir.display()
            );
        }
        // The same validation Run has always used: the raw view through the
        // config pipeline (MachineSetup::build_config is exactly this over
        // its own to_raw()).
        let mut cfg = Config::try_from(staged)?;
        crate::config::resolve_bundled_rom(&mut cfg)?;
        self.build_and_run_machine(&cfg, raw)
    }

    /// Build a machine for `cfg` and swap it in (shared by the configuration
    /// screen's Run and the dropped-WHDLoad-game reboot): session audio
    /// sink, real-drive handover, then [`Self::run_machine`]. On failure the
    /// running machine stays as it was; the caller reports the error in its
    /// own place (panel status line or OSD).
    fn build_and_run_machine(&mut self, cfg: &Config, raw: RawConfig) -> Result<()> {
        // Remember the session's realtime request so later live sink rebuilds
        // (device switch, disconnect recovery) reuse it.
        self.realtime_priority = cfg.emulation.realtime_priority;
        let realtime = crate::priority::requested(self.realtime_priority);
        // The configured Audio output drives the session selection (default
        // device, a named device, or Disabled).
        self.audio_output = crate::audio::AudioOutput::from_config(
            cfg.audio.output_enabled,
            cfg.audio.output_device.as_deref(),
        );
        let audio: Box<dyn AudioSink> =
            crate::audio::open_output_sink(realtime, &self.audio_output)
                .context("Audio init failed")?;
        // Let go of any real floppy drive the outgoing machine holds before
        // building the new one. The interface can only be open once, and the
        // machine being replaced is not dropped until `run_machine` swaps it
        // in -- so without this the new machine tries to open a device its
        // predecessor still owns, and is told it is in use.
        #[cfg(feature = "fluxbridge")]
        self.emu.bus_mut().floppy.release_bridges();
        // This path boots a fresh machine, never a save state, so a real
        // ROM is required here.
        match crate::emulator::build_machine(cfg, audio, true, false) {
            Ok(emu) => {
                self.run_machine(emu, cfg, raw);
                Ok(())
            }
            Err(e) => {
                // The machine that is staying put had its drives taken away
                // above; give them back rather than leaving it with empty bays
                // because a different configuration failed to build.
                #[cfg(feature = "fluxbridge")]
                self.attach_configured_bridges();
                Err(e)
            }
        }
    }

    /// Replace the live machine with a freshly built one (configuration screen
    /// Run), refreshing the host-side presentation/runtime state to match and
    /// powering it on. The previous (placeholder or running) machine, and its
    /// audio sink, are dropped here.
    fn run_machine(&mut self, emu: Emulator, cfg: &Config, raw: RawConfig) {
        // Anything the on-screen keyboard is holding belongs to the
        // machine being replaced, and has to be handed back while that
        // machine is still here: the new one would otherwise come up with
        // caps drawn latched that its keyboard MCU never heard of.
        self.release_keyboard_panel_holds();
        self.emu = emu;
        // Any heat map the analyzer pane armed went with the machine that
        // was just replaced, so the pane owns nothing on this one until it
        // arms a map here.
        self.heatmap_armed_by_panel = false;
        // The real machine may bridge serial to MIDI; the config-screen
        // placeholder never does, so recompute now that the machine is live.
        #[cfg(feature = "midi")]
        {
            self.serial_is_midi = self.emu.bus_mut().midi_serial_mut().is_some();
        }
        self.machine_config = raw;
        // Re-derive the sampler from the launcher's parallel config and attach it
        // to the fresh machine (the printer attaches inside build_machine, since
        // its byte sink is Send).
        self.sampler = crate::sampler::SamplerRequest::from_config(&cfg.parallel);
        self.attach_session_sampler();
        self.disk_playlists = cfg.floppy_playlists.clone();
        self.disk_write_protected = std::array::from_fn(|i| {
            cfg.floppy.drives[i]
                .as_ref()
                .map(|d| d.write_protected)
                .unwrap_or(true)
        });
        self.disk_playlist_index = [0; 4];
        self.overscan = crate::config::resolve_overscan(cfg.overscan);
        self.apply_pixel_aspect(crate::config::resolve_pixel_aspect(cfg.pixel_aspect));
        self.apply_display_scaling(cfg.scaling);
        // Apply the configured start-up window state; the runtime toggles
        // (Cmd+F, Cmd+Shift+F) take over from here. Reuse the toggles so the
        // surface/window resize stays in one place.
        let is_fullscreen = self
            .render
            .as_ref()
            .map(|r| r.window.fullscreen().is_some());
        if is_fullscreen == Some(!cfg.full_screen) {
            self.toggle_fullscreen();
        }
        if super::status_bar_hidden() == cfg.status_bar {
            self.toggle_status_bar();
        }
        self.warp_speed = cfg.emulation.warp_speed;
        // Reset the host joystick source to the new machine's configured
        // start-up mode (a previous live Cmd+J toggle does not carry over).
        self.joystick_input_mode = cfg.joystick_input_mode;
        self.set_mouse_sensitivity(cfg.mouse_sensitivity);
        self.mouse_capture = cfg.mouse_capture;
        self.autofire_hz = cfg.autofire_hz;
        // Rewind history belongs to the machine that recorded it, so the new
        // machine starts a fresh ring under its own config (or none at all).
        self.rewind_budget_mb = cfg.emulation.rewind_budget_mb;
        self.rewind_interval_frames = cfg.emulation.rewind_interval_frames;
        self.rewind_armed = cfg.emulation.rewind;
        if self.rewind_armed {
            self.arm_rewind_ring();
        } else if !self.debugger_wants_time_travel() {
            self.emu.disable_time_travel();
        }
        self.keyboard_joy_held = [keymap::HeldKeys::default(); keymap::MAPPING_COUNT];
        self.about_machine_lines = crate::config::about_machine_lines(cfg);
        // The threaded path picks the new settings up from the next render
        // job; the recreated deinterlacer covers the synchronous fallback.
        self.deinterlace = crate::config::resolve_deinterlace(cfg.deinterlace);
        self.phosphor = crate::config::resolve_phosphor(cfg.phosphor);
        self.deinterlacer = Deinterlacer::with_settings(self.deinterlace, self.phosphor);
        let shader = crate::config::resolve_shader(cfg.shader.clone());
        self.custom_shader_path = match &shader {
            crate::config::ShaderMode::Custom(path) => Some(path.clone()),
            _ => None,
        };
        self.crt_shader_kind = shader.kind();
        // The device already exists here, so a user shader is compiled now
        // rather than in `resumed`; a bad one falls back to no shader. With
        // none configured, the previous machine's pipeline is dropped rather
        // than left loaded for the CRT Shader item to cycle back to.
        let mut shader_error = None;
        if self.crt_shader_kind == crate::config::ShaderKind::Custom {
            if let Err(msg) = self.reload_custom_shader() {
                shader_error = Some(msg);
                self.crt_shader_kind = crate::config::ShaderKind::None;
            }
        } else if let Some(r) = self.render.as_mut() {
            r.crt_shader.clear_custom();
        }
        self.shader_strength = crate::config::resolve_shader_strength(cfg.shader_strength);
        self.bezel = crate::config::resolve_bezel(cfg.bezel);
        self.bezel_last = last_bezel_style(self.bezel);
        self.perf_overlay = crate::config::resolve_perf_overlay(cfg.perf_overlay);
        self.perf = PerfOverlay::default();
        self.set_tint(crate::config::resolve_tint(cfg.tint));
        crate::video::set_menu_scale(cfg.menu_scale);
        #[cfg(feature = "mt32")]
        {
            crate::video::set_mt32_lcd(cfg.serial.mt32_lcd);
            // The panel belongs to a module that is both fitted and asked
            // for: a machine built without one would otherwise keep the
            // last one's strip, dead and taking up room.
            let fitted = self
                .emu
                .bus_mut()
                .midi_serial_mut()
                .is_some_and(|sink| sink.mt32_selected());
            self.set_mt32_panel_shown(fitted && cfg.serial.mt32_panel);
            self.mt32_panel.reset();
            self.tell_panel_the_rom_version();
            self.report_mt32_fault();
        }
        self.ui.menu_open = false;
        self.ui.panel = None;
        self.powered_on = true;
        self.cpu_halted = false;
        self.paused = false;
        self.reset_render_pipeline();
        // The last overlay set here is the one that gets drawn, so a shader
        // that failed to load has to travel in this message rather than in
        // one of its own.
        self.show_osd(match shader_error {
            Some(msg) => format!("Machine started (CRT shader: off, custom failed: {msg})"),
            None => "Machine started".to_string(),
        });
        self.request_redraw();
    }

    fn close_tool_panel(&mut self, kind: ToolPanelKind) {
        match kind {
            ToolPanelKind::Debugger => {
                if self.debugger_panel.is_some() {
                    self.paused = self.paused_before_debugger;
                    self.last_debug_stop = None;
                    self.sync_live_audio_suspension();
                }
                self.debugger_panel = None;
                self.debugger_tool_window = None;
            }
            ToolPanelKind::Console => {
                if self.console_panel.is_some() {
                    self.paused = self.paused_before_console;
                    self.sync_live_audio_suspension();
                }
                self.console_panel = None;
                self.console_tool_window = None;
            }
            ToolPanelKind::FrameAnalyzer => {
                if self.frame_analyzer_panel.is_some() {
                    self.paused = self.paused_before_analyzer;
                    self.emu.bus_mut().set_frame_analyzer_enabled(false);
                    self.sync_live_audio_suspension();
                }
                self.analyzer_dragging = false;
                self.frame_analyzer_panel = None;
                self.frame_analyzer_tool_window = None;
                // Release the heat map only if this pane armed it. A map
                // armed over the control protocol belongs to that session
                // and keeps recording after the pane closes.
                if self.heatmap_armed_by_panel {
                    self.emu.bus_mut().set_heat_map(None);
                    self.heatmap_armed_by_panel = false;
                }
                // Release the underlay buffers (frame render + up-to-2MiB
                // chip RAM snapshot) while the analyzer is closed.
                self.analyzer_underlay_fb = std::rc::Rc::new(Vec::new());
                self.analyzer_underlay_rows = 0;
                self.analyzer_underlay_frame = None;
                self.analyzer_underlay_input = None;
            }
        }
        if self.debugger_panel.is_none() && self.console_panel.is_none() {
            self.emu.machine.ui_set_pc_history_enabled(false);
        }
        // Hand the mouse back if this was the last panel holding it. With
        // two panels open, closing one leaves the other still wanting the
        // cursor, and the check inside declines until that one goes too.
        self.restore_mouse_capture_after_ui();
        // In auto mode the grab is owed to the machine regardless of
        // whether this panel is the one that borrowed it.
        self.apply_auto_mouse_capture();
        self.request_redraw();
    }

    /// Persist a completed calibration session and close the panel.
    fn save_calibration(&mut self) {
        let Some(Panel::Calibration(session)) = self.ui.panel.as_ref() else {
            return;
        };
        match self.gamepad.save_calibration(session) {
            Ok(()) => {
                self.ui.panel = None;
                self.show_osd("Gamepad calibration saved");
            }
            Err(e) => {
                warn!("gamepad calibration save failed: {e:#}");
                self.show_osd("Calibration save failed (see log)");
            }
        }
    }

    /// Toggle borderless fullscreen on the main window. Borderless (not
    /// exclusive) keeps the compositor path and the existing Resized-driven
    /// surface rebuild; the presentation already letterboxes any window
    /// shape, so no display-mode change is wanted.
    fn toggle_fullscreen(&mut self) {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        if window.fullscreen().is_some() {
            window.set_fullscreen(None);
            info!("fullscreen off");
            self.show_osd("Fullscreen off");
        } else {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            info!("fullscreen on");
            self.show_osd(format!(
                "Fullscreen on ({HOST_SHORTCUT_MODIFIER_LABEL}+F restores)"
            ));
            // Fullscreen leaves no desktop to reach for, so auto mode takes
            // the grab here as well as on focus: entering fullscreen does
            // not itself change the focus, so no Focused event follows.
            self.apply_auto_mouse_capture();
        }
    }

    /// Toggle warp speed: emulation runs unpaced (as fast as the host
    /// allows) until switched back, when pacing re-anchors to "now".
    fn toggle_warp(&mut self) {
        let warp = self.emu.paced();
        self.emu.set_paced(!warp);
        if warp {
            let limit = self.warp_speed.label();
            info!("warp speed on (emulation unpaced, limit {limit})");
            self.show_osd(format!("Warp speed on ({limit})"));
        } else {
            info!("warp speed off (real-time pacing)");
            self.show_osd("Warp speed off");
        }
    }

    /// How many emulated frames to retire before presenting the next frame, and
    /// an optional wall-clock budget that bounds that burst. Warp's output frame
    /// skip applies only while warp is engaged and not doing headless capture;
    /// real-time pacing and headless capture both run one frame per presented
    /// frame. The `Max` level returns a budget so the burst presents at vsync
    /// rather than spinning to its frame cap.
    fn warp_burst_plan(&self, headless_capture: bool) -> (usize, Option<std::time::Duration>) {
        if self.emu.paced() || headless_capture {
            return (1, None);
        }
        (
            self.warp_speed.frame_cap(),
            self.warp_speed
                .time_budget_ms()
                .map(std::time::Duration::from_millis),
        )
    }

    /// Cycle the warp/turbo output frame-skip level (2x -> 4x -> 8x -> 16x ->
    /// Max). Takes effect immediately when warp is engaged; otherwise it just
    /// arms the level the next warp toggle will use.
    fn cycle_warp_speed(&mut self) {
        self.warp_speed = self.warp_speed.next();
        let limit = self.warp_speed.label();
        info!("warp limit: {limit}");
        let active = !self.emu.paced();
        if active {
            self.show_osd(format!("Warp limit: {limit}"));
        } else {
            self.show_osd(format!("Warp limit: {limit} (warp off)"));
        }
        self.request_redraw();
    }

    /// Interactive shortcut / menu state save: write the whole
    /// emulated machine to an auto-named file in the working directory and
    /// flash the filename on screen. Runs between frames by construction
    /// (the event loop only dispatches input/menu events outside step_frame).
    fn save_state_interactive(&mut self) {
        self.suspend_live_audio_for_host_io();
        let path = crate::savestate::auto_filename();
        match self.emu.save_state(&path) {
            Ok(()) => {
                info!("save state written: {}", path.display());
                self.show_osd(format!("Saved {}", display_file_name(&path)));
            }
            Err(e) => {
                warn!("save state failed ({}): {e:#}", path.display());
                self.show_osd("State save failed (see log)");
            }
        }
        self.finish_host_io_pause();
    }

    /// Save to numbered slot `slot` (1-based). Overwrites silently: a quick
    /// save is expected to be instant, and the previous contents of the slot
    /// are what the user is replacing.
    fn quick_save_state(&mut self, slot: usize) {
        self.quick_save_state_at(slot, crate::savestate::slot_path(slot));
    }

    /// Test/frontend seam for slot roots that must not touch the host's real
    /// per-user state directory.
    fn quick_save_state_at(&mut self, slot: usize, path: Option<PathBuf>) {
        let Some(path) = path else {
            self.show_osd("No per-user directory for save slots");
            return;
        };
        self.suspend_live_audio_for_host_io();
        let result = crate::paths::ensure_parent(&path)
            .map_err(anyhow::Error::from)
            .and_then(|()| self.emu.save_state(&path));
        match result {
            Ok(()) => {
                info!("save state written to slot {slot}: {}", path.display());
                self.show_osd(format!("Slot {slot} saved"));
            }
            Err(e) => {
                warn!("slot {slot} save failed ({}): {e:#}", path.display());
                self.show_osd(format!("Slot {slot} save failed (see log)"));
            }
        }
        self.finish_host_io_pause();
    }

    /// Restore numbered slot `slot` (1-based). An empty slot is reported
    /// rather than treated as an error: the menu and the hotkeys cover all
    /// ten, and most of them are usually unused.
    fn quick_load_state(&mut self, slot: usize, event_loop: Option<&ActiveEventLoop>) {
        self.quick_load_state_at(slot, crate::savestate::slot_path(slot), event_loop);
    }

    /// Test/frontend seam paired with [`Self::quick_save_state_at`].
    fn quick_load_state_at(
        &mut self,
        slot: usize,
        path: Option<PathBuf>,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        let Some(path) = path else {
            self.show_osd("No per-user directory for save slots");
            return;
        };
        if !path.exists() {
            self.show_osd(format!("Slot {slot} is empty"));
            return;
        }
        self.suspend_live_audio_for_host_io();
        if self.load_state_from_path(&path) {
            self.show_osd(format!("Slot {slot} loaded"));
            if let Some(event_loop) = event_loop {
                event_loop.set_control_flow(ControlFlow::Poll);
            }
        }
        self.finish_host_io_pause();
    }

    /// Pick a save-state file and restore it (shortcut / menu). On
    /// success the machine continues from the state's timeline: power is
    /// forced on, any CPU halt is cleared, and the display re-renders from
    /// the restored Bus. On failure the running machine is untouched.
    fn load_state_from_dialog(&mut self, event_loop: Option<&ActiveEventLoop>) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load save state")
            .add_filter("Copperline save states", &["clstate"])
            .pick_file();

        // Re-baseline pacing after the modal dialog, as for floppies; a
        // successful load re-anchors again to the restored timeline inside
        // Emulator::load_state.
        if let Some(path) = picked {
            if self.load_state_from_path(&path) {
                if let Some(event_loop) = event_loop {
                    event_loop.set_control_flow(ControlFlow::Poll);
                }
            }
        }
        self.finish_host_io_pause();
    }

    fn load_state_from_path(&mut self, path: &std::path::Path) -> bool {
        // The restored machine carries its own keyboard state, so the
        // strip lets go of its holds against the machine that is still
        // here. Done before the attempt rather than after a success: a
        // release sent into the restored machine would be a key it never
        // saw pressed.
        self.release_keyboard_panel_holds();
        match self.emu.load_state(path) {
            Ok(outcome) => {
                info!(
                    "save state loaded: {} ({})",
                    path.display(),
                    outcome.summary
                );
                // The pre-boot configuration screen runs on a placeholder
                // machine with a silent NullSink (see build_placeholder_machine);
                // a state loaded over it would keep that null sink and play no
                // audio. Detect that case before powering on and give the
                // restored machine a live host output below, mirroring the
                // launcher's Run path. A machine that already has a real sink
                // (any normal running session) is left untouched.
                let restoring_over_placeholder = self.restoring_over_placeholder();
                self.powered_on = true;
                self.cpu_halted = false;
                // Force a fresh presentation: the restored frame counter
                // may equal (or precede) the last rendered one.
                self.reset_render_pipeline();
                if matches!(self.ui.panel, Some(Panel::Launcher(_))) {
                    self.ui.panel = None;
                }
                if restoring_over_placeholder {
                    self.install_live_audio_after_placeholder_load();
                }
                if outcome.reconfigured {
                    // The state was built on a different machine; the host
                    // has been reconfigured to match it (see log for the
                    // specifics). The disk-swap playlists are host-side and
                    // describe the previous machine's drives, so drop them
                    // rather than let stale swap affordances show in the
                    // status bar; the restored drives keep whatever disks
                    // the state embedded.
                    self.disk_playlists = std::array::from_fn(|_| Vec::new());
                    self.show_osd(format!(
                        "Loaded {} (reconfigured to {})",
                        display_file_name(path),
                        outcome.summary
                    ));
                } else {
                    self.show_osd(format!("Loaded {}", display_file_name(path)));
                }
                self.request_redraw();
                true
            }
            Err(e) => {
                warn!("save state load failed ({}): {e:#}", path.display());
                self.show_osd("State load failed (see log)");
                false
            }
        }
    }

    /// Pick a Kickstart ROM (and an optional extended ROM) and fit it,
    /// cold-resetting the machine as if the chip had been swapped and the
    /// power cycled (menu "Load Kickstart ROM..."). The main ROM is 512 KiB,
    /// or 256 KiB for a Kickstart 1.x part (mirrored up to the full window);
    /// an extended ROM is 512 KiB ($E00000) or 256 KiB ($F00000).
    /// On any error the running machine keeps its current ROM.
    fn load_rom_from_dialog(&mut self) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load Kickstart ROM (512 or 256 KiB)")
            .add_filter("Amiga ROM images", &["rom", "bin"])
            .pick_file();
        if let Some(main_path) = picked {
            // Offer an optional extended ROM (AROS/CDTV/CD32). Cancelling skips it
            // and removes any extended ROM currently fitted.
            let ext_path = rfd::FileDialog::new()
                .set_title("Load extended ROM (optional; Cancel to skip)")
                .add_filter("Amiga ROM images", &["rom", "bin"])
                .pick_file();

            let result = (|| -> anyhow::Result<()> {
                let rom = std::fs::read(&main_path)
                    .map_err(|e| anyhow::anyhow!("reading ROM {}: {e}", main_path.display()))?;
                let ext = match &ext_path {
                    Some(p) => Some(std::fs::read(p).map_err(|e| {
                        anyhow::anyhow!("reading extended ROM {}: {e}", p.display())
                    })?),
                    None => None,
                };
                self.emu.reload_rom(rom, ext)
            })();

            match result {
                Ok(()) => {
                    info!("boot ROM loaded: {}", main_path.display());
                    self.powered_on = true;
                    self.cpu_halted = false;
                    // The cold reset restarts the frame timeline; force a repaint.
                    self.reset_render_pipeline();
                    self.show_osd(format!("ROM: {}", display_file_name(&main_path)));
                    self.request_redraw();
                }
                Err(e) => {
                    warn!("ROM load failed ({}): {e:#}", main_path.display());
                    self.show_osd("ROM load failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// Start or stop the video+audio capture (shortcut / menu item).
    fn toggle_recording(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    fn start_recording(&mut self) {
        self.start_recording_to(crate::recorder::auto_filename());
    }

    fn start_recording_to(&mut self, path: PathBuf) {
        match crate::recorder::VideoRecorder::create(&path, FB_WIDTH, present_height()) {
            Ok(rec) => {
                // The Paula tap collects the mixed stereo output from this
                // point on; capture_recorder_output drains it every frame.
                self.emu.bus_mut().paula.set_audio_capture_enabled(true);
                info!("recording video+audio to {}", path.display());
                self.show_osd(format!("Recording {}", display_file_name(&path)));
                self.recorder = Some(rec);
            }
            Err(e) => {
                warn!("recording start failed: {e:#}");
                self.show_osd("Recording start failed (see log)");
            }
        }
        self.request_redraw();
    }

    fn stop_recording(&mut self) {
        let Some(mut rec) = self.recorder.take() else {
            return;
        };
        let samples = self.emu.bus_mut().paula.take_captured_audio();
        self.emu.bus_mut().paula.set_audio_capture_enabled(false);
        rec.push_audio(&samples);
        let seconds = rec.recorded_seconds();
        let path = rec.path().to_path_buf();
        match rec.finish() {
            Ok(()) => {
                info!(
                    "recording saved: {} ({seconds:.1}s of emulated time)",
                    path.display()
                );
                self.show_osd(format!(
                    "Saved {} ({seconds:.1}s)",
                    display_file_name(&path)
                ));
            }
            Err(e) => {
                warn!("recording save failed ({}): {e:#}", path.display());
                self.show_osd("Recording save failed (see log)");
            }
        }
        self.request_redraw();
    }

    /// Feed the active recording: drain the audio captured during the
    /// quantum just stepped and, when a new emulated frame was rendered,
    /// append it with the presentation-scaled picture.
    fn capture_recorder_output(&mut self, rendered: bool) {
        if self.recorder.is_none() {
            return;
        }
        let samples = self.emu.bus_mut().paula.take_captured_audio();
        let mut failure = None;
        if let Some(rec) = self.recorder.as_mut() {
            rec.push_audio(&samples);
            if rendered {
                // The recorder's frame size is fixed at FB_WIDTH; average a
                // 35 ns canvas's pixel pairs down to it first.
                if self.present_width != FB_WIDTH {
                    screenshot::downsample_x_into(
                        &self.present_fb,
                        self.present_width,
                        self.present_rows,
                        FB_WIDTH,
                        &mut self.record_scratch_fb,
                    );
                    screenshot::scale_y_into(
                        &self.record_scratch_fb,
                        FB_WIDTH,
                        self.present_rows,
                        present_height(),
                        &mut self.record_fb,
                    );
                } else {
                    screenshot::scale_y_into(
                        &self.present_fb,
                        FB_WIDTH,
                        self.present_rows,
                        present_height(),
                        &mut self.record_fb,
                    );
                }
                if let Err(e) = rec.push_frame(&self.record_fb) {
                    failure = Some(e);
                }
            }
        }
        if let Some(e) = failure {
            warn!("recording frame write failed, stopping capture: {e:#}");
            self.stop_recording();
        }
    }

    fn debugger_toggle_run(&mut self) {
        self.paused = !self.paused;
        self.last_debug_stop = None;
        // Run/Pause inside the debugger is an explicit choice; closing the
        // window must not revert it.
        self.paused_before_debugger = self.paused;
        self.sync_live_audio_suspension();
    }

    /// Execute a single instruction while paused in the debugger.
    fn debugger_step(&mut self) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_instructions(1) {
            error!("debugger step halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
    }

    /// Step over a subroutine call while paused: run a BSR/JSR/TRAP callee to
    /// completion and stop at the following instruction (a plain single step
    /// otherwise). Bounded so a call that never returns cannot wedge the UI.
    fn debugger_step_over(&mut self) {
        const STEP_OVER_BUDGET: usize = 5_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_over(STEP_OVER_BUDGET) {
            error!("debugger step-over halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
        self.finish_render_for_current_frame();
    }

    /// Step out of the current subroutine while paused: run until it returns to
    /// its caller. Bounded so a routine that never returns cannot wedge the UI.
    fn debugger_step_out(&mut self) {
        const STEP_OUT_BUDGET: usize = 5_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_out(STEP_OUT_BUDGET) {
            error!("debugger step-out halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
        self.finish_render_for_current_frame();
    }

    /// Run one whole video frame while paused, refreshing the display so
    /// mid-frame raster effects can be inspected frame by frame. A
    /// scheduler quantum is shorter than a PAL frame, so step until the
    /// frame counter advances (bounded for safety).
    fn debugger_step_frame(&mut self) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        let target = self.emu.bus().emulated_frames() + 1;
        for _ in 0..8 {
            if let Err(e) = self.emu.step_frame() {
                error!("debugger frame step halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
                break;
            }
            if self.surface_debug_stop() {
                break;
            }
            if self.emu.bus().emulated_frames() >= target {
                break;
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run until the PC reaches the address typed in the entry box,
    /// bounded so a never-hit address cannot wedge the UI.
    fn debugger_run_to(&mut self) {
        const RUN_TO_BUDGET: usize = 2_000_000;
        let Some(panel) = self.debugger_panel.as_ref() else {
            return;
        };
        let Some(addr) = panel.entry_addr() else {
            self.show_osd("Run to: type a hex address first");
            return;
        };
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_run_to_pc(addr, RUN_TO_BUDGET) {
            Ok(true) => {}
            // A breakpoint/watch hit on the way is reported instead of
            // the budget message.
            Ok(false) => {
                if !self.surface_debug_stop() {
                    self.show_osd(format!("PC ${addr:06X} not reached (budget)"));
                }
            }
            Err(e) => {
                error!("debugger run-to halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run to the start of the next scanline (the end of the current one),
    /// stopping at exact beam granularity via a one-shot beam trap. The
    /// raster analogue of Step: walk a Copper effect line by line.
    fn debugger_run_to_line_end(&mut self) {
        const RUN_TO_LINE_BUDGET: usize = 2_000_000;
        let (vpos, frame_lines) = {
            let bus = self.emu.bus();
            (bus.agnus.vpos, bus.agnus.current_frame_lines())
        };
        let target = ((vpos + 1) % frame_lines.max(1)).min(u32::from(u16::MAX)) as u16;
        self.run_to_beam_target(target, None, RUN_TO_LINE_BUDGET, "Line end");
    }

    /// Toggle a beam trap from the entry box ("VPOS [HPOS]", decimal).
    fn debugger_toggle_beam_trap(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_beam_spec(&panel.entry));
        let Some((vpos, hpos)) = spec else {
            self.show_osd("Beam: type \"VPOS [HPOS]\" (decimal) first");
            return;
        };
        let set = self.emu.bus_mut().ui_toggle_beam_trap(vpos, hpos);
        let mut msg = format!("Beam trap v{vpos}");
        if let Some(hpos) = hpos {
            msg.push_str(&format!(" h{hpos}"));
        }
        msg.push_str(if set { " set" } else { " removed" });
        self.show_osd(msg);
    }

    /// Arm a waveform (VCD) capture from the Waveform tab's entry box:
    /// an order-free "[PATH] [TRIGGER] [DURATION] [SIGNALS]" spec, with
    /// an empty entry meaning all defaults (trigger now, one frame, all
    /// signals, timestamped path).
    fn debugger_wave_arm(&mut self) {
        let entry = self
            .debugger_panel
            .as_ref()
            .map(|panel| panel.entry.clone())
            .unwrap_or_default();
        let opts = match crate::waveform::parse_wave_args(entry.split_whitespace()) {
            Ok(opts) => opts,
            Err(e) => {
                self.show_osd(format!("Waveform: {e}"));
                return;
            }
        };
        let summary = format!(
            "Waveform armed ({}) -> {}",
            opts.trigger,
            opts.path.display()
        );
        match self.emu.machine.ui_wave_start(opts) {
            Ok(()) => self.show_osd(summary),
            Err(e) => self.show_osd(format!("Waveform: {e}")),
        }
    }

    /// Stop the waveform capture (Waveform tab), finishing the VCD file.
    fn debugger_wave_stop(&mut self) {
        match self.emu.machine.ui_wave_stop() {
            Some(status) => self.show_osd(format!(
                "Waveform stopped: {} samples in {}",
                status.samples,
                status.path.display()
            )),
            None => self.show_osd("No waveform capture"),
        }
    }

    /// Toggle bitplane `plane` in the presented picture (Video tab).
    fn debugger_toggle_plane(&mut self, plane: usize) {
        let shown = self.emu.bus_mut().ui_toggle_layer_plane(plane);
        self.show_osd(format!(
            "Bitplane {} {}",
            plane + 1,
            if shown { "shown" } else { "hidden" }
        ));
        self.rerender_after_debug_view_change();
    }

    /// Toggle sprite `sprite` in the presented picture (Video tab).
    fn debugger_toggle_sprite(&mut self, sprite: usize) {
        let shown = self.emu.bus_mut().ui_toggle_layer_sprite(sprite);
        self.show_osd(format!(
            "Sprite {sprite} {}",
            if shown { "shown" } else { "hidden" }
        ));
        self.rerender_after_debug_view_change();
    }

    /// Re-render and re-present the current frame after a debug-only view
    /// change (layer isolation). Uses the pure snapshot render, so unlike
    /// the normal frame path nothing feeds back into the machine: toggling
    /// a layer while paused cannot perturb the emulation.
    fn rerender_after_debug_view_change(&mut self) {
        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        let h_shift = if self.hcenter {
            // Re-resolving the same frame through the latch is idempotent:
            // the same snapshot yields the same class and the same shift.
            self.presentation_latch
                .presentation_h_shift(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        bitplane::render_display_only(self.emu.bus(), &mut self.fb);
        let geometry = self.emu.bus().frame_geometry();
        let canvas_scale = self.emu.bus().frame_canvas_scale();
        let field_rows = post_process_rendered_field(
            &mut self.fb,
            geometry,
            canvas_scale,
            self.emu.bus().frame_presentation_h_window(),
            self.emu.bus().frame_presentation_v_window(),
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        let base = self.emu.bus().frame_render_base();
        let (rows, width) = self.deinterlacer.present_field_into(
            &self.fb,
            field_rows,
            FB_WIDTH * canvas_scale,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
            &mut self.present_fb,
        );
        self.present_rows = rows;
        self.present_width = width;
        self.request_redraw();
    }

    /// Toggle an exception catchpoint from the entry box ("irq N",
    /// "trap N", or "vec N").
    fn debugger_toggle_catch(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_catch_spec(&panel.entry));
        let Some(vector) = spec else {
            self.show_osd("Catch: type \"irq N\", \"trap N\", or \"vec N\" first");
            return;
        };
        let set = self.emu.machine.ui_toggle_catch(vector);
        self.show_osd(format!(
            "Catch {} {}",
            crate::debugger::exception_vector_name(vector),
            if set { "set" } else { "removed" }
        ));
    }

    /// Toggle a Copper breakpoint at the entry address (Copper tab).
    fn debugger_toggle_copper_break(&mut self) {
        let Some(addr) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.entry_addr())
        else {
            self.show_osd("CBreak: type a hex Copper-list address first");
            return;
        };
        let set = self.emu.bus_mut().ui_toggle_copper_break(addr);
        self.show_osd(format!(
            "Copper breakpoint ${:06X} {}",
            addr & 0x00FF_FFFE,
            if set { "set" } else { "removed" }
        ));
    }

    /// Run until the Copper retires one instruction (Copper tab CStep).
    fn debugger_step_copper(&mut self) {
        const COPPER_STEP_BUDGET: usize = 2_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_step_copper(COPPER_STEP_BUDGET) {
            Ok(true) => {
                self.surface_debug_stop();
            }
            Ok(false) => self.show_osd("Copper did not advance (stopped or DMA off)"),
            Err(e) => {
                error!("copper step halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run until the beam reaches the analyzer's selected slot, stopping
    /// at exact colour-clock granularity via a one-shot beam trap.
    fn frame_analyzer_run_to_slot(&mut self) {
        const RUN_TO_SLOT_BUDGET: usize = 2_000_000;
        let Some((vpos, hpos)) = self
            .frame_analyzer_panel
            .as_ref()
            .map(|panel| (panel.selected_vpos, panel.selected_hpos))
        else {
            return;
        };
        self.emu.bus_mut().set_frame_analyzer_enabled(true);
        self.run_to_beam_target(vpos, Some(hpos), RUN_TO_SLOT_BUDGET, "Beam slot");
    }

    /// Shared run-to-beam-position transport: pause bookkeeping, the
    /// bounded run, stop reporting, and the display refresh.
    fn run_to_beam_target(&mut self, vpos: u16, hpos: Option<u16>, budget: usize, what: &str) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_run_to_beam(vpos, hpos, budget) {
            Ok(true) => {
                self.surface_debug_stop();
            }
            Ok(false) => self.show_osd(format!("{what} not reached (budget)")),
            Err(e) => {
                error!("debugger run-to-beam halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Step one instruction backward, reconstructed from the snapshot ring.
    fn debugger_reverse_step(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_step(1) {
            Ok(ReverseOutcome::Found(_)) => {}
            Ok(ReverseOutcome::BeyondHistory) => self.show_osd("Reverse: beyond recorded history"),
            Ok(ReverseOutcome::NotFound) => self.show_osd("Reverse: nothing earlier to step to"),
            Err(e) => error!("reverse step halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Step one emulated video frame backward, reconstructed from the
    /// snapshot ring.
    fn debugger_reverse_frame(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_frame() {
            Ok(ReverseOutcome::Found(_)) => {}
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd("Reverse frame: beyond recorded history")
            }
            Ok(ReverseOutcome::NotFound) => {
                self.show_osd("Reverse frame: no earlier frame to step to")
            }
            Err(e) => error!("reverse frame step halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Run backward to the previous breakpoint hit (reconstructed from the
    /// snapshot ring).
    fn debugger_reverse_continue(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_continue() {
            Ok(ReverseOutcome::Found((_, reason))) => {
                let message = format!("Reverse: {reason}");
                info!("debugger stop: {message}");
                self.last_debug_stop = Some(message.clone());
                self.show_osd(message);
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.tab = ui::DebugTab::Break;
                }
            }
            Ok(ReverseOutcome::NotFound) => self.show_osd("Reverse run: no earlier stop hit"),
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd("Reverse run: beyond recorded history")
            }
            Err(e) => error!("reverse continue halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Surface a pending breakpoint/watchpoint hit: pause the machine,
    /// bring up the debugger window, and report the reason. Returns true
    /// when a stop was pending. Also reports (once) a CPU double-fault
    /// halt -- the guest is dead at that point, so it must not pass
    /// silently.
    fn surface_debug_stop(&mut self) -> bool {
        if self.emu.machine.cpu_double_faulted() {
            if !self.reported_double_fault {
                self.reported_double_fault = true;
                let message = format!(
                    "CPU halted: double fault at pc ${:06X} (bus/address error during exception)",
                    self.emu.machine.pc() & self.emu.machine.ui_addr_mask()
                );
                warn!("{message}");
                self.last_debug_stop = Some(message.clone());
                if let Some(panel) = self.console_panel.as_mut() {
                    panel.push_output(format!("!{message}"));
                }
                self.paused = true;
                self.sync_live_audio_suspension();
                #[cfg(feature = "control")]
                if !self.control_complete_pending("double_fault", &message) {
                    self.control_notify_stopped("double_fault", &message);
                }
                self.show_osd(message);
                self.request_redraw();
                return true;
            }
        } else {
            self.reported_double_fault = false;
        }
        let Some(stop) = self.emu.machine.take_ui_debug_stop() else {
            return false;
        };
        let message = stop.describe();
        info!("debugger stop: {message}");
        // A stop while a remote resume is pending answers the client and
        // pauses without commandeering the local debugger window.
        #[cfg(feature = "control")]
        if self.control_completes_stop(&stop) {
            self.paused = true;
            self.sync_live_audio_suspension();
            self.last_debug_stop = Some(message.clone());
            self.show_osd(message);
            self.request_redraw();
            return true;
        }
        self.paused = true;
        self.paused_before_debugger = true;
        self.sync_live_audio_suspension();
        self.open_debugger();
        self.last_debug_stop = Some(message.clone());
        self.show_osd(message);
        self.request_redraw();
        true
    }

    /// Toggle a PC breakpoint from the entry box. The entry may carry an
    /// optional condition and ignore count: "ADDR [LHS OP RHS] [IGN N]".
    fn debugger_toggle_breakpoint(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_break_spec(&panel.entry));
        let Some((addr, cond, ignore)) = spec else {
            self.show_osd("Break: ADDR [LHS OP RHS] [IGN N] e.g. C033C2 D0 EQ 5");
            return;
        };
        let set = self.emu.machine.ui_set_breakpoint(addr, cond, ignore);
        let mut msg = format!(
            "Breakpoint ${:06X} {}",
            addr & self.emu.machine.ui_addr_mask(),
            if set { "set" } else { "removed" }
        );
        if set {
            if let Some(cond) = &cond {
                msg.push_str(&format!(" when {}", cond.describe()));
            }
            if ignore > 0 {
                msg.push_str(&format!(" ign {ignore}"));
            }
        }
        self.show_osd(msg);
    }

    /// Toggle a memory word watchpoint at the entry-box address.
    fn debugger_toggle_watchpoint(&mut self) {
        let Some(addr) = self.debugger_entry_addr("Watch") else {
            return;
        };
        let addr = addr & self.emu.machine.ui_addr_mask() & !1;
        let set = self.emu.machine.ui_toggle_watch(addr);
        self.show_osd(format!(
            "Watchpoint ${addr:06X} {}",
            if set { "set" } else { "removed" }
        ));
    }

    /// Toggle a chipset-register write watch. The entry accepts either a
    /// bare register offset (96) or a full address (DFF096).
    fn debugger_toggle_reg_watch(&mut self) {
        let Some(addr) = self.debugger_entry_addr("Reg") else {
            return;
        };
        let off = (addr & 0x1FE) as u16;
        let set = self.emu.machine.ui_toggle_reg_watch(off);
        self.show_osd(format!(
            "{} (${off:03X}) write watch {}",
            crate::debugger::custom_reg_name(off),
            if set { "set" } else { "removed" }
        ));
    }

    /// The debugger entry-box address, or an OSD prompt when empty.
    fn debugger_entry_addr(&mut self, what: &str) -> Option<u32> {
        let panel = self.debugger_panel.as_ref()?;
        let addr = panel.entry_addr();
        if addr.is_none() {
            self.show_osd(format!("{what}: type a hex address first"));
        }
        addr
    }

    /// Page the Memory tab's hex dump up or down.
    fn debugger_mem_page(&mut self, direction: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if panel.tab == ui::DebugTab::Memory {
                // Bits mode pages by one bitmap screenful; hex by one page.
                let delta = if panel.mem_view_bits {
                    (panel.mem_bitmap_stride * ui::mem_bitmap_rows() as u32).max(1)
                } else {
                    ui::MEM_PAGE_BYTES
                };
                panel.mem_addr = if direction < 0 {
                    panel.mem_addr.wrapping_sub(delta)
                } else {
                    panel.mem_addr.wrapping_add(delta)
                } & self.emu.machine.ui_addr_mask();
                if !panel.mem_view_bits {
                    panel.mem_addr &= !0xF;
                }
            }
        }
    }

    /// Scroll the Memory tab by `rows` display rows (16 bytes each in the
    /// hex view, one stride in the bitmap view).
    fn debugger_mem_scroll(&mut self, rows: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if panel.tab == ui::DebugTab::Memory && rows != 0 {
                let step = if panel.mem_view_bits {
                    panel.mem_bitmap_stride.max(1)
                } else {
                    16
                };
                let delta = step.wrapping_mul(rows.unsigned_abs());
                panel.mem_addr = if rows < 0 {
                    panel.mem_addr.wrapping_sub(delta)
                } else {
                    panel.mem_addr.wrapping_add(delta)
                } & self.emu.machine.ui_addr_mask();
                self.request_redraw();
            }
        }
    }

    /// Find the entry's hex byte pattern in CPU-visible memory, starting
    /// past the previous hit (or the current page) and wrapping around
    /// the decoded memory map once.
    fn debugger_mem_find(&mut self) {
        let Some(panel) = self.debugger_panel.as_ref() else {
            return;
        };
        let Some(pattern) = panel.find_pattern() else {
            self.show_osd("Find: type hex byte pairs first (e.g. 4E75)");
            return;
        };
        let start = panel
            .mem_last_find
            .map(|addr| addr.wrapping_add(1))
            .unwrap_or(panel.mem_addr)
            & self.emu.machine.ui_addr_mask();
        let regions = self.emu.bus().searchable_regions();
        let found = console::search_cpu_memory(&self.emu.machine, &regions, &pattern, start);
        match found {
            Some(addr) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.mem_last_find = Some(addr);
                    panel.mem_addr = addr & !0xF;
                }
                self.show_osd(format!("Found at ${addr:06X}"));
            }
            None => self.show_osd("Pattern not found"),
        }
        self.request_redraw();
    }

    /// Save the entry's "ADDR LEN" region of CPU-visible memory to a file
    /// picked in a save dialog (the GUI counterpart of the headless
    /// COPPERLINE_DBG_RAMDUMP knob).
    fn debugger_mem_save_region(&mut self) {
        let Some((addr, len)) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.region_spec())
        else {
            self.show_osd("Save: type \"ADDR LEN\" (hex) first");
            return;
        };
        // Through the machine's address bus, like every other debugger
        // surface: the file name and the OSD then name the address the
        // bytes actually came from on a 24-bit model, and a 32-bit dump
        // above the 24-bit space passes through untouched on 020+.
        let addr = addr & self.emu.machine.ui_addr_mask();
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Save memory region")
            .set_file_name(format!("mem-{addr:06X}-{len:X}.bin"))
            .save_file();
        if let Some(path) = picked {
            let bytes = self.emu.machine.debug_read_memory(addr, len as usize);
            match std::fs::write(&path, &bytes) {
                Ok(()) => self.show_osd(format!(
                    "Saved ${addr:06X}+${len:X} to {}",
                    display_file_name(&path)
                )),
                Err(e) => {
                    warn!("memory region save failed ({}): {e:#}", path.display());
                    self.show_osd("Memory save failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// Report the last instruction that wrote the word at the entry
    /// address, replayed from the reverse-debug snapshot ring (the GUI
    /// counterpart of GDB's "monitor last-writer").
    fn debugger_mem_writer(&mut self) {
        use crate::timetravel::ReverseOutcome;
        let Some(addr) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.entry_addr())
        else {
            self.show_osd("Writer: type a hex address first");
            return;
        };
        let addr = addr & self.emu.machine.ui_addr_mask() & !1;
        let before = self.emu.retired_instructions();
        match self.emu.tt_last_writer(addr, before) {
            Ok(ReverseOutcome::Found(rec)) => {
                let message = format!(
                    "${:06X}: {:04X}->{:04X} by pc ${:06X} (frame {})",
                    rec.addr,
                    rec.old,
                    rec.new,
                    rec.pc & self.emu.machine.ui_addr_mask(),
                    rec.frame
                );
                info!("last-writer {message}");
                self.last_debug_stop = Some(format!("Last writer {message}"));
                self.show_osd(message);
            }
            Ok(ReverseOutcome::NotFound) => {
                self.show_osd(format!("No write to ${addr:06X} in retained history"))
            }
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd(format!("Write to ${addr:06X} predates history"))
            }
            Err(e) => {
                error!("last-writer failed: {e:?}");
                self.show_osd("Last-writer failed (see log)");
            }
        }
        self.finish_render_for_current_frame();
        self.request_redraw();
    }

    /// Toggle the Memory tab between hex and the 1-bpp bitplane view. An
    /// entry holding a small decimal number sets the bitmap row stride.
    fn debugger_mem_toggle_bits(&mut self) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if let Some(stride) = panel
                .entry
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|stride| (1..=512).contains(stride))
            {
                panel.mem_bitmap_stride = stride;
            }
            panel.mem_view_bits = !panel.mem_view_bits;
            let mode = if panel.mem_view_bits {
                format!("bitmap (stride {} bytes)", panel.mem_bitmap_stride)
            } else {
                "hex".to_string()
            };
            self.show_osd(format!("Memory view: {mode}"));
            self.request_redraw();
        }
    }

    /// Write the entry box's value live while paused: a memory word from
    /// "ADDR VALUE" on the Memory tab, or a register from "REG VALUE" on the
    /// CPU tab. The panel borrow is resolved into a plain action first so the
    /// emulator can then be borrowed mutably to perform the write.
    fn debugger_poke(&mut self) {
        enum Poke {
            Mem(u32, u16),
            Reg(usize, u32),
            MemHelp,
            RegHelp,
            None,
        }
        let action = match self.debugger_panel.as_ref() {
            Some(panel) => match panel.tab {
                ui::DebugTab::Memory => match panel.poke_target() {
                    Some((addr, value)) => Poke::Mem(addr, value),
                    None => Poke::MemHelp,
                },
                ui::DebugTab::Cpu => match panel.reg_poke() {
                    Some((reg, value)) => Poke::Reg(reg, value),
                    None => Poke::RegHelp,
                },
                _ => Poke::None,
            },
            None => Poke::None,
        };
        match action {
            Poke::Mem(addr, value) => {
                let written = self
                    .emu
                    .machine
                    .debug_write_memory(addr, &value.to_be_bytes());
                if written == 2 {
                    self.show_osd(format!("Poked ${value:04X} -> ${addr:06X}"));
                } else {
                    self.show_osd(format!("${addr:06X} is not writable RAM"));
                }
            }
            Poke::Reg(reg, value) => {
                self.emu.machine.debug_set_register(reg, value);
                self.show_osd(format!("{} <- ${value:X}", gdb_reg_label(reg)));
            }
            Poke::MemHelp => self.show_osd("Poke: type \"ADDR VALUE\" (hex) first"),
            Poke::RegHelp => self.show_osd("Set Reg: type \"REG VALUE\" e.g. D0 1234"),
            Poke::None => {}
        }
    }

    /// Build the per-redraw view data for the open panel, if any.
    fn build_panel_view_data(&self) -> Option<ui::PanelViewData> {
        match self.ui.panel.as_ref()? {
            Panel::About => Some(ui::PanelViewData::About(ui::AboutView {
                machine_lines: self.about_machine_lines.clone(),
            })),
            Panel::Shortcuts => Some(ui::PanelViewData::Shortcuts),
            // Self-contained: the panel's own state is everything it draws.
            Panel::InputMap(_) => None,
            Panel::Calibration(session) => Some(ui::PanelViewData::Calibration(
                build_calibration_view(session),
            )),
            Panel::Debugger(panel) => Some(ui::PanelViewData::Debugger(Box::new(
                self.build_debugger_view(panel),
            ))),
            Panel::FrameAnalyzer(panel) => Some(ui::PanelViewData::FrameAnalyzer(Box::new(
                self.build_frame_analyzer_view(panel),
            ))),
            // The console, configuration, and drop-chooser panels render
            // from their own state.
            Panel::Console(_) => None,
            Panel::Launcher(_) => None,
            Panel::DropChooser(_) => None,
        }
    }

    fn build_tool_panel_view_data(&self, kind: ToolPanelKind) -> Option<ui::PanelViewData> {
        match kind {
            ToolPanelKind::Debugger => self.debugger_panel.as_ref().map(|panel| {
                ui::PanelViewData::Debugger(Box::new(self.build_debugger_view(panel)))
            }),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_panel.as_ref().map(|panel| {
                ui::PanelViewData::FrameAnalyzer(Box::new(self.build_frame_analyzer_view(panel)))
            }),
            // The console panel carries everything it renders.
            ToolPanelKind::Console => None,
        }
    }

    fn build_frame_analyzer_view(&self, panel: &ui::FrameAnalyzerPanel) -> ui::FrameAnalyzerView {
        let bus = self.emu.bus();
        let status = format!(
            "{} frame {} {:.2}s",
            if self.paused { "paused" } else { "running" },
            bus.emulated_frames(),
            bus.emulated_seconds()
        );
        // The heat map records bus activity, not beam slots, so it has
        // something to show even on a frame the analyzer captured no trace
        // for: built before the no-trace return and carried by both arms.
        let heat = self.build_analyzer_heat_view(panel);
        let Some(trace) = bus.frame_bus_trace() else {
            return ui::FrameAnalyzerView {
                running: !self.paused,
                status,
                trace: None,
                underlay: None,
                scrub: false,
                heat,
            };
        };
        let underlay = (panel.underlay_active() && self.analyzer_underlay_rows > 0).then(|| {
            ui::AnalyzerUnderlayView {
                fb: std::rc::Rc::clone(&self.analyzer_underlay_fb),
                rows: self.analyzer_underlay_rows,
                width: self.analyzer_underlay_width,
            }
        });
        let selected_vpos = usize::from(panel.selected_vpos).min(trace.rows.saturating_sub(1));
        let selected_hpos = usize::from(panel.selected_hpos).min(trace.cols.saturating_sub(1));
        let selected_owner_code = trace.owner_code_at(selected_vpos, selected_hpos);
        let selected_owner = owner_name_from_code(selected_owner_code);
        let mut owners = Vec::with_capacity(trace.rows * trace.cols);
        for vpos in 0..trace.rows {
            if let Some(row) = trace.owner_row(vpos) {
                owners.extend_from_slice(row);
            }
        }
        // Marker capacity bounds the per-redraw copy, not what is worth
        // seeing: a Copper writing a palette split on every line stays
        // well inside it.
        const MARKER_CAP: usize = 4000;
        let markers = bus
            .frame_render_events()
            .iter()
            .take(MARKER_CAP)
            .map(|event| ui::AnalyzerMarker {
                vpos: event.vpos.min(u32::from(u16::MAX)) as u16,
                hpos: event.hpos.min(u32::from(u16::MAX)) as u16,
                offset: event.offset & 0x01FE,
                value: event.value,
                source: match event.source {
                    BeamWriteSource::Cpu => "cpu",
                    BeamWriteSource::CpuCopperIrq => "irq",
                    BeamWriteSource::Copper => "copper",
                },
            })
            .collect();
        // Frame-start DIW/DDF overlays, decoded with the display model's
        // own rules (DiwHigh carries the OCS implicit bits or the ECS
        // DIWHIGH extension; DIW h units are lores pixels, two per cck).
        let base = bus.frame_render_base();
        let diw_programmed = !(base.diwstrt == 0 && base.diwstop == 0);
        let (diw_v, diw_h_cck) = if diw_programmed {
            let v0 = base.diwhigh.v_start(base.diwstrt);
            let mut v1 = base.diwhigh.v_stop(base.diwstop);
            if v1 <= v0 {
                // Hardware vstop wrap: a stop at or above the start means
                // the window runs past the 8-bit rollover.
                v1 += 0x100;
            }
            let h0 = base.diwhigh.h_start(base.diwstrt) / 2;
            let h1 = base.diwhigh.h_stop(base.diwstop) / 2;
            (Some((v0, v1)), Some((h0, h1)))
        } else {
            (None, None)
        };
        let ddf_cck = diw_programmed.then_some((base.ddfstrt & 0x00FE, base.ddfstop & 0x00FE));
        // Annotate the selected slot with the blit whose beam span
        // contains it, so clicking a blitter run names the blit.
        let selected_beam = (selected_vpos as u16, selected_hpos as u16);
        let selected_blit = trace.blits.iter().enumerate().find_map(|(i, blit)| {
            let end = blit.end?;
            (blit.start <= selected_beam && selected_beam <= end).then(|| {
                format!(
                    "in blit #{i} ({}x{} D ${:06X})",
                    blit.width_words,
                    blit.height,
                    blit.dpt & 0x00FF_FFFF
                )
            })
        });
        ui::FrameAnalyzerView {
            running: !self.paused,
            status,
            underlay,
            scrub: panel.show_scrub,
            heat,
            trace: Some(ui::AnalyzerTraceView {
                frame: trace.frame,
                seconds: trace.seconds,
                rows: trace.rows,
                cols: trace.cols,
                line_cck: trace.line_cck,
                visible_start_vpos: trace.visible_start_vpos,
                visible_lines: trace.visible_lines,
                display_hpos_start: trace.display_hpos_start,
                display_hpos_end: trace.display_hpos_end,
                owner_cck: trace.owner_cck,
                blitter_busy_cck: trace.blitter_busy_cck,
                blitter_starve_cck: trace.blitter_starve_cck,
                partial: trace.partial,
                selected_vpos,
                selected_hpos,
                selected_owner,
                selected_owner_code,
                owners,
                markers,
                selected_blit,
                diw_v,
                diw_h_cck,
                ddf_cck,
            }),
        }
    }

    /// The Memory tab's picture of the live heat map, or None while no map
    /// is armed. Everything is read out here rather than in the drawing
    /// code: the map lives on the bus, and the panel only ever sees the
    /// rendered grid, the census, and the pinned cell's record.
    fn build_analyzer_heat_view(
        &self,
        panel: &ui::FrameAnalyzerPanel,
    ) -> Option<ui::AnalyzerHeatView> {
        let bus = self.emu.bus();
        let map = bus.heat_map()?;
        let frame = bus.emulated_frames();
        let mut image = vec![0xFF00_0000u32; heatmap::CELLS];
        map.render(frame, &mut image);
        let bytes_per_cell = map.bytes_per_cell();
        // The map's census reports only the touchers holding cells; the
        // column wants every toucher, in a fixed order, so the rows read as
        // a legend and do not move as activity comes and goes.
        let counts = map.census(frame);
        let census = HEAT_TOUCHERS
            .iter()
            .map(|toucher| {
                let cells = counts
                    .iter()
                    .find(|(recorded, _)| recorded == toucher)
                    .map_or(0, |(_, cells)| *cells);
                ui::AnalyzerHeatCensusRow {
                    name: toucher.name(),
                    colour: toucher.colour(),
                    cells,
                    bytes: cells as u64 * u64::from(bytes_per_cell),
                }
            })
            .collect();
        let selected = panel.heat_selected.map(|cell| {
            let record = map.cell(cell);
            ui::AnalyzerHeatCell {
                cell,
                toucher: record.map(|(toucher, _)| toucher.name()),
                colour: record.map_or(0, |(toucher, _)| toucher.colour()),
                // The stamp is the frame counter's low 32 bits, the same
                // arithmetic the map's own fade uses.
                age_frames: record.map(|(_, stamp)| (frame as u32).saturating_sub(stamp)),
            }
        });
        Some(ui::AnalyzerHeatView {
            image,
            base: map.base(),
            span: map.span(),
            bytes_per_cell,
            frame,
            census,
            selected,
        })
    }

    /// Snapshot the machine into the debugger panel's formatted lines.
    /// Everything reads through side-effect-free peeks, so inspecting
    /// state never perturbs the emulation.
    fn build_debugger_view(&self, panel: &ui::DebuggerPanel) -> ui::DebuggerView {
        let machine = &self.emu.machine;
        let bus = self.emu.bus();
        let mut status = format!(
            "{} frame {} {:.2}s",
            if self.paused { "paused" } else { "running" },
            bus.emulated_frames(),
            bus.emulated_seconds()
        );
        // Reverse-debug position and history depth, when the ring is armed.
        if let Some(ring) = self.emu.time_travel_ring() {
            if !ring.is_empty() {
                status.push_str(&format!(
                    "  | pos {} rev {} snaps, {} MB",
                    self.emu.retired_instructions(),
                    ring.len(),
                    ring.used_bytes() / (1024 * 1024),
                ));
            }
        }
        let read = |addr: u32| bus.peek_word_any(addr);
        let mut lines: Vec<ui::DbgLine> = Vec::new();
        let mut bitmap: Option<ui::MemBitmapView> = None;
        let mut video: Option<ui::VideoView> = None;
        let mut audio: Option<ui::AudioScopeView> = None;
        match panel.tab {
            ui::DebugTab::Cpu => {
                let pc = machine.pc();
                let sr = machine.sr();
                lines.push(ui::DbgLine::plain(format!(
                    "PC {pc:08X}   SR {sr:04X} [{}]{}",
                    ui::sr_flags(sr),
                    if machine.stopped() { "   STOPPED" } else { "" }
                )));
                lines.push(ui::DbgLine::plain(""));
                for (name, regs) in [("D", 0usize), ("A", 1)] {
                    for half in 0..2 {
                        let row: Vec<String> = (0..4)
                            .map(|i| {
                                let reg = half * 4 + i;
                                let value = if regs == 0 {
                                    machine.d(reg)
                                } else {
                                    machine.a(reg)
                                };
                                format!("{name}{reg} {value:08X}")
                            })
                            .collect();
                        lines.push(ui::DbgLine::plain(row.join("   ")));
                    }
                }
                lines.push(ui::DbgLine::plain(""));
                // "How did I get here": the most recent retired PCs
                // (oldest first; the console's HISTORY command shows the
                // full ring with disassembly).
                let history = machine.ui_pc_history();
                if !history.is_empty() {
                    let recent: Vec<String> = history
                        .iter()
                        .rev()
                        .take(8)
                        .rev()
                        .map(|pc| format!("{pc:06X}"))
                        .collect();
                    lines.push(ui::DbgLine::plain(format!("recent {}", recent.join(" "))));
                    lines.push(ui::DbgLine::plain(""));
                }
                if let Some(origin) = panel.disasm_addr {
                    lines.push(ui::DbgLine::plain(format!(
                        "Disassembly pinned at ${origin:06X} (empty box + Enter follows PC)"
                    )));
                }
                let breaks = machine.ui_breaks();
                let mut addr = panel.disasm_addr.unwrap_or(pc) & !1;
                for _ in 0..24 {
                    let (text, len) = crate::disasm::disassemble(read, addr, machine.cpu_type());
                    // A leading bullet marks a line that carries a breakpoint.
                    let marker = if breaks.is_breakpoint(addr) { "*" } else { " " };
                    let line = format!("{marker}{addr:08X}  {text}");
                    lines.push(if addr == pc {
                        ui::DbgLine::hilit(line)
                    } else {
                        ui::DbgLine::plain(line)
                    });
                    addr = addr.wrapping_add(len);
                }
            }
            ui::DebugTab::Chipset => {
                let agnus = &bus.agnus;
                let base = bus.current_render_base();
                let intreq = bus.cpu_visible_intreq();
                let intena = bus.paula.intena;
                lines.push(ui::DbgLine::hilit(format!(
                    "Beam vpos {:>3} hpos {:>3}   frame {}",
                    agnus.vpos,
                    agnus.hpos,
                    bus.emulated_frames()
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "DMACON {:04X}  {}",
                    agnus.dmacon,
                    ui::dmacon_flags(agnus.dmacon)
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "INTENA {:04X}  {}",
                    intena,
                    ui::int_flags(intena)
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "INTREQ {:04X}  {}",
                    intreq,
                    ui::int_flags(intreq)
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "COP1LC {:06X}   COP2LC {:06X}   COPPC {:06X} ({})",
                    agnus.cop1lc,
                    agnus.cop2lc,
                    bus.copper.pc(),
                    if bus.copper.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "BPLCON0 {:04X}  BPLCON1 {:04X}  BPLCON2 {:04X}  FMODE {:04X}",
                    base.bplcon0, base.bplcon1, base.bplcon2, base.fmode
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "DIWSTRT {:04X}  DIWSTOP {:04X}  DDFSTRT {:04X}  DDFSTOP {:04X}",
                    base.diwstrt, base.diwstop, base.ddfstrt, base.ddfstop
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "BPL1MOD {}  BPL2MOD {}",
                    base.bpl1mod, base.bpl2mod
                )));
                lines.push(ui::DbgLine::plain(""));
                for (label, ptrs) in [("BPLPT", &base.bplpt), ("SPRPT", &base.sprpt)] {
                    let row: Vec<String> = ptrs.iter().map(|p| format!("{p:06X}")).collect();
                    lines.push(ui::DbgLine::plain(format!("{label} {}", row.join(" "))));
                }
                lines.push(ui::DbgLine::plain(""));
                let colors = base.palette.hi_words();
                for half in 0..2 {
                    let row: Vec<String> = (0..16)
                        .map(|i| format!("{:03X}", colors[half * 16 + i] & 0x0FFF))
                        .collect();
                    lines.push(ui::DbgLine::plain(format!(
                        "COLOR{:02} {}",
                        half * 16,
                        row.join(" ")
                    )));
                }
            }
            ui::DebugTab::Video => {
                let base = bus.frame_render_base();
                let aga = base.agnus_revision == crate::chipset::agnus::AgnusRevision::AgaAlice;
                let bplcon0 = base.bplcon0;
                let nplanes =
                    (((bplcon0 >> 12) & 7) as usize + (((bplcon0 >> 4) & 1) as usize * 8)).min(8);
                let res = if bplcon0 & 0x0040 != 0 {
                    "shres"
                } else if bplcon0 & 0x8000 != 0 {
                    "hires"
                } else {
                    "lores"
                };
                let mut modes = String::new();
                if bplcon0 & 0x0800 != 0 {
                    modes.push_str("  HAM");
                }
                if bplcon0 & 0x0400 != 0 {
                    modes.push_str("  DPF");
                }
                let header = format!(
                    "BPLCON0 {bplcon0:04X}: {nplanes} planes {res}{modes}   DMACON: BPLEN {} SPREN {}",
                    if base.dmacon & 0x0100 != 0 {
                        "on"
                    } else {
                        "off"
                    },
                    if base.dmacon & 0x0020 != 0 {
                        "on"
                    } else {
                        "off"
                    },
                );
                let captured = bus.frame_captured_sprite_lines();
                let sprites = (0..8)
                    .map(|sprite| {
                        let pos = base.sprpos[sprite];
                        let ctl = base.sprctl[sprite];
                        let vstart = (pos >> 8) | ((ctl & 0x04) << 6);
                        let vstop = (ctl >> 8) | ((ctl & 0x02) << 7);
                        let hstart = ((pos & 0xFF) << 1) | (ctl & 0x01);
                        let attached = ctl & 0x80 != 0;
                        let mut dma_lines: Vec<&crate::bus::CapturedSpriteLine> = captured
                            .iter()
                            .filter(|line| line.sprite == sprite)
                            .collect();
                        dma_lines.sort_by_key(|line| line.beam_y);
                        let text = format!(
                            "SPR{sprite} v{vstart}-{vstop} h{hstart}{}{}  dma lines {}",
                            if attached { " att" } else { "" },
                            if base.spr_armed[sprite] { " armed" } else { "" },
                            dma_lines.len(),
                        );
                        // Thumbnail: sample the DMA lines to the thumb
                        // height; classic 2-bpp decode against the pair's
                        // frame-start palette bank (an attached pair or an
                        // AGA BPLCON4 bank shifts real colours, but shape
                        // is what the thumbnail is for).
                        let total = dma_lines.len();
                        let rows = total.min(ui::VIDEO_THUMB_MAX_ROWS);
                        let mut thumb = vec![0u32; rows * 16];
                        for row in 0..rows {
                            let line = dma_lines[row * total / rows.max(1)];
                            for x in 0..16usize {
                                let bit = 15 - x;
                                let idx =
                                    ((line.data >> bit) & 1) | ((((line.datb) >> bit) & 1) << 1);
                                if idx == 0 {
                                    continue;
                                }
                                let entry = 16 + (sprite / 2) * 4 + idx as usize;
                                let rgb = base.palette.rgb24(entry);
                                thumb[row * 16 + x] =
                                    rgba((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
                            }
                        }
                        ui::SpriteRowView {
                            text,
                            thumb,
                            thumb_rows: rows,
                        }
                    })
                    .collect();
                let palette_entries = if aga { 256 } else { 32 };
                let palette = (0..palette_entries)
                    .map(|entry| {
                        let rgb = base.palette.rgb24(entry);
                        rgba((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF)
                    })
                    .collect();
                let masks = bus.ui_layer_masks();
                video = Some(ui::VideoView {
                    header,
                    plane_mask: masks.planes,
                    nplanes,
                    sprite_mask: masks.sprites,
                    sprites,
                    palette,
                });
            }
            ui::DebugTab::Copper => {
                // Leave room for the CBreak/CStep buttons drawn at the top
                // of the content area.
                for _ in 0..ui::COPPER_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                let agnus = &bus.agnus;
                // Anchor the listing on the current instruction's start:
                // mid-instruction the PC already points at the second word.
                let anchor = bus
                    .copper
                    .pc()
                    .wrapping_sub(if bus.copper.mid_instruction() { 2 } else { 0 });
                let state = if bus.copper.is_running() {
                    "running".to_string()
                } else if let Some(wait) = bus.copper.waiting() {
                    let pos = wait.position_bits();
                    format!("waiting v{} h{}", (pos >> 8) & 0xFF, pos & 0xFE)
                } else {
                    "stopped".to_string()
                };
                lines.push(ui::DbgLine::plain(format!(
                    "COP1LC {:06X}   COP2LC {:06X}   COPPC {:06X} ({state})",
                    agnus.cop1lc,
                    agnus.cop2lc,
                    bus.copper.pc(),
                )));
                lines.push(ui::DbgLine::plain(""));
                // Follow the live Copper around its PC (a stopped Copper
                // shows the head of the COP1 list instead). Breakpointed
                // addresses are marked with `*`.
                let stopped = !bus.copper.is_running() && bus.copper.waiting().is_none();
                let start = if stopped {
                    agnus.cop1lc
                } else {
                    anchor.saturating_sub(5 * 4)
                };
                let cbreaks = bus.ui_copper_breaks();
                for (addr, text) in crate::disasm::dump_copper_list(read, start, 30) {
                    let marker = if cbreaks.contains(&addr) { "*" } else { " " };
                    let line = format!("{marker}{addr:06X}  {text}");
                    lines.push(if !stopped && addr == anchor {
                        ui::DbgLine::hilit(line)
                    } else {
                        ui::DbgLine::plain(line)
                    });
                }
            }
            ui::DebugTab::Audio => {
                let dmacon = bus.agnus.dmacon;
                let master = dmacon & 0x0200 != 0; // DMACON DMAEN
                let adkcon = bus.paula.adkcon;
                // Audio interrupt-pending latches live in INTREQ bits 7..10
                // (AUD0..AUD3); use the CPU-visible copy like the Chipset tab.
                let intreq = bus.cpu_visible_intreq();
                // Per-channel AUDxEN bit (AUD0..AUD3 = bits 0..3).
                let auden: Vec<&str> = (0..4)
                    .map(|ch| if dmacon & (1 << ch) != 0 { "1" } else { "." })
                    .collect();
                let header = format!(
                    "DMACON {:04X}  DMAEN {}  AUDEN {}   ADKCON {:04X}  {}",
                    dmacon,
                    if master { "on" } else { "off" },
                    auden.join(" "),
                    adkcon,
                    ui::adkcon_audio_flags(adkcon),
                );
                let mut channels: Vec<ui::AudioRowView> = Vec::with_capacity(4);
                for ch in 0..4 {
                    let Some(a) = bus.paula.audio_channel_debug(ch) else {
                        continue;
                    };
                    let dma_on = master && (dmacon & (1 << ch)) != 0;
                    let mut text: Vec<ui::DbgLine> = Vec::new();
                    let head = format!(
                        "AUD{} [{}]  DMA {}  IRQ {}",
                        ch,
                        a.state,
                        if dma_on { "on" } else { "off" },
                        if intreq & (1 << (7 + ch)) != 0 {
                            "pend"
                        } else {
                            "-"
                        },
                    );
                    // Highlight a channel that is actively streaming samples.
                    text.push(if a.playing {
                        ui::DbgLine::hilit(head)
                    } else {
                        ui::DbgLine::plain(head)
                    });
                    text.push(ui::DbgLine::plain(format!(
                        "  LC {:06X}  LEN {:04X}  PER {:04X}  VOL {:02X}",
                        a.lc, a.len, a.per, a.vol
                    )));
                    text.push(ui::DbgLine::plain(format!(
                        "  PTR {:06X}  cnt {:04X}  percnt {:05X}  vol {:02X}  out {}",
                        a.ptr, a.audlen, a.percnt, a.audvol, a.current
                    )));
                    let mut pending: Vec<&str> = Vec::new();
                    if a.intreq2 {
                        pending.push("intreq2");
                    }
                    if a.sm_request {
                        pending.push("dma-req");
                    }
                    if a.agnus_request {
                        pending.push("dma-req-latched");
                    }
                    if !pending.is_empty() {
                        text.push(ui::DbgLine::plain(format!(
                            "  pending: {}",
                            pending.join(" ")
                        )));
                    }
                    channels.push(ui::AudioRowView {
                        text,
                        muted: bus.paula.channel_muted(ch),
                        scope: bus.paula.audio_scope_samples(ch),
                    });
                }
                let cd_scope = bus.paula.cd_scope_samples();
                let cd_active = cd_scope.iter().any(|&s| s != 0);
                let cd_peak = cd_scope
                    .iter()
                    .map(|&s| (s as i16).abs())
                    .max()
                    .unwrap_or(0);
                // A SCSI CD-ROM drive reports its play operation (state,
                // track, position); the CDTV/CD32 controllers stream
                // without one, so their row keeps the scope-derived state.
                let cd_status = bus
                    .scsi_cd_playback_line()
                    .unwrap_or_else(|| if cd_active { "playing" } else { "idle" }.to_string());
                let cd = ui::AudioRowView {
                    text: vec![
                        ui::DbgLine::hilit(format!("CD-DA  {cd_status}")),
                        ui::DbgLine::plain(format!("  peak {cd_peak:>3}")),
                    ],
                    muted: bus.paula.cd_muted(),
                    scope: cd_scope,
                };
                // Mirror the text into `lines` for the headless/text fallback
                // and the non-empty-view invariant; the tab itself is drawn
                // graphically from the structured view.
                lines.push(ui::DbgLine::hilit(header.clone()));
                for row in channels.iter().chain(std::iter::once(&cd)) {
                    lines.push(ui::DbgLine::plain(""));
                    lines.extend(row.text.iter().cloned());
                }
                audio = Some(ui::AudioScopeView {
                    header,
                    channels,
                    cd,
                });
            }
            ui::DebugTab::Memory => {
                // Leave room for the Find/Save/Writer/Bits buttons drawn at
                // the top of the content area.
                for _ in 0..ui::MEM_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                if panel.mem_view_bits {
                    let stride = panel.mem_bitmap_stride.max(1) as usize;
                    let rows = ui::mem_bitmap_rows();
                    let base = panel.mem_addr & machine.ui_addr_mask();
                    lines.push(ui::DbgLine::plain(format!(
                        "bitplane at ${base:06X}, stride {stride} bytes ({} px), {rows} rows",
                        stride * 8
                    )));
                    bitmap = Some(ui::MemBitmapView {
                        base,
                        stride,
                        rows,
                        data: machine.debug_read_memory(base, stride * rows),
                    });
                } else {
                    lines.push(ui::DbgLine::plain(
                        "$ box: jump / \"ADDR VALUE\" poke / \"ADDR LEN\" save / hex bytes find",
                    ));
                    lines.push(ui::DbgLine::plain(""));
                    let base = panel.mem_addr & machine.ui_addr_mask() & !0xF;
                    for row in 0..16u32 {
                        let addr = base.wrapping_add(row * 16) & machine.ui_addr_mask();
                        let mut bytes = [0u8; 16];
                        for word in 0..8u32 {
                            let value = bus.peek_word_any(addr.wrapping_add(word * 2));
                            bytes[word as usize * 2] = (value >> 8) as u8;
                            bytes[word as usize * 2 + 1] = value as u8;
                        }
                        lines.push(ui::DbgLine::plain(ui::hex_dump_row(addr, &bytes)));
                    }
                }
            }
            ui::DebugTab::IoMap => {
                const ROWS: usize = 26;
                const COLS: usize = 3;
                const PER_PAGE: usize = ROWS * COLS;
                let sel = usize::from(panel.iomap_sel & 0x1FE) / 2;
                let page = sel / PER_PAGE;
                lines.push(ui::DbgLine::plain(format!(
                    "custom registers $DFF000-$DFF1FE  (page {}/{}; arrows/wheel move, $ box jumps)",
                    page + 1,
                    256usize.div_ceil(PER_PAGE)
                )));
                lines.push(ui::DbgLine::plain(""));
                for row in 0..ROWS {
                    let mut text = String::new();
                    let mut row_has_sel = false;
                    for col in 0..COLS {
                        let idx = page * PER_PAGE + col * ROWS + row;
                        if idx >= 256 {
                            continue;
                        }
                        let off = (idx * 2) as u16;
                        let value = bus
                            .debug_custom_word(off)
                            .map(|v| format!("{v:04X}"))
                            .unwrap_or_else(|| "----".to_string());
                        let cursor = if idx == sel {
                            row_has_sel = true;
                            '>'
                        } else {
                            ' '
                        };
                        text.push_str(&format!(
                            "{cursor}{off:03X} {:<8} {value}   ",
                            crate::debugger::custom_reg_name(off)
                        ));
                    }
                    let text = text.trim_end().to_string();
                    lines.push(if row_has_sel {
                        ui::DbgLine::hilit(text)
                    } else {
                        ui::DbgLine::plain(text)
                    });
                }
                lines.push(ui::DbgLine::plain(""));
                let off = panel.iomap_sel & 0x1FE;
                let value = bus.debug_custom_word(off);
                lines.push(ui::DbgLine::hilit(format!(
                    "${off:03X} {} = {}",
                    crate::debugger::custom_reg_name(off),
                    value
                        .map(|v| format!("${v:04X}"))
                        .unwrap_or_else(|| "(no latch)".to_string())
                )));
                if let Some(value) = value {
                    for line in crate::debugger::custom_reg_bit_decode(off, value) {
                        lines.push(ui::DbgLine::plain(format!("  {line}")));
                    }
                }
            }
            ui::DebugTab::Break => {
                // Leave room for the toggle buttons drawn at the top of
                // the content area.
                for _ in 0..ui::BREAK_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                if let Some(stop) = &self.last_debug_stop {
                    lines.push(ui::DbgLine::hilit(format!("Stopped: {stop}")));
                    lines.push(ui::DbgLine::plain(""));
                }
                lines.push(ui::DbgLine::plain(
                    "Type a hex address in the $ box, then a toggle button.",
                ));
                lines.push(ui::DbgLine::plain(
                    "Reg takes a custom-register offset (96) or address (DFF096).",
                ));
                lines.push(ui::DbgLine::plain(
                    "Break cond: ADDR [LHS OP RHS] [IGN N]  e.g. C033C2 D0 EQ 5",
                ));
                lines.push(ui::DbgLine::plain(
                    "  ops EQ NE LT GT LE GE AND; operand Dn An PC SR Mhex hex",
                ));
                lines.push(ui::DbgLine::plain(
                    "Beam takes decimal \"VPOS [HPOS]\" (stop when the beam gets there).",
                ));
                lines.push(ui::DbgLine::plain(
                    "Catch takes \"irq N\", \"trap N\", or \"vec N\" (stop entering the vector).",
                ));
                lines.push(ui::DbgLine::plain(""));
                let breaks = self.emu.machine.ui_breaks();
                lines.push(ui::DbgLine::plain("Breakpoints:"));
                if breaks.breakpoints.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for bp in &breaks.breakpoints {
                    let mut text = format!("  ${:06X}", bp.addr);
                    if let Some(cond) = &bp.cond {
                        text.push_str(&format!("  {}", cond.describe()));
                    }
                    if bp.ignore > 0 {
                        text.push_str(&format!("  ign {}/{}", bp.hits, bp.ignore));
                    }
                    lines.push(ui::DbgLine::plain(text));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Watchpoints (word, stop on change):"));
                if breaks.watches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for watch in &breaks.watches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  ${:06X}  now {:04X}{}",
                        watch.addr,
                        bus.peek_word_any(watch.addr),
                        watch
                            .filter
                            .map(|f| format!("  [{} only]", f.label()))
                            .unwrap_or_default()
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Register watches (stop on write):"));
                if breaks.reg_watches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for off in &breaks.reg_watches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  {} (${off:03X})",
                        crate::debugger::custom_reg_name(*off)
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Exception catchpoints (stop entering the vector):",
                ));
                if breaks.catches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for vector in &breaks.catches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  {} (vector {vector})",
                        crate::debugger::exception_vector_name(*vector)
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Copper breakpoints (set on Copper tab):",
                ));
                let cbreaks = bus.ui_copper_breaks();
                if cbreaks.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for addr in cbreaks {
                    lines.push(ui::DbgLine::plain(format!("  ${addr:06X}")));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Beam traps (stop at beam position):"));
                let beam_traps = bus.ui_beam_traps();
                if beam_traps.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for trap in beam_traps {
                    let mut text = format!("  v{}", trap.vpos);
                    if let Some(hpos) = trap.hpos {
                        text.push_str(&format!(" h{hpos}"));
                    } else {
                        text.push_str(" (line start)");
                    }
                    if trap.once {
                        text.push_str("  once");
                    }
                    lines.push(ui::DbgLine::plain(text));
                }
            }
            ui::DebugTab::Waveform => {
                // Leave room for the Arm/Stop buttons drawn at the top of
                // the content area.
                for _ in 0..ui::WAVEFORM_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                match self.emu.machine.ui_wave_status() {
                    Some(status) => {
                        for (index, text) in
                            console::wave_status_lines(&status).into_iter().enumerate()
                        {
                            lines.push(if index == 0 {
                                ui::DbgLine::hilit(text)
                            } else {
                                ui::DbgLine::plain(text)
                            });
                        }
                    }
                    None => lines.push(ui::DbgLine::plain("No waveform capture.")),
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Arm records chipset signals to a VCD file for GTKWave.",
                ));
                lines.push(ui::DbgLine::plain(
                    "Type an order-free spec in the box, then Arm. Empty = defaults",
                ));
                lines.push(ui::DbgLine::plain(
                    "(trigger now, 1 frame, all signals, copperline-wave-*.vcd).",
                ));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Trigger:  NOW  PC=ADDR  BEAM=VPOS[:HPOS]  REG=OFF  TIME=SECS",
                ));
                lines.push(ui::DbgLine::plain(
                    "Duration: Ncck (bare N)  Nf (frames)  Nms  Ns",
                ));
                lines.push(ui::DbgLine::plain(
                    "Signals:  comma list of beam,bus,cpu,copper,blitter,regs,irq,audio",
                ));
                lines.push(ui::DbgLine::plain(
                    "Path:     any other token (e.g. OUT.VCD)",
                ));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Example:  OUT.VCD PC=C033C2 20000CCK CPU,BUS,COPPER",
                ));
                lines.push(ui::DbgLine::plain(
                    "The console WAVE command does the same (Cmd/Alt+K).",
                ));
            }
        }
        // Keep lines inside the panel; the blitter clips at the texture
        // edge, not the panel edge.
        for line in &mut lines {
            if line.text.len() > 82 {
                line.text.truncate(82);
            }
        }
        ui::DebuggerView {
            running: !self.paused,
            reverse_available: self.emu.time_travel_enabled(),
            status,
            lines,
            bitmap,
            video,
            audio,
        }
    }

    /// Pick one or more disk images for a drive. The selection replaces
    /// the drive's swap playlist; the first image is inserted right away
    /// and the rest are queued for the swap button / shortcut.
    fn load_drive_disks_from_dialog(&mut self, drive_idx: usize) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title(format!("Load DF{drive_idx} disk image(s)"))
            .add_filter("Amiga disk images", crate::floppy::IMAGE_EXTENSIONS)
            .pick_files();

        // The modal file dialog blocks this (the main/emulation) thread, so
        // wall-clock time advanced while emulated time stood still. Re-baseline
        // the pacing anchor whether or not a file was chosen, otherwise the
        // pacer would fast-forward to catch up and corrupt pacing for the
        // freshly inserted disk. insert_disk_image -> bus floppy
        // insert_disk_image already asserts the disk-change/eject signal.
        if let Some(paths) = picked {
            self.insert_disk_playlist(drive_idx, paths);
        }
        self.finish_host_io_pause();
    }

    /// Replace a drive's swap playlist with `paths` and insert the first
    /// image, with the standard OSD. Shared by the load dialog and window
    /// drops.
    fn insert_disk_playlist(&mut self, drive_idx: usize, paths: Vec<PathBuf>) {
        let Some(path) = paths.first().cloned() else {
            return;
        };
        let count = paths.len();
        self.disk_playlists[drive_idx] = paths;
        self.disk_playlist_index[drive_idx] = 0;
        let name = display_file_name(&path);
        if self.insert_disk_image(drive_idx, path, self.disk_write_protected[drive_idx]) {
            if count > 1 {
                self.show_osd(format!("DF{drive_idx}: {name} (1/{count})"));
            } else {
                self.show_osd(format!("DF{drive_idx}: {name}"));
            }
        } else {
            self.show_osd(format!("DF{drive_idx}: load failed (see log)"));
        }
    }

    /// Advance the disk-swap playlist of the first drive that has more
    /// than one image queued (the disk-swap shortcut). With no multi-disk
    /// drive, just shows a notice.
    fn cycle_disk(&mut self) {
        let Some(drive) =
            (0..self.disk_playlists.len()).find(|&idx| self.disk_playlists[idx].len() > 1)
        else {
            self.show_osd("No alternate disk configured");
            return;
        };
        self.swap_drive_disk(drive);
    }

    /// Insert the next disk in a drive's swap playlist, wrapping around,
    /// and flash the new filename on screen.
    fn swap_drive_disk(&mut self, drive_idx: usize) {
        let count = self.disk_playlists[drive_idx].len();
        if count < 2 {
            self.show_osd(format!("DF{drive_idx}: no other disk queued"));
            return;
        }
        let next = (self.disk_playlist_index[drive_idx] + 1) % count;
        let path = self.disk_playlists[drive_idx][next].clone();
        let write_protected = self.disk_write_protected[drive_idx];
        self.disk_playlist_index[drive_idx] = next;
        let name = display_file_name(&path);
        if self.insert_disk_image(drive_idx, path, write_protected) {
            self.show_osd(format!("DF{drive_idx}: {name} ({}/{count})", next + 1));
        } else {
            self.show_osd(format!("DF{drive_idx}: swap failed (see log)"));
        }
    }

    fn eject_drive_disk(&mut self, drive_idx: usize) {
        if !self.emu.bus().floppy.disk_inserted(drive_idx) {
            self.show_osd(format!("DF{drive_idx}: no disk"));
            return;
        }
        match self.emu.bus_mut().floppy.eject_disk_image(drive_idx) {
            Ok(()) => {
                info!("floppy.df{drive_idx} ejected");
                self.show_osd(format!("DF{drive_idx}: ejected"));
                self.request_redraw();
            }
            Err(e) => warn!("floppy.df{drive_idx} eject failed: {e:#}"),
        }
    }

    /// Pick a CD image and mount it with the media-change notification,
    /// ejecting any current disc first.
    fn load_cd_from_dialog(&mut self) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load CD image")
            .add_filter("CD images", &["cue", "iso", "chd"])
            .pick_file();

        // Re-baseline pacing after the modal dialog, as for floppies.
        if let Some(path) = picked {
            self.insert_cd_image_from_path(&path);
        }
        self.finish_host_io_pause();
    }

    /// Mount a CD image with the media-change notification, ejecting any
    /// current disc first. Shared by the load dialog, window drops, and
    /// scheduled `--insert-cd-after` events.
    fn insert_cd_image_from_path(&mut self, path: &std::path::Path) {
        match crate::cdrom::CdImage::load(path) {
            Ok(image) => {
                info!("cd image: {} ({})", path.display(), image.describe());
                self.emu.bus_mut().cd_insert_disc(image, path);
                self.show_osd(format!("CD: {}", display_file_name(path)));
                self.request_redraw();
            }
            Err(e) => {
                warn!("cd image load failed ({}): {e:#}", path.display());
                self.show_osd("CD: load failed (see log)");
            }
        }
    }

    fn eject_cd(&mut self) {
        if !self.emu.bus().cd_disc_inserted() {
            self.show_osd("CD: no disc");
            return;
        }
        self.emu.bus_mut().cd_eject_disc();
        self.show_osd("CD: ejected");
        self.request_redraw();
    }

    /// Route files dropped on the window: floppy images to a drive
    /// (directly, or via the chooser panel when several drives could take
    /// them), a CD image (cue/iso/chd) to the CD drive, and everything else to
    /// an explanatory notice. winit reports drops with no cursor position,
    /// so the target drive can only be picked after the fact.
    fn handle_dropped_files(&mut self, files: Vec<PathBuf>) {
        // The configuration screen runs on a placeholder machine: an insert
        // would target hardware the launcher is about to rebuild, and the
        // chooser would replace the launcher panel and its unsaved state.
        // A WHDLoad package is configuration rather than media, so it lands
        // in the setup's Game field exactly as the WHDLoad page's Browse
        // would.
        if matches!(self.ui.panel, Some(Panel::Launcher(_))) {
            let mut refused = false;
            for path in files {
                if classify_dropped_media(&path) == DroppedMediaKind::WhdloadGame {
                    let path = whdload_game_config_path(path);
                    let name = display_file_name(&path);
                    if let Some(state) = self.launcher_state_mut() {
                        state.edit_cancel();
                        state.setup.set_path(LauncherField::WhdloadGame, path);
                        state.status = Some(StatusMessage::ok(format!("WHDLoad game: {name}")));
                    }
                } else {
                    refused = true;
                }
            }
            if refused {
                self.show_osd("Close the machine screen to drop disks");
            }
            return;
        }
        let mut floppies: Vec<PathBuf> = Vec::new();
        let mut cd: Option<PathBuf> = None;
        let mut whdload: Option<PathBuf> = None;
        let mut notice: Option<&'static str> = None;
        for path in files {
            match classify_dropped_media(&path) {
                DroppedMediaKind::Floppy => floppies.push(path),
                // One disc tray; the first CD image wins.
                DroppedMediaKind::Cd => cd = cd.or(Some(path)),
                // One machine to reboot; the first game wins.
                DroppedMediaKind::WhdloadGame => whdload = whdload.or(Some(path)),
                DroppedMediaKind::HardDisk => {
                    notice = Some("Hard disks are configured in the machine screen");
                }
                DroppedMediaKind::Rom => {
                    notice = Some("Kickstart ROMs are configured in the machine screen");
                }
            }
        }
        let mut handled = false;
        if let Some(path) = whdload {
            self.boot_whdload_game(whdload_game_config_path(path));
            handled = true;
        }
        if let Some(path) = cd {
            if self.emu.bus().cd_drive_present() {
                self.insert_cd_image_from_path(&path);
            } else {
                self.show_osd("No CD drive on this machine");
            }
            handled = true;
        }
        if !floppies.is_empty() {
            let connected: Vec<usize> = (0..4)
                .filter(|&idx| self.emu.bus().floppy.drive_connected(idx))
                .collect();
            match connected.len() {
                0 => self.show_osd("No floppy drive connected"),
                1 => self.insert_disk_playlist(connected[0], floppies),
                _ => {
                    // The chooser takes the panel slot; an open menu or an
                    // informational panel (About, Shortcuts...) yields to it.
                    self.ui.menu_open = false;
                    if self.ui.panel.is_some() {
                        self.close_panel();
                    }
                    self.open_drop_chooser(floppies, connected);
                }
            }
            handled = true;
        }
        if !handled {
            if let Some(text) = notice {
                self.show_osd(text);
            }
        }
    }

    /// Reboot into a dropped WHDLoad package: stage it against the session's
    /// own configuration (explicit machine, ROM, and memory choices there
    /// still win over the WHDLoad derivation, exactly as on the command
    /// line) and swap the running machine for the staged one, as the
    /// configuration screen's Run does. The dropped game lands in
    /// `[whdload] game` on the remembered config, so a reopened
    /// configuration screen (and a save) carries it, while the derived
    /// machine and mounts stay out of it.
    fn boot_whdload_game(&mut self, game: PathBuf) {
        let mut raw = self.machine_config.clone();
        raw.whdload.game = Some(game.to_string_lossy().into_owned());
        let name = display_file_name(&game);
        match self.stage_and_run(raw) {
            Ok(()) => self.show_osd(format!("WHDLoad: {name}")),
            Err(e) => {
                warn!("whdload boot failed ({}): {e:#}", game.display());
                self.show_osd(format!("WHDLoad failed: {}", short_status_error(&e)));
            }
        }
    }

    /// Open the modal drive chooser for dropped floppy images. Drive labels
    /// are snapshotted now; the panel is modal, so they cannot go stale
    /// under it.
    fn open_drop_chooser(&mut self, disks: Vec<PathBuf>, connected: Vec<usize>) {
        let floppy = &self.emu.bus().floppy;
        let drives = connected
            .into_iter()
            .map(|drive| {
                let label = match floppy.inserted_disk_name(drive) {
                    Some(name) => format!("DF{drive}: {name}"),
                    None => format!("DF{drive} (empty)"),
                };
                ui::DropDriveEntry { drive, label }
            })
            .collect();
        let disk_label = display_file_name(&disks[0]);
        self.ui.panel = Some(Panel::DropChooser(ui::DropChooserState {
            disks,
            disk_label,
            drives,
        }));
        self.request_redraw();
    }

    /// Chooser click or digit key: insert the pending dropped disks into
    /// the picked drive and close the panel.
    fn drop_chooser_route(&mut self, drive_idx: usize) {
        let state = match self.ui.panel.take() {
            Some(Panel::DropChooser(state)) => state,
            other => {
                self.ui.panel = other;
                return;
            }
        };
        self.insert_disk_playlist(drive_idx, state.disks);
        self.request_redraw();
    }

    fn insert_disk_image(
        &mut self,
        drive_idx: usize,
        path: PathBuf,
        write_protected: bool,
    ) -> bool {
        self.suspend_live_audio_for_host_io();
        let result = match self.emu.bus_mut().floppy.insert_disk_image(
            drive_idx,
            path.clone(),
            write_protected,
        ) {
            Ok(()) => {
                self.last_fdd_track = None;
                info!("floppy.df{} inserted {}", drive_idx, path.display());
                if let Some(rec) = self.input_recorder.as_mut() {
                    rec.record_disk_insert(drive_idx, &path, self.emu.bus().emulated_seconds());
                }
                // Reverse-debug: mark the media change so replay across it warns
                // (the inserted image is host-file state, not in the log).
                self.emu
                    .tt_note_input(crate::inputsched::ReplayAction::DiskChange);
                self.request_redraw();
                true
            }
            Err(e) => {
                warn!(
                    "floppy.df{} insert failed ({}): {e:#}",
                    drive_idx,
                    path.display()
                );
                false
            }
        };
        self.finish_host_io_pause();
        result
    }

    fn suspend_live_audio_for_host_io(&mut self) {
        self.emu.set_live_audio_suspended(true);
    }

    /// Whether a state is being loaded over the pre-boot placeholder machine
    /// that hosts the configuration screen: powered off, the launcher panel
    /// open, and the silent NullSink still installed. Only then does a load need
    /// to install a real audio output; every normal running session already has
    /// one. Evaluate this before powering on / dismissing the launcher.
    fn restoring_over_placeholder(&self) -> bool {
        !self.powered_on
            && matches!(self.ui.panel, Some(Panel::Launcher(_)))
            && self.emu.bus().paula.audio.is_null_sink()
    }

    /// Replace the placeholder machine's silent NullSink with a live host audio
    /// output after a save state is loaded over the configuration screen. This
    /// mirrors the launcher Run path (`launcher_run`): the configuration screen
    /// itself stays silent, but a machine started from it -- by Run or by a state
    /// load -- gets real sound. On audio-init failure the state stays loaded and
    /// the machine simply runs without sound, exactly as a failed Run does.
    fn install_live_audio_after_placeholder_load(&mut self) {
        match crate::audio::open_output_sink(
            crate::priority::requested(self.realtime_priority),
            &self.audio_output,
        ) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio = sink;
                // Apply the current suspension state to the freshly installed
                // stream (it should be live now: powered on and not paused).
                self.sync_live_audio_suspension();
            }
            Err(e) => {
                warn!("audio init after state load failed; continuing without sound: {e:#}");
            }
        }
    }

    /// If the live output device was lost mid-run (unplugged, or the system
    /// default switched away), rebuild the sink on the current default output and
    /// reset the session's selected device to Default (so the runtime menu shows
    /// it) so sound continues. The cpal error callback only flags the loss; the
    /// stream is rebuilt here on the main thread, where creating a (macOS
    /// `!Send`) cpal stream is allowed. Falls back to a silent sink if no device
    /// can be opened, so this never spins retrying a dead machine.
    fn recover_audio_if_device_lost(&mut self) {
        if !self.emu.bus().paula.audio.device_lost() {
            return;
        }
        warn!("audio: output device lost; falling back to the default output device");
        // The named device is gone, so the session is back on the default; reset
        // the selection so the runtime menu reflects "Default" too. (A disabled
        // sink never reports a lost device, so we can only get here from a device.)
        self.audio_output = crate::audio::AudioOutput::Default;
        // Reopen on the system default, not the previously named device, which
        // is the one that went away.
        match CpalSink::new(crate::priority::requested(self.realtime_priority), None) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio = Box::new(sink);
                self.sync_live_audio_suspension();
                self.show_osd("Audio device lost! Switched to Default".to_string());
            }
            Err(e) => {
                warn!("audio: no fallback output device; continuing without sound: {e:#}");
                self.emu.bus_mut().paula.audio = Box::new(crate::audio::NullSink);
            }
        }
    }

    fn finish_host_io_pause(&mut self) {
        self.emu.reanchor_realtime_clock();
        self.sync_live_audio_suspension();
    }

    fn sync_live_audio_suspension(&mut self) {
        let suspended = !self.powered_on || self.cpu_halted || self.paused;
        self.emu.set_live_audio_suspended(suspended);
    }

    /// Resize the presentation surface to a new window size. Shared by the
    /// Resized event and by the synchronous path of request_inner_size (see
    /// snap_window_to_canvas), which on some backends returns the applied
    /// size instead of delivering an event.
    fn apply_surface_size(&mut self, size: PhysicalSize<u32>) {
        if let Some(r) = self.render.as_mut() {
            // A zero-sized resize is minimization (Windows reports the
            // minimized client area as 0x0). Leave the surface untouched and
            // stop rendering until the restore delivers a nonzero size (see
            // Render::minimized).
            r.minimized = size.width == 0 || size.height == 0;
            if r.minimized {
                return;
            }
            // The integer fit (and with it the supersample factor) follows
            // the surface size, so re-plan for the new one; the surface
            // resize below is what recomputes the scaling matrix and clip
            // rect from it.
            if let Err(e) = sync_main_present_scaling(r, (size.width, size.height)) {
                warn!("resize texture buffer for new surface size failed: {e}");
            }
            if let Err(e) = r.resize_surface(size) {
                warn!("resize surface failed: {e}");
            }
        }
        // Resizing the surface discards its contents, leaving it blank (white)
        // until the next present. When the machine is powered off (or paused)
        // the event loop is in Wait mode and produces no frames, so without an
        // explicit repaint here the window can sit white after the
        // scale-factor/resize event that macOS delivers right after window
        // creation.
        self.request_redraw();
    }

    /// Bring the surface up to the host window's current size before drawing,
    /// when a resize has not reached us as a Resized event yet.
    ///
    /// `pixels` reconfigures its swapchain from the size the last
    /// `resize_surface` gave it, and its render retries the acquire in an
    /// unbounded loop: on a driver that rejects a swapchain whose extent
    /// disagrees with the window (Mesa's X11 Vulkan WSI returns
    /// VK_ERROR_OUT_OF_DATE_KHR), a stale size makes that loop rebuild the
    /// swapchain forever instead of hanging on to a wrongly-scaled frame.
    /// Rendering runs inside the event callback, so the loop also starves the
    /// Resized event that would have corrected the size: the window never
    /// comes back, and the churn goes on until the display server runs the
    /// client out of resource ids. Entering or leaving fullscreen is the
    /// common way in, the window manager resizing the window a moment before
    /// the event reaches us (issue #362, upstream parasyte/pixels#460).
    fn resync_surface_size(&mut self) {
        let Some(r) = self.render.as_ref() else {
            return;
        };
        let Some(size) = surface_resize_for_draw(r.surface_size, r.window.inner_size()) else {
            return;
        };
        self.apply_surface_size(size);
    }

    /// Tool-window counterpart of `resync_surface_size`, for the same reason:
    /// these windows are freely resizable too.
    fn resync_tool_surface_size(&mut self, kind: ToolPanelKind) {
        let Some(tool) = self.tool_window(kind) else {
            return;
        };
        let Some(size) = surface_resize_for_draw(tool.surface_size, tool.window.inner_size())
        else {
            return;
        };
        self.apply_tool_surface_size(kind, size);
    }

    /// Tool-window counterpart of `apply_surface_size`, shared by that window's
    /// Resized event and the synchronous `request_inner_size` path.
    fn apply_tool_surface_size(&mut self, kind: ToolPanelKind, size: PhysicalSize<u32>) {
        if let Some(tool) = self.tool_window_mut(kind) {
            // Same minimized-present deadlock guard as the main window.
            tool.minimized = size.width == 0 || size.height == 0;
            if tool.minimized {
                return;
            }
            let _ = tool.resize_surface(size);
        }
        self.request_redraw();
    }

    /// Size the window to the presentation canvas, unless it is fullscreen: the
    /// request resizes nothing there and instead shrinks the drawable into a
    /// corner (macOS and Windows; Linux window managers ignore it), so leave the
    /// display-sized surface alone and let the presentation scale into it.
    ///
    /// Only the two things that change the canvas height -- the pixel aspect
    /// and the status bar -- call this, and only for a window still at the old
    /// canvas size. Nothing else may take a window the user has sized.
    ///
    /// `request_inner_size` is only asynchronous when it returns `None`. Wayland
    /// applies the resize client-side and returns the new size with no `Resized`
    /// event to follow, so the surface must be resized here or the stale extent
    /// misplaces every click through `cursor_texture_position`.
    fn snap_window_to_canvas(&mut self) {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        if window.fullscreen().is_some() {
            return;
        }
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        if let Some(applied) = window.request_inner_size(size) {
            self.apply_surface_size(applied);
            // This backend applied the request synchronously, so no Resized
            // event remains to consume it.
            self.snap_request_deadline = None;
            self.window_manually_sized = false;
        } else {
            self.snap_request_deadline = Some(Instant::now() + CANVAS_SNAP_RESPONSE_TIMEOUT);
        }
    }

    /// Follow a canvas-height change with the window, or arrange to when
    /// fullscreen gives the window back.
    ///
    /// `was_canvas_sized` is the verdict taken before the change, so a
    /// window the user has resized keeps its size. Fullscreen is the other
    /// way a window is not the canvas's to size, and there the snap cannot
    /// happen now: the request resizes nothing, and on the way out the
    /// window returns at the *old* canvas's size, which would then be read
    /// as a drag and strand it letterboxed for the rest of the run. So it
    /// is remembered and taken when the window is the canvas's again.
    fn follow_canvas_change(&mut self, was_canvas_sized: bool) {
        if was_canvas_sized {
            self.snap_window_to_canvas();
        } else if !self.window_manually_sized {
            // Not the user's, so only fullscreen can be holding it.
            self.snap_when_windowed = true;
        }
    }

    /// Classify a resize of the main window: the user's own drag, or the
    /// window following a canvas change.
    ///
    /// A snap asks for the canvas size; what comes back may be clamped by
    /// the platform or rounded by the scale factor, and that near miss must
    /// not read as a drag or the window stops following the canvas for the
    /// rest of the run. A drag onto the canvas size hands it back.
    fn note_window_resize(&mut self, size: PhysicalSize<u32>) {
        // Read what is needed and let the borrow go: a drag delivers these
        // continuously, so this takes nothing it has to hold on to.
        let Some((fullscreen, scale)) = self
            .render
            .as_ref()
            .map(|r| (r.window.fullscreen().is_some(), r.window.scale_factor()))
        else {
            return;
        };
        // Fullscreen sizes the window itself; leave the standing verdict.
        if fullscreen {
            return;
        }
        // The window is back from fullscreen with a canvas change owing:
        // this size is the old canvas's, not a drag. Take the snap instead
        // of classifying it, or the stale size is what gets remembered.
        if std::mem::take(&mut self.snap_when_windowed) {
            self.snap_window_to_canvas();
            return;
        }
        let logical_w = f64::from(size.width) / scale;
        let logical_h = f64::from(size.height) / scale;
        if resize_is_canvas_owned(
            &mut self.snap_request_deadline,
            Instant::now(),
            logical_w,
            logical_h,
            window_present_height(),
        ) {
            self.window_manually_sized = false;
            return;
        }
        self.window_manually_sized = true;
    }

    /// Whether the main window still belongs to the canvas rather than to the
    /// user -- i.e. it has not been manually resized (fullscreen counts as
    /// resized). Lets a canvas change snap an untouched window to the new
    /// size while leaving a resized one alone.
    fn window_is_canvas_sized(&self) -> bool {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return false;
        };
        if window.fullscreen().is_some() {
            return false;
        }
        !self.window_manually_sized
    }

    /// Cmd/Alt+M: turn the monitor bezel off, or back on to whichever
    /// front was last chosen. Picking a style is the menu's job; this is
    /// the on-off for the one already picked, so it never changes which.
    fn toggle_bezel(&mut self) {
        let style = if self.bezel.is_on() {
            BezelStyle::None
        } else {
            self.bezel_last
        };
        self.set_bezel(style);
    }

    /// Draw a given monitor front for the rest of the run (the config file
    /// default is unchanged; set `[display] bezel` to make it stick).
    fn set_bezel(&mut self, style: BezelStyle) {
        self.bezel = style;
        if style.is_on() {
            self.bezel_last = style;
        }
        info!("monitor bezel: {}", style.label());
        self.show_osd(format!("Monitor bezel: {}", style.menu_label()));
        self.request_redraw();
    }

    /// Cmd/Alt+P: toggle the performance overlay for the rest of the run
    /// (the config file default is unchanged; set `[display] perf_overlay`
    /// to make it stick).
    fn toggle_perf_overlay(&mut self) {
        self.perf_overlay = !self.perf_overlay;
        self.perf = PerfOverlay::default();
        let label = if self.perf_overlay { "on" } else { "off" };
        info!("performance overlay: {label}");
        self.show_osd(format!("Performance overlay: {label}"));
        self.request_redraw();
    }

    /// Resample the performance overlay counters and reformat its lines
    /// when the interval has elapsed. A run-state flip (pause, power-off,
    /// halt) publishes the idle readout immediately and re-baselines, so
    /// rates are never computed across the boundary.
    fn update_perf_overlay(&mut self, running: bool) {
        if !self.perf_overlay {
            return;
        }
        let now = Instant::now();
        if let Some(base) = &self.perf.baseline {
            if base.running == running && now.duration_since(base.at) < PERF_SAMPLE_INTERVAL {
                return;
            }
        }
        let audio = self.emu.bus().live_audio_status();
        let counters = self.emu.perf_counters();
        let current = PerfBaseline {
            at: now,
            running,
            emulated_frames: self.emu.bus().emulated_frames(),
            emulated_seconds: self.emu.bus().emulated_seconds(),
            busy: counters.busy,
            audio_underrun_frames: audio.callback_underrun_frames,
        };
        let audio_lead_ms = audio.output_lead_seconds * 1000.0;
        let readout = match &self.perf.baseline {
            Some(base) if base.running == running && running => {
                perf_readout(base, &current, audio_lead_ms, counters.pacer_slips)
            }
            // First sample after enabling, a run-state flip, or an idle
            // machine: rates are zero by definition, only the levels show.
            _ => PerfReadout {
                audio_lead_ms,
                pacer_slips: counters.pacer_slips,
                ..Default::default()
            },
        };
        self.perf.baseline = Some(current);
        let lines = perf_overlay_lines(&readout);
        if lines != self.perf.lines {
            self.perf.lines = lines;
            self.perf.revision = self.perf.revision.wrapping_add(1);
        }
    }

    /// Install a screen tint and its presentation table together.
    fn set_tint(&mut self, tint: crate::config::Tint) {
        self.tint = tint;
        self.tint_lut = tint_lut(tint);
    }

    /// Compile the configured user shader against the live device. The full
    /// message goes to the log; the returned one-line summary is for the
    /// caller to fold into whatever overlay it is already showing. A failure
    /// leaves no pipeline, so the caller falls back to no shader rather than
    /// to a stale one.
    fn reload_custom_shader(&mut self) -> Result<(), String> {
        let fail = |msg: String| {
            error!("[display] shader: {msg}");
            Err(msg.lines().next().unwrap_or_default().to_string())
        };
        let Some(path) = self.custom_shader_path.clone() else {
            return fail("no custom shader configured".to_string());
        };
        let Some(r) = self.render.as_mut() else {
            return fail(format!(
                "cannot load shader {} before the window exists",
                path.display()
            ));
        };
        let format = r.pixels.render_texture_format();
        match r.crt_shader.load_custom(r.pixels.device(), format, &path) {
            Ok(()) => Ok(()),
            Err(msg) => fail(msg),
        }
    }

    /// Switch the presentation pixel aspect live: the canvas height (and
    /// with it the backing texture and the window) changes between the
    /// 4:3 and the square-pixel size, so the texture must be rebuilt like
    /// a DPI change (see resync_render_scale) and the window re-sized.
    fn apply_pixel_aspect(&mut self, aspect: PixelAspect) {
        if aspect == super::pixel_aspect() {
            return;
        }
        // A video recording's frame size is fixed when the encoder is
        // created; refuse to change the presentation under it.
        if self.recorder.is_some() {
            self.show_osd("Stop the video recording before changing pixel aspect");
            return;
        }
        // Decide before the change (it feeds window_present_height) whether the
        // window is still canvas-sized, so a manual resize survives.
        let was_canvas_sized = self.window_is_canvas_sized();
        super::set_pixel_aspect(aspect);
        if let Some(r) = self.render.as_mut() {
            // The canvas height changes with the aspect, so re-plan: the
            // integer fit (and its supersample factor) is re-decided for the
            // new canvas, and the texture resized to it.
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                warn!("resize texture buffer for pixel aspect failed: {e}");
            }
        }
        // Tool windows share the canvas-sized texture layout (panel
        // centring reads the live canvas height), so their buffers and
        // windows must follow the new size too.
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        for kind in ToolPanelKind::ALL {
            let mut applied = None;
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for pixel aspect failed: {e}");
                }
                applied = tool.window.request_inner_size(size);
            }
            // Synchronous on Wayland, with no Resized event to follow.
            if let Some(applied) = applied {
                self.apply_tool_surface_size(kind, applied);
            }
        }
        self.follow_canvas_change(was_canvas_sized);
        self.request_redraw();
    }

    /// Switch how the presentation canvas is scaled into the window live.
    ///
    /// The canvas itself never changes -- integer mode may re-render it at a
    /// different supersample factor, but its pixel content, the window size
    /// and a video recording (whose frames are the 1x canvas, averaged down
    /// like any supersample) all carry on -- so unlike a pixel-aspect switch
    /// there is no recording to refuse and no window to re-size.
    fn apply_display_scaling(&mut self, scaling: DisplayScaling) {
        if scaling == super::display_scaling() {
            return;
        }
        super::set_display_scaling(scaling);
        if let Some(r) = self.render.as_mut() {
            // A minimized window has no surface to re-plan against; the
            // Resized event that restores it re-plans itself.
            if !r.minimized {
                let size = r.window.inner_size();
                if let Err(e) = sync_main_present_scaling(r, (size.width, size.height)) {
                    warn!("resize texture buffer for display scaling failed: {e}");
                }
                // The sync only stores the mode (and resizes the texture), so
                // re-apply the current surface size: that is what recomputes
                // the scaling matrix and the clip rect the cursor mapping,
                // the shader passes and the RTG pass all read.
                if let Err(e) = r.resize_surface(size) {
                    warn!("resize surface for display scaling failed: {e}");
                }
            }
        }
        self.show_osd(format!("Scaling: {}", scaling.label()));
        self.request_redraw();
    }

    /// Show or hide the status bar. An untouched window resizes to gain or lose
    /// the bar's strip; a window the user has manually resized keeps its size
    /// (and fullscreen keeps its size too), with the display reflowing to fit --
    /// the presentation already letterboxes any window shape. Only the display
    /// is recorded, so a recording is unaffected. Bound to the shortcut and menu.
    fn toggle_status_bar(&mut self) {
        // Decide before the flag flips (it feeds window_present_height) whether
        // the window is still canvas-sized, so a manual resize survives.
        let was_canvas_sized = self.window_is_canvas_sized();
        let hidden = !super::status_bar_hidden();
        super::set_status_bar_hidden(hidden);
        if let Some(r) = self.render.as_mut() {
            // The canvas gains or loses the bar's strip, so re-plan: the
            // integer fit is re-decided for the new canvas height and the
            // texture resized to it (see apply_pixel_aspect).
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                // The draw helpers size themselves from the hidden flag, so a
                // failed resize must not commit the toggle: a taller canvas over
                // an unchanged, shorter buffer would index past it. Revert and
                // leave the flag and buffer consistent.
                warn!("resize texture buffer for status bar toggle failed: {e}");
                super::set_status_bar_hidden(!hidden);
                // The plan above was made for a canvas that never
                // materialised; re-plan for the one the flag went back to.
                let _ = sync_main_present_scaling(r, (surface.width, surface.height));
                return;
            }
        }
        // Every tool window (Debugger, Frame Analyzer, Console) draws through
        // draw_panel_layer, which indexes its buffer by the same canvas height
        // (window_present_height), so resize all their buffers to match too, or
        // a later tool draw could index past a now-too-small buffer. Buffer
        // only: unlike a pixel-aspect switch, leave a tool window's own size
        // alone.
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for status bar toggle failed: {e}");
                }
                tool.window.request_redraw();
            }
        }
        // Only snap an unresized window to the new canvas size; a resized window
        // keeps its dimensions and the display reflows into it.
        self.follow_canvas_change(was_canvas_sized);
        self.request_redraw();
        if hidden {
            self.show_osd(format!(
                "Status bar hidden ({HOST_SHORTCUT_MODIFIER_LABEL}+Shift+F restores)"
            ));
        } else {
            self.show_osd("Status bar restored");
        }
    }

    fn request_main_redraw(&self) {
        if let Some(render) = self.render.as_ref() {
            if !render.minimized {
                render.window.request_redraw();
            }
        }
    }

    fn request_redraw(&self) {
        self.request_main_redraw();
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window(kind) {
                if !tool.minimized {
                    tool.window.request_redraw();
                }
            }
        }
    }

    fn update_host_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
        if !modifiers.shift_key()
            && !raw_device_qualifier_family_held(
                &self.raw_device_held_rawkeys,
                AMIGA_RAWKEY_LEFT_SHIFT,
                AMIGA_RAWKEY_RIGHT_SHIFT,
            )
        {
            self.release_amiga_rawkey_if_held(AMIGA_RAWKEY_LEFT_SHIFT);
            self.release_amiga_rawkey_if_held(AMIGA_RAWKEY_RIGHT_SHIFT);
        }
        if !modifiers.alt_key()
            && !raw_device_qualifier_family_held(
                &self.raw_device_held_rawkeys,
                AMIGA_RAWKEY_LEFT_ALT,
                AMIGA_RAWKEY_RIGHT_ALT,
            )
        {
            self.release_amiga_rawkey_if_held(AMIGA_RAWKEY_LEFT_ALT);
            self.release_amiga_rawkey_if_held(AMIGA_RAWKEY_RIGHT_ALT);
        }
    }

    fn release_amiga_rawkey_if_held(&mut self, rawkey: u8) {
        if rawkey_is_held(&self.held_rawkeys, rawkey) {
            self.handle_amiga_key_event(rawkey, false);
        }
    }

    /// Whether the machine is being told `rawkey` is down -- by the host
    /// keyboard, by the on-screen one, or by both.
    fn amiga_rawkey_held(&self, rawkey: u8) -> bool {
        rawkey_is_held(&self.held_rawkeys, rawkey)
            || rawkey_is_held(&self.panel_held_rawkeys, rawkey)
    }

    /// A transition from the host keyboard.
    fn handle_amiga_key_event(&mut self, rawkey: u8, pressed: bool) {
        self.handle_amiga_key_event_from(KeySource::Host, rawkey, pressed);
    }

    /// A transition from `source`, reaching the machine only when it moves
    /// the aggregate held state (see [`KeySource`]).
    fn handle_amiga_key_event_from(&mut self, source: KeySource, rawkey: u8, pressed: bool) {
        let idx = rawkey_index(rawkey);
        let held = match source {
            KeySource::Host => &self.held_rawkeys,
            KeySource::Panel => &self.panel_held_rawkeys,
        };
        // Per-source: a winit auto-repeat re-presses a key this source
        // already has down, and a source can be told to let go of
        // something it never took.
        if rawkey_transition_is_duplicate(held, rawkey, pressed) {
            return;
        }
        let was_held = self.amiga_rawkey_held(rawkey);
        match source {
            KeySource::Host => self.held_rawkeys[idx] = pressed,
            KeySource::Panel => self.panel_held_rawkeys[idx] = pressed,
        }
        // The other source is holding the same key, so the aggregate did
        // not move: the machine already believes what this transition
        // would tell it, and a recorded or replayed copy of it would
        // reproduce the second holder rather than the keystroke.
        if was_held == self.amiga_rawkey_held(rawkey) {
            return;
        }

        // Ctrl+Amiga+Amiga is no longer consumed host-side: the chord
        // travels to the keyboard MCU like every other transition, and
        // the MCU runs the authentic $78 reset-warning / 500 ms KCLK
        // reset protocol.
        if let Some(rec) = self.input_recorder.as_mut() {
            rec.record_key(rawkey, pressed, self.emu.bus().emulated_seconds());
        }

        if pressed {
            self.emu.bus_mut().enqueue_key(rawkey);
        } else {
            self.emu.bus_mut().enqueue_key_event(rawkey, false);
        }
        // Reverse-debug: note the transition so replay can reproduce it.
        self.emu
            .tt_note_input(crate::inputsched::ReplayAction::Key { rawkey, pressed });
    }

    /// Start or stop the input recording (shortcut / menu item). On
    /// stop, the recorded session is written as a scripted-input file
    /// that `--script FILE` replays.
    fn toggle_input_recording(&mut self) {
        match self.input_recorder.take() {
            Some(rec) => {
                let events = rec.events_recorded();
                let script = rec.finish();
                let path = crate::inputrec::auto_filename();
                match std::fs::write(&path, script) {
                    Ok(()) => {
                        info!(
                            "input recording saved: {} ({events} events)",
                            path.display()
                        );
                        self.show_osd(format!(
                            "Saved {} ({events} events)",
                            display_file_name(&path)
                        ));
                    }
                    Err(e) => {
                        warn!("input recording save failed ({}): {e:#}", path.display());
                        self.show_osd("Input recording save failed (see log)");
                    }
                }
            }
            None => {
                let now = self.emu.bus().emulated_seconds();
                self.input_recorder = Some(crate::inputrec::InputRecorder::new(now));
                info!("input recording started at {now:.3}s emulated time");
                self.show_osd(format!(
                    "Recording input ({HOST_SHORTCUT_MODIFIER_LABEL}+Shift+R to stop)"
                ));
            }
        }
        self.request_redraw();
    }

    fn save_screenshot(&self, path: &std::path::Path) {
        // COPPERLINE_SHOT_RAW saves the raw woven framebuffer (716x570
        // for standard fields, the native scan height for programmable
        // modes): the presentation resampler blends adjacent lines, so
        // per-scanline forensics need the unscaled field.
        let src_rows = self.present_rows;
        let result = if self.rtg_present_dims.is_some() {
            // An RTG board's frame already has one presentation row per
            // board row: save it at that height, matching the control
            // protocol's capture, instead of scaling to the chipset glass.
            screenshot::save(
                path,
                &self.present_fb[..src_rows * self.present_width],
                self.present_width as u32,
                src_rows as u32,
            )
        } else {
            save_present_frame(
                path,
                &self.present_fb,
                src_rows,
                self.present_width,
                self.overscan,
                self.present_tv_aperture_rows,
            )
        };
        match result {
            Ok(()) => info!("screenshot saved: {}", path.display()),
            Err(e) => warn!("screenshot save failed ({}): {e:#}", path.display()),
        }
    }

    /// Interactive screenshot grab: save to an auto-named PNG and
    /// flash the filename on screen. The overlay is painted into the
    /// presentation texture after the frame is captured, so it never
    /// appears in the saved image.
    fn take_screenshot(&mut self) {
        self.finish_render_for_current_frame();
        let path = screenshot::auto_filename();
        self.save_screenshot(&path);
        self.show_osd(format!("Saved {}", display_file_name(&path)));
    }

    /// Show a transient overlay message over the display for
    /// [`OSD_DURATION`]. The message is cleared automatically; while it is
    /// visible the event loop keeps redrawing even when paused/idle so it
    /// fades on time.
    fn show_osd(&mut self, text: impl Into<String>) {
        self.osd = Some(Osd {
            text: text.into(),
            expires_at: Instant::now() + OSD_DURATION,
            warning: false,
        });
        self.request_redraw();
    }

    /// Say something that did not go as asked, in amber. Otherwise as
    /// [`Self::show_osd`].
    #[cfg(feature = "mt32")]
    fn warn_osd(&mut self, text: impl Into<String>) {
        self.osd = Some(Osd {
            text: text.into(),
            expires_at: Instant::now() + OSD_DURATION,
            warning: true,
        });
        self.request_redraw();
    }

    /// The overlay text to draw this frame, or None when nothing is
    /// active. Expired overlays are dropped as a side effect.
    fn active_osd_text(&mut self) -> Option<(String, bool)> {
        match &self.osd {
            Some(osd) if Instant::now() < osd.expires_at => Some((osd.text.clone(), osd.warning)),
            Some(_) => {
                self.osd = None;
                None
            }
            None => None,
        }
    }

    fn dump_frame_if_due(&mut self) -> bool {
        let Some(state) = self.frame_dump.as_ref() else {
            return false;
        };
        if self.emu.bus().emulated_seconds() < state.start_secs as f64 {
            return false;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        if state.last_saved_emulated_frame == Some(emulated_frame) {
            return false;
        }
        self.finish_render_for_current_frame();
        if self.last_rendered_emulated_frame != Some(emulated_frame) {
            return false;
        }

        let Some(state) = self.frame_dump.as_mut() else {
            return false;
        };
        let path = state.dir.join(format!("frame-{:06}.png", state.dumped));
        if crate::envcfg::flag("COPPERLINE_DUMP_RENDER_META") {
            log_frame_dump_metadata(state.dumped, &self.emu);
        }
        let src_rows = self.present_rows;
        let result = save_present_frame(
            &path,
            &self.present_fb,
            src_rows,
            self.present_width,
            self.overscan,
            self.present_tv_aperture_rows,
        );
        match result {
            Ok(()) => {
                state.last_saved_emulated_frame = Some(emulated_frame);
                state.dumped += 1;
                if state.dumped == 1 || state.dumped == state.count || state.dumped % 25 == 0 {
                    info!(
                        "frame dump: saved {}/{} ({})",
                        state.dumped,
                        state.count,
                        path.display()
                    );
                }
            }
            Err(e) => {
                warn!("frame dump failed ({}): {e:#}", path.display());
                self.frame_dump = None;
                return true;
            }
        }

        if state.dumped >= state.count {
            info!(
                "frame dump complete: saved {} frames to {}",
                state.count,
                state.dir.display()
            );
            self.emu.report_stats();
            self.emu.bus().poll_stats.dump_top("at frame dump");
            self.frame_dump = None;
            true
        } else {
            false
        }
    }

    /// Toggle host power. Powering off cold-resets the machine (clearing
    /// RAM) and parks a test screen on the display; powering on boots the
    /// freshly cold machine. The redraw keeps the status-bar button and
    /// display current.
    fn toggle_power(&mut self) {
        if self.powered_on {
            self.power_off();
        } else {
            self.powered_on = true;
            self.sync_live_audio_suspension();
            #[cfg(feature = "fluxbridge")]
            self.attach_configured_bridges();
            // The lent disks powering off gave up, lent again -- the session
            // still holds them, so no permission is asked twice.
            self.attach_configured_host_disks();
            info!("power button: machine powered on (cold boot)");
        }
        self.request_redraw();
    }

    /// Open the real floppy drives this machine's configuration asks for.
    ///
    /// Powering off let go of them, so powering back on has to take them
    /// again. A drive that will not open is logged rather than refused: the
    /// machine comes up with an empty bay, which is what an Amiga with a dead
    /// drive does, and the alternative is a power button that does nothing.
    #[cfg(feature = "fluxbridge")]
    fn attach_configured_bridges(&mut self) {
        let raw = self.machine_config.clone();
        let cfg = match crate::config::Config::try_from(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!("could not re-read the configuration to open the physical drives: {e:#}");
                return;
            }
        };
        if !cfg.floppy.bridges.iter().any(Option::is_some) {
            return;
        }
        let floppy = &mut self.emu.bus_mut().floppy;
        if let Err(e) = crate::emulator::attach_floppy_bridges(floppy, &cfg) {
            warn!("physical floppy drive not available: {e:#}");
        }
    }

    /// Put the real disks back on the machine's cables after a power cycle.
    ///
    /// Powering off hands them to the host, so powering on has to take them
    /// again or the machine comes back up with the slot empty. Nothing is
    /// asked of the user: the disks were taken from the host once and are
    /// still held, so this only puts them back where the guest looks for them.
    fn attach_configured_host_disks(&mut self) {
        let raw = self.machine_config.clone();
        let cfg = match crate::config::Config::try_from(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!("could not re-read the configuration to open the real disks: {e:#}");
                return;
            }
        };
        if cfg.host_disks.is_empty() {
            return;
        }
        let back = self.emu.bus_mut().attach_host_disks(&cfg);
        if back > 0 {
            info!("power button: {back} host disk(s) back on with the machine");
        }
    }

    /// Toggle host-level pause. Pausing freezes the emulator in place
    /// (it stops stepping but stays powered on), so the current frame is
    /// held and emulation resumes from the same point when unpaused.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.sync_live_audio_suspension();
        if self.paused {
            info!("pause button: emulation paused");
            // A user pause completes a remote client's pending resume;
            // the client learns where the machine stopped.
            #[cfg(feature = "control")]
            self.control_complete_pending("user_pause", "paused from the window");
        } else {
            info!("pause button: emulation resumed");
        }
        self.request_redraw();
    }

    /// Power off: drop into a cold-boot state (RAM cleared) and park the
    /// test screen, so a later power-on comes up as a clean power cycle.
    fn power_off(&mut self) {
        // A key held on the on-screen keyboard is let go before the power
        // goes, so the cold-boot machine starts with the caps up and
        // nothing latched against the machine that just stopped.
        self.release_keyboard_panel_holds();
        self.powered_on = false;
        self.paused = false;
        self.sync_live_audio_suspension();
        // A real drive is powered by the machine: with the Amiga off it stops,
        // and the interface belongs to the host again. Holding it open would
        // leave it clicking as though the machine were still running, and
        // nothing else -- including the next machine this window builds --
        // could open it.
        #[cfg(feature = "fluxbridge")]
        self.emu.bus_mut().floppy.release_bridges();
        // A real hard disk is different: the machine only ever borrowed it
        // from the session's own hold, which the launcher still shows as
        // attached. The machine's copies go -- an off machine holds nothing
        // -- but the disk stays taken, so powering back on lends it again
        // without a second permission prompt, and only the launcher's
        // Unmount (or quitting) actually hands it back to the host.
        let released = self.emu.bus_mut().release_host_disks();
        if released > 0 {
            info!("power button: {released} host disk(s) off with the machine, still held for it");
        }
        info!("power button: machine powered off (cold boot state)");
        #[cfg(feature = "control")]
        self.control_complete_pending("pause", "power state changed");
        if let Err(e) = self.emu.power_on_reset() {
            error!("cold power-on reset failed: {e:#}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        } else {
            self.cpu_halted = false;
            self.sync_live_audio_suspension();
        }
        self.held_rawkeys = [false; 128];
        self.reset_render_pipeline();
        self.last_fdd_track = None;
        paint_test_screen(&mut self.fb);
        self.deinterlacer
            .push_field(&self.fb, FB_HEIGHT, FB_WIDTH, false, true, true);
        self.refresh_present_from_deinterlacer();
    }

    fn reset_emulator(&mut self, clear_host_keys: bool) {
        // The strip's latches are a host-side affordance: they must not
        // ride through a reset and be re-reported by the MCU's power-up
        // stream, which is what `begin_power_up` does with anything the
        // matrix still shows held.
        self.release_keyboard_panel_holds();
        if let Err(e) = self.emu.keyboard_reset() {
            error!("keyboard reset failed: {e:#}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        } else {
            self.cpu_halted = false;
            self.sync_live_audio_suspension();
            self.reset_render_pipeline();
            self.last_fdd_track = None;
            if clear_host_keys {
                self.held_rawkeys = [false; 128];
            }
        }
    }

    fn refresh_present_from_deinterlacer(&mut self) {
        let rows = self.deinterlacer.output_rows();
        let width = self.deinterlacer.output_width();
        let active = rows * width;
        self.present_fb.resize(active, 0);
        self.present_fb
            .copy_from_slice(&self.deinterlacer.output()[..active]);
        self.present_rows = rows;
        self.present_width = width;
    }

    fn reset_render_pipeline(&mut self) {
        self.render_generation = self.render_generation.wrapping_add(1);
        self.last_rendered_emulated_frame = None;
        self.last_submitted_render_frame = None;
        self.presentation_latch.reset();
        self.last_main_redraw_state = None;
        self.main_presentation_dirty = true;
        let _ = self.collect_threaded_render_results(false);
    }

    fn apply_threaded_render_result(&mut self, result: RenderWorkerResult) -> bool {
        // Only one job is in flight at a time, so the returned snapshot is
        // always the freshest one to recycle.
        let mut input = result.input;
        input.release_shared_frame_data();
        self.render_recycle_input = Some(input);
        if result.generation != self.render_generation {
            if self.render_recycle_fb.is_empty() {
                self.render_recycle_fb = result.presentation_fb;
            }
            return false;
        }

        if result.reused_previous {
            self.render_recycle_fb = result.presentation_fb;
            self.last_rendered_emulated_frame = Some(result.emulated_frame);
            return true;
        }

        self.emu.bus_mut().record_video_render_frame(result.timing);
        let next_tv_aperture_rows = self
            .presentation_latch
            .resolve_tv_aperture(result.tv_aperture);
        let unchanged = self.rtg_present_dims.is_none()
            && self.present_tv_aperture_rows == next_tv_aperture_rows
            && self.present_programmable == result.programmable
            && presentation_pixels_equal(
                &self.present_fb,
                self.present_rows,
                self.present_width,
                &result.presentation_fb,
                result.present_rows,
                result.present_width,
            );
        if unchanged {
            self.render_recycle_fb = result.presentation_fb;
            self.last_rendered_emulated_frame = Some(result.emulated_frame);
            return true;
        }

        self.main_presentation_dirty = true;
        let old = std::mem::replace(&mut self.present_fb, result.presentation_fb);
        self.render_recycle_fb = old;
        self.present_rows = result.present_rows;
        self.present_width = result.present_width;
        self.present_tv_aperture_rows = next_tv_aperture_rows;
        self.present_programmable = result.programmable;
        self.rtg_present_dims = None;
        self.last_rendered_emulated_frame = Some(result.emulated_frame);
        true
    }

    fn collect_threaded_render_results(&mut self, wait: bool) -> bool {
        let mut rendered = false;
        loop {
            let result = match self.render_worker.as_ref() {
                Some(worker) if wait => match worker.recv() {
                    Ok(result) => result,
                    Err(_) => {
                        self.render_worker = None;
                        return rendered;
                    }
                },
                Some(worker) => match worker.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return rendered,
                    Err(TryRecvError::Disconnected) => {
                        self.render_worker = None;
                        return rendered;
                    }
                },
                None => return rendered,
            };
            rendered |= self.apply_threaded_render_result(result);
            if wait {
                return rendered;
            }
        }
    }

    fn render_emulated_frame_threaded(&mut self) -> bool {
        let mut rendered = self.collect_threaded_render_results(false);
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_submitted_render_frame, emulated_frame) {
            return rendered;
        }

        let input = match self.render_recycle_input.take() {
            Some(mut input) => {
                input.refill_from_bus(self.emu.bus());
                input
            }
            None => bitplane::RenderInput::from_bus(self.emu.bus()),
        };
        let h_shift = if self.hcenter {
            self.presentation_latch
                .presentation_h_shift(&input.render_base(), self.overscan)
        } else {
            0
        };
        let job = RenderJob {
            generation: self.render_generation,
            input,
            h_shift,
            overscan: self.overscan,
            deinterlace: self.deinterlace,
            phosphor: self.phosphor,
            presentation_fb: std::mem::take(&mut self.render_recycle_fb),
        };
        let send_result = self
            .render_worker
            .as_ref()
            .expect("threaded render path without worker")
            .send(job);
        match send_result {
            Ok(()) => {
                self.last_submitted_render_frame = Some(emulated_frame);
            }
            Err(job) => {
                warn!("render worker stopped; falling back to synchronous rendering");
                self.render_recycle_fb = job.presentation_fb;
                self.render_recycle_input = Some(job.input);
                self.render_worker = None;
                rendered |= self.render_emulated_frame_sync();
            }
        }
        rendered | self.collect_threaded_render_results(false)
    }

    fn finish_render_for_current_frame(&mut self) -> bool {
        if !self.powered_on {
            return false;
        }
        if !self.emu.bus().frame_render_available() {
            return false;
        }
        let target = self.emu.bus().emulated_frames();
        let mut rendered = self.render_emulated_frame_if_needed();
        while self.render_worker.is_some() && self.last_rendered_emulated_frame != Some(target) {
            rendered |= self.collect_threaded_render_results(true);
        }
        rendered
    }

    /// Present the RTG board frame when one is driving the display: the
    /// board frame (own resolution) is scaled horizontally into the
    /// FB_WIDTH-stride presentation buffer, and the shared vertical scaling
    /// maps its rows to the output height. Returns `None` when no RTG board
    /// is active (native chipset presentation as usual).
    fn render_rtg_frame_if_active(&mut self) -> Option<bool> {
        if !self.emu.bus().rtg_active() {
            return None;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_rendered_emulated_frame, emulated_frame) {
            return Some(false);
        }
        let mut rtg = std::mem::take(&mut self.rtg_fb);
        let mut present = std::mem::take(&mut self.present_fb);
        let composed = compose_rtg_present(self.emu.bus(), &mut rtg, &mut present);
        self.rtg_fb = rtg;
        self.present_fb = present;
        let Some((rows, native_w, native_h)) = composed else {
            // rtg_active() is true but the frame did not compose (e.g. MODE
            // set before ORIG_RES): fall back to the chipset render rather
            // than freezing on the stale frame.
            self.rtg_present_dims = None;
            return None;
        };
        // The native frame stays in `rtg_fb`; the window presents it at full
        // resolution through the RTG texture, while `present_fb` keeps the
        // FB_WIDTH version the screenshot path reads.
        if self.rtg_present_dims.is_none() {
            // Entering RTG is a presentation discontinuity. Advance the
            // generation so the render worker clears its chipset repeated-
            // frame and deinterlace history before native output resumes;
            // otherwise an exact pre-RTG input match could retain this RTG
            // buffer instead of producing the first returning chipset frame.
            self.render_generation = self.render_generation.wrapping_add(1);
            self.presentation_latch.reset();
        }
        self.rtg_present_dims = Some((native_w, native_h));
        self.main_presentation_dirty = true;
        self.present_rows = rows;
        self.present_width = FB_WIDTH;
        self.present_tv_aperture_rows = None;
        self.present_programmable = false;
        self.last_rendered_emulated_frame = Some(emulated_frame);
        self.last_submitted_render_frame = Some(emulated_frame);
        Some(true)
    }

    fn render_emulated_frame_if_needed(&mut self) -> bool {
        if !self.emu.bus().frame_render_available() {
            return false;
        }
        // Drain in-flight chipset render results first (recycling their
        // buffers) so a stale result cannot land on top of an RTG frame.
        // Their "new frame applied" outcome must propagate to the caller,
        // which uses it to schedule the window redraw.
        let mut rendered = false;
        if self.render_worker.is_some() {
            rendered = self.collect_threaded_render_results(false);
        }
        if let Some(rtg_rendered) = self.render_rtg_frame_if_active() {
            return rendered | rtg_rendered;
        }
        if self.render_worker.is_some() {
            return rendered | self.render_emulated_frame_threaded();
        }
        rendered | self.render_emulated_frame_sync()
    }

    fn render_emulated_frame_sync(&mut self) -> bool {
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_rendered_emulated_frame, emulated_frame) {
            return false;
        }

        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        let h_shift = if self.hcenter {
            self.presentation_latch
                .presentation_h_shift(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        bitplane::render(self.emu.bus_mut(), &mut self.fb);
        let geometry = self.emu.bus().frame_geometry();
        let canvas_scale = self.emu.bus().frame_canvas_scale();
        let field_rows = post_process_rendered_field(
            &mut self.fb,
            geometry,
            canvas_scale,
            self.emu.bus().frame_presentation_h_window(),
            self.emu.bus().frame_presentation_v_window(),
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        let base = self.emu.bus().frame_render_base();
        // Standard 15 kHz fields line-double / weave to 2x rows; a
        // programmable progressive scan already carries every line.
        let mut next_present_fb = std::mem::take(&mut self.render_recycle_fb);
        let (rows, width) = self.deinterlacer.present_field_into(
            &self.fb,
            field_rows,
            FB_WIDTH * canvas_scale,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
            &mut next_present_fb,
        );
        let next_tv_aperture_rows = self
            .presentation_latch
            .resolve_tv_aperture(standard_tv_aperture_frame(geometry, rows, &base));
        let unchanged = self.rtg_present_dims.is_none()
            && self.present_tv_aperture_rows == next_tv_aperture_rows
            && self.present_programmable == geometry.programmable
            && presentation_pixels_equal(
                &self.present_fb,
                self.present_rows,
                self.present_width,
                &next_present_fb,
                rows,
                width,
            );
        if unchanged {
            self.render_recycle_fb = next_present_fb;
        } else {
            self.main_presentation_dirty = true;
            let old = std::mem::replace(&mut self.present_fb, next_present_fb);
            self.render_recycle_fb = old;
            self.present_rows = rows;
            self.present_width = width;
            self.present_tv_aperture_rows = next_tv_aperture_rows;
            self.present_programmable = geometry.programmable;
        }
        self.rtg_present_dims = None;
        self.last_rendered_emulated_frame = Some(emulated_frame);
        self.last_submitted_render_frame = Some(emulated_frame);
        true
    }
}

mod bezel;
mod console;
#[cfg(feature = "control")]
mod control;
mod crt_shader;
mod host_input;
mod kbdpanel;
#[cfg(feature = "mt32")]
mod mt32panel;
mod present;
mod rtg_texture;
mod statusbar;
pub(super) use present::{scale_rect, texture_height, texture_width, Rect};
pub(super) use statusbar::{draw_rect_bevel, fill_rect, fill_rect_blend};

pub use host_input::parse_amiga_key;
use host_input::*;
use present::*;
use statusbar::*;

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-or-later

//! winit + pixels integration. The emulator core runs synchronously on the
//! main thread inside `about_to_wait`; by default a worker renders the
//! completed frame while the main thread advances the next frame. winit and
//! wgpu presentation stay on the main thread.

use super::deinterlace::{Deinterlacer, OUT_HEIGHT};
use super::launcher::{LauncherField, LauncherState, MachineSetup, StatusMessage};
use super::ui::{self, Panel, UiControl, UiState};
use super::{
    bitplane, font, present_height, FrameGeometry, FB_HEIGHT, FB_PIXELS, FB_WIDTH,
    HOST_SHORTCUT_MODIFIER_LABEL, MAX_FB_PIXELS, MAX_VISIBLE_LINES,
};
use crate::audio::{AudioSink, CpalSink};
use crate::bus::{
    BeamWriteSource, FrontPanelStatus, RenderRegisterSnapshot, VideoRenderFrameTiming,
};
use crate::config::{Config, Overscan, PixelAspect, RawConfig, WarpSpeed};
use crate::emulator::Emulator;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardJoystickKey {
    Up,
    Down,
    Left,
    Right,
    FireRightCtrl,
    FireRightAlt,
    Red,
    Blue,
    Green,
    Yellow,
    Play,
    Rewind,
    Forward,
}

/// Host keys currently held for keyboard joystick emulation. Fire has
/// multiple aliases, so this tracks individual keys and resolves them to
/// one port state when applied.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct KeyboardJoystickHeld {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    fire_right_ctrl: bool,
    fire_right_alt: bool,
    red: bool,
    blue: bool,
    green: bool,
    yellow: bool,
    play: bool,
    rwd: bool,
    ffw: bool,
}

impl KeyboardJoystickHeld {
    fn set(&mut self, key: KeyboardJoystickKey, held: bool) {
        match key {
            KeyboardJoystickKey::Up => self.up = held,
            KeyboardJoystickKey::Down => self.down = held,
            KeyboardJoystickKey::Left => self.left = held,
            KeyboardJoystickKey::Right => self.right = held,
            KeyboardJoystickKey::FireRightCtrl => self.fire_right_ctrl = held,
            KeyboardJoystickKey::FireRightAlt => self.fire_right_alt = held,
            KeyboardJoystickKey::Red => self.red = held,
            KeyboardJoystickKey::Blue => self.blue = held,
            KeyboardJoystickKey::Green => self.green = held,
            KeyboardJoystickKey::Yellow => self.yellow = held,
            KeyboardJoystickKey::Play => self.play = held,
            KeyboardJoystickKey::Rewind => self.rwd = held,
            KeyboardJoystickKey::Forward => self.ffw = held,
        }
    }

    fn is_set(&self, key: KeyboardJoystickKey) -> bool {
        match key {
            KeyboardJoystickKey::Up => self.up,
            KeyboardJoystickKey::Down => self.down,
            KeyboardJoystickKey::Left => self.left,
            KeyboardJoystickKey::Right => self.right,
            KeyboardJoystickKey::FireRightCtrl => self.fire_right_ctrl,
            KeyboardJoystickKey::FireRightAlt => self.fire_right_alt,
            KeyboardJoystickKey::Red => self.red,
            KeyboardJoystickKey::Blue => self.blue,
            KeyboardJoystickKey::Green => self.green,
            KeyboardJoystickKey::Yellow => self.yellow,
            KeyboardJoystickKey::Play => self.play,
            KeyboardJoystickKey::Rewind => self.rwd,
            KeyboardJoystickKey::Forward => self.ffw,
        }
    }

    fn joystick_state(&self) -> crate::gamepad::JoystickState {
        crate::gamepad::JoystickState {
            up: self.up,
            down: self.down,
            left: self.left,
            right: self.right,
            fire: self.fire_right_ctrl || self.fire_right_alt || self.red,
            button2: self.blue,
            green: self.green,
            yellow: self.yellow,
            play: self.play,
            rwd: self.rwd,
            ffw: self.ffw,
        }
    }
}

/// FS-UAE-compatible keyboard joystick/gamepad emulation:
/// cursor keys for directions; Right Ctrl/Right Alt for fire; CD32 extras
/// on C/X/D/S/Return/Z/A.
fn keyboard_joystick_key_for(code: KeyCode) -> Option<KeyboardJoystickKey> {
    Some(match code {
        KeyCode::ArrowUp => KeyboardJoystickKey::Up,
        KeyCode::ArrowDown => KeyboardJoystickKey::Down,
        KeyCode::ArrowLeft => KeyboardJoystickKey::Left,
        KeyCode::ArrowRight => KeyboardJoystickKey::Right,
        KeyCode::ControlRight => KeyboardJoystickKey::FireRightCtrl,
        KeyCode::AltRight => KeyboardJoystickKey::FireRightAlt,
        KeyCode::KeyC => KeyboardJoystickKey::Red,
        KeyCode::KeyX => KeyboardJoystickKey::Blue,
        KeyCode::KeyD => KeyboardJoystickKey::Green,
        KeyCode::KeyS => KeyboardJoystickKey::Yellow,
        KeyCode::Enter | KeyCode::NumpadEnter => KeyboardJoystickKey::Play,
        KeyCode::KeyZ => KeyboardJoystickKey::Rewind,
        KeyCode::KeyA => KeyboardJoystickKey::Forward,
        _ => return None,
    })
}

/// Whether the active mode routes the keyboard joystick mapping to port 2. With
/// only the two explicit modes this is a direct read of the mode -- `Keyboard`
/// captures the arrow/fire keys, `Gamepad` lets every key reach the Amiga.
fn joystick_mode_uses_keyboard(mode: JoystickInputMode) -> bool {
    matches!(mode, JoystickInputMode::Keyboard)
}

/// The port-2 controls currently held by `--joy-after` scripting.
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
const STATUS_BAR_HEIGHT: usize = 44;
/// Logical window height: the presentation canvas for the active pixel
/// aspect plus the status bar below it.
fn window_present_height() -> usize {
    present_height() + STATUS_BAR_HEIGHT
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
const VOLUME_SLIDER_W: usize = 72;
const VOLUME_SLIDER_H: usize = 8;
const VOLUME_KNOB_W: usize = 8;
const VOLUME_KNOB_H: usize = 16;
const VOLUME_GLYPH_X: usize = VOLUME_SLIDER_X - 16;
// Joystick input-source toggle: a compact icon button just left of the volume
// glyph, in the otherwise-free slot before the right-hand control cluster. The
// widest media layout (four floppies plus a CD) ends at x=372, so a 22px button
// here clears both the media controls and the speaker glyph; this is verified by
// `joystick_toggle_clears_worst_case_media`.
const JOY_TOGGLE_W: usize = 22;
const JOY_TOGGLE_X: usize = VOLUME_GLYPH_X - 2 - JOY_TOGGLE_W;
const STANDARD_PAL_VISIBLE_WIDTH: usize = 320 * 2;
const STANDARD_PAL_VISIBLE_LINES: usize = 256;
const STANDARD_PAL_VISIBLE_START_VPOS: u32 = 0x2C;
// Default TV presentation keeps a small consumer-visible overscan margin while
// still hiding the deep edge columns that often contain unfinished effects.
const TV_HORIZONTAL_OVERSCAN_MARGIN: usize = 24 * 2;
const TV_PAL_PRESENT_WIDTH: usize = STANDARD_PAL_VISIBLE_WIDTH + 2 * 26;
const TV_PAL_PRESENT_HEIGHT: usize = 540;
const TV_PAL_PRESENT_SOURCE_X: usize = bitplane::STANDARD_VISIBLE_X0 - 26;
const TV_PAL_PRESENT_SOURCE_Y: usize = 18;
const TV_PAL_LIVE_PAD_X: usize = (FB_WIDTH - TV_PAL_PRESENT_WIDTH) / 2;
const STATUS_BG: u32 = rgba(28, 28, 26);
const STATUS_TOP: u32 = rgba(78, 76, 70);
const STATUS_BOTTOM: u32 = rgba(12, 12, 11);
const LED_BEZEL_DARK: u32 = rgba(8, 8, 7);
const LED_BEZEL_LIGHT: u32 = rgba(78, 76, 68);
const POWER_LED_ON: u32 = rgba(232, 31, 24);
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
const STATUS_TEXT: u32 = rgba(174, 170, 154);
const VOLUME_FILL: u32 = rgba(44, 178, 94);
const VOLUME_FILL_HIGHLIGHT: u32 = rgba(128, 244, 150);
const WINDOW_TITLE: &str = concat!("Copperline ", env!("COPPERLINE_DISPLAY_VERSION"));
const COPPERLINE_LOGO_PNG: &[u8] = include_bytes!("../../assets/brand/copperline-logo.png");
const COPPERLINE_ICON_PNG: &[u8] = include_bytes!("../../assets/brand/copperline-icon.png");
const MOUSE_MOTION_SCALE: f64 = 1.0;
/// How long a transient on-screen overlay message (screenshot saved,
/// disk swapped) stays visible.
const OSD_DURATION: std::time::Duration = std::time::Duration::from_millis(2500);
/// On-screen overlay colours (packed R,G,B,A in memory order).
const OSD_TEXT: u32 = rgba(236, 236, 232);
const OSD_SHADOW: u32 = rgba(0, 0, 0);
const OSD_BG: u32 = rgba(10, 10, 12);
const RECORD_DOT: u32 = rgba(229, 56, 48);
const AMIGA_RAWKEY_LEFT_SHIFT: u8 = 0x60;
const AMIGA_RAWKEY_RIGHT_SHIFT: u8 = 0x61;
const AMIGA_RAWKEY_LEFT_ALT: u8 = 0x64;
const AMIGA_RAWKEY_RIGHT_ALT: u8 = 0x65;

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

use anyhow::{anyhow, Result};
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
use winit::dpi::LogicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, RawKeyEvent,
    WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Icon, Window, WindowAttributes, WindowId};

fn rawkey_index(rawkey: u8) -> usize {
    (rawkey & 0x7F) as usize
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

pub struct App {
    emu: Emulator,
    fb: Vec<u32>,
    /// Merges rendered fields into the double-height presentation
    /// buffer that the window texture, screenshots, and frame dumps
    /// read (see [`deinterlace`](super::deinterlace)).
    deinterlacer: Deinterlacer,
    /// Active presentation buffer, already deinterlaced/line-doubled and
    /// post-processed. The first `present_rows * FB_WIDTH` pixels are valid.
    present_fb: Vec<u32>,
    present_rows: usize,
    present_standard_tv_aperture: bool,
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
    /// Emulated frame `analyzer_underlay_fb` was rendered for.
    analyzer_underlay_frame: Option<u64>,
    /// Recycled snapshot buffers for the underlay's side-effect-free render.
    analyzer_underlay_input: Option<bitplane::RenderInput>,
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
    auto_shot: Option<(f32, PathBuf)>,
    pending_auto_shot: Option<(f32, PathBuf)>,
    /// Scheduled --save-state-after capture: write a save state once
    /// emulated time reaches the deadline, then keep running.
    auto_save_state: Option<(f32, PathBuf)>,
    pending_auto_save_state: Option<(f32, PathBuf)>,
    frame_dump: Option<FrameDumpState>,
    pending_frame_dump: Option<FrameDumpSpec>,
    auto_keys: Vec<ScheduledKey>,
    pending_auto_keys: Vec<KeyPressSpec>,
    /// Scheduled mouse-button press/release events from --click-after.
    /// `Press` and `Release` deadlines per requested click.
    auto_clicks: Vec<ScheduledClick>,
    pending_auto_clicks: Vec<(f32, MouseButtonKind, u32)>,
    /// Scheduled port-2 joystick/CD32-pad events from --joy-after, plus
    /// the controls currently held. `auto_joy_engaged` stays true once
    /// any scripted joy event has fired so the state keeps overriding
    /// the (absent) physical pad, including the final release.
    auto_joys: Vec<ScheduledJoy>,
    pending_auto_joys: Vec<(f32, JoyButtonKind, u32)>,
    auto_joy_held: AutoJoyHeld,
    auto_joy_engaged: bool,
    /// Scheduled relative port-1 mouse motions from --mouse-after,
    /// one-shot per entry; (at_emulated_secs, dx, dy).
    auto_mouse: Vec<(f64, i32, i32)>,
    pending_auto_mouse: Vec<(f32, i32, i32)>,
    auto_disk_inserts: Vec<ScheduledDiskInsert>,
    pending_auto_disk_inserts: Vec<DiskInsertSpec>,
    /// Live-input recorder: logs every input event that reaches the
    /// emulated machine and writes a --script-replayable file on stop.
    /// None while not recording.
    input_recorder: Option<crate::inputrec::InputRecorder>,
    /// --record-input destination: when set, the recorder runs for the
    /// whole session and the script is written here on exit (the Drop
    /// impl catches every exit path, including the headless captures).
    record_input_path: Option<PathBuf>,
    modifiers: ModifiersState,
    held_rawkeys: [bool; 128],
    raw_device_held_rawkeys: [bool; 128],
    main_window_focused: bool,
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
    mouse_delta_remainder: (f64, f64),
    last_rendered_emulated_frame: Option<u64>,
    last_submitted_render_frame: Option<u64>,
    render_generation: u64,
    last_fdd_track: Option<u8>,
    /// Transient on-screen overlay message (screenshot saved, disk
    /// swapped), or None when nothing is being shown.
    osd: Option<Osd>,
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
    /// Host USB gamepad reader (pure-Rust, no SDL2), mapped to the emulated
    /// port-2 digital joystick via a per-pad calibration. A no-op when no
    /// input backend is available (e.g. headless CI) or the pad is not yet
    /// calibrated.
    gamepad: crate::gamepad::GamepadReader,
    /// Host source policy for the emulated port-2 joystick/CD32 pad.
    joystick_input_mode: JoystickInputMode,
    /// Output frame-skip level for warp/turbo mode: how many emulated frames
    /// are retired per presented frame while warp is engaged. Presentation is
    /// vsync-gated, so this is what decouples warp speed from the host monitor
    /// refresh rate. Adjustable from the Emulator menu and the keyboard.
    warp_speed: WarpSpeed,
    /// Mapped host keys currently held for keyboard joystick emulation.
    keyboard_joy_held: KeyboardJoystickHeld,
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
    /// Active video+audio capture (shortcut or the menu's Record Video item),
    /// or None when not recording. Frames and the matching mixer audio are
    /// appended on emulated-frame boundaries, so captures stay in sync
    /// even under warp or host stutter.
    recorder: Option<crate::recorder::VideoRecorder>,
    /// Scratch presentation-scaled framebuffer for the recorder (same
    /// vertical resample as screenshots).
    record_fb: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledClick {
    press_at_emulated_secs: f64,
    release_at_emulated_secs: f64,
    button: MouseButtonKind,
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
    /// True while the host window is minimized (Windows delivers a 0x0
    /// Resized). Presenting while minimized deadlocks on Windows: DWM stops
    /// consuming swapchain frames, so once the in-flight buffers fill,
    /// pixels.render() blocks the main thread, the message pump dies, and
    /// the window can never be restored (which is what would unblock the
    /// present). Skip all rendering until a nonzero resize restores it.
    minimized: bool,
}

struct ToolWindow {
    window: Arc<Window>,
    pixels: Pixels<'static>,
    texture_scale: usize,
    cursor_pos: Option<(i32, i32)>,
    /// Same Windows minimized-present deadlock hazard as Render::minimized.
    minimized: bool,
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

struct RenderJob {
    generation: u64,
    input: bitplane::RenderInput,
    h_shift: usize,
    overscan: Overscan,
    presentation_fb: Vec<u32>,
}

struct RenderWorkerResult {
    generation: u64,
    emulated_frame: u64,
    timing: VideoRenderFrameTiming,
    presentation_fb: Vec<u32>,
    present_rows: usize,
    standard_tv_aperture: bool,
    /// The job's frame snapshot, handed back for buffer reuse.
    input: bitplane::RenderInput,
}

struct RenderWorker {
    job_tx: Option<SyncSender<RenderJob>>,
    result_rx: Receiver<RenderWorkerResult>,
    handle: Option<JoinHandle<()>>,
}

impl RenderWorker {
    fn new(phosphor: f32) -> Self {
        let (job_tx, job_rx) = mpsc::sync_channel::<RenderJob>(1);
        let (result_tx, result_rx) = mpsc::channel::<RenderWorkerResult>();
        let handle = std::thread::Builder::new()
            .name("copperline-render".to_string())
            .spawn(move || {
                let mut fb = vec![0u32; MAX_FB_PIXELS];
                let mut deinterlacer = Deinterlacer::with_phosphor(phosphor);
                while let Ok(job) = job_rx.recv() {
                    let result = render_job_to_presentation(job, &mut fb, &mut deinterlacer);
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

impl App {
    pub fn new(
        emu: Emulator,
        power_on: bool,
        screenshot_after: Option<(f32, PathBuf)>,
        save_state_after: Option<(f32, PathBuf)>,
        frame_dump: Option<FrameDumpSpec>,
        press_after: Vec<KeyPressSpec>,
        click_after: Vec<(f32, MouseButtonKind, u32)>,
        joy_after: Vec<(f32, JoyButtonKind, u32)>,
        mouse_after: Vec<(f32, i32, i32)>,
        disk_insert_after: Vec<DiskInsertSpec>,
        record_input: Option<PathBuf>,
        disk_playlists: [Vec<PathBuf>; 4],
        disk_write_protected: [bool; 4],
        overscan: Overscan,
        phosphor: f32,
        warp_speed: WarpSpeed,
        joystick_input_mode: JoystickInputMode,
        about_machine_lines: Vec<String>,
        machine_config: RawConfig,
    ) -> Self {
        // Headless capture runs drive themselves off emulated time, so a
        // powered-off start would simply hang. Force power on for those.
        let powered_on = power_on
            || screenshot_after.is_some()
            || save_state_after.is_some()
            || frame_dump.is_some();
        let render_worker = threaded_render_enabled().then(|| {
            info!("threaded render pipeline enabled");
            RenderWorker::new(phosphor)
        });
        Self {
            emu,
            fb: vec![0u32; MAX_FB_PIXELS],
            deinterlacer: Deinterlacer::with_phosphor(phosphor),
            present_fb: vec![0u32; FB_WIDTH * OUT_HEIGHT],
            present_rows: OUT_HEIGHT,
            present_standard_tv_aperture: true,
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
            analyzer_underlay_frame: None,
            analyzer_underlay_input: None,
            render_worker,
            render_recycle_fb: Vec::new(),
            render_recycle_input: None,
            cpu_halted: false,
            powered_on,
            paused: false,
            auto_shot: None,
            pending_auto_shot: screenshot_after,
            auto_save_state: None,
            pending_auto_save_state: save_state_after,
            frame_dump: None,
            pending_frame_dump: frame_dump,
            auto_keys: Vec::new(),
            pending_auto_keys: press_after,
            auto_clicks: Vec::new(),
            pending_auto_clicks: click_after,
            auto_joys: Vec::new(),
            pending_auto_joys: joy_after,
            auto_joy_held: AutoJoyHeld::default(),
            auto_joy_engaged: false,
            auto_mouse: Vec::new(),
            pending_auto_mouse: mouse_after,
            auto_disk_inserts: Vec::new(),
            pending_auto_disk_inserts: disk_insert_after,
            input_recorder: record_input
                .is_some()
                .then(|| crate::inputrec::InputRecorder::new(0.0)),
            record_input_path: record_input,
            modifiers: ModifiersState::empty(),
            held_rawkeys: [false; 128],
            raw_device_held_rawkeys: [false; 128],
            main_window_focused: false,
            cursor_pos: None,
            last_display_cursor_pos: None,
            last_cursor_phys: None,
            volume_dragging: false,
            analyzer_dragging: false,
            mouse_captured: false,
            mouse_delta_remainder: (0.0, 0.0),
            last_rendered_emulated_frame: None,
            last_submitted_render_frame: None,
            render_generation: 0,
            last_fdd_track: None,
            osd: None,
            disk_playlists,
            disk_write_protected,
            disk_playlist_index: [0; 4],
            hcenter: hcenter_enabled(),
            overscan,
            gamepad: crate::gamepad::GamepadReader::new(),
            joystick_input_mode,
            warp_speed,
            keyboard_joy_held: KeyboardJoystickHeld::default(),
            ui: UiState::default(),
            about_machine_lines,
            machine_config,
            paused_before_debugger: false,
            paused_before_analyzer: false,
            paused_before_console: false,
            last_debug_stop: None,
            recorder: None,
            record_fb: Vec::new(),
        }
    }

    /// Poll the active host joystick source and drive the emulated port-2
    /// joystick. Called once per scheduler quantum. In Gamepad mode a calibrated
    /// physical pad drives the port; in Keyboard mode the keyboard mapping does,
    /// so port 2 stays usable without a physical controller.
    fn pump_joystick_input(&mut self) {
        let gamepad_state = match self.joystick_input_mode {
            JoystickInputMode::Keyboard => None,
            JoystickInputMode::Gamepad => self.gamepad.poll(),
        };

        match gamepad_state {
            Some(state) => self.apply_joystick_state(state),
            // No physical pad but --joy-after scripting has fired: keep
            // asserting the scripted state so it survives this release
            // path and drives the upcoming scheduler quantum.
            None if self.auto_joy_engaged => self.apply_auto_joy_state(),
            None if self.keyboard_joystick_enabled() => self.apply_keyboard_joystick_state(),
            // Pad gone/uncalibrated and keyboard fallback disabled: release
            // everything so nothing sticks.
            None if self.emu.bus().input.joystick_port2 => {
                self.release_port2_joystick();
            }
            None => {}
        }
    }

    fn keyboard_joystick_enabled(&self) -> bool {
        joystick_mode_uses_keyboard(self.joystick_input_mode)
    }

    fn apply_joystick_state(&mut self, state: crate::gamepad::JoystickState) {
        let input = &mut self.emu.bus_mut().input;
        input.set_joystick_port2(
            state.up,
            state.down,
            state.left,
            state.right,
            state.fire,
            state.button2,
        );
        input.set_cd32_buttons_port2(state.play, state.rwd, state.ffw, state.green, state.yellow);
    }

    fn apply_keyboard_joystick_state(&mut self) {
        self.apply_joystick_state(self.keyboard_joy_held.joystick_state());
    }

    fn release_port2_joystick(&mut self) {
        let input = &mut self.emu.bus_mut().input;
        input.set_joystick_port2(false, false, false, false, false, false);
        input.set_cd32_buttons_port2(false, false, false, false, false);
    }

    fn cycle_joystick_input_mode(&mut self) {
        self.set_joystick_input_mode(self.joystick_input_mode.next());
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
        self.show_osd(format!("Joystick input: {}", mode.label()));
    }

    /// Consume a mapped host key as joystick input when keyboard joystick
    /// emulation is active. Releases for previously consumed mapped keys
    /// are also swallowed, even if a gamepad has taken over meanwhile.
    fn handle_keyboard_joystick_key(&mut self, code: KeyCode, pressed: bool) -> bool {
        let Some(key) = keyboard_joystick_key_for(code) else {
            return false;
        };
        let was_held = self.keyboard_joy_held.is_set(key);
        if !self.keyboard_joystick_enabled() && !was_held {
            return false;
        }
        self.keyboard_joy_held.set(key, pressed);
        if self.keyboard_joystick_enabled() {
            self.apply_keyboard_joystick_state();
        }
        true
    }

    /// Drive the emulated port-2 joystick/CD32 pad from the --joy-after
    /// held-control set.
    fn apply_auto_joy_state(&mut self) {
        let held = self.auto_joy_held;
        let input = &mut self.emu.bus_mut().input;
        input.set_joystick_port2(
            held.up, held.down, held.left, held.right, held.red, held.blue,
        );
        input.set_cd32_buttons_port2(held.play, held.rwd, held.ffw, held.green, held.yellow);
        // Reverse-debug: note the held state so replay can reproduce it.
        self.emu.tt_note_input(crate::inputsched::ReplayAction::Joy(
            crate::inputsched::JoyState {
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
        ));
    }

    pub fn run(self) -> Result<()> {
        let event_loop = EventLoop::new().map_err(|e| anyhow!("EventLoop::new: {e}"))?;
        event_loop.set_control_flow(ControlFlow::Poll);
        let mut app = self;
        event_loop
            .run_app(&mut app)
            .map_err(|e| anyhow!("event loop: {e}"))?;
        Ok(())
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
            self.pending_auto_shot.is_some() || self.pending_frame_dump.is_some();
        let attrs = WindowAttributes::default()
            .with_title(WINDOW_TITLE)
            .with_window_icon(copperline_window_icon())
            .with_visible(!headless_capture)
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
        let texture_scale = texture_scale_for_window(&window);
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
        let pixels = match build_pixels_for_window(window.clone(), texture_scale, true) {
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
        self.render = Some(Render {
            window,
            pixels,
            texture_scale,
            minimized: false,
        });
        // Paint at least once so the status bar (and power button) is
        // visible immediately, even when the machine starts powered off
        // and no emulated frame is being produced yet. A powered-off
        // start shows the test screen rather than a black display.
        if !self.powered_on {
            paint_test_screen(&mut self.fb);
            self.deinterlacer
                .push_field(&self.fb, FB_HEIGHT, false, true, true);
            self.refresh_present_from_deinterlacer();
        }
        self.request_redraw();
        if let Some((secs, path)) = self.pending_auto_shot.take() {
            info!(
                "auto-screenshot armed: will save {} after {:.1}s emulated time",
                path.display(),
                secs
            );
            self.auto_shot = Some((secs.max(0.0), path));
        }
        if let Some((secs, path)) = self.pending_auto_save_state.take() {
            info!(
                "auto-save-state armed: will save {} after {:.1}s emulated time",
                path.display(),
                secs
            );
            self.auto_save_state = Some((secs.max(0.0), path));
        }
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
        for (secs, button, dur_ms) in self.pending_auto_clicks.drain(..) {
            let press_at = secs.max(0.0) as f64;
            let release_at = press_at + dur_ms as f64 / 1000.0;
            info!(
                "auto-click armed: {:?} press at {:.1}s emulated, hold {}ms",
                button, secs, dur_ms
            );
            self.auto_clicks.push(ScheduledClick {
                press_at_emulated_secs: press_at,
                release_at_emulated_secs: release_at,
                button,
                pressed: false,
            });
        }
        for (secs, dx, dy) in self.pending_auto_mouse.drain(..) {
            self.auto_mouse.push((secs.max(0.0) as f64, dx, dy));
        }
        if !self.auto_mouse.is_empty() {
            info!(
                "auto-mouse armed: {} scheduled motions",
                self.auto_mouse.len()
            );
        }
        for (secs, button, dur_ms) in self.pending_auto_joys.drain(..) {
            let press_at = secs.max(0.0) as f64;
            let release_at = press_at + dur_ms as f64 / 1000.0;
            info!(
                "auto-joy armed: {:?} press at {:.1}s emulated, hold {}ms",
                button, secs, dur_ms
            );
            self.auto_joys.push(ScheduledJoy {
                press_at_emulated_secs: press_at,
                release_at_emulated_secs: release_at,
                button,
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
                        ..
                    },
                ..
            } => {
                if self.should_drop_repeated_main_key(code, state, repeat) {
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
                    (KeyCode::KeyW, ElementState::Pressed)
                        if host_shortcut_modifier_pressed(self.modifiers) =>
                    {
                        self.toggle_warp()
                    }
                    (other, state) => {
                        let pressed = state == ElementState::Pressed;
                        if pressed && self.ui_handle_key(other) {
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
                let layout = bar_layout(&self.media_bar());
                if bar_hover_changed(&layout, previous_cursor_pos, self.cursor_pos)
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
            WindowEvent::Focused(focused) => {
                self.main_window_focused = focused;
                if !focused {
                    self.volume_dragging = false;
                    self.analyzer_dragging = false;
                    self.set_mouse_captured(false);
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
                            self.ui.menu_open = false;
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
                if pressed && !self.mouse_captured && self.cursor_pos.is_some_and(cursor_in_display)
                {
                    self.set_mouse_captured(true);
                }
                let input = &mut self.emu.bus_mut().input;
                match button {
                    MouseButton::Left => input.lmb_port1 = pressed,
                    MouseButton::Right => input.rmb_port1 = pressed,
                    MouseButton::Middle => input.mmb_port1 = pressed,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.mouse_captured
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
                // and grabs the mouse. Rebuild the texture for the new scale.
                if let Some(r) = self.render.as_mut() {
                    resync_render_scale(&mut r.pixels, &mut r.texture_scale, scale_factor);
                }
                self.request_redraw();
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = self.render.as_mut() {
                    // A zero-sized resize is minimization (Windows reports
                    // the minimized client area as 0x0). Leave the surface
                    // untouched and stop rendering until the restore
                    // delivers a nonzero size (see Render::minimized).
                    r.minimized = size.width == 0 || size.height == 0;
                    if r.minimized {
                        return;
                    }
                    let _ = r.pixels.resize_surface(size.width, size.height);
                }
                // Resizing the surface discards its contents, leaving it
                // blank (white) until the next present. When the machine is
                // powered off (or paused) the event loop is in Wait mode and
                // produces no frames, so without an explicit repaint here the
                // window can sit white after the scale-factor/resize event
                // that macOS delivers right after window creation.
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
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
                let view = StatusBarView {
                    status,
                    powered_on: self.powered_on,
                    paused: self.paused,
                    media,
                    joystick_input_mode: self.joystick_input_mode,
                    hover,
                };
                let osd = self.active_osd_text();
                let ui_hover = self.cursor_pos.and_then(|p| self.main_ui_control_at(p));
                let warp = !self.emu.paced();
                let warp_speed = self.warp_speed;
                let recording = self.recorder.is_some();
                let input_recording = self.input_recorder.is_some();
                let ui_data = self.build_panel_view_data();
                if let Some(r) = self.render.as_mut() {
                    let frame = r.pixels.frame_mut();
                    copy_window_present_frame(
                        &self.present_fb,
                        self.present_rows,
                        frame,
                        r.texture_scale,
                        self.overscan,
                        self.present_standard_tv_aperture,
                    );
                    draw_status_bar(frame, &view, r.texture_scale);
                    if recording {
                        // Painted into the presentation texture only, so
                        // the badge never appears in the recorded file.
                        draw_record_badge(frame, r.texture_scale);
                    }
                    if let Some(text) = &osd {
                        draw_osd(frame, text, r.texture_scale);
                    }
                    ui::draw(
                        frame,
                        r.texture_scale,
                        &self.ui,
                        ui_hover,
                        ui_data.as_ref(),
                        warp,
                        warp_speed,
                        recording,
                        input_recording,
                        self.joystick_input_mode,
                        super::pixel_aspect(),
                    );
                    if let Err(e) = r.pixels.render() {
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
        if self.render.is_none() {
            return;
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
        event_loop.set_control_flow(if running || osd_active || calibrating {
            ControlFlow::Poll
        } else {
            ControlFlow::Wait
        });
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
            let input = &mut self.emu.bus_mut().input;
            if input.joystick_port2 {
                input.set_joystick_port2(false, false, false, false, false, false);
                input.set_cd32_buttons_port2(false, false, false, false, false);
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
        let headless_capture = self.auto_shot.is_some() || self.frame_dump.is_some();
        // Run one scheduler quantum. Rebuild the host framebuffer only
        // when Agnus has crossed into a new frame; the expensive renderer
        // reconstructs a completed hardware frame, not an instruction slice.
        if running {
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
                frames_done += 1;
                // A breakpoint/watchpoint hit pauses the machine and brings
                // the debugger window up with the reason; end the burst so the
                // stop surfaces at the frame where it happened.
                if self.surface_debug_stop() {
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
        }
        let now = Instant::now();
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
        // the step, to decide the warp burst.) Only the emulator window tracks
        // every presented frame; tool windows were paced above.
        if rendered && !headless_capture {
            self.request_main_redraw();
        }

        if self.dump_frame_if_due(now, event_loop) {
            return;
        }

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
                info!("auto-click pressing: {:?}", c.button);
                set_mouse_button(&mut self.emu, c.button, true);
                c.pressed = true;
            }
            if c.pressed && emu_secs >= c.release_at_emulated_secs {
                info!("auto-click releasing: {:?}", c.button);
                set_mouse_button(&mut self.emu, c.button, false);
                return false;
            }
            true
        });
        // Fire any scheduled --joy-after events into the port-2
        // joystick/CD32-pad state, then assert the held set (input polling
        // re-applies it every quantum while scripting is engaged).
        let mut joy_changed = false;
        let held = &mut self.auto_joy_held;
        self.auto_joys.retain_mut(|j| {
            if !j.pressed && emu_secs >= j.press_at_emulated_secs {
                info!("auto-joy pressing: {:?}", j.button);
                held.set(j.button, true);
                j.pressed = true;
                joy_changed = true;
            }
            if j.pressed && emu_secs >= j.release_at_emulated_secs {
                info!("auto-joy releasing: {:?}", j.button);
                held.set(j.button, false);
                joy_changed = true;
                return false;
            }
            true
        });
        if joy_changed {
            self.auto_joy_engaged = true;
            self.apply_auto_joy_state();
        }
        // Fire any scheduled --mouse-after relative motions (one-shot
        // each); these land on the same port-1 quadrature counters as
        // live captured-mouse movement.
        let mut mouse_deltas = Vec::new();
        self.auto_mouse.retain(|&(at, dx, dy)| {
            if emu_secs >= at {
                mouse_deltas.push((dx, dy));
                false
            } else {
                true
            }
        });
        for (dx, dy) in mouse_deltas {
            self.add_mouse_delta_i32(dx, dy);
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
        // Scheduled --save-state-after capture. step_frame has completed for
        // this quantum, so the machine is at the frame-boundary quiescent
        // point save states require. Unlike the auto-screenshot this does
        // not exit: a state save is a capture along the way, not the end of
        // a verification run.
        if let Some((secs, path)) = self.auto_save_state.take() {
            if self.emu.bus().emulated_seconds() >= secs as f64 {
                match self.emu.save_state(&path) {
                    Ok(()) => info!("auto-save-state saved: {}", path.display()),
                    Err(e) => warn!("auto-save-state failed ({}): {e:#}", path.display()),
                }
            } else {
                self.auto_save_state = Some((secs, path));
            }
        }
        if let Some((secs, path)) = self.auto_shot.take() {
            if self.emu.bus().emulated_seconds() >= secs as f64 {
                let emulated_frame = self.emu.bus().emulated_frames();
                self.finish_render_for_current_frame();
                if self.last_rendered_emulated_frame != Some(emulated_frame) {
                    self.auto_shot = Some((secs, path));
                    return;
                }
                self.save_screenshot(&path);
                self.emu.report_stats();
                self.emu.bus().poll_stats.dump_top("at screenshot");
                // Evaluate an untargeted reverse watchpoint at run end.
                if let Err(e) = self.emu.tt_finalize_reverse_watch() {
                    warn!("reverse watchpoint evaluation failed: {e:#}");
                }
                event_loop.exit();
            } else {
                self.auto_shot = Some((secs, path));
            }
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

/// A one-line, length-bounded form of an error for the configuration panel's
/// status line (the full chain still goes to the log).
fn short_status_error(err: &anyhow::Error) -> String {
    let msg = err.to_string();
    let first_line = msg.lines().next().unwrap_or("").trim();
    first_line.chars().take(96).collect()
}

fn set_mouse_button(emu: &mut Emulator, button: MouseButtonKind, pressed: bool) {
    let input = &mut emu.bus_mut().input;
    let index = match button {
        MouseButtonKind::Left => {
            input.lmb_port1 = pressed;
            0
        }
        MouseButtonKind::Right => {
            input.rmb_port1 = pressed;
            1
        }
        MouseButtonKind::Middle => {
            input.mmb_port1 = pressed;
            2
        }
    };
    // Reverse-debug: note the transition so replay can reproduce it.
    emu.tt_note_input(crate::inputsched::ReplayAction::MouseButton { index, pressed });
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

pub(super) const fn rgba(r: u32, g: u32, b: u32) -> u32 {
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

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
        self.set_mouse_captured(!self.mouse_captured);
    }

    /// COPPERLINE_DIAG_CURSOR: trace how the most recent click maps from host
    /// physical coordinates through pixels' window_pos_to_pixel into a
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
        let mapped = phys.map(|p| r.pixels.window_pos_to_pixel((p.x as f32, p.y as f32)));
        let pos = phys.and_then(|p| cursor_texture_position(&r.pixels, p, r.texture_scale));
        let region = match pos {
            Some(p) if cursor_in_status_bar(p) => "status_bar",
            Some(p) if cursor_in_display(p) => "display(->capture)",
            Some(_) => "other",
            None => "none",
        };
        info!(
            "[DIAG_CURSOR] button={button:?} phys={phys:?} scale_factor={scale_factor:.4} \
             inner={}x{} texture_scale={} window_pos_to_pixel={mapped:?} mapped_pos={pos:?} \
             region={region} (present_h={} window_present_h={} fb_w={FB_WIDTH})",
            inner.width,
            inner.height,
            r.texture_scale,
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
        let input = &mut self.emu.bus_mut().input;
        input.lmb_port1 = false;
        input.rmb_port1 = false;
        input.mmb_port1 = false;
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
                self.add_mouse_delta_i32(dx, dy);
            }
        }
        self.last_display_cursor_pos = Some(pos);
    }

    fn add_host_mouse_delta(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.mouse_delta_remainder.0 += dx * MOUSE_MOTION_SCALE;
        self.mouse_delta_remainder.1 += dy * MOUSE_MOTION_SCALE;
        let ix = take_integral_mouse_delta(&mut self.mouse_delta_remainder.0);
        let iy = take_integral_mouse_delta(&mut self.mouse_delta_remainder.1);
        if ix != 0 || iy != 0 {
            self.add_mouse_delta_i32(ix, iy);
        }
    }

    fn add_mouse_delta_i32(&mut self, dx: i32, dy: i32) {
        self.emu.bus_mut().input.add_mouse_delta_port1(dx, dy);
        // Reverse-debug: note the motion so replay can reproduce it.
        self.emu
            .tt_note_input(crate::inputsched::ReplayAction::MouseMove { dx, dy });
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

    /// Removable-media status for the bar controls: which drives exist,
    /// what is inserted, and whether a CD drive is fitted this session.
    fn media_bar(&self) -> MediaBar {
        let bus = self.emu.bus();
        let drives = std::array::from_fn(|idx| DriveBar {
            connected: bus.floppy.drive_connected(idx),
            inserted: bus.floppy.disk_inserted(idx),
            multi: self.disk_playlists[idx].len() > 1,
        });
        let cd = bus.cd_drive_present().then(|| bus.cd_disc_inserted());
        MediaBar { drives, cd }
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
        [
            (ToolPanelKind::Debugger, self.debugger_tool_window.as_ref()),
            (
                ToolPanelKind::FrameAnalyzer,
                self.frame_analyzer_tool_window.as_ref(),
            ),
            (ToolPanelKind::Console, self.console_tool_window.as_ref()),
        ]
        .into_iter()
        .find_map(|(kind, tool)| {
            tool.is_some_and(|tool| tool.window.id() == window_id)
                .then_some(kind)
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
                if let Some(tool) = self.tool_window_mut(kind) {
                    // Same minimized-present deadlock guard as the main
                    // window's Resized handler.
                    tool.minimized = size.width == 0 || size.height == 0;
                    if tool.minimized {
                        return;
                    }
                    let _ = tool.pixels.resize_surface(size.width, size.height);
                }
                self.request_redraw();
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
                    self.debugger_mem_scroll(rows);
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

    /// Run the action behind a clicked status-bar control (volume is
    /// handled separately because it starts a drag).
    fn activate_bar_control(&mut self, control: BarControl) {
        match control {
            BarControl::Power => self.toggle_power(),
            BarControl::Pause => self.toggle_pause(),
            BarControl::Reboot => self.reset_emulator(true),
            BarControl::Screenshot => self.take_screenshot(),
            BarControl::Menu => {
                self.ui.menu_open = !self.ui.menu_open;
                self.request_redraw();
            }
            BarControl::DriveLoad(idx) => self.load_drive_disks_from_dialog(idx),
            BarControl::DriveSwap(idx) => self.swap_drive_disk(idx),
            BarControl::DriveEject(idx) => self.eject_drive_disk(idx),
            BarControl::CdLoad => self.load_cd_from_dialog(),
            BarControl::CdEject => self.eject_cd(),
            BarControl::Joystick => {
                self.cycle_joystick_input_mode();
                self.request_redraw();
            }
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
            UiControl::MenuItem(item) => {
                self.ui.menu_open = false;
                match item {
                    ui::MenuItem::FrameAnalyzer => self.open_frame_analyzer(),
                    ui::MenuItem::About => self.ui.panel = Some(Panel::About),
                    ui::MenuItem::Shortcuts => self.ui.panel = Some(Panel::Shortcuts),
                    ui::MenuItem::Calibration => {
                        self.ui.panel =
                            Some(Panel::Calibration(crate::gamepad::CalibrationSession::new()));
                    }
                    ui::MenuItem::Debugger => self.open_debugger(),
                    ui::MenuItem::Console => self.open_console(),
                    ui::MenuItem::JoystickInput => self.cycle_joystick_input_mode(),
                    ui::MenuItem::PixelAspect => self.toggle_pixel_aspect(),
                    ui::MenuItem::Warp => self.toggle_warp(),
                    ui::MenuItem::WarpLimit => self.cycle_warp_speed(),
                    ui::MenuItem::Record => self.toggle_recording(),
                    ui::MenuItem::RecordInput => self.toggle_input_recording(),
                    ui::MenuItem::SaveState => self.save_state_interactive(),
                    ui::MenuItem::LoadState => self.load_state_from_dialog(event_loop),
                    ui::MenuItem::LoadRom => self.load_rom_from_dialog(),
                    ui::MenuItem::MachineConfig => self.open_launcher(),
                }
            }
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
                }
            }
            UiControl::LauncherCycle { field, forward } => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.cycle(field, forward);
                    state.status = None;
                }
            }
            UiControl::LauncherToggle(field) => {
                if let Some(state) = self.launcher_state_mut() {
                    state.setup.toggle(field);
                    state.status = None;
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
                    state.status = Some(StatusMessage::ok("Reset to defaults"));
                }
            }
            UiControl::LauncherBrowse(field) => self.launcher_browse(field),
            UiControl::LauncherZorroAdd => self.launcher_add_zorro(),
            UiControl::LauncherLoad => self.launcher_load(),
            UiControl::LauncherSave => self.launcher_save(),
            UiControl::LauncherRun => self.launcher_run(),
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
            _ => {}
        }
        self.request_redraw();
    }

    /// Keys consumed by the open menu/panel (Escape, debugger hex entry).
    /// Returns true when the key was handled and must not reach the Amiga.
    fn ui_handle_key(&mut self, code: KeyCode) -> bool {
        if self.ui.active() {
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
            // Route keys to a focused plugin-option text field, if any.
            if self.launcher_handle_edit_key(code) {
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
    fn launcher_handle_edit_key(&mut self, code: KeyCode) -> bool {
        let handled = {
            let Some(state) = self.launcher_state_mut() else {
                return false;
            };
            if state.editing().is_none() {
                return false;
            }
            if let Some(ch) = entry_char_for_key(code) {
                state.edit_push(ch);
            } else {
                match code {
                    KeyCode::Backspace => state.edit_backspace(),
                    KeyCode::Enter | KeyCode::NumpadEnter => state.edit_commit(),
                    // Swallow other keys while a field has focus.
                    _ => {}
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
        false
    }

    fn ui_handle_frame_analyzer_key(&mut self, code: KeyCode) -> bool {
        if self.frame_analyzer_panel.is_none() {
            return false;
        }
        let control = match code {
            KeyCode::KeyF => Some(UiControl::AnalyzerFrame),
            KeyCode::KeyR => Some(UiControl::AnalyzerRun),
            KeyCode::KeyU => Some(UiControl::AnalyzerUnderlay),
            KeyCode::KeyB => Some(UiControl::AnalyzerScrub),
            KeyCode::KeyT => Some(UiControl::AnalyzerRunTo),
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
        if let Some((dhpos, dvpos)) = delta {
            self.frame_analyzer_move_selection(dhpos, dvpos);
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
            // release it so the window's controls are reachable.
            self.set_mouse_captured(false);
            self.ui.panel = None;
            self.paused_before_debugger = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            let mut panel = ui::DebuggerPanel::new();
            // Start the memory view at the current program counter's
            // neighbourhood; it is usually what you came to look at.
            panel.mem_addr = self.emu.machine.pc() & 0x00FF_FFF0;
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
            self.set_mouse_captured(false);
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
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::Debugger, true);
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::FrameAnalyzer, true);
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::Console, true);
    }

    /// Frame-loop variant of ensure_tool_windows_for_open_panels: still
    /// creates/destroys windows to match the open panels every call, but
    /// paces the repaint of existing windows to TOOL_REDRAW_INTERVAL.
    fn refresh_tool_windows_paced(&mut self, event_loop: &ActiveEventLoop) {
        let due = self.last_tool_redraw.elapsed() >= TOOL_REDRAW_INTERVAL;
        if due {
            self.last_tool_redraw = Instant::now();
        }
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::Debugger, due);
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::FrameAnalyzer, due);
        self.ensure_tool_window_for_kind(event_loop, ToolPanelKind::Console, due);
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
        let pixels = match build_pixels_for_window(window.clone(), texture_scale, false) {
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
        *self.tool_window_slot(kind) = Some(ToolWindow {
            window,
            pixels,
            texture_scale,
            cursor_pos: None,
            minimized: false,
        });
        self.request_redraw();
    }

    fn open_frame_analyzer(&mut self) {
        if self.frame_analyzer_panel.is_none() {
            self.set_mouse_captured(false);
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
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_scrub = !panel.show_scrub;
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
        let fb = std::rc::Rc::make_mut(&mut self.analyzer_underlay_fb);
        fb.resize(MAX_FB_PIXELS, 0);
        fb.fill(0);
        let _ = bitplane::render_from_input(input, fb.as_mut_slice());
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

    /// Close the open main-window overlay panel.
    fn close_panel(&mut self) {
        self.analyzer_dragging = false;
        self.ui.panel = None;
        self.resize_for_active_panel();
        self.request_redraw();
    }

    /// Open the machine-configuration screen, seeded from the running (or
    /// last-applied) machine so it reflects the current settings.
    pub fn open_launcher(&mut self) {
        self.ui.menu_open = false;
        self.ui.panel = Some(Panel::Launcher(Box::new(LauncherState::from_raw(
            &self.machine_config,
        ))));
        self.resize_for_active_panel();
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
            | LauncherField::ScsiRomOdd => dialog.add_filter("ROM images", &["rom", "bin"]),
            LauncherField::Df0Image
            | LauncherField::Df1Image
            | LauncherField::Df2Image
            | LauncherField::Df3Image => dialog.add_filter(
                "Floppy images",
                &["adf", "adz", "dms", "scp", "gz", "ipf", "zip"],
            ),
            LauncherField::CdImage => dialog.add_filter("CD images", &["cue", "iso", "bin"]),
            LauncherField::Cd32Nvram => dialog.add_filter("NVRAM images", &["bin", "nv", "sav"]),
            _ => dialog.add_filter("Hard disk images", &["hdf", "img", "bin"]),
        };
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let picked = dialog.pick_file();
        if let Some(path) = picked {
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
    /// AROS resolution, audio-device and machine-construction errors all stay
    /// in the panel as a status line; only success swaps the live machine.
    fn launcher_run(&mut self) {
        // Capture a name/option typed but not yet committed with Enter.
        if let Some(state) = self.launcher_state_mut() {
            state.edit_commit();
        }
        let mut cfg = match self.launcher_state().map(|s| s.setup.build_config()) {
            Some(Ok(cfg)) => cfg,
            Some(Err(e)) => {
                self.set_launcher_status(StatusMessage::err(short_status_error(&e)));
                return;
            }
            None => return,
        };
        if let Err(e) = crate::config::resolve_bundled_rom(&mut cfg) {
            self.set_launcher_status(StatusMessage::err(short_status_error(&e)));
            return;
        }
        let realtime = crate::priority::requested(cfg.emulation.realtime_priority);
        let audio: Box<dyn AudioSink> = match CpalSink::new(realtime) {
            Ok(sink) => Box::new(sink),
            Err(e) => {
                self.set_launcher_status(StatusMessage::err(format!(
                    "Audio init failed: {}",
                    short_status_error(&e)
                )));
                return;
            }
        };
        // The launcher boots a fresh machine, never a save state, so a real
        // ROM is required here.
        let emu = match crate::emulator::build_machine(&cfg, audio, true, false) {
            Ok(emu) => emu,
            Err(e) => {
                self.set_launcher_status(StatusMessage::err(short_status_error(&e)));
                return;
            }
        };
        let raw = self
            .launcher_state()
            .map(|s| s.setup.to_raw())
            .unwrap_or_default();
        self.run_machine(emu, &cfg, raw);
    }

    /// Replace the live machine with a freshly built one (configuration screen
    /// Run), refreshing the host-side presentation/runtime state to match and
    /// powering it on. The previous (placeholder or running) machine, and its
    /// audio sink, are dropped here.
    fn run_machine(&mut self, emu: Emulator, cfg: &Config, raw: RawConfig) {
        self.emu = emu;
        self.machine_config = raw;
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
        self.warp_speed = cfg.emulation.warp_speed;
        // Reset the host joystick source to the new machine's configured
        // start-up mode (a previous live Cmd+J toggle does not carry over).
        self.joystick_input_mode = cfg.joystick_input_mode;
        self.keyboard_joy_held = KeyboardJoystickHeld::default();
        self.about_machine_lines = crate::config::about_machine_lines(cfg);
        self.deinterlacer =
            Deinterlacer::with_phosphor(crate::config::resolve_phosphor(cfg.phosphor));
        self.ui.menu_open = false;
        self.ui.panel = None;
        self.powered_on = true;
        self.cpu_halted = false;
        self.paused = false;
        self.reset_render_pipeline();
        self.resize_for_active_panel();
        self.show_osd("Machine started");
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
        self.resize_for_active_panel();
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
                    self.resize_for_active_panel();
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
                screenshot::scale_y_into(
                    &self.present_fb,
                    FB_WIDTH,
                    self.present_rows,
                    present_height(),
                    &mut self.record_fb,
                );
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
            presentation_h_shift_for(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        bitplane::render_display_only(self.emu.bus(), &mut self.fb);
        let geometry = self.emu.bus().frame_geometry();
        let field_rows = post_process_rendered_field(
            &mut self.fb,
            geometry,
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        let base = self.emu.bus().frame_render_base();
        self.deinterlacer.push_field(
            &self.fb,
            field_rows,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
        );
        self.refresh_present_from_deinterlacer();
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
    /// when a stop was pending.
    fn surface_debug_stop(&mut self) -> bool {
        let Some(stop) = self.emu.machine.take_ui_debug_stop() else {
            return false;
        };
        let message = stop.describe();
        info!("debugger stop: {message}");
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
            addr & 0x00FF_FFFF,
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
        let addr = addr & 0x00FF_FFFE;
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
                } & 0x00FF_FFFF;
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
                } & 0x00FF_FFFF;
                self.request_redraw();
            }
        }
    }

    /// Find the entry's hex byte pattern in CPU-visible memory, starting
    /// past the previous hit (or the current page) and wrapping around
    /// the 24-bit space once.
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
            & 0x00FF_FFFF;
        const SPACE: u64 = 0x0100_0000;
        const CHUNK: usize = 4096;
        let mut offset = 0u64;
        let mut found = None;
        while offset < SPACE {
            let base = ((u64::from(start) + offset) % SPACE) as u32;
            // Overlap chunks by the pattern length so matches spanning a
            // chunk boundary are seen.
            let bytes = self
                .emu
                .machine
                .debug_read_memory(base, CHUNK + pattern.len() - 1);
            if let Some(hit) = bytes
                .windows(pattern.len())
                .position(|window| window == pattern)
            {
                found = Some(base.wrapping_add(hit as u32) & 0x00FF_FFFF);
                break;
            }
            offset += CHUNK as u64;
        }
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
        let addr = addr & 0x00FF_FFFE;
        let before = self.emu.retired_instructions();
        match self.emu.tt_last_writer(addr, before) {
            Ok(ReverseOutcome::Found(rec)) => {
                let message = format!(
                    "${:06X}: {:04X}->{:04X} by pc ${:06X} (frame {})",
                    rec.addr,
                    rec.old,
                    rec.new,
                    rec.pc & 0x00FF_FFFF,
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
            Panel::Calibration(session) => Some(ui::PanelViewData::Calibration(
                build_calibration_view(session),
            )),
            Panel::Debugger(panel) => Some(ui::PanelViewData::Debugger(Box::new(
                self.build_debugger_view(panel),
            ))),
            Panel::FrameAnalyzer(panel) => Some(ui::PanelViewData::FrameAnalyzer(Box::new(
                self.build_frame_analyzer_view(panel),
            ))),
            // The console and configuration panels render from their own state.
            Panel::Console(_) => None,
            Panel::Launcher(_) => None,
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
        let Some(trace) = bus.frame_bus_trace() else {
            return ui::FrameAnalyzerView {
                running: !self.paused,
                status,
                trace: None,
                underlay: None,
                scrub: false,
            };
        };
        let underlay = (panel.underlay_active() && self.analyzer_underlay_rows > 0).then(|| {
            ui::AnalyzerUnderlayView {
                fb: std::rc::Rc::clone(&self.analyzer_underlay_fb),
                rows: self.analyzer_underlay_rows,
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
                    if base.dmacon & 0x0100 != 0 { "on" } else { "off" },
                    if base.dmacon & 0x0020 != 0 { "on" } else { "off" },
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
                    text.push(if a.state == "Running" {
                        ui::DbgLine::hilit(head)
                    } else {
                        ui::DbgLine::plain(head)
                    });
                    text.push(ui::DbgLine::plain(format!(
                        "  LC {:06X}  LEN {:04X}  PER {:04X}  VOL {:02X}",
                        a.lc, a.len, a.per, a.vol
                    )));
                    text.push(ui::DbgLine::plain(format!(
                        "  PTR {:06X}  words {:04X}  acc {:04X}  ph{}  out {}",
                        a.ptr, a.words_left, a.period_acc, a.phase, a.current
                    )));
                    let mut pending: Vec<&str> = Vec::new();
                    if a.dma_disable_pending {
                        pending.push("dma-disable");
                    }
                    if a.restart_pending {
                        pending.push("restart");
                    }
                    if a.manual_pending {
                        pending.push("manual");
                    }
                    if a.dma_request {
                        pending.push("dma-req");
                    }
                    if a.next_word_ready {
                        pending.push("next-word");
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
                let cd = ui::AudioRowView {
                    text: vec![
                        ui::DbgLine::hilit(format!(
                            "CD-DA  {}",
                            if cd_active { "playing" } else { "idle" }
                        )),
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
                    let base = panel.mem_addr & 0x00FF_FFFF;
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
                    let base = panel.mem_addr & 0x00FF_FFF0;
                    for row in 0..16u32 {
                        let addr = base.wrapping_add(row * 16) & 0x00FF_FFFF;
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
            .add_filter(
                "Amiga disk images",
                &["adf", "adz", "dms", "scp", "gz", "zip"],
            )
            .pick_files();

        // The modal file dialog blocks this (the main/emulation) thread, so
        // wall-clock time advanced while emulated time stood still. Re-baseline
        // the pacing anchor whether or not a file was chosen, otherwise the
        // pacer would fast-forward to catch up and corrupt pacing for the
        // freshly inserted disk. insert_disk_image -> bus floppy
        // insert_disk_image already asserts the disk-change/eject signal.
        if let Some(paths) = picked.filter(|paths| !paths.is_empty()) {
            let count = paths.len();
            let path = paths[0].clone();
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
        self.finish_host_io_pause();
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
            .set_title("Load CD image (cue sheet)")
            .add_filter("CD cue sheets", &["cue"])
            .pick_file();

        // Re-baseline pacing after the modal dialog, as for floppies.
        if let Some(path) = picked {
            match crate::cdrom::CdImage::load(&path) {
                Ok(image) => {
                    info!("cd image: {} ({})", path.display(), image.describe());
                    self.emu.bus_mut().cd_insert_disc(image);
                    self.show_osd(format!("CD: {}", display_file_name(&path)));
                    self.request_redraw();
                }
                Err(e) => {
                    warn!("cd image load failed ({}): {e:#}", path.display());
                    self.show_osd("CD: load failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
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
        match CpalSink::new(crate::priority::requested(false)) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio = Box::new(sink);
                // Apply the current suspension state to the freshly installed
                // stream (it should be live now: powered on and not paused).
                self.sync_live_audio_suspension();
            }
            Err(e) => {
                warn!("audio init after state load failed; continuing without sound: {e:#}");
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

    fn resize_for_active_panel(&self) {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        let _ = window.request_inner_size(size);
    }

    /// Menu "Pixel Aspect": flip between the 4:3 CRT presentation and
    /// square pixels for the rest of the run (the config file default is
    /// unchanged; set `[display] pixel_aspect` to make it stick).
    fn toggle_pixel_aspect(&mut self) {
        let next = match super::pixel_aspect() {
            PixelAspect::Tv => PixelAspect::Square,
            PixelAspect::Square => PixelAspect::Tv,
        };
        self.apply_pixel_aspect(next);
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
        super::set_pixel_aspect(aspect);
        if let Some(r) = self.render.as_mut() {
            if let Err(e) = r.pixels.resize_buffer(
                texture_width(r.texture_scale) as u32,
                texture_height(r.texture_scale) as u32,
            ) {
                warn!("resize texture buffer for pixel aspect failed: {e}");
            }
        }
        // Tool windows share the canvas-sized texture layout (panel
        // centring reads the live canvas height), so their buffers and
        // windows must follow the new size too.
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        for kind in [ToolPanelKind::Debugger, ToolPanelKind::FrameAnalyzer] {
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for pixel aspect failed: {e}");
                }
                let _ = tool.window.request_inner_size(size);
            }
        }
        self.resize_for_active_panel();
        self.request_redraw();
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
        if let Some(tool) = self.debugger_tool_window.as_ref() {
            if !tool.minimized {
                tool.window.request_redraw();
            }
        }
        if let Some(tool) = self.frame_analyzer_tool_window.as_ref() {
            if !tool.minimized {
                tool.window.request_redraw();
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

    fn handle_amiga_key_event(&mut self, rawkey: u8, pressed: bool) {
        if rawkey_transition_is_duplicate(&self.held_rawkeys, rawkey, pressed) {
            return;
        }
        let idx = rawkey_index(rawkey);
        self.held_rawkeys[idx] = pressed;

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
        let result = save_present_frame(
            path,
            &self.present_fb,
            src_rows,
            self.overscan,
            self.present_standard_tv_aperture,
        );
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
        });
        self.request_redraw();
    }

    /// The overlay text to draw this frame, or None when nothing is
    /// active. Expired overlays are dropped as a side effect.
    fn active_osd_text(&mut self) -> Option<String> {
        match &self.osd {
            Some(osd) if Instant::now() < osd.expires_at => Some(osd.text.clone()),
            Some(_) => {
                self.osd = None;
                None
            }
            None => None,
        }
    }

    fn dump_frame_if_due(&mut self, _now: Instant, event_loop: &ActiveEventLoop) -> bool {
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
            self.overscan,
            self.present_standard_tv_aperture,
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
                event_loop.exit();
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
            event_loop.exit();
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
            info!("power button: machine powered on (cold boot)");
        }
        self.request_redraw();
    }

    /// Toggle host-level pause. Pausing freezes the emulator in place
    /// (it stops stepping but stays powered on), so the current frame is
    /// held and emulation resumes from the same point when unpaused.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.sync_live_audio_suspension();
        if self.paused {
            info!("pause button: emulation paused");
        } else {
            info!("pause button: emulation resumed");
        }
        self.request_redraw();
    }

    /// Power off: drop into a cold-boot state (RAM cleared) and park the
    /// test screen, so a later power-on comes up as a clean power cycle.
    fn power_off(&mut self) {
        self.powered_on = false;
        self.paused = false;
        self.sync_live_audio_suspension();
        info!("power button: machine powered off (cold boot state)");
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
            .push_field(&self.fb, FB_HEIGHT, false, true, true);
        self.refresh_present_from_deinterlacer();
    }

    fn reset_emulator(&mut self, clear_host_keys: bool) {
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
        let active = rows * FB_WIDTH;
        self.present_fb.resize(active, 0);
        self.present_fb
            .copy_from_slice(&self.deinterlacer.output()[..active]);
        self.present_rows = rows;
    }

    fn reset_render_pipeline(&mut self) {
        self.render_generation = self.render_generation.wrapping_add(1);
        self.last_rendered_emulated_frame = None;
        self.last_submitted_render_frame = None;
        let _ = self.collect_threaded_render_results(false);
    }

    fn apply_threaded_render_result(&mut self, result: RenderWorkerResult) -> bool {
        // Only one job is in flight at a time, so the returned snapshot is
        // always the freshest one to recycle.
        self.render_recycle_input = Some(result.input);
        if result.generation != self.render_generation {
            if self.render_recycle_fb.is_empty() {
                self.render_recycle_fb = result.presentation_fb;
            }
            return false;
        }

        self.emu.bus_mut().record_video_render_frame(result.timing);
        let old = std::mem::replace(&mut self.present_fb, result.presentation_fb);
        self.render_recycle_fb = old;
        self.present_rows = result.present_rows;
        self.present_standard_tv_aperture = result.standard_tv_aperture;
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
            presentation_h_shift_for(&input.render_base(), self.overscan)
        } else {
            0
        };
        let job = RenderJob {
            generation: self.render_generation,
            input,
            h_shift,
            overscan: self.overscan,
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

    fn render_emulated_frame_if_needed(&mut self) -> bool {
        if !self.emu.bus().frame_render_available() {
            return false;
        }
        if self.render_worker.is_some() {
            return self.render_emulated_frame_threaded();
        }
        self.render_emulated_frame_sync()
    }

    fn render_emulated_frame_sync(&mut self) -> bool {
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_rendered_emulated_frame, emulated_frame) {
            return false;
        }

        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        let h_shift = if self.hcenter {
            presentation_h_shift_for(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        bitplane::render(self.emu.bus_mut(), &mut self.fb);
        let geometry = self.emu.bus().frame_geometry();
        let field_rows = post_process_rendered_field(
            &mut self.fb,
            geometry,
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        let base = self.emu.bus().frame_render_base();
        // Standard 15 kHz fields line-double / weave to 2x rows; a
        // programmable progressive scan already carries every line.
        self.deinterlacer.push_field(
            &self.fb,
            field_rows,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
        );
        self.refresh_present_from_deinterlacer();
        self.present_standard_tv_aperture =
            uses_standard_pal_tv_aperture(geometry, self.present_rows, &base);
        self.last_rendered_emulated_frame = Some(emulated_frame);
        self.last_submitted_render_frame = Some(emulated_frame);
        true
    }
}

mod console;
mod host_input;
mod present;
mod statusbar;
pub(super) use present::{scale_rect, texture_height, texture_width, Rect};
pub(super) use statusbar::{draw_rect_bevel, fill_rect, fill_rect_blend};

pub use host_input::parse_amiga_key;
use host_input::*;
pub(crate) use present::center_present_frame_for_visible_start;
use present::*;
use statusbar::*;

#[cfg(test)]
mod tests;

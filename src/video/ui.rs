// SPDX-License-Identifier: GPL-3.0-or-later

//! In-window menu and overlay sub-windows (about, keyboard shortcuts,
//! gamepad calibration, debugger). Everything is drawn into the
//! presentation texture over the emulated display, styled after the
//! classic Amiga look: white menus with inverted highlights and blue
//! window title bars. This module owns layout, hit-testing and drawing;
//! `window.rs` routes events to it and builds the per-frame view data
//! (register snapshots, disassembly text) the panels render.

use super::launcher::{self, EditTarget, LauncherField, LauncherState, LauncherTab, RowKind};
use super::window::{
    draw_rect_bevel, fill_rect, fill_rect_blend, rgba, scale_rect, JoystickInputMode, Rect,
    BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE, BUTTON_FACE_HOVER,
};
use super::{font, present_height, FB_WIDTH, HOST_SHORTCUT_MODIFIER_LABEL};
use crate::config::{MachineModel, PixelAspect, WarpSpeed};
use crate::debugger::{BreakCond, CondOp, CondOperand};
use crate::heatmap;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

const MENU_BG: u32 = rgba(238, 238, 232);
const MENU_TEXT: u32 = rgba(12, 12, 14);
const MENU_HILIGHT_BG: u32 = rgba(0, 85, 170);
const MENU_HILIGHT_TEXT: u32 = rgba(255, 255, 255);
/// A scroll row that has run out of list in its direction.
const MENU_TEXT_DISABLED: u32 = rgba(150, 150, 146);
const MENU_EDGE: u32 = rgba(12, 12, 14);
const PANEL_BG: u32 = rgba(30, 32, 36);
const PANEL_TITLE_BG: u32 = rgba(0, 85, 170);
const PANEL_TITLE_TEXT: u32 = rgba(255, 255, 255);
const PANEL_TEXT: u32 = rgba(214, 216, 208);
const PANEL_TEXT_DIM: u32 = rgba(136, 138, 130);
const PANEL_TEXT_HILIGHT: u32 = rgba(120, 255, 150);
const PANEL_TEXT_ACCENT: u32 = rgba(255, 184, 80);
const BUTTON_TEXT: u32 = rgba(220, 222, 214);
const BUTTON_TEXT_DISABLED: u32 = rgba(120, 120, 112);
/// DDF fetch-bound verticals on the Frame Analyzer heatmap.
const DDF_LINE: u32 = rgba(80, 200, 220);
const ENTRY_BG: u32 = rgba(8, 10, 8);
const ENTRY_TEXT: u32 = rgba(27, 220, 71);
const SCRIM: u32 = rgba(0, 0, 0);
const SCRIM_ALPHA: f32 = 0.45;
// Audio-tab oscilloscope trace colours (Paula ch0..3 then CD-DA).
const AUDIO_SCOPE_COLORS: [u32; 5] = [
    rgba(120, 255, 150), // ch0 green
    rgba(96, 200, 255),  // ch1 cyan
    rgba(230, 130, 245), // ch2 magenta
    rgba(240, 214, 96),  // ch3 yellow
    rgba(255, 170, 90),  // CD amber
];
const AUDIO_MUTE_FACE: u32 = rgba(96, 44, 44);

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

/// Status-bar anchor for the menu button; the pop-up opens above it.
pub const MENU_BUTTON_X: usize = FB_WIDTH - 220;
pub const MENU_BUTTON_W: usize = 22;

const MENU_ITEM_H: usize = 20;
const MENU_PAD: usize = 3;
/// Font pixel scale labels are drawn at, and the text inset from each side of
/// the popup. The widest label is "Joystick Input  [keyboard]" (26 chars);
/// sizing the popup to it keeps every item's text (and its trailing "...")
/// inside the menu background instead of spilling past the right edge.
const MENU_TEXT_PX: usize = 2;
const MENU_TEXT_INSET: usize = 8;
const MENU_MAX_LABEL_CHARS: usize = 26;
const MENU_W: usize = 2 * MENU_TEXT_INSET + MENU_MAX_LABEL_CHARS * font::GLYPH_W * MENU_TEXT_PX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    FrameAnalyzer,
    About,
    Shortcuts,
    Calibration,
    Debugger,
    Console,
    JoystickInput,
    Port1Device,
    Port2Device,
    Autofire,
    InputMapping,
    #[cfg(feature = "midi")]
    MidiInput,
    #[cfg(feature = "midi")]
    MidiOutput,
    SamplerInput,
    SamplerGain,
    PixelAspect,
    CrtShader,
    FloppySpeed,
    Fullscreen,
    StatusBar,
    AudioOutput,
    AudioFilter,
    Warp,
    WarpLimit,
    Rewind,
    Record,
    RecordInput,
    SaveState,
    LoadState,
    QuickSave,
    QuickLoad,
    SaveSlot,
    LoadRom,
    MachineConfig,
}

/// The menu items, top to bottom. The MIDI device items appear only when the
/// serial port is in MIDI mode, and the sampler items only when a parallel-port
/// sampler is attached, so the list is built per open rather than fixed.
pub fn menu_items(midi_active: bool, sampler_active: bool) -> Vec<MenuItem> {
    let _ = midi_active;
    // 12 leading + up to 2 MIDI + 2 sampler + pixel aspect + CRT shader +
    // floppy speed + 15 trailing items = 34, sized so appending never
    // reallocates.
    let mut items = Vec::with_capacity(34);
    items.extend([
        MenuItem::MachineConfig,
        MenuItem::FrameAnalyzer,
        MenuItem::Debugger,
        MenuItem::Console,
        MenuItem::AudioOutput,
        MenuItem::AudioFilter,
        MenuItem::Calibration,
        MenuItem::JoystickInput,
        MenuItem::Port1Device,
        MenuItem::Port2Device,
        MenuItem::Autofire,
        MenuItem::InputMapping,
    ]);
    #[cfg(feature = "midi")]
    if midi_active {
        items.push(MenuItem::MidiInput);
        items.push(MenuItem::MidiOutput);
    }
    if sampler_active {
        items.push(MenuItem::SamplerInput);
        items.push(MenuItem::SamplerGain);
    }
    items.push(MenuItem::PixelAspect);
    items.push(MenuItem::CrtShader);
    items.push(MenuItem::FloppySpeed);
    items.extend([
        MenuItem::Fullscreen,
        MenuItem::StatusBar,
        MenuItem::Warp,
        MenuItem::WarpLimit,
        MenuItem::Rewind,
        MenuItem::Record,
        MenuItem::RecordInput,
        MenuItem::SaveState,
        MenuItem::LoadState,
        MenuItem::QuickSave,
        MenuItem::QuickLoad,
        MenuItem::SaveSlot,
        MenuItem::LoadRom,
        MenuItem::Shortcuts,
        MenuItem::About,
    ]);
    items
}

/// Bundled state a menu label may show, so the label helper takes one argument.
#[derive(Clone, Copy)]
pub struct MenuLabels<'a> {
    pub warp: bool,
    pub warp_speed: WarpSpeed,
    /// True while the host window is fullscreen (the menu item toggles it).
    pub fullscreen: bool,
    /// True while the status bar is hidden (the menu item toggles it).
    pub status_bar_hidden: bool,
    pub recording: bool,
    pub input_recording: bool,
    /// Whether rewind history is being recorded (the Rewind item toggles it).
    pub rewind: bool,
    /// Numbered save-state slot the Quick Save / Quick Load items act on.
    pub save_slot: usize,
    /// Autofire rate in Hz shown on the Autofire item; 0 is "off".
    pub autofire_hz: u8,
    pub joystick_input_mode: JoystickInputMode,
    /// Devices currently plugged into the two game ports (hot-pluggable
    /// through the Port 1/2 Device items).
    pub port_devices: [crate::bus::PortDevice; 2],
    pub pixel_aspect: PixelAspect,
    /// Window shader pass in effect (the CRT Shader item cycles it).
    pub shader: crate::config::ShaderKind,
    /// Current `[floppy] speed` value (a percentage, or 0 for turbo).
    pub floppy_speed: u16,
    /// Current MIDI input/output device names (empty when not applicable).
    #[cfg_attr(not(feature = "midi"), allow(dead_code))]
    pub midi_in: &'a str,
    #[cfg_attr(not(feature = "midi"), allow(dead_code))]
    pub midi_out: &'a str,
    /// Current audio output label: "Default", a device name, or "Disabled"
    /// (empty is treated as "Default").
    pub audio_output: &'a str,
    /// Current Paula filter override (Auto/On/Off), shown on the Audio Filter
    /// item.
    pub audio_filter: crate::config::AudioFilterMode,
    /// Current sampler input device name (empty is treated as "Default") and
    /// gain label (e.g. "2x"); only shown when a sampler is attached.
    pub sampler_input: &'a str,
    pub sampler_gain: &'a str,
}

fn menu_item_label(item: MenuItem, s: MenuLabels) -> String {
    match item {
        MenuItem::FrameAnalyzer => "Frame Analyzer...".to_string(),
        MenuItem::About => "About...".to_string(),
        MenuItem::Shortcuts => "Keyboard Shortcuts...".to_string(),
        MenuItem::Calibration => "Calibrate Gamepad...".to_string(),
        MenuItem::Debugger => "Debugger...".to_string(),
        MenuItem::Console => "Console...".to_string(),
        MenuItem::JoystickInput => format!("Joystick Input  [{}]", s.joystick_input_mode.label()),
        MenuItem::Port1Device => format!("Port 1 Device  [{}]", s.port_devices[0].label()),
        MenuItem::Port2Device => format!("Port 2 Device  [{}]", s.port_devices[1].label()),
        MenuItem::Autofire => format!(
            "Autofire {:>13}",
            format!("[{}]", crate::config::autofire_label(s.autofire_hz))
        ),
        MenuItem::InputMapping => "Input Mapping...".to_string(),
        MenuItem::PixelAspect => {
            let value = match s.pixel_aspect {
                PixelAspect::Tv => "tv",
                PixelAspect::Square => "square",
            };
            format!("Pixel Aspect {:>8}", format!("[{value}]"))
        }
        // Right-pad like Pixel Aspect above so the closing bracket stays put
        // as the value width changes ("off" vs "scanlines").
        MenuItem::CrtShader => {
            format!("CRT Shader {:>11}", format!("[{}]", s.shader.label()))
        }
        // Right-pad like Warp Limit below so the closing bracket stays put
        // as the value width changes (100% vs turbo).
        MenuItem::FloppySpeed => {
            format!(
                "Floppy Speed {:>7}",
                format!("[{}]", crate::floppy::speed_label(s.floppy_speed))
            )
        }
        #[cfg(feature = "midi")]
        MenuItem::MidiInput => format!("MIDI In  [{}]", clip_menu_value(s.midi_in)),
        #[cfg(feature = "midi")]
        MenuItem::MidiOutput => format!("MIDI Out [{}]", clip_menu_value(s.midi_out)),
        MenuItem::AudioOutput => {
            let name = if s.audio_output.is_empty() {
                "Default"
            } else {
                s.audio_output
            };
            format!("Audio Out [{}]", clip_menu_value(name))
        }
        MenuItem::AudioFilter => {
            let value = match s.audio_filter {
                crate::config::AudioFilterMode::Auto => "auto",
                crate::config::AudioFilterMode::On => "on",
                crate::config::AudioFilterMode::Off => "off",
            };
            format!("Audio Filter {:>7}", format!("[{value}]"))
        }
        MenuItem::SamplerInput => {
            let name = if s.sampler_input.is_empty() {
                "Default"
            } else {
                s.sampler_input
            };
            format!("Sampler In [{}]", clip_menu_value(name))
        }
        MenuItem::SamplerGain => format!("Sampler Gain {:>5}", format!("[{}]", s.sampler_gain)),
        MenuItem::Fullscreen if s.fullscreen => "Fullscreen      [on]".to_string(),
        MenuItem::Fullscreen => "Fullscreen     [off]".to_string(),
        MenuItem::StatusBar if s.status_bar_hidden => "Status Bar     [off]".to_string(),
        MenuItem::StatusBar => "Status Bar      [on]".to_string(),
        MenuItem::Warp if s.warp => "Warp Speed      [on]".to_string(),
        MenuItem::Warp => "Warp Speed     [off]".to_string(),
        // Right-pad so the closing bracket stays put as the value width
        // changes (2x/8x vs 16x/Max), aligning with the Warp Speed row above.
        MenuItem::WarpLimit => {
            format!(
                "Warp Limit     {:>5}",
                format!("[{}]", s.warp_speed.label())
            )
        }
        MenuItem::Rewind if s.rewind => "Rewind          [on]".to_string(),
        MenuItem::Rewind => "Rewind         [off]".to_string(),
        MenuItem::Record if s.recording => "Stop Video Recording".to_string(),
        MenuItem::Record => "Record Video".to_string(),
        MenuItem::RecordInput if s.input_recording => "Stop Input Recording".to_string(),
        MenuItem::RecordInput => "Record Input".to_string(),
        MenuItem::SaveState => "Save State".to_string(),
        MenuItem::LoadState => "Load State...".to_string(),
        MenuItem::QuickSave => format!("Quick Save {:>9}", format!("[slot {}]", s.save_slot)),
        MenuItem::QuickLoad => format!("Quick Load {:>9}", format!("[slot {}]", s.save_slot)),
        MenuItem::SaveSlot => format!("Save Slot {:>10}", format!("[{}]", s.save_slot)),
        MenuItem::LoadRom => "Load Kickstart ROM...".to_string(),
        MenuItem::MachineConfig => "Machine Configuration...".to_string(),
    }
}

/// Clip a device name so a "MIDI Out [name]" / "Audio Out [name]" /
/// "Sampler In [name]" label stays within the popup.
fn clip_menu_value(name: &str) -> String {
    const MAX: usize = MENU_MAX_LABEL_CHARS - 13; // widest prefix "Sampler In [" plus "]"
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let kept: String = name.chars().take(MAX.saturating_sub(1)).collect();
    format!("{kept}~")
}

/// Labels of the two scroll rows a scrolling menu grows at its ends. Centred,
/// so they read as chrome rather than as another item.
const MENU_SCROLL_UP_LABEL: &str = "^ more ^";
const MENU_SCROLL_DOWN_LABEL: &str = "v more v";

/// How a menu of `item_count` items is laid out.
///
/// The menu is anchored to the bottom of the display and grows upward, so a
/// long list (the MIDI and sampler items appear only in some sessions) can
/// reach the top edge. It degrades in two stages: first the rows tighten
/// toward the height of the label font itself, and only when even that does
/// not fit does the list scroll, giving up a row at each end to the scroll
/// controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MenuLayout {
    item_h: usize,
    /// Items on screen at once. Equal to `item_count` when not scrolling.
    visible: usize,
    scrolling: bool,
}

impl MenuLayout {
    /// Total rows drawn, counting the two scroll rows.
    fn rows(&self) -> usize {
        self.visible + if self.scrolling { 2 } else { 0 }
    }

    /// Largest first-visible index, i.e. the scroll position that puts the
    /// last item at the bottom.
    fn max_scroll(&self, item_count: usize) -> usize {
        item_count.saturating_sub(self.visible)
    }
}

fn menu_layout(item_count: usize) -> MenuLayout {
    let avail = present_height().saturating_sub(2 + 2 * MENU_PAD);
    let floor = MENU_TEXT_PX * font::GLYPH_H;
    let item_h = (avail / item_count.max(1)).clamp(floor, MENU_ITEM_H);
    if item_count * item_h <= avail {
        return MenuLayout {
            item_h,
            visible: item_count,
            scrolling: false,
        };
    }
    // Too many items for the display even at the tightest row height: show a
    // window into the list, with a scroll row above and below it.
    let rows = avail / floor;
    MenuLayout {
        item_h: floor,
        visible: rows.saturating_sub(2).max(1),
        scrolling: true,
    }
}

/// Clamp a scroll position to the range this item count allows.
pub fn clamp_menu_scroll(scroll: usize, item_count: usize) -> usize {
    scroll.min(menu_layout(item_count).max_scroll(item_count))
}

fn menu_rect(item_count: usize) -> Rect {
    let layout = menu_layout(item_count);
    let h = layout.rows() * layout.item_h + 2 * MENU_PAD;
    let right = MENU_BUTTON_X + MENU_BUTTON_W;
    Rect {
        x: right.saturating_sub(MENU_W),
        y: present_height().saturating_sub(h + 2),
        w: MENU_W,
        h,
    }
}

/// The rect of the `row`-th drawn row (scroll rows included), regardless of
/// what occupies it.
fn menu_row_rect(row: usize, item_count: usize) -> Rect {
    let menu = menu_rect(item_count);
    let layout = menu_layout(item_count);
    Rect {
        x: menu.x + 1,
        y: menu.y + MENU_PAD + row * layout.item_h,
        w: menu.w - 2,
        h: layout.item_h,
    }
}

/// The rect of menu item `index` (an index into the whole list) at scroll
/// position `scroll`, or `None` when it is scrolled out of view.
fn menu_item_rect(index: usize, item_count: usize, scroll: usize) -> Option<Rect> {
    let layout = menu_layout(item_count);
    let scroll = scroll.min(layout.max_scroll(item_count));
    let offset = index.checked_sub(scroll)?;
    if offset >= layout.visible {
        return None;
    }
    Some(menu_row_rect(
        offset + usize::from(layout.scrolling),
        item_count,
    ))
}

/// The two scroll rows, when the menu is scrolling: the first and last drawn
/// rows. Each is reported with whether it can still move in its direction, so
/// the draw can grey out an exhausted end and the hit test can ignore it.
fn menu_scroll_rows(item_count: usize, scroll: usize) -> Option<[(UiControl, Rect, bool); 2]> {
    let layout = menu_layout(item_count);
    if !layout.scrolling {
        return None;
    }
    let scroll = scroll.min(layout.max_scroll(item_count));
    Some([
        (
            UiControl::MenuScrollUp,
            menu_row_rect(0, item_count),
            scroll > 0,
        ),
        (
            UiControl::MenuScrollDown,
            menu_row_rect(layout.rows() - 1, item_count),
            scroll < layout.max_scroll(item_count),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Panels (overlay sub-windows)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugTab {
    Cpu,
    Chipset,
    Copper,
    Video,
    Audio,
    Memory,
    IoMap,
    Break,
    Waveform,
}

pub const DEBUG_TABS: [DebugTab; 9] = [
    DebugTab::Cpu,
    DebugTab::Chipset,
    DebugTab::Copper,
    DebugTab::Video,
    DebugTab::Audio,
    DebugTab::Memory,
    DebugTab::IoMap,
    DebugTab::Break,
    DebugTab::Waveform,
];

fn debug_tab_label(tab: DebugTab) -> &'static str {
    match tab {
        DebugTab::Cpu => "CPU",
        DebugTab::Chipset => "Chipset",
        DebugTab::Copper => "Copper",
        DebugTab::Video => "Video",
        DebugTab::Audio => "Audio",
        DebugTab::Memory => "Memory",
        DebugTab::IoMap => "IO Map",
        DebugTab::Break => "Break",
        DebugTab::Waveform => "Wave",
    }
}

/// Interactive state of the debugger sub-window.
#[derive(Clone)]
pub struct DebuggerPanel {
    pub tab: DebugTab,
    /// Base address of the Memory tab's hex dump.
    pub mem_addr: u32,
    /// Pinned disassembly origin for the CPU tab; None follows the PC.
    pub disasm_addr: Option<u32>,
    /// The hex address being typed into the entry box.
    pub entry: String,
    /// Whether the entry box has keyboard focus.
    pub entry_active: bool,
    /// Memory tab: where the last Find hit landed, so repeating Find
    /// continues past it instead of re-finding the same match.
    pub mem_last_find: Option<u32>,
    /// Memory tab: render the page as a 1-bpp bitplane instead of hex.
    pub mem_view_bits: bool,
    /// Memory tab bitmap mode: row stride in bytes (40 = a standard
    /// 320-pixel-wide plane).
    pub mem_bitmap_stride: u32,
    /// IO Map tab: the selected custom-register word offset ($000-$1FE).
    pub iomap_sel: u16,
}

impl DebuggerPanel {
    pub fn new() -> Self {
        Self {
            tab: DebugTab::Cpu,
            mem_addr: 0,
            disasm_addr: None,
            entry: String::new(),
            entry_active: false,
            mem_last_find: None,
            mem_view_bits: false,
            mem_bitmap_stride: 40,
            iomap_sel: 0x096,
        }
    }

    /// The typed address: the first whitespace-separated token parsed as hex.
    /// (Poke uses a second token; the address consumers only need the first.)
    pub fn entry_addr(&self) -> Option<u32> {
        parse_hex_u32(self.entry.split_whitespace().next()?)
    }

    /// Memory poke target: two hex tokens "ADDR VALUE", as an even address and
    /// the 16-bit word to write there.
    pub fn poke_target(&self) -> Option<(u32, u16)> {
        let mut tokens = self.entry.split_whitespace();
        let addr = parse_hex_u32(tokens.next()?)?;
        let value = parse_hex_u32(tokens.next()?)?;
        Some((addr & !1, value as u16))
    }

    /// Register poke target: a register name then a hex value, e.g. "D0 1234"
    /// or "PC F80000". Returns the GDB-style register index and the value.
    pub fn reg_poke(&self) -> Option<(usize, u32)> {
        let mut tokens = self.entry.split_whitespace();
        let reg = parse_reg_name(tokens.next()?)?;
        let value = parse_hex_u32(tokens.next()?)?;
        Some((reg, value))
    }

    /// Memory-search pattern: the entry's tokens concatenated as hex byte
    /// pairs ("C0 FFEE" and "C0FFEE" both match the bytes C0 FF EE).
    pub fn find_pattern(&self) -> Option<Vec<u8>> {
        let joined: String = self.entry.split_whitespace().collect();
        if joined.is_empty() || !joined.len().is_multiple_of(2) {
            return None;
        }
        (0..joined.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&joined[i..i + 2], 16).ok())
            .collect()
    }

    /// Region spec for Save region: "ADDR LEN", both hex. The address is
    /// taken as written -- a dump can start anywhere the CPU decodes,
    /// including the motherboard, CPU-slot, and Zorro III RAM above the
    /// 24-bit space -- and only the length is capped, at 16 MiB per dump.
    pub fn region_spec(&self) -> Option<(u32, u32)> {
        let mut tokens = self.entry.split_whitespace();
        let addr = parse_hex_u32(tokens.next()?)?;
        let len = parse_hex_u32(tokens.next()?)?;
        if tokens.next().is_some() || len == 0 || len > 0x0100_0000 {
            return None;
        }
        Some((addr, len))
    }

    pub fn push_entry_char(&mut self, ch: char) {
        // Alphanumerics and spaces: hex for addresses/values, letters for
        // register names (Dn/An/PC/SR), memory operands (M<hex>), and the
        // breakpoint-condition mnemonics (EQ/NE/LT/GT/LE/GE/AND/IGN). A leading
        // or doubled space is dropped so the tokens stay clean. The extra
        // punctuation set serves the Waveform tab's trigger/duration/signal
        // specs (PC=..., BEAM=V:H, CPU,BUS, 2.5S) and output paths (both
        // separator styles, for Windows).
        let punctuation = matches!(ch, '=' | ':' | ',' | '.' | '-' | '_' | '/' | '\\');
        if (!ch.is_ascii_alphanumeric() && ch != ' ' && !punctuation) || self.entry.len() >= 40 {
            return;
        }
        if ch == ' ' && (self.entry.is_empty() || self.entry.ends_with(' ')) {
            return;
        }
        self.entry.push(ch.to_ascii_uppercase());
    }

    pub fn backspace_entry(&mut self) {
        self.entry.pop();
    }
}

impl Default for DebuggerPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Which view of the traced machine the Frame Analyzer shows: the beam
/// (what owned the chip bus at each colour clock) or memory (what last
/// touched each block of the address space).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnalyzerTab {
    Beam,
    Memory,
}

pub const ANALYZER_TABS: [AnalyzerTab; 2] = [AnalyzerTab::Beam, AnalyzerTab::Memory];

fn analyzer_tab_label(tab: AnalyzerTab) -> &'static str {
    match tab {
        AnalyzerTab::Beam => "Beam",
        AnalyzerTab::Memory => "Memory",
    }
}

/// A one-click heat map window: a named region of the address space
/// (chip RAM, the whole 24-bit space, a RAM board) to point the map at.
#[derive(Clone)]
pub struct HeatPreset {
    pub label: String,
    pub base: u32,
    pub span: u32,
}

/// Interactive state of the frame analyzer pane.
#[derive(Clone)]
pub struct FrameAnalyzerPanel {
    pub tab: AnalyzerTab,
    pub selected_vpos: u16,
    pub selected_hpos: u16,
    /// Draw the rendered frame under the DMA heatmap so bus activity can
    /// be correlated spatially with the picture.
    pub show_underlay: bool,
    /// Beam scrub: show the picture only up to the selected slot -- what
    /// the CRT had drawn when the beam was there. Implies the underlay.
    pub show_scrub: bool,
    /// Memory tab: the address-space windows offered as buttons. Empty
    /// until window.rs builds them from the machine's memory map.
    pub heat_presets: Vec<HeatPreset>,
    /// Memory tab: the pinned cell (an index into the 256x256 grid) whose
    /// address range and last toucher are reported under the map.
    pub heat_selected: Option<usize>,
}

impl FrameAnalyzerPanel {
    pub fn new() -> Self {
        Self {
            tab: AnalyzerTab::Beam,
            selected_vpos: 0x2C,
            selected_hpos: 0x28,
            show_underlay: false,
            show_scrub: false,
            heat_presets: Vec::new(),
            heat_selected: None,
        }
    }

    /// Whether the picture underlay is active (directly or via scrub).
    pub fn underlay_active(&self) -> bool {
        self.show_underlay || self.show_scrub
    }
}

impl Default for FrameAnalyzerPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Interactive state of the debugger console: a command line with
/// history over a scrollback of output lines. The console owns
/// everything it renders, so it needs no per-redraw view data.
#[derive(Clone, Default)]
pub struct ConsolePanel {
    /// The command being typed.
    pub input: String,
    /// Scrollback, oldest first, capped at [`CONSOLE_SCROLLBACK_LINES`].
    pub output: std::collections::VecDeque<String>,
    /// Lines scrolled back from the tail (0 = pinned to the newest).
    pub scroll: usize,
    /// Previously executed commands, oldest first.
    pub history: Vec<String>,
    /// Index into `history` while browsing with Up/Down; None = live.
    pub history_pos: Option<usize>,
}

/// Scrollback capacity of the console, in lines.
pub const CONSOLE_SCROLLBACK_LINES: usize = 500;

impl ConsolePanel {
    pub fn push_output(&mut self, line: impl Into<String>) {
        if self.output.len() >= CONSOLE_SCROLLBACK_LINES {
            self.output.pop_front();
        }
        self.output.push_back(line.into());
    }

    pub fn push_input_char(&mut self, ch: char) {
        // Any printable ASCII (the interpreter is case-insensitive, so
        // what you type or paste is what you see).
        if !(' '..='~').contains(&ch) || self.input.len() >= 72 {
            return;
        }
        // Doubled leading spaces never help a command line.
        if ch == ' ' && (self.input.is_empty() || self.input.ends_with(' ')) {
            return;
        }
        self.input.push(ch);
        self.history_pos = None;
    }

    /// Browse command history: `delta` -1 = older, +1 = newer. Leaving
    /// the newest entry restores an empty line.
    pub fn history_step(&mut self, delta: i32) {
        if self.history.is_empty() {
            return;
        }
        let pos = match (self.history_pos, delta) {
            (None, d) if d < 0 => Some(self.history.len() - 1),
            (None, _) => None,
            (Some(0), d) if d < 0 => Some(0),
            (Some(p), d) if d < 0 => Some(p - 1),
            (Some(p), _) if p + 1 < self.history.len() => Some(p + 1),
            (Some(_), _) => None,
        };
        self.history_pos = pos;
        self.input = pos.map(|p| self.history[p].clone()).unwrap_or_default();
    }
}

/// One drive target offered by the drop chooser.
pub struct DropDriveEntry {
    pub drive: usize,
    /// Ready-made button label, e.g. "DF0: workbench.adf" or "DF1 (empty)".
    pub label: String,
}

/// State of the dropped-disk drive chooser. Everything is snapshotted at
/// open time: the panel is modal, so the drive labels cannot change under
/// it, and no per-frame view data is needed.
pub struct DropChooserState {
    /// The dropped image paths; all become the chosen drive's swap playlist.
    pub disks: Vec<std::path::PathBuf>,
    /// Header line naming what is being inserted (first file's name).
    pub disk_label: String,
    /// One entry per connected drive, in DF order.
    pub drives: Vec<DropDriveEntry>,
}

/// Interactive state of the Input Mapping panel: a working copy of the
/// keyboard map that is only committed to disk on Save, plus which mapping is
/// on screen and which row (if any) is waiting for a key press.
pub struct InputMapPanel {
    /// Keyboard mapping being edited (0 = controller 1, 1 = controller 2).
    pub mapping: usize,
    /// Control armed for capture: the next bindable key press binds to it.
    pub capturing: Option<crate::keymap::JoyControl>,
    /// Working copy of the map. Edits here do not reach the live machine
    /// until Save.
    pub map: crate::keymap::KeyMap,
    /// Feedback line under the table.
    pub message: String,
}

impl InputMapPanel {
    pub fn new(map: crate::keymap::KeyMap) -> Self {
        Self {
            mapping: 0,
            capturing: None,
            map,
            message: "Click Set, then press the key to bind.".to_string(),
        }
    }

    /// Bind a captured host key to the armed control. Returns false (and
    /// leaves the row armed) for a key that cannot be bound, so a stray press
    /// does not silently cancel the capture.
    pub fn capture_key(&mut self, code: winit::keyboard::KeyCode) -> bool {
        let Some(control) = self.capturing else {
            return false;
        };
        if !crate::keymap::is_bindable(code) {
            self.message = "That key cannot be bound to a controller.".to_string();
            return false;
        }
        self.map.bind(self.mapping, control, code);
        self.capturing = None;
        self.message = format!(
            "{} bound to {}.",
            control.label(),
            crate::keymap::short_key_label(code)
        );
        true
    }
}

/// An open overlay sub-window.
pub enum Panel {
    About,
    Shortcuts,
    Calibration(crate::gamepad::CalibrationSession),
    /// Keyboard controller remapping. Boxed like the launcher: it carries a
    /// whole working copy of the key map, far larger than the other variants.
    InputMap(Box<InputMapPanel>),
    Debugger(DebuggerPanel),
    FrameAnalyzer(FrameAnalyzerPanel),
    Console(ConsolePanel),
    /// The pre-boot machine-configuration screen. Boxed: its state is far
    /// larger than the other variants.
    Launcher(Box<LauncherState>),
    /// Drive chooser for dropped disk images: winit reports file drops
    /// with no cursor position, so with several connected drives the drop
    /// lands anywhere on the window and the target is picked here.
    DropChooser(DropChooserState),
}

/// Menu/panel state owned by the window.
#[derive(Default)]
pub struct UiState {
    pub menu_open: bool,
    /// First visible menu item when the list is too long for the display.
    /// Always 0 for a menu that fits; reset each time the menu opens.
    pub menu_scroll: usize,
    pub panel: Option<Panel>,
}

impl UiState {
    /// Whether the UI is consuming pointer/keyboard input.
    pub fn active(&self) -> bool {
        self.menu_open || self.panel.is_some()
    }

    /// The UI control under `pos`, if any. `midi_active`/`sampler_active` select
    /// the same menu item list the draw uses. `PanelBody` swallows clicks on a
    /// panel's background so they never reach the emulated display.
    pub fn control_at(
        &self,
        pos: (i32, i32),
        midi_active: bool,
        sampler_active: bool,
    ) -> Option<UiControl> {
        if self.menu_open {
            let items = menu_items(midi_active, sampler_active);
            // The scroll rows sit where items would otherwise be, so they are
            // tested first; an exhausted end swallows the click as chrome.
            if let Some(rows) = menu_scroll_rows(items.len(), self.menu_scroll) {
                for (control, rect, enabled) in rows {
                    if rect.contains(pos) {
                        return Some(if enabled {
                            control
                        } else {
                            UiControl::PanelBody
                        });
                    }
                }
            }
            for (index, item) in items.iter().enumerate() {
                if menu_item_rect(index, items.len(), self.menu_scroll)
                    .is_some_and(|rect| rect.contains(pos))
                {
                    return Some(UiControl::MenuItem(*item));
                }
            }
            return menu_rect(items.len())
                .contains(pos)
                .then_some(UiControl::PanelBody);
        }
        self.panel
            .as_ref()
            .and_then(|panel| panel_control_at(panel, pos))
    }
}

pub fn panel_control_at(panel: &Panel, pos: (i32, i32)) -> Option<UiControl> {
    let rect = panel_rect(panel);
    if close_button_rect(rect).contains(pos) {
        return Some(UiControl::PanelClose);
    }
    match panel {
        Panel::Calibration(session) => {
            for (control, button_rect) in cal_button_rects(rect) {
                if button_rect.contains(pos) && cal_button_enabled(control, session) {
                    return Some(control);
                }
            }
        }
        Panel::InputMap(_) => {
            for (control, button_rect) in input_map_control_rects(rect) {
                if button_rect.contains(pos) {
                    return Some(control);
                }
            }
        }
        Panel::Debugger(panel) => {
            for (index, tab) in DEBUG_TABS.iter().enumerate() {
                if debug_tab_rect(rect, index).contains(pos) {
                    return Some(UiControl::DebugTab(*tab));
                }
            }
            for (control, button_rect) in debug_button_rects(rect) {
                if button_rect.contains(pos) {
                    return Some(control);
                }
            }
            if panel.tab == DebugTab::Break {
                for (control, button_rect) in break_tab_button_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
            if panel.tab == DebugTab::Copper {
                for (control, button_rect) in copper_tab_button_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
            if panel.tab == DebugTab::Memory {
                for (control, button_rect) in mem_tab_button_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
            if panel.tab == DebugTab::Video {
                for (control, button_rect) in video_tab_toggle_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
            if panel.tab == DebugTab::Audio {
                for (control, button_rect) in audio_tab_button_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
            if panel.tab == DebugTab::Waveform {
                for (control, button_rect) in waveform_tab_button_rects(rect) {
                    if button_rect.contains(pos) {
                        return Some(control);
                    }
                }
            }
        }
        // The console has no controls beyond the shared close button and
        // the click-swallowing body.
        Panel::Console(_) => {}
        Panel::FrameAnalyzer(panel) => {
            for (index, tab) in ANALYZER_TABS.iter().enumerate() {
                if analyzer_tab_rect(rect, index).contains(pos) {
                    return Some(UiControl::AnalyzerTab(*tab));
                }
            }
            // Each tab only offers its own controls: the beam picks and
            // checkboxes are not drawn on the Memory tab, and the map is
            // not drawn on the Beam tab, so neither may be hit there.
            match panel.tab {
                AnalyzerTab::Beam => {
                    if let Some(control) = analyzer_pick_control(rect, pos) {
                        return Some(control);
                    }
                    if analyzer_underlay_rect(rect).contains(pos) {
                        return Some(UiControl::AnalyzerUnderlay);
                    }
                    if analyzer_scrub_rect(rect).contains(pos) {
                        return Some(UiControl::AnalyzerScrub);
                    }
                }
                AnalyzerTab::Memory => {
                    for (control, button_rect) in analyzer_preset_rects(rect, &panel.heat_presets) {
                        if button_rect.contains(pos) {
                            return Some(control);
                        }
                    }
                    if let Some(control) = analyzer_heat_pick_control(rect, pos) {
                        return Some(control);
                    }
                }
            }
            for (control, button_rect) in analyzer_tab_button_rects(rect, panel.tab) {
                if button_rect.contains(pos) {
                    return Some(control);
                }
            }
        }
        Panel::Launcher(state) => {
            if let Some(control) = launcher_control_at(rect, state, pos) {
                return Some(control);
            }
        }
        Panel::DropChooser(state) => {
            for (control, button_rect) in drop_chooser_button_rects(rect, state) {
                if button_rect.contains(pos) {
                    return Some(control);
                }
            }
        }
        Panel::About | Panel::Shortcuts => {}
    }
    rect.contains(pos).then_some(UiControl::PanelBody)
}

/// A clickable UI control, used for hit-testing and hover highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiControl {
    MenuItem(MenuItem),
    PanelClose,
    /// Anywhere on a panel that is not a specific control (swallows the
    /// click so it does not fall through to the display).
    PanelBody,
    CalSkip,
    CalCancel,
    CalSave,
    DebugTab(DebugTab),
    DebugRun,
    DebugStep,
    /// Step over a call: run the callee to completion, stopping at the
    /// instruction after a BSR/JSR/TRAP (a plain single step otherwise).
    DebugStepOver,
    /// Step out: run until the current subroutine returns to its caller.
    DebugStepOut,
    DebugStepFrame,
    DebugRunTo,
    /// Run to the start of the next scanline (end of the current line),
    /// stopping at exact beam granularity via a one-shot beam trap.
    DebugRunLine,
    /// Scroll a too-long menu one item toward the start of the list.
    MenuScrollUp,
    /// Scroll a too-long menu one item toward the end of the list.
    MenuScrollDown,
    /// Input Mapping: show keyboard mapping N (0 = controller 1).
    RemapSet(usize),
    /// Input Mapping: arm control N (an index into `keymap::CONTROLS`) for
    /// key capture.
    RemapBind(usize),
    /// Input Mapping: unbind every key from control N.
    RemapClear(usize),
    /// Input Mapping: restore the built-in bindings.
    RemapDefaults,
    /// Input Mapping: persist the edited map and apply it.
    RemapSave,
    /// Reverse-debug: step one instruction backward (reconstructed from the
    /// snapshot ring).
    DebugReverseStep,
    /// Reverse-debug: step to the previous Agnus frame counter crossing.
    DebugReverseFrame,
    /// Reverse-debug: run backward to the previous breakpoint/watch hit.
    DebugReverseRun,
    DebugMemPrev,
    DebugMemNext,
    DebugEntry,
    /// Poke: on the Memory tab write a word from the entry box's "ADDR VALUE";
    /// on the CPU tab set a register from "REG VALUE".
    DebugPoke,
    /// Break tab: toggle a PC breakpoint at the entry address.
    DebugBreakToggle,
    /// Break tab: toggle a memory word watchpoint at the entry address.
    DebugWatchToggle,
    /// Break tab: toggle a chipset-register write watch at the entry
    /// address (an offset or a full $DFFxxx address).
    DebugRegToggle,
    /// Break tab: toggle a beam trap at the entry's decimal "VPOS [HPOS]"
    /// position (halt when the Agnus beam reaches it).
    DebugBeamToggle,
    /// Break tab: toggle an exception catchpoint from the entry box
    /// ("irq N", "trap N", or "vec N").
    DebugCatchToggle,
    /// Copper tab: toggle a Copper breakpoint at the entry address (halt
    /// when the Copper's PC arrives there).
    DebugCopperBreakToggle,
    /// Copper tab: run until the Copper retires one instruction.
    DebugCopperStep,
    /// Memory tab: find the entry's hex byte pattern, continuing past the
    /// previous hit.
    DebugMemFind,
    /// Memory tab: save the "ADDR LEN" region in the entry box to a file.
    DebugMemSave,
    /// Memory tab: report the last instruction that wrote the entry
    /// address (a reverse-history query; needs the snapshot ring).
    DebugMemWriter,
    /// Memory tab: toggle between the hex dump and the 1-bpp bitplane
    /// view (an entry with a small decimal number sets the row stride).
    DebugMemBits,
    /// Video tab: toggle bitplane `n` (0-7) in the presented picture.
    DebugPlaneToggle(usize),
    /// Video tab: toggle sprite `n` (0-7) in the presented picture.
    DebugSpriteToggle(usize),
    /// Break tab: remove all breakpoints and watchpoints.
    DebugBreaksClear,
    /// Waveform tab: arm a VCD capture from the entry box's order-free
    /// "[PATH] [TRIGGER] [DURATION] [SIGNALS]" spec (empty = defaults).
    DebugWaveArm,
    /// Waveform tab: stop the capture, finishing the file.
    DebugWaveStop,
    /// Audio tab: toggle mute for a channel (0..3 = Paula, 4 = CD audio).
    DebugAudioMute(usize),
    /// Frame analyzer: run/pause the machine while keeping the pane open.
    AnalyzerRun,
    /// Frame analyzer: step/capture one complete Agnus frame.
    AnalyzerFrame,
    /// Frame analyzer: select a slot. Coordinates are normalized to 0..1023
    /// so window.rs can map them through the current trace dimensions.
    AnalyzerPick {
        x: u16,
        y: u16,
        scanline: bool,
    },
    /// Frame analyzer: toggle the rendered-frame picture underlay beneath
    /// the DMA heatmap.
    AnalyzerUnderlay,
    /// Frame analyzer: toggle beam scrubbing (the underlay shows only
    /// what the CRT had drawn up to the selected slot).
    AnalyzerScrub,
    /// Frame analyzer: run until the beam reaches the selected slot
    /// (a one-shot beam trap at the selected vpos/hpos).
    AnalyzerRunTo,
    /// Frame analyzer: switch between the beam and memory views.
    AnalyzerTab(AnalyzerTab),
    /// Memory tab: point the heat map at preset window `n` (an index into
    /// the panel's preset list).
    AnalyzerHeatPreset(u8),
    /// Memory tab: pick a heat map cell, in grid coordinates (0..=255 on
    /// both axes, so the mapping does not depend on the map's pixel size).
    AnalyzerHeatPick {
        x: u8,
        y: u8,
    },
    /// Configuration screen: pick a machine model.
    LauncherModel(MachineModel),
    /// Configuration screen: switch the category tab.
    LauncherTab(LauncherTab),
    /// Configuration screen: step a cycle/stepper field one value.
    LauncherCycle {
        field: LauncherField,
        forward: bool,
    },
    /// Configuration screen: flip a toggle field.
    LauncherToggle(LauncherField),
    /// Configuration screen: open a file dialog for a path field.
    LauncherBrowse(LauncherField),
    /// Configuration screen: clear a path field.
    LauncherClear(LauncherField),
    /// Configuration screen: focus a drive's volume-name field for text entry.
    LauncherDriveNameEdit(LauncherField),
    /// Boot Priority page: focus a drive's boot-priority field for typing.
    LauncherDriveBootpriEdit(LauncherField),
    /// Boot Priority page: toggle a drive's Bootable box.
    LauncherDriveBootToggle(LauncherField),
    /// Configuration screen: add a Zorro metadata board file.
    LauncherZorroAdd,
    /// Configuration screen: remove the Zorro board at this index.
    LauncherZorroRemove(usize),
    /// Plugin config: step an enum/int option of a Zorro board.
    LauncherBoardCycle {
        board: usize,
        opt: usize,
        forward: bool,
    },
    /// Plugin config: flip a bool option of a Zorro board.
    LauncherBoardToggle {
        board: usize,
        opt: usize,
    },
    /// Plugin config: pick a file for a file-typed board option.
    LauncherBoardBrowse {
        board: usize,
        opt: usize,
    },
    /// Plugin config: revert a board option to its manifest default.
    LauncherBoardClear {
        board: usize,
        opt: usize,
    },
    /// Plugin config: focus a string/int board option for text entry.
    LauncherBoardEdit {
        board: usize,
        opt: usize,
    },
    /// Configuration screen: load a .toml configuration.
    LauncherLoad,
    /// Configuration screen: save the configuration to a .toml file.
    LauncherSave,
    /// Configuration screen: reset to the selected profile's defaults.
    LauncherDefaults,
    /// Configuration screen: build and run the configured machine.
    LauncherRun,
    /// Drop chooser: insert the dropped disk(s) into this drive.
    DropDrive(usize),
}

fn panel_dims(panel: &Panel) -> (usize, usize) {
    match panel {
        Panel::About => (560, 380),
        Panel::Shortcuts => (600, shortcuts_panel_height()),
        Panel::Calibration(_) => (620, 372),
        Panel::InputMap(_) => (INPUT_MAP_W, input_map_panel_height()),
        Panel::Debugger(_) => (684, 520),
        Panel::FrameAnalyzer(_) => (700, 526),
        Panel::Console(_) => (700, 460),
        Panel::Launcher(_) => (LAUNCHER_W, LAUNCHER_H),
        Panel::DropChooser(state) => (
            460,
            TITLE_H
                + DROP_HEADER_H
                + state.drives.len() * (DROP_BUTTON_H + DROP_BUTTON_GAP)
                + DROP_FOOTER_H,
        ),
    }
}

fn panel_title(panel: &Panel) -> &'static str {
    match panel {
        Panel::About => "About Copperline",
        Panel::Shortcuts => "Keyboard Shortcuts",
        Panel::Calibration(_) => "Gamepad Calibration",
        Panel::InputMap(_) => "Input Mapping",
        Panel::Debugger(_) => "Debugger",
        Panel::FrameAnalyzer(_) => "Frame Analyzer",
        Panel::Console(_) => "Console",
        Panel::Launcher(_) => "Machine Configuration",
        Panel::DropChooser(_) => "Insert Disk",
    }
}

fn panel_rect(panel: &Panel) -> Rect {
    let (w, h) = panel_dims(panel);
    Rect {
        x: (FB_WIDTH.saturating_sub(w)) / 2,
        y: (present_height().saturating_sub(h)) / 2,
        w,
        h,
    }
}

const TITLE_H: usize = 22;

fn close_button_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + rect.w - TITLE_H,
        y: rect.y,
        w: TITLE_H,
        h: TITLE_H,
    }
}

// Calibration buttons along the panel's bottom edge.
const CAL_BUTTON_W: usize = 96;
const CAL_BUTTON_H: usize = 22;

fn cal_button_rects(rect: Rect) -> [(UiControl, Rect); 3] {
    let y = rect.y + rect.h - CAL_BUTTON_H - 8;
    let button = |i: usize| Rect {
        x: rect.x + rect.w - (3 - i) * (CAL_BUTTON_W + 8),
        y,
        w: CAL_BUTTON_W,
        h: CAL_BUTTON_H,
    };
    [
        (UiControl::CalSkip, button(0)),
        (UiControl::CalCancel, button(1)),
        (UiControl::CalSave, button(2)),
    ]
}

fn cal_button_enabled(control: UiControl, session: &crate::gamepad::CalibrationSession) -> bool {
    match control {
        UiControl::CalSkip => session.can_skip(),
        UiControl::CalSave => session.done(),
        _ => true,
    }
}

// Drop chooser: a header naming the dropped disk, then one large target
// button per connected drive, and a key-hint footer.
const DROP_BUTTON_H: usize = 30;
const DROP_BUTTON_GAP: usize = 8;
const DROP_HEADER_H: usize = 46;
const DROP_FOOTER_H: usize = 24;

fn drop_chooser_button_rects(rect: Rect, state: &DropChooserState) -> Vec<(UiControl, Rect)> {
    state
        .drives
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            (
                UiControl::DropDrive(entry.drive),
                Rect {
                    x: rect.x + 16,
                    y: rect.y + TITLE_H + DROP_HEADER_H + index * (DROP_BUTTON_H + DROP_BUTTON_GAP),
                    w: rect.w - 32,
                    h: DROP_BUTTON_H,
                },
            )
        })
        .collect()
}

// Debugger chrome: a tab row under the title and a control row at the
// bottom with the transport buttons and the shared hex-entry box.
// 9 tabs at 70+4 px fit the 684 px panel; the longest label (Chipset,
// 7 glyphs at 8 px) still leaves 7 px of padding a side.
const DEBUG_TAB_W: usize = 70;
const DEBUG_TAB_H: usize = 18;
const DEBUG_BUTTON_H: usize = 20;

fn debug_tab_rect(rect: Rect, index: usize) -> Rect {
    Rect {
        x: rect.x + 8 + index * (DEBUG_TAB_W + 4),
        y: rect.y + TITLE_H + 4,
        w: DEBUG_TAB_W,
        h: DEBUG_TAB_H,
    }
}

fn debug_button_rects(rect: Rect) -> [(UiControl, Rect); 14] {
    let y = rect.y + rect.h - DEBUG_BUTTON_H - 6;
    // Step Over / Step Out share a second transport row just above the main
    // one; the main row is already full edge to edge.
    let y2 = rect.y + rect.h - 2 * DEBUG_BUTTON_H - 10;
    let button = |x: usize, w: usize| Rect {
        x: rect.x + x,
        y,
        w,
        h: DEBUG_BUTTON_H,
    };
    let button2 = |x: usize, w: usize| Rect {
        x: rect.x + x,
        y: y2,
        w,
        h: DEBUG_BUTTON_H,
    };
    [
        (UiControl::DebugRun, button(8, 64)),
        (UiControl::DebugStep, button(76, 56)),
        (UiControl::DebugStepFrame, button(136, 64)),
        (UiControl::DebugRunTo, button(204, 76)),
        (UiControl::DebugEntry, button(284, 110)),
        (UiControl::DebugMemPrev, button(398, 28)),
        (UiControl::DebugMemNext, button(430, 28)),
        // Reverse-debug transport, in the free space at the row's right end.
        (UiControl::DebugReverseFrame, button(466, 76)),
        (UiControl::DebugReverseStep, button(546, 66)),
        (UiControl::DebugReverseRun, button(616, 60)),
        // Forward step-over / step-out on the second row.
        (UiControl::DebugStepOver, button2(8, 90)),
        (UiControl::DebugStepOut, button2(102, 84)),
        // Poke (Memory tab) / Set Reg (CPU tab), on the second row.
        (UiControl::DebugPoke, button2(200, 90)),
        // Run to the end of the current scanline, on the second row.
        (UiControl::DebugRunLine, button2(294, 56)),
    ]
}

/// Top of a debugger tab's content area (under the tab row).
fn debug_content_top(rect: Rect) -> usize {
    rect.y + TITLE_H + 4 + DEBUG_TAB_H + 6
}

/// Content lines the Break tab's view must leave blank so the toggle
/// buttons drawn at the top of the content area do not overlap text.
pub const BREAK_TAB_HEADER_LINES: usize = 3;

/// The Break tab's toggle buttons, drawn at the top of the content area.
fn break_tab_button_rects(rect: Rect) -> [(UiControl, Rect); 6] {
    let y = debug_content_top(rect);
    let button = |i: usize| Rect {
        x: rect.x + 10 + i * 98,
        y,
        w: 90,
        h: DEBUG_BUTTON_H,
    };
    [
        (UiControl::DebugBreakToggle, button(0)),
        (UiControl::DebugWatchToggle, button(1)),
        (UiControl::DebugRegToggle, button(2)),
        (UiControl::DebugBeamToggle, button(3)),
        (UiControl::DebugCatchToggle, button(4)),
        (UiControl::DebugBreaksClear, button(5)),
    ]
}

/// Content lines the Waveform tab's view must leave blank so the Arm and
/// Stop buttons drawn at the top of the content area do not overlap text.
pub const WAVEFORM_TAB_HEADER_LINES: usize = 3;

/// The Waveform tab's buttons, drawn at the top of the content area.
fn waveform_tab_button_rects(rect: Rect) -> [(UiControl, Rect); 2] {
    let y = debug_content_top(rect);
    let button = |i: usize| Rect {
        x: rect.x + 10 + i * 98,
        y,
        w: 90,
        h: DEBUG_BUTTON_H,
    };
    [
        (UiControl::DebugWaveArm, button(0)),
        (UiControl::DebugWaveStop, button(1)),
    ]
}

/// Parse the Break tab's entry as an exception catchpoint: "irq N"
/// (interrupt level 1-7), "trap N" (TRAP #0-15), or "vec N" (a raw
/// decimal exception vector number).
pub fn parse_catch_spec(entry: &str) -> Option<u16> {
    let mut tokens = entry.split_whitespace();
    let kind = tokens.next()?;
    let n = tokens.next()?.parse::<u16>().ok()?;
    if tokens.next().is_some() {
        return None;
    }
    if kind.eq_ignore_ascii_case("irq") {
        (1..=7).contains(&n).then_some(24 + n)
    } else if kind.eq_ignore_ascii_case("trap") {
        (n <= 15).then_some(32 + n)
    } else if kind.eq_ignore_ascii_case("vec") {
        (2..=255).contains(&n).then_some(n)
    } else {
        None
    }
}

/// Content lines the Copper tab's view must leave blank so the buttons
/// drawn at the top of the content area do not overlap text.
pub const COPPER_TAB_HEADER_LINES: usize = 3;

/// Content lines the Memory tab's view must leave blank so the buttons
/// drawn at the top of the content area do not overlap text.
pub const MEM_TAB_HEADER_LINES: usize = 3;

// Video tab layout: a header line, the plane/sprite layer-toggle rows,
// eight sprite rows (decode text plus a thumbnail), and the palette grid.
const VIDEO_TOGGLE_W: usize = 34;
const VIDEO_TOGGLE_H: usize = 16;
const VIDEO_TOGGLE_X: usize = 86;
const VIDEO_SPRITE_ROW_H: usize = 26;
/// Sprite thumbnails sample the sprite's captured DMA lines down to this
/// many rows.
pub const VIDEO_THUMB_MAX_ROWS: usize = 24;
const VIDEO_THUMB_X: usize = 560;
const VIDEO_PALETTE_CELL_W: usize = 20;
const VIDEO_PALETTE_CELL_H: usize = 8;

fn video_toggle_row_y(rect: Rect, row: usize) -> usize {
    debug_content_top(rect) + 14 + row * (VIDEO_TOGGLE_H + 4)
}

fn video_sprites_top(rect: Rect) -> usize {
    video_toggle_row_y(rect, 2) + 6
}

fn video_palette_top(rect: Rect) -> usize {
    video_sprites_top(rect) + 8 * VIDEO_SPRITE_ROW_H + 12
}

/// The Video tab's 16 layer-isolation toggles: bitplanes 1-8 then
/// sprites 0-7.
fn video_tab_toggle_rects(rect: Rect) -> [(UiControl, Rect); 16] {
    let button = |row: usize, i: usize| Rect {
        x: rect.x + VIDEO_TOGGLE_X + i * (VIDEO_TOGGLE_W + 4),
        y: video_toggle_row_y(rect, row),
        w: VIDEO_TOGGLE_W,
        h: VIDEO_TOGGLE_H,
    };
    std::array::from_fn(|k| {
        if k < 8 {
            (UiControl::DebugPlaneToggle(k), button(0, k))
        } else {
            (UiControl::DebugSpriteToggle(k - 8), button(1, k - 8))
        }
    })
}

/// The Memory tab's buttons, drawn at the top of the content area.
fn mem_tab_button_rects(rect: Rect) -> [(UiControl, Rect); 4] {
    let y = debug_content_top(rect);
    let button = |i: usize| Rect {
        x: rect.x + 10 + i * 98,
        y,
        w: 90,
        h: DEBUG_BUTTON_H,
    };
    [
        (UiControl::DebugMemFind, button(0)),
        (UiControl::DebugMemSave, button(1)),
        (UiControl::DebugMemWriter, button(2)),
        (UiControl::DebugMemBits, button(3)),
    ]
}

/// The Copper tab's buttons, drawn at the top of the content area.
fn copper_tab_button_rects(rect: Rect) -> [(UiControl, Rect); 2] {
    let y = debug_content_top(rect);
    let button = |i: usize| Rect {
        x: rect.x + 10 + i * 98,
        y,
        w: 90,
        h: DEBUG_BUTTON_H,
    };
    [
        (UiControl::DebugCopperBreakToggle, button(0)),
        (UiControl::DebugCopperStep, button(1)),
    ]
}

// Audio tab layout: a header line, four Paula channel blocks, then a CD row.
// Each block has a mute button on the left, text detail in the middle, and an
// oscilloscope box on the right.
const AUDIO_HEADER_H: usize = 16;
const AUDIO_ROW_H: usize = 46;
const AUDIO_CD_ROW_H: usize = 30;
const AUDIO_MUTE_W: usize = 54;
const AUDIO_TEXT_X: usize = 70;
const AUDIO_SCOPE_X: usize = 470;

/// Geometry of one Audio-tab row: (mute button rect, scope box rect). `idx`
/// 0..3 are the Paula channels, 4 is the CD-DA row.
fn audio_row_geom(rect: Rect, idx: usize) -> (Rect, Rect) {
    let top = debug_content_top(rect) + AUDIO_HEADER_H + idx.min(4) * AUDIO_ROW_H;
    let row_h = if idx >= 4 {
        AUDIO_CD_ROW_H
    } else {
        AUDIO_ROW_H
    };
    let mute = Rect {
        x: rect.x + 8,
        y: top,
        w: AUDIO_MUTE_W,
        h: row_h.saturating_sub(8),
    };
    let scope = Rect {
        x: rect.x + AUDIO_SCOPE_X,
        y: top,
        w: rect.w.saturating_sub(AUDIO_SCOPE_X + 10),
        h: row_h.saturating_sub(8),
    };
    (mute, scope)
}

/// The five Audio-tab mute buttons (four Paula channels then CD).
fn audio_tab_button_rects(rect: Rect) -> [(UiControl, Rect); 5] {
    std::array::from_fn(|i| (UiControl::DebugAudioMute(i), audio_row_geom(rect, i).0))
}

/// A Frame Analyzer tab button, sized and placed like the debugger's tab
/// row so the two tool windows read as the same chrome.
fn analyzer_tab_rect(rect: Rect, index: usize) -> Rect {
    Rect {
        x: rect.x + 8 + index * (DEBUG_TAB_W + 4),
        y: rect.y + TITLE_H + 4,
        w: DEBUG_TAB_W,
        h: DEBUG_TAB_H,
    }
}

/// Top of a Frame Analyzer tab's content area (under the tab row). Both
/// tabs start their header line here; the beam tab's older layout is this
/// row and everything below it, shifted down by the tab row.
fn analyzer_content_top(rect: Rect) -> usize {
    rect.y + TITLE_H + 4 + DEBUG_TAB_H + 8
}

fn analyzer_raster_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 10,
        y: analyzer_content_top(rect) + 34,
        w: 448,
        h: 246,
    }
}

fn analyzer_scanline_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 10,
        y: analyzer_content_top(rect) + 326,
        w: 512,
        h: 34,
    }
}

/// Height of one Memory-tab preset button.
const ANALYZER_PRESET_H: usize = 16;

/// The Memory tab's preset buttons, left to right under the hint line.
/// Each is sized to its label; a preset that would run past the panel's
/// right margin is dropped rather than clipped, and because the draw and
/// the hit test share this list, a dropped one is neither drawn nor
/// clickable.
fn analyzer_preset_rects(rect: Rect, presets: &[HeatPreset]) -> Vec<(UiControl, Rect)> {
    let limit = rect.x + rect.w.saturating_sub(10);
    let mut x = rect.x + 10;
    let mut out = Vec::with_capacity(presets.len());
    for (index, preset) in presets.iter().enumerate().take(u8::MAX as usize + 1) {
        let w = preset.label.chars().count() * font::GLYPH_W + 16;
        if x + w > limit {
            break;
        }
        out.push((
            UiControl::AnalyzerHeatPreset(index as u8),
            Rect {
                x,
                y: analyzer_content_top(rect) + 28,
                w,
                h: ANALYZER_PRESET_H,
            },
        ));
        x += w + 6;
    }
    out
}

/// The Memory tab's map: one square pixel block per grid cell, 368 px on
/// a side so the 256-cell grid samples up cleanly inside the panel.
fn analyzer_heat_map_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 10,
        y: analyzer_content_top(rect) + 50,
        w: 368,
        h: 368,
    }
}

/// Left edge of the census/legend column, right of the map.
fn analyzer_heat_census_x(rect: Rect) -> usize {
    let map = analyzer_heat_map_rect(rect);
    map.x + map.w + 16
}

/// Which grid cell `pos` lands on, proportionally like
/// [`analyzer_pick_control`] but resolved all the way to grid
/// coordinates: the grid is a fixed 256x256 whatever the map's pixel
/// size, so nothing downstream has to re-scale.
fn analyzer_heat_pick_control(rect: Rect, pos: (i32, i32)) -> Option<UiControl> {
    let map = analyzer_heat_map_rect(rect);
    if !map.contains(pos) {
        return None;
    }
    let last = heatmap::GRID - 1;
    let x = (pos.0 - map.x as i32).max(0) as usize;
    let y = (pos.1 - map.y as i32).max(0) as usize;
    Some(UiControl::AnalyzerHeatPick {
        x: ((x * heatmap::GRID) / map.w.max(1)).min(last) as u8,
        y: ((y * heatmap::GRID) / map.h.max(1)).min(last) as u8,
    })
}

/// The transport buttons for `tab`. The Memory tab has no selected beam
/// slot, so the To slot button (like the underlay and scrub checkboxes)
/// is beam-only.
fn analyzer_tab_button_rects(rect: Rect, tab: AnalyzerTab) -> Vec<(UiControl, Rect)> {
    let all = analyzer_button_rects(rect);
    match tab {
        AnalyzerTab::Beam => all.to_vec(),
        AnalyzerTab::Memory => all[..2].to_vec(),
    }
}

fn analyzer_button_rects(rect: Rect) -> [(UiControl, Rect); 3] {
    let y = rect.y + rect.h - DEBUG_BUTTON_H - 6;
    [
        (
            UiControl::AnalyzerRun,
            Rect {
                x: rect.x + 8,
                y,
                w: 70,
                h: DEBUG_BUTTON_H,
            },
        ),
        (
            UiControl::AnalyzerFrame,
            Rect {
                x: rect.x + 84,
                y,
                w: 76,
                h: DEBUG_BUTTON_H,
            },
        ),
        (
            UiControl::AnalyzerRunTo,
            Rect {
                x: rect.x + 166,
                y,
                w: 76,
                h: DEBUG_BUTTON_H,
            },
        ),
    ]
}

/// Label of the picture-underlay checkbox on the analyzer's button row.
const ANALYZER_UNDERLAY_LABEL: &str = "Picture underlay";
/// Label of the beam-scrub checkbox next to it.
const ANALYZER_SCRUB_LABEL: &str = "Beam scrub";

/// Hit/draw rect of the picture-underlay checkbox: a 12x12 tick box plus
/// its label, sitting on the button row right of the To slot button.
fn analyzer_underlay_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + 258,
        y: rect.y + rect.h - DEBUG_BUTTON_H - 6,
        w: 12 + 6 + ANALYZER_UNDERLAY_LABEL.len() * font::GLYPH_W,
        h: DEBUG_BUTTON_H,
    }
}

/// Hit/draw rect of the beam-scrub checkbox, right of the underlay one.
fn analyzer_scrub_rect(rect: Rect) -> Rect {
    let underlay = analyzer_underlay_rect(rect);
    Rect {
        x: underlay.x + underlay.w + 16,
        y: underlay.y,
        w: 12 + 6 + ANALYZER_SCRUB_LABEL.len() * font::GLYPH_W,
        h: DEBUG_BUTTON_H,
    }
}

fn analyzer_pick_control(rect: Rect, pos: (i32, i32)) -> Option<UiControl> {
    for (pick_rect, scanline) in [
        (analyzer_raster_rect(rect), false),
        (analyzer_scanline_rect(rect), true),
    ] {
        if !pick_rect.contains(pos) {
            continue;
        }
        let x = (pos.0 - pick_rect.x as i32).max(0) as usize;
        let y = (pos.1 - pick_rect.y as i32).max(0) as usize;
        let nx = ((x * 1023) / pick_rect.w.max(1)).min(1023) as u16;
        let ny = ((y * 1023) / pick_rect.h.max(1)).min(1023) as u16;
        return Some(UiControl::AnalyzerPick {
            x: nx,
            y: ny,
            scanline,
        });
    }
    None
}

/// Bytes shown per Memory-tab page (16 rows of 16).
pub const MEM_PAGE_BYTES: u32 = 256;

// ---------------------------------------------------------------------------
// View data built by window.rs each redraw
// ---------------------------------------------------------------------------

pub struct AboutView {
    /// Emulated-machine summary lines (built once at startup).
    pub machine_lines: Vec<String>,
}

pub struct CalRow {
    pub label: &'static str,
    pub binding: String,
    pub current: bool,
}

pub struct CalibrationView {
    pub pad_line: String,
    pub rows: Vec<CalRow>,
    pub status: String,
}

#[derive(Clone)]
pub struct DbgLine {
    pub text: String,
    pub highlight: bool,
}

impl DbgLine {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            highlight: false,
        }
    }

    pub fn hilit(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            highlight: true,
        }
    }
}

/// The Memory tab's 1-bpp bitplane view: `stride` bytes per row of plane
/// data starting at `base`, drawn as pixels (set bit = light) so bitmap
/// graphics in RAM can be eyeballed directly.
pub struct MemBitmapView {
    pub base: u32,
    pub stride: usize,
    pub rows: usize,
    /// Row-major plane data, `stride` bytes per row, `rows` rows.
    pub data: Vec<u8>,
}

/// Rows of plane data the Memory tab's bitmap view shows (its fixed
/// pixel budget inside the panel at 2x2 pixels per bit). The debugger
/// panel is fixed-size (see `panel_dims`), so this is a constant fit.
pub fn mem_bitmap_rows() -> usize {
    let panel_h = 520;
    let top = TITLE_H + 4 + DEBUG_TAB_H + 6 + MEM_TAB_HEADER_LINES * 10 + 14;
    let bottom = panel_h - 2 * DEBUG_BUTTON_H - 16;
    bottom.saturating_sub(top) / 2
}

/// One sprite row of the Video tab: a decoded state line plus a
/// thumbnail rendered from the frame's captured sprite DMA lines.
pub struct SpriteRowView {
    pub text: String,
    /// Thumbnail pixels, 16 wide by `thumb_rows`, already in framebuffer
    /// RGBA; 0 marks a transparent sprite pixel.
    pub thumb: Vec<u32>,
    pub thumb_rows: usize,
}

/// The Video tab: bitplane/sprite layer isolation and visual chip state.
pub struct VideoView {
    /// BPLCON0/DMACON decode line.
    pub header: String,
    /// Bit n set = bitplane n drawn (the debug isolation mask).
    pub plane_mask: u8,
    /// Planes active in BPLCON0, to grey out toggles beyond the mode.
    pub nplanes: usize,
    /// Bit n set = sprite n drawn.
    pub sprite_mask: u8,
    pub sprites: Vec<SpriteRowView>,
    /// Palette swatches in framebuffer RGBA: 32 entries (OCS/ECS) or the
    /// full 256 (AGA).
    pub palette: Vec<u32>,
}

pub struct DebuggerView {
    /// False while the machine is paused (the debugger's usual state).
    pub running: bool,
    /// Whether reverse debugging is armed (snapshot ring present), gating the
    /// reverse transport buttons.
    pub reverse_available: bool,
    /// Status summary drawn in the title bar (frame count, emulated time).
    pub status: String,
    /// Pre-formatted content lines of the active tab.
    pub lines: Vec<DbgLine>,
    /// The Memory tab's bitplane view, when its Bits mode is active.
    pub bitmap: Option<MemBitmapView>,
    /// The Video tab's layer/palette view. Some only when it is active.
    pub video: Option<VideoView>,
    /// Structured data for the Audio tab's per-channel mute buttons and
    /// oscilloscopes. Some only when the Audio tab is active; the plain text
    /// is also mirrored into `lines` for headless/text use.
    pub audio: Option<AudioScopeView>,
}

/// Per-channel and CD audio state for the debugger Audio tab.
pub struct AudioScopeView {
    /// Header line (DMACON / AUDEN / ADKCON summary).
    pub header: String,
    /// The four Paula channels, in order.
    pub channels: Vec<AudioRowView>,
    /// The CD-DA row.
    pub cd: AudioRowView,
}

/// One row of the Audio tab: text detail, mute state, and a scope trace.
pub struct AudioRowView {
    /// Formatted detail lines for this channel/row.
    pub text: Vec<DbgLine>,
    /// Whether this channel/stream is developer-muted.
    pub muted: bool,
    /// Oscilloscope samples (oldest..newest, output level -128..127).
    pub scope: Vec<i8>,
}

pub struct AnalyzerMarker {
    pub vpos: u16,
    pub hpos: u16,
    /// Custom-register word offset into $DFF000 of the write.
    pub offset: u16,
    pub value: u16,
    /// Writer: "cpu", "irq" (CPU inside the Copper-triggered interrupt
    /// window), or "copper".
    pub source: &'static str,
}

impl AnalyzerMarker {
    fn label(&self) -> String {
        format!(
            "{} {}=${:04X} v{} h{}",
            self.source,
            crate::debugger::custom_reg_name(self.offset & 0x01FE),
            self.value,
            self.vpos,
            self.hpos,
        )
    }

    /// Whether this marker sits close enough to beam slot
    /// (`vpos`, `hpos`) to be reported for it: within a line vertically
    /// and two colour clocks horizontally, roughly one heatmap pixel.
    fn near(&self, vpos: usize, hpos: usize) -> bool {
        (i64::from(self.vpos) - vpos as i64).abs() <= 1
            && (i64::from(self.hpos) - hpos as i64).abs() <= 2
    }
}

pub struct AnalyzerTraceView {
    pub frame: u64,
    pub seconds: f64,
    pub rows: usize,
    pub cols: usize,
    pub line_cck: u32,
    pub visible_start_vpos: u32,
    pub visible_lines: usize,
    pub display_hpos_start: u32,
    pub display_hpos_end: u32,
    pub owner_cck: [u64; 9],
    pub blitter_busy_cck: u64,
    pub blitter_starve_cck: [u64; 9],
    pub partial: bool,
    pub selected_vpos: usize,
    pub selected_hpos: usize,
    pub selected_owner: &'static str,
    pub selected_owner_code: u8,
    pub owners: Vec<u8>,
    pub markers: Vec<AnalyzerMarker>,
    /// "in blit #N ..." when the selected slot lies inside a recorded
    /// blit's beam span.
    pub selected_blit: Option<String>,
    /// Frame-start display window: (v_start, v_stop) beam lines (stop
    /// already unwrapped past 255 where applicable) and (h_start, h_stop)
    /// in colour clocks. None when DIW is unprogrammed.
    pub diw_v: Option<(u16, u16)>,
    pub diw_h_cck: Option<(u16, u16)>,
    /// Frame-start bitplane fetch bounds (DDFSTRT, DDFSTOP) in colour
    /// clocks.
    pub ddf_cck: Option<(u16, u16)>,
}

impl AnalyzerTraceView {
    fn owner_code_at(&self, vpos: usize, hpos: usize) -> u8 {
        if vpos >= self.rows || hpos >= self.cols {
            return b'.';
        }
        self.owners[vpos * self.cols + hpos]
    }

    fn owner_row(&self, vpos: usize) -> Option<&[u8]> {
        if vpos >= self.rows || self.cols == 0 {
            return None;
        }
        let start = vpos * self.cols;
        Some(&self.owners[start..start + self.cols])
    }
}

/// Beam-space render of the traced frame for the analyzer's picture
/// underlay. Row 0 is beam line `visible_start_vpos`; each colour clock
/// spans four hi-res pixels from `display_hpos_start` (the same footprint
/// as the heatmap's white display box), so no presentation recentring may
/// be applied to this buffer.
pub struct AnalyzerUnderlayView {
    pub fb: std::rc::Rc<Vec<u32>>,
    pub rows: usize,
    /// Pixels per row: FB_WIDTH classically, twice that for a 35 ns
    /// super-hi-res canvas.
    pub width: usize,
}

/// One line of the Memory tab's census column: how much of the window a
/// single toucher currently holds. Every toucher gets a row, including
/// the ones with nothing, so the column doubles as the legend and does
/// not jump about as activity comes and goes.
pub struct AnalyzerHeatCensusRow {
    pub name: &'static str,
    /// The toucher's colour as [`crate::heatmap::Toucher::colour`] gives
    /// it (0xAARRGGBB), not in the presentation texture's byte order.
    pub colour: u32,
    pub cells: usize,
    /// Bytes those cells cover (`cells * bytes_per_cell`).
    pub bytes: u64,
}

/// The pinned cell's record, read out of the live map by window.rs.
/// Only the pinned cell can carry one: the hovered cell is known to the
/// drawing code alone, which can name its addresses but has no way to
/// ask the map what touched it.
pub struct AnalyzerHeatCell {
    /// Index into the 256x256 grid.
    pub cell: usize,
    /// What last touched it, or None for a cell nothing has touched.
    pub toucher: Option<&'static str>,
    /// Its toucher's colour (0xAARRGGBB, as the heat map paints it).
    pub colour: u32,
    /// Frames since that touch; None when there is no touch to age.
    pub age_frames: Option<u32>,
}

/// The Memory tab's view of the address space.
pub struct AnalyzerHeatView {
    /// [`crate::heatmap::CELLS`] pixels straight from
    /// `HeatMap::render`: 0xAARRGGBB, already faded by age.
    pub image: Vec<u32>,
    /// First address the grid covers, and the span it maps.
    pub base: u32,
    pub span: u32,
    pub bytes_per_cell: u32,
    /// Frame the image was rendered for.
    pub frame: u64,
    /// One row per toucher, in Toucher code order, zero rows included.
    pub census: Vec<AnalyzerHeatCensusRow>,
    /// The pinned cell's record, when a cell is pinned and the map has
    /// something recorded for it.
    pub selected: Option<AnalyzerHeatCell>,
}

pub struct FrameAnalyzerView {
    pub running: bool,
    pub status: String,
    pub trace: Option<AnalyzerTraceView>,
    pub underlay: Option<AnalyzerUnderlayView>,
    /// Beam scrubbing: the underlay shows only what the CRT had drawn up
    /// to the selected slot; the rest ghosts at low brightness.
    pub scrub: bool,
    /// The Memory tab's data; None while the heat map is not armed.
    pub heat: Option<AnalyzerHeatView>,
}

pub enum PanelViewData {
    About(AboutView),
    Shortcuts,
    Calibration(CalibrationView),
    Debugger(Box<DebuggerView>),
    FrameAnalyzer(Box<FrameAnalyzerView>),
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw_panel_text(
    frame: &mut [u8],
    x: usize,
    y: usize,
    text: &str,
    color: u32,
    px: usize,
    texture_scale: usize,
) {
    font::draw_text(
        frame,
        super::window::texture_width(texture_scale),
        super::window::texture_height(texture_scale),
        x * texture_scale,
        y * texture_scale,
        text,
        color,
        px * texture_scale,
    );
}

fn draw_text_button(
    frame: &mut [u8],
    rect: Rect,
    label: &str,
    enabled: bool,
    hover: bool,
    texture_scale: usize,
) {
    let face = if hover && enabled {
        BUTTON_FACE_HOVER
    } else {
        BUTTON_FACE
    };
    let scaled = scale_rect(rect, texture_scale);
    fill_rect(frame, scaled, face, texture_scale);
    draw_rect_bevel(
        frame,
        scaled,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        texture_scale,
    );
    let color = if enabled {
        BUTTON_TEXT
    } else {
        BUTTON_TEXT_DISABLED
    };
    let text_w = label.chars().count() * font::GLYPH_W;
    let x = rect.x + rect.w.saturating_sub(text_w) / 2;
    let y = rect.y + rect.h.saturating_sub(font::GLYPH_H) / 2;
    draw_panel_text(frame, x, y, label, color, 1, texture_scale);
}

fn draw_panel_chrome(frame: &mut [u8], panel: &Panel, hover: Option<UiControl>, scale: usize) {
    let rect = panel_rect(panel);
    // Dim the display behind the window so the panel reads as modal.
    fill_rect_blend(
        frame,
        scale_rect(
            Rect {
                x: 0,
                y: 0,
                w: FB_WIDTH,
                h: present_height(),
            },
            scale,
        ),
        SCRIM,
        SCRIM_ALPHA,
        scale,
    );
    let scaled = scale_rect(rect, scale);
    fill_rect(frame, scaled, PANEL_BG, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
    // Title bar.
    let title = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        w: rect.w - 2,
        h: TITLE_H - 1,
    };
    fill_rect(frame, scale_rect(title, scale), PANEL_TITLE_BG, scale);
    draw_panel_text(
        frame,
        rect.x + 10,
        rect.y + (TITLE_H - 16) / 2,
        panel_title(panel),
        PANEL_TITLE_TEXT,
        2,
        scale,
    );
    // Close gadget: classic square with an inner square.
    let close = close_button_rect(rect);
    let close_hover = hover == Some(UiControl::PanelClose);
    let face = if close_hover {
        BUTTON_FACE_HOVER
    } else {
        PANEL_TITLE_BG
    };
    let close_scaled = scale_rect(
        Rect {
            x: close.x + 1,
            y: close.y + 1,
            w: close.w - 2,
            h: close.h - 1,
        },
        scale,
    );
    fill_rect(frame, close_scaled, face, scale);
    draw_rect_bevel(
        frame,
        close_scaled,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    let inner = Rect {
        x: close.x + close.w / 2 - 4,
        y: close.y + close.h / 2 - 4,
        w: 8,
        h: 8,
    };
    fill_rect(frame, scale_rect(inner, scale), PANEL_TITLE_TEXT, scale);
    let hole = Rect {
        x: inner.x + 2,
        y: inner.y + 2,
        w: 4,
        h: 4,
    };
    fill_rect(frame, scale_rect(hole, scale), face, scale);
}

fn draw_menu(
    frame: &mut [u8],
    hover: Option<UiControl>,
    midi_active: bool,
    sampler_active: bool,
    scroll: usize,
    labels: MenuLabels,
    scale: usize,
) {
    let items = menu_items(midi_active, sampler_active);
    let rect = menu_rect(items.len());
    let scaled = scale_rect(rect, scale);
    fill_rect(frame, scaled, MENU_BG, scale);
    draw_rect_bevel(frame, scaled, MENU_EDGE, MENU_EDGE, scale);
    let text_y = |row: Rect| row.y + row.h.saturating_sub(MENU_TEXT_PX * font::GLYPH_H) / 2;
    for (index, item) in items.iter().enumerate() {
        let Some(item_rect) = menu_item_rect(index, items.len(), scroll) else {
            continue;
        };
        let hovered = hover == Some(UiControl::MenuItem(*item));
        let (bg, fg) = if hovered {
            (MENU_HILIGHT_BG, MENU_HILIGHT_TEXT)
        } else {
            (MENU_BG, MENU_TEXT)
        };
        if hovered {
            fill_rect(frame, scale_rect(item_rect, scale), bg, scale);
        }
        draw_panel_text(
            frame,
            item_rect.x + MENU_TEXT_INSET,
            text_y(item_rect),
            &menu_item_label(*item, labels),
            fg,
            MENU_TEXT_PX,
            scale,
        );
    }
    let Some(rows) = menu_scroll_rows(items.len(), scroll) else {
        return;
    };
    for (control, row, enabled) in rows {
        let label = if control == UiControl::MenuScrollUp {
            MENU_SCROLL_UP_LABEL
        } else {
            MENU_SCROLL_DOWN_LABEL
        };
        let hovered = enabled && hover == Some(control);
        if hovered {
            fill_rect(frame, scale_rect(row, scale), MENU_HILIGHT_BG, scale);
        }
        let fg = match (enabled, hovered) {
            // A dimmed end says "this is as far as the list goes" rather
            // than leaving a dead-looking gap.
            (false, _) => MENU_TEXT_DISABLED,
            (true, true) => MENU_HILIGHT_TEXT,
            (true, false) => MENU_TEXT,
        };
        let width = font::text_width(label, MENU_TEXT_PX);
        draw_panel_text(
            frame,
            row.x + row.w.saturating_sub(width) / 2,
            text_y(row),
            label,
            fg,
            MENU_TEXT_PX,
            scale,
        );
    }
}

/// Word-wrap `text` so no panel line is cropped: the first line holds up to
/// `first_width` characters, continuations up to `rest_width` (they are drawn
/// indented). Words longer than a whole line are hard-split.
fn wrap_text(text: &str, first_width: usize, rest_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while !word.is_empty() {
            let width = if lines.is_empty() {
                first_width
            } else {
                rest_width
            }
            .max(1);
            let cur_len = cur.chars().count();
            let sep = usize::from(!cur.is_empty());
            if cur_len + sep + word.len() <= width {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.extend(word.drain(..));
            } else if cur.is_empty() {
                let take = width.min(word.len());
                cur.extend(word.drain(..take));
                lines.push(std::mem::take(&mut cur));
            } else {
                lines.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Contributors and Patreon sponsors credited in the About panel. Keep
/// both in step with CREDITS.md and the website's Community section.
const CONTRIBUTORS: &[&str] = &["Bernie Innocenti", "Lee Hobson", "jbl007"];
const PATREON_SPONSORS: &[&str] = &["Lee Hobson"];

fn draw_about(frame: &mut [u8], rect: Rect, view: &AboutView, scale: usize) {
    let cx = |text: &str, px: usize| rect.x + rect.w.saturating_sub(text.len() * 8 * px) / 2;
    let title = "Copperline";
    let mut y = rect.y + TITLE_H + 14;
    draw_panel_text(frame, cx(title, 3), y, title, PANEL_TEXT_HILIGHT, 3, scale);
    y += 30;
    let version = concat!("version ", env!("COPPERLINE_DISPLAY_VERSION"));
    draw_panel_text(frame, cx(version, 1), y, version, PANEL_TEXT_DIM, 1, scale);
    y += 14;
    let tagline = "A cycle-stepped Amiga emulator";
    draw_panel_text(frame, cx(tagline, 2), y, tagline, PANEL_TEXT, 2, scale);
    y += 22;
    let author = "by Andrew \"LinuxJedi\" Hutchings";
    draw_panel_text(frame, cx(author, 1), y, author, PANEL_TEXT_DIM, 1, scale);
    y += 24;
    let max_chars = (rect.w - 48) / 16;
    for line in &view.machine_lines {
        for (i, part) in wrap_text(line, max_chars, max_chars.saturating_sub(1))
            .iter()
            .enumerate()
        {
            // Continuation lines are indented by one glyph cell.
            let x = rect.x + 24 + if i == 0 { 0 } else { 16 };
            draw_panel_text(frame, x, y, part, PANEL_TEXT, 2, scale);
            y += 18;
        }
    }
    y += 10;
    for line in [
        "m68k CPU core (MIT)",
        "font8x8 by Daniel Hepper / Marcel Sondaar",
        "winit + pixels + cpal + gilrs",
    ] {
        draw_panel_text(frame, rect.x + 24, y, line, PANEL_TEXT_DIM, 1, scale);
        y += 12;
    }
    y += 10;
    let contributors = format!("Contributors: {}", CONTRIBUTORS.join(", "));
    draw_panel_text(frame, rect.x + 24, y, &contributors, PANEL_TEXT, 1, scale);
    y += 12;
    let sponsors = format!("Patreon sponsors: {}", PATREON_SPONSORS.join(", "));
    draw_panel_text(frame, rect.x + 24, y, &sponsors, PANEL_TEXT, 1, scale);
}

fn draw_drop_chooser(
    frame: &mut [u8],
    rect: Rect,
    state: &DropChooserState,
    hover: Option<UiControl>,
    scale: usize,
) {
    // The title bar carries the verb ("Insert Disk"); the header just
    // names the image, truncated to the panel width.
    let max_chars = (rect.w - 32) / 16;
    let mut header = state.disk_label.clone();
    if header.chars().count() > max_chars {
        header = header.chars().take(max_chars.saturating_sub(2)).collect();
        header.push_str("..");
    }
    let mut y = rect.y + TITLE_H + 10;
    draw_panel_text(frame, rect.x + 16, y, &header, PANEL_TEXT, 2, scale);
    y += 20;
    if state.disks.len() > 1 {
        let note = format!(
            "{} disks: extras queue as the drive's swap playlist",
            state.disks.len()
        );
        draw_panel_text(frame, rect.x + 16, y, &note, PANEL_TEXT_DIM, 1, scale);
    }
    for (index, (control, button_rect)) in drop_chooser_button_rects(rect, state)
        .into_iter()
        .enumerate()
    {
        let mut label = format!("{}  {}", index + 1, state.drives[index].label);
        // draw_text_button does not clip; keep long disk names inside.
        let max_label_chars = button_rect.w.saturating_sub(8) / font::GLYPH_W;
        if label.chars().count() > max_label_chars {
            label = label
                .chars()
                .take(max_label_chars.saturating_sub(2))
                .collect();
            label.push_str("..");
        }
        draw_text_button(
            frame,
            button_rect,
            &label,
            true,
            hover == Some(control),
            scale,
        );
    }
    let hint = format!("1-{} selects - Esc cancels", state.drives.len());
    draw_panel_text(
        frame,
        rect.x + 16,
        rect.y + rect.h - DROP_FOOTER_H + 6,
        &hint,
        PANEL_TEXT_DIM,
        1,
        scale,
    );
}

/// Full-display hint drawn while files hover over the window in a drag.
/// Not a Panel: it must not gate input, and winit reports no positions
/// during a file drag, so it can only announce that a drop will land.
pub fn draw_drop_hint(frame: &mut [u8], texture_scale: usize) {
    fill_rect_blend(
        frame,
        scale_rect(
            Rect {
                x: 0,
                y: 0,
                w: FB_WIDTH,
                h: present_height(),
            },
            texture_scale,
        ),
        SCRIM,
        SCRIM_ALPHA,
        texture_scale,
    );
    let text = "Drop disk image to insert";
    let px = 2;
    let x = FB_WIDTH.saturating_sub(text.len() * 8 * px) / 2;
    let y = present_height() / 2 - 8;
    draw_panel_text(frame, x, y, text, PANEL_TEXT_HILIGHT, px, texture_scale);
}

/// Vertical pitch of a shortcut row. The panel is sized from this and the
/// row count, and must stay inside `present_height()`.
const SHORTCUT_ROW_H: usize = 20;
/// Trailing note lines under the shortcut table, and their pitch.
const SHORTCUT_NOTES: [&str; 3] = [
    "Shortcuts: Cmd on macOS, Alt on Linux/Windows",
    "Amiga modifiers: Alt, Cmd/Super=Amiga, Ctrl",
    "In the debugger: S step, O over, U out, F frame, R run/pause",
];
const SHORTCUT_NOTE_H: usize = 12;

/// Panel height that exactly holds the table plus the notes, so adding a row
/// does not silently push the last one off the bottom.
fn shortcuts_panel_height() -> usize {
    TITLE_H
        + 14
        + SHORTCUT_ROWS.len() * SHORTCUT_ROW_H
        + 8
        + SHORTCUT_NOTES.len() * SHORTCUT_NOTE_H
        + 10
}

const SHORTCUT_ROWS: [(&str, &str, bool); 21] = [
    ("Q", "Quit", true),
    ("S", "Save screenshot", true),
    ("R", "Record video on/off", true),
    ("Shift+R", "Record input on/off", true),
    ("Shift+S", "Save state", true),
    ("Shift+L", "Load state", true),
    ("1-0", "Quick-save to a slot", true),
    ("Shift+1-0", "Quick-load from slot", true),
    ("D", "Swap queued disk", true),
    ("G", "Capture mouse", true),
    ("B", "Debugger", true),
    ("K", "Console", true),
    ("J", "Joystick input mode", true),
    ("Shift+A", "Cycle audio output", true),
    ("F", "Fullscreen on/off", true),
    ("Shift+F", "Status bar on/off", true),
    ("W", "Warp speed on/off", true),
    ("Shift+W", "Warp limit (2x..Max)", true),
    ("Z", "Rewind one step", true),
    ("Esc", "Close menu/window", false),
    ("Ctrl+Ami+Ami", "Keyboard reset", false),
];

fn draw_shortcuts(frame: &mut [u8], rect: Rect, scale: usize) {
    let mut y = rect.y + TITLE_H + 14;
    for (key, action, host_shortcut) in SHORTCUT_ROWS {
        let key_label = if host_shortcut {
            format!("{HOST_SHORTCUT_MODIFIER_LABEL}+{key}")
        } else {
            key.to_string()
        };
        draw_panel_text(
            frame,
            rect.x + 24,
            y,
            &key_label,
            PANEL_TEXT_ACCENT,
            2,
            scale,
        );
        draw_panel_text(frame, rect.x + 248, y, action, PANEL_TEXT, 2, scale);
        y += SHORTCUT_ROW_H;
    }
    y += 8;
    for line in SHORTCUT_NOTES {
        draw_panel_text(frame, rect.x + 24, y, line, PANEL_TEXT_DIM, 1, scale);
        y += SHORTCUT_NOTE_H;
    }
}

// Input Mapping panel geometry. One row per control, two mapping tabs above
// them, and the action buttons on the bottom edge like the other panels.
// Widths are sized off the longest label and the longest default binding
// list, so nothing collides: labels are drawn at the panel text size and the
// binding column (which can hold four aliases) at half that.
const INPUT_MAP_W: usize = 640;
const MAP_ROW_H: usize = 24;
const MAP_TAB_W: usize = 132;
const MAP_TAB_H: usize = 22;
const MAP_BUTTON_H: usize = 20;
const MAP_SET_W: usize = 62;
const MAP_CLEAR_W: usize = 62;
const MAP_ACTION_W: usize = 96;
const MAP_ACTION_H: usize = 22;
const MAP_MARGIN: usize = 16;
/// Font scale of the control labels, and of the binding list beside them.
const MAP_LABEL_PX: usize = 2;
const MAP_BINDING_PX: usize = 1;
/// Left edge of the binding column, and of the row's two buttons.
const MAP_BINDING_X: usize = 272;
const MAP_SET_X: usize = 480;
/// Footnote under the table, naming the pad-only controls once instead of
/// repeating "(CD32)" on five rows.
const MAP_NOTE: &str = "Green, Yellow, Play, Rewind and Forward are CD32 pad buttons.";

fn input_map_rows_top(rect: Rect) -> usize {
    rect.y + TITLE_H + 10 + MAP_TAB_H + 12
}

fn input_map_panel_height() -> usize {
    TITLE_H
        + 10
        + MAP_TAB_H
        + 12
        + crate::keymap::CONTROLS.len() * MAP_ROW_H
        + 10
        + 2 * 14 // message + footnote lines
        + 8
        + MAP_ACTION_H
        + 8
}

/// Characters that fit a column `width` pixels wide at font scale `px`.
fn columns_for(width: usize, px: usize) -> usize {
    width / (font::GLYPH_W * px)
}

/// Clip `text` to `max` characters, marking the cut so a truncated binding
/// list does not read as the whole list.
fn clip_to_columns(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}~")
}

/// Every clickable control in the panel, with its rect: the two mapping tabs,
/// a Set and a Clear button per row, then Defaults / Save.
fn input_map_control_rects(rect: Rect) -> Vec<(UiControl, Rect)> {
    let mut out = Vec::with_capacity(2 * crate::keymap::CONTROLS.len() + 4);
    for set in 0..crate::keymap::MAPPING_COUNT {
        out.push((
            UiControl::RemapSet(set),
            Rect {
                x: rect.x + MAP_MARGIN + set * (MAP_TAB_W + 8),
                y: rect.y + TITLE_H + 10,
                w: MAP_TAB_W,
                h: MAP_TAB_H,
            },
        ));
    }
    let top = input_map_rows_top(rect);
    for (i, _) in crate::keymap::CONTROLS.iter().enumerate() {
        let y = top + i * MAP_ROW_H + (MAP_ROW_H - MAP_BUTTON_H) / 2;
        out.push((
            UiControl::RemapBind(i),
            Rect {
                x: rect.x + MAP_SET_X,
                y,
                w: MAP_SET_W,
                h: MAP_BUTTON_H,
            },
        ));
        out.push((
            UiControl::RemapClear(i),
            Rect {
                x: rect.x + MAP_SET_X + MAP_SET_W + 8,
                y,
                w: MAP_CLEAR_W,
                h: MAP_BUTTON_H,
            },
        ));
    }
    let action_y = rect.y + rect.h - MAP_ACTION_H - 8;
    for (i, control) in [UiControl::RemapDefaults, UiControl::RemapSave]
        .into_iter()
        .enumerate()
    {
        out.push((
            control,
            Rect {
                x: rect.x + rect.w - (2 - i) * (MAP_ACTION_W + 8),
                y: action_y,
                w: MAP_ACTION_W,
                h: MAP_ACTION_H,
            },
        ));
    }
    out
}

fn draw_input_map(
    frame: &mut [u8],
    rect: Rect,
    panel: &InputMapPanel,
    hover: Option<UiControl>,
    scale: usize,
) {
    let controls = input_map_control_rects(rect);
    let mapping = panel.map.mapping(panel.mapping);
    for (control, button_rect) in &controls {
        match *control {
            UiControl::RemapSet(set) => {
                let label = if set == 0 {
                    "Controller 1"
                } else {
                    "Controller 2"
                };
                draw_launcher_chip(
                    frame,
                    *button_rect,
                    label,
                    set == panel.mapping,
                    hover == Some(*control),
                    false,
                    scale,
                );
            }
            UiControl::RemapBind(i) => {
                let armed = panel.capturing == Some(crate::keymap::CONTROLS[i]);
                let label = if armed { "..." } else { "Set" };
                draw_text_button(
                    frame,
                    *button_rect,
                    label,
                    true,
                    hover == Some(*control),
                    scale,
                );
            }
            UiControl::RemapClear(i) => {
                let bound = !mapping.keys(crate::keymap::CONTROLS[i]).is_empty();
                draw_text_button(
                    frame,
                    *button_rect,
                    "Clear",
                    bound,
                    hover == Some(*control),
                    scale,
                );
            }
            UiControl::RemapDefaults => draw_text_button(
                frame,
                *button_rect,
                "Defaults",
                true,
                hover == Some(*control),
                scale,
            ),
            UiControl::RemapSave => draw_text_button(
                frame,
                *button_rect,
                "Save",
                true,
                hover == Some(*control),
                scale,
            ),
            _ => {}
        }
    }

    let top = input_map_rows_top(rect);
    let label_cols = columns_for(MAP_BINDING_X - MAP_MARGIN - 8, MAP_LABEL_PX);
    let binding_cols = columns_for(MAP_SET_X - MAP_BINDING_X - 8, MAP_BINDING_PX);
    for (i, control) in crate::keymap::CONTROLS.iter().enumerate() {
        let armed = panel.capturing == Some(*control);
        let label_colour = if armed {
            PANEL_TEXT_HILIGHT
        } else {
            PANEL_TEXT
        };
        draw_panel_text(
            frame,
            rect.x + MAP_MARGIN,
            top + i * MAP_ROW_H + (MAP_ROW_H - font::GLYPH_H * MAP_LABEL_PX) / 2,
            &clip_to_columns(control.label(), label_cols),
            label_colour,
            MAP_LABEL_PX,
            scale,
        );
        let binding = mapping.binding_text(*control);
        let binding_colour = if armed {
            PANEL_TEXT_HILIGHT
        } else if binding == "-" {
            PANEL_TEXT_DIM
        } else {
            PANEL_TEXT_ACCENT
        };
        draw_panel_text(
            frame,
            rect.x + MAP_BINDING_X,
            top + i * MAP_ROW_H + (MAP_ROW_H - font::GLYPH_H * MAP_BINDING_PX) / 2,
            &clip_to_columns(&binding, binding_cols),
            binding_colour,
            MAP_BINDING_PX,
            scale,
        );
    }

    let message_y = top + crate::keymap::CONTROLS.len() * MAP_ROW_H + 10;
    draw_panel_text(
        frame,
        rect.x + MAP_MARGIN,
        message_y,
        &panel.message,
        PANEL_TEXT_ACCENT,
        1,
        scale,
    );
    draw_panel_text(
        frame,
        rect.x + MAP_MARGIN,
        message_y + 14,
        MAP_NOTE,
        PANEL_TEXT_DIM,
        1,
        scale,
    );
}

fn draw_calibration(
    frame: &mut [u8],
    rect: Rect,
    view: &CalibrationView,
    hover: Option<UiControl>,
    session: &crate::gamepad::CalibrationSession,
    scale: usize,
) {
    let mut y = rect.y + TITLE_H + 10;
    draw_panel_text(frame, rect.x + 16, y, &view.pad_line, PANEL_TEXT, 2, scale);
    y += 24;
    for row in &view.rows {
        let (marker, color) = if row.current {
            (">", PANEL_TEXT_HILIGHT)
        } else if row.binding.is_empty() {
            (" ", PANEL_TEXT_DIM)
        } else {
            (" ", PANEL_TEXT)
        };
        draw_panel_text(frame, rect.x + 16, y, marker, PANEL_TEXT_HILIGHT, 2, scale);
        draw_panel_text(frame, rect.x + 36, y, row.label, color, 2, scale);
        draw_panel_text(frame, rect.x + 388, y, &row.binding, color, 2, scale);
        y += 18;
    }
    y += 6;
    draw_panel_text(
        frame,
        rect.x + 16,
        y,
        &view.status,
        PANEL_TEXT_ACCENT,
        1,
        scale,
    );
    for (control, button_rect) in cal_button_rects(rect) {
        let label = match control {
            UiControl::CalSkip => "Skip",
            UiControl::CalCancel => "Cancel",
            _ => "Save",
        };
        draw_text_button(
            frame,
            button_rect,
            label,
            cal_button_enabled(control, session),
            hover == Some(control),
            scale,
        );
    }
}

fn draw_debugger(
    frame: &mut [u8],
    rect: Rect,
    panel: &DebuggerPanel,
    view: &DebuggerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    // Status summary on the right of the title bar.
    let status_w = view.status.chars().count() * font::GLYPH_W;
    draw_panel_text(
        frame,
        rect.x + rect.w - TITLE_H - 8 - status_w.min(rect.w.saturating_sub(TITLE_H + 16)),
        rect.y + (TITLE_H - 8) / 2,
        &view.status,
        PANEL_TITLE_TEXT,
        1,
        scale,
    );
    // Tabs.
    for (index, tab) in DEBUG_TABS.iter().enumerate() {
        let tab_rect = debug_tab_rect(rect, index);
        let selected = panel.tab == *tab;
        let hovered = hover == Some(UiControl::DebugTab(*tab));
        let face = if selected {
            ENTRY_BG
        } else if hovered {
            BUTTON_FACE_HOVER
        } else {
            BUTTON_FACE
        };
        let scaled = scale_rect(tab_rect, scale);
        fill_rect(frame, scaled, face, scale);
        draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
        let label = debug_tab_label(*tab);
        let text_w = label.chars().count() * font::GLYPH_W;
        draw_panel_text(
            frame,
            tab_rect.x + tab_rect.w.saturating_sub(text_w) / 2,
            tab_rect.y + (DEBUG_TAB_H - 8) / 2,
            label,
            if selected { ENTRY_TEXT } else { BUTTON_TEXT },
            1,
            scale,
        );
    }
    // Break-tab toggle buttons at the top of the content area (the view
    // leaves BREAK_TAB_HEADER_LINES blank so text starts below them).
    if panel.tab == DebugTab::Break {
        for (control, button_rect) in break_tab_button_rects(rect) {
            let label = match control {
                UiControl::DebugBreakToggle => "Break +/-",
                UiControl::DebugWatchToggle => "Watch +/-",
                UiControl::DebugRegToggle => "Reg +/-",
                UiControl::DebugBeamToggle => "Beam +/-",
                UiControl::DebugCatchToggle => "Catch +/-",
                _ => "Clear all",
            };
            let enabled = match control {
                UiControl::DebugBreaksClear => true,
                UiControl::DebugBeamToggle => parse_beam_spec(&panel.entry).is_some(),
                UiControl::DebugCatchToggle => parse_catch_spec(&panel.entry).is_some(),
                _ => panel.entry_addr().is_some(),
            };
            draw_text_button(
                frame,
                button_rect,
                label,
                enabled,
                hover == Some(control),
                scale,
            );
        }
    }
    // Waveform-tab buttons at the top of the content area (the view leaves
    // WAVEFORM_TAB_HEADER_LINES blank so text starts below them).
    if panel.tab == DebugTab::Waveform {
        for (control, button_rect) in waveform_tab_button_rects(rect) {
            let (label, enabled) = match control {
                UiControl::DebugWaveArm => (
                    "Arm",
                    crate::waveform::parse_wave_args(panel.entry.split_whitespace()).is_ok(),
                ),
                _ => ("Stop", true),
            };
            draw_text_button(
                frame,
                button_rect,
                label,
                enabled,
                hover == Some(control),
                scale,
            );
        }
    }
    // Copper-tab buttons at the top of the content area (the view leaves
    // COPPER_TAB_HEADER_LINES blank so text starts below them).
    if panel.tab == DebugTab::Copper {
        for (control, button_rect) in copper_tab_button_rects(rect) {
            let (label, enabled) = match control {
                UiControl::DebugCopperBreakToggle => ("CBreak +/-", panel.entry_addr().is_some()),
                _ => ("CStep", true),
            };
            draw_text_button(
                frame,
                button_rect,
                label,
                enabled,
                hover == Some(control),
                scale,
            );
        }
    }
    // Memory-tab buttons at the top of the content area.
    if panel.tab == DebugTab::Memory {
        for (control, button_rect) in mem_tab_button_rects(rect) {
            let (label, enabled) = match control {
                UiControl::DebugMemFind => ("Find", panel.find_pattern().is_some()),
                UiControl::DebugMemSave => ("Save...", panel.region_spec().is_some()),
                UiControl::DebugMemWriter => ("Writer?", panel.entry_addr().is_some()),
                _ => (if panel.mem_view_bits { "Hex" } else { "Bits" }, true),
            };
            draw_text_button(
                frame,
                button_rect,
                label,
                enabled,
                hover == Some(control),
                scale,
            );
        }
    }
    // The Audio tab is drawn as a custom graphical layout (mute buttons and
    // oscilloscopes); every other tab is a plain list of content lines.
    if panel.tab == DebugTab::Audio {
        if let Some(audio) = &view.audio {
            draw_audio_tab(frame, rect, audio, hover, scale);
        }
    } else {
        // Content lines. Two transport rows sit at the bottom now (the main row
        // plus the Step Over/Out row), so the text area ends above both.
        let content_top = debug_content_top(rect);
        let content_bottom = rect.y + rect.h - 2 * DEBUG_BUTTON_H - 16;
        let pitch = 10;
        let max_lines = content_bottom.saturating_sub(content_top) / pitch;
        for (index, line) in view.lines.iter().take(max_lines).enumerate() {
            let color = if line.highlight {
                PANEL_TEXT_HILIGHT
            } else {
                PANEL_TEXT
            };
            draw_panel_text(
                frame,
                rect.x + 10,
                content_top + index * pitch,
                &line.text,
                color,
                1,
                scale,
            );
        }
    }
    // The Memory tab's bitplane view, drawn below its caption lines.
    if panel.tab == DebugTab::Memory {
        if let Some(bitmap) = &view.bitmap {
            draw_mem_bitmap(frame, rect, bitmap, scale);
        }
    }
    // The Video tab is drawn as a custom graphical layout.
    if panel.tab == DebugTab::Video {
        if let Some(video) = &view.video {
            draw_video_tab(frame, rect, video, hover, scale);
        }
    }
    // Transport buttons and the hex-entry box.
    for (control, button_rect) in debug_button_rects(rect) {
        match control {
            UiControl::DebugEntry => {
                let scaled = scale_rect(button_rect, scale);
                fill_rect(frame, scaled, ENTRY_BG, scale);
                draw_rect_bevel(frame, scaled, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, scale);
                let caret = if panel.entry_active { "_" } else { "" };
                let text = format!("${}{}", panel.entry, caret);
                draw_panel_text(
                    frame,
                    button_rect.x + 6,
                    button_rect.y + (DEBUG_BUTTON_H - 8) / 2,
                    &text,
                    ENTRY_TEXT,
                    1,
                    scale,
                );
            }
            _ => {
                let label = match control {
                    UiControl::DebugRun => {
                        if view.running {
                            "Pause"
                        } else {
                            "Run"
                        }
                    }
                    UiControl::DebugStep => "Step",
                    UiControl::DebugStepOver => "Step Over",
                    UiControl::DebugStepOut => "Step Out",
                    UiControl::DebugStepFrame => "Frame",
                    UiControl::DebugRunTo => "Run to $",
                    UiControl::DebugRunLine => "Line",
                    UiControl::DebugReverseStep => "< Step",
                    UiControl::DebugReverseFrame => "< Frame",
                    UiControl::DebugReverseRun => "< Run",
                    UiControl::DebugMemPrev => "<",
                    UiControl::DebugMemNext => ">",
                    UiControl::DebugPoke => {
                        if panel.tab == DebugTab::Cpu {
                            "Set Reg"
                        } else {
                            "Poke"
                        }
                    }
                    _ => "",
                };
                let enabled = match control {
                    UiControl::DebugMemPrev | UiControl::DebugMemNext => {
                        panel.tab == DebugTab::Memory
                    }
                    UiControl::DebugRunTo => panel.entry_addr().is_some(),
                    UiControl::DebugPoke => match panel.tab {
                        DebugTab::Memory => panel.poke_target().is_some(),
                        DebugTab::Cpu => panel.reg_poke().is_some(),
                        _ => false,
                    },
                    UiControl::DebugReverseStep
                    | UiControl::DebugReverseFrame
                    | UiControl::DebugReverseRun => view.reverse_available,
                    _ => true,
                };
                draw_text_button(
                    frame,
                    button_rect,
                    label,
                    enabled,
                    hover == Some(control),
                    scale,
                );
            }
        }
    }
}

/// Draw the Memory tab's 1-bpp plane view: 2x2 pixels per bit, set bits
/// light, clipped to the panel width (a wide stride simply runs off the
/// right edge, like a real overwide screen).
fn draw_mem_bitmap(frame: &mut [u8], rect: Rect, bitmap: &MemBitmapView, scale: usize) {
    let origin_x = rect.x + 10;
    let origin_y = rect.y + TITLE_H + 4 + DEBUG_TAB_H + 6 + MEM_TAB_HEADER_LINES * 10 + 14;
    let max_w = rect.w.saturating_sub(20);
    let plot = Rect {
        x: origin_x,
        y: origin_y,
        w: (bitmap.stride * 8 * 2).min(max_w),
        h: bitmap.rows * 2,
    };
    fill_rect(frame, scale_rect(plot, scale), rgba(16, 18, 20), scale);
    let set = rgba(214, 224, 230);
    for row in 0..bitmap.rows {
        for byte_col in 0..bitmap.stride {
            let Some(&byte) = bitmap.data.get(row * bitmap.stride + byte_col) else {
                continue;
            };
            if byte == 0 {
                continue;
            }
            for bit in 0..8 {
                if byte & (0x80 >> bit) == 0 {
                    continue;
                }
                let x = (byte_col * 8 + bit) * 2;
                if x + 2 > max_w {
                    break;
                }
                fill_rect(
                    frame,
                    scale_rect(
                        Rect {
                            x: origin_x + x,
                            y: origin_y + row * 2,
                            w: 2,
                            h: 2,
                        },
                        scale,
                    ),
                    set,
                    scale,
                );
            }
        }
    }
    draw_outline(frame, plot, BUTTON_EDGE_LIGHT, scale);
}

/// Lines of scrollback visible in the console's output area.
pub fn console_visible_lines() -> usize {
    // Fixed panel height (see panel_dims): title bar, then the output
    // area at 10px pitch, leaving the input line and a margin.
    let panel_h = 460;
    (panel_h - TITLE_H - 10 - (CONSOLE_INPUT_H + 12)) / 10
}

const CONSOLE_INPUT_H: usize = 20;

/// Draw the debugger console: scrollback text over a prompt line.
fn draw_console(frame: &mut [u8], rect: Rect, panel: &ConsolePanel, scale: usize) {
    let visible = console_visible_lines();
    let total = panel.output.len();
    // scroll counts lines back from the tail.
    let end = total.saturating_sub(panel.scroll.min(total.saturating_sub(visible)));
    let start = end.saturating_sub(visible);
    let mut y = rect.y + TITLE_H + 6;
    for line in panel.output.iter().skip(start).take(end - start) {
        let (text, color) = if let Some(cmd) = line.strip_prefix("> ") {
            (format!("> {cmd}"), PANEL_TEXT_HILIGHT)
        } else if let Some(rest) = line.strip_prefix('!') {
            (rest.to_string(), PANEL_TEXT_ACCENT)
        } else {
            (line.clone(), PANEL_TEXT)
        };
        let mut text = text;
        text.truncate(84);
        draw_panel_text(frame, rect.x + 10, y, &text, color, 1, scale);
        y += 10;
    }
    if panel.scroll > 0 {
        draw_panel_text(
            frame,
            rect.x + rect.w - 110,
            rect.y + TITLE_H + 6,
            &format!("[-{} lines]", panel.scroll),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    // Prompt line in an entry-style box at the bottom.
    let entry = Rect {
        x: rect.x + 8,
        y: rect.y + rect.h - CONSOLE_INPUT_H - 6,
        w: rect.w - 16,
        h: CONSOLE_INPUT_H,
    };
    let scaled = scale_rect(entry, scale);
    fill_rect(frame, scaled, ENTRY_BG, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, scale);
    let mut prompt = format!("> {}_", panel.input);
    prompt.truncate(84);
    draw_panel_text(
        frame,
        entry.x + 6,
        entry.y + (CONSOLE_INPUT_H - 8) / 2,
        &prompt,
        ENTRY_TEXT,
        1,
        scale,
    );
}

/// Draw the Video tab: the BPLCON0/DMACON header, the plane and sprite
/// layer-isolation toggle rows, eight sprite rows (decode text plus a
/// thumbnail from the frame's sprite DMA), and the palette grid.
fn draw_video_tab(
    frame: &mut [u8],
    rect: Rect,
    video: &VideoView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = debug_content_top(rect);
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        &video.header,
        PANEL_TEXT_HILIGHT,
        1,
        scale,
    );
    for (row, label) in ["Planes", "Sprites"].iter().enumerate() {
        draw_panel_text(
            frame,
            rect.x + 10,
            video_toggle_row_y(rect, row) + (VIDEO_TOGGLE_H - 8) / 2,
            label,
            PANEL_TEXT,
            1,
            scale,
        );
    }
    for (control, button_rect) in video_tab_toggle_rects(rect) {
        let (label, shown, exists) = match control {
            UiControl::DebugPlaneToggle(plane) => (
                format!("{}", plane + 1),
                video.plane_mask & (1 << plane) != 0,
                plane < video.nplanes,
            ),
            UiControl::DebugSpriteToggle(sprite) => (
                format!("{sprite}"),
                video.sprite_mask & (1 << sprite) != 0,
                true,
            ),
            _ => continue,
        };
        // A hidden layer draws with the disabled text style so the
        // toggle row doubles as the isolation-state display; planes
        // beyond the current BPLCON0 depth stay clickable (a mid-frame
        // Copper can raise the depth) but are marked with a dot.
        let label = if exists { label } else { format!("{label}.") };
        draw_text_button(
            frame,
            button_rect,
            &label,
            shown,
            hover == Some(control),
            scale,
        );
    }
    let sprites_top = video_sprites_top(rect);
    for (sprite, row) in video.sprites.iter().enumerate() {
        let y = sprites_top + sprite * VIDEO_SPRITE_ROW_H;
        draw_panel_text(frame, rect.x + 10, y + 4, &row.text, PANEL_TEXT, 1, scale);
        // Thumbnail: 16 sprite pixels wide at 2x, one panel pixel per
        // sampled DMA line, over a dark backdrop.
        let thumb = Rect {
            x: rect.x + VIDEO_THUMB_X,
            y,
            w: 16 * 2,
            h: VIDEO_SPRITE_ROW_H.saturating_sub(2),
        };
        fill_rect(frame, scale_rect(thumb, scale), rgba(14, 16, 18), scale);
        for line in 0..row.thumb_rows.min(thumb.h) {
            for x in 0..16usize {
                let pix = row.thumb[line * 16 + x];
                if pix == 0 {
                    continue;
                }
                fill_rect(
                    frame,
                    scale_rect(
                        Rect {
                            x: thumb.x + x * 2,
                            y: thumb.y + line,
                            w: 2,
                            h: 1,
                        },
                        scale,
                    ),
                    pix,
                    scale,
                );
            }
        }
        draw_outline(frame, thumb, BUTTON_EDGE_DARK, scale);
    }
    let palette_top = video_palette_top(rect);
    draw_panel_text(
        frame,
        rect.x + 10,
        palette_top,
        &format!("Palette ({} entries)", video.palette.len()),
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    for (idx, &color) in video.palette.iter().enumerate() {
        let cell = Rect {
            x: rect.x + 10 + (idx % 32) * VIDEO_PALETTE_CELL_W,
            y: palette_top + 12 + (idx / 32) * VIDEO_PALETTE_CELL_H,
            w: VIDEO_PALETTE_CELL_W - 1,
            h: VIDEO_PALETTE_CELL_H - 1,
        };
        fill_rect(frame, scale_rect(cell, scale), color, scale);
    }
}

/// Draw the Audio tab: a header line, four Paula channel blocks and a CD row,
/// each with a mute button, text detail, and an output oscilloscope.
fn draw_audio_tab(
    frame: &mut [u8],
    rect: Rect,
    audio: &AudioScopeView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = debug_content_top(rect);
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        &audio.header,
        PANEL_TEXT_HILIGHT,
        1,
        scale,
    );
    for idx in 0..5 {
        let (mute_rect, scope_rect) = audio_row_geom(rect, idx);
        let row = if idx < 4 {
            audio.channels.get(idx)
        } else {
            Some(&audio.cd)
        };
        let Some(row) = row else { continue };
        let control = UiControl::DebugAudioMute(idx);
        draw_mute_button(frame, mute_rect, row.muted, hover == Some(control), scale);
        // Text detail lines to the right of the mute button.
        for (line, dbg) in row.text.iter().enumerate() {
            let color = if dbg.highlight {
                PANEL_TEXT_HILIGHT
            } else {
                PANEL_TEXT
            };
            draw_panel_text(
                frame,
                rect.x + AUDIO_TEXT_X,
                mute_rect.y + line * 10,
                &dbg.text,
                color,
                1,
                scale,
            );
        }
        let color = AUDIO_SCOPE_COLORS[idx.min(4)];
        draw_audio_scope(frame, scope_rect, &row.scope, color, row.muted, scale);
    }
}

/// A single mute toggle button: red-tinted face and "Muted" label when active.
fn draw_mute_button(frame: &mut [u8], rect: Rect, muted: bool, hover: bool, scale: usize) {
    let face = if muted {
        AUDIO_MUTE_FACE
    } else if hover {
        BUTTON_FACE_HOVER
    } else {
        BUTTON_FACE
    };
    let scaled = scale_rect(rect, scale);
    fill_rect(frame, scaled, face, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
    let label = if muted { "Muted" } else { "Mute" };
    let text_w = label.chars().count() * font::GLYPH_W;
    let x = rect.x + rect.w.saturating_sub(text_w) / 2;
    let y = rect.y + rect.h.saturating_sub(font::GLYPH_H) / 2;
    draw_panel_text(frame, x, y, label, BUTTON_TEXT, 1, scale);
}

/// Draw one oscilloscope box: dark background, centre zero line, and a trace
/// of the newest samples (greyed when muted).
fn draw_audio_scope(
    frame: &mut [u8],
    box_rect: Rect,
    samples: &[i8],
    color: u32,
    muted: bool,
    scale: usize,
) {
    let scaled = scale_rect(box_rect, scale);
    fill_rect(frame, scaled, ENTRY_BG, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, scale);
    if box_rect.w < 3 || box_rect.h < 3 {
        return;
    }
    // Interior, inset one pixel from the bevel.
    let inner = Rect {
        x: box_rect.x + 1,
        y: box_rect.y + 1,
        w: box_rect.w - 2,
        h: box_rect.h - 2,
    };
    let centre_y = inner.y + inner.h / 2;
    // Zero line.
    fill_rect_clipped(
        frame,
        Rect {
            x: inner.x,
            y: centre_y,
            w: inner.w,
            h: 1,
        },
        inner,
        PANEL_TEXT_DIM,
        scale,
    );
    if samples.is_empty() {
        return;
    }
    let trace = if muted { PANEL_TEXT_DIM } else { color };
    // Map the newest `inner.w` samples across the box (1 sample per column),
    // connecting consecutive points with a vertical span so the trace reads as
    // a continuous waveform. Amplitude: +/-128 maps to half the box height.
    let half = (inner.h / 2).max(1);
    let start = samples.len().saturating_sub(inner.w);
    let window = &samples[start..];
    let sample_y = |s: i8| -> usize {
        let offset = (s as i32 * half as i32) / 128;
        (centre_y as i32 - offset).clamp(inner.y as i32, (inner.y + inner.h - 1) as i32) as usize
    };
    let mut prev_y = sample_y(window[0]);
    for (col, &s) in window.iter().enumerate() {
        let x = inner.x + col;
        let y = sample_y(s);
        let (top, bottom) = (prev_y.min(y), prev_y.max(y));
        fill_rect_clipped(
            frame,
            Rect {
                x,
                y: top,
                w: 1,
                h: bottom - top + 1,
            },
            inner,
            trace,
            scale,
        );
        prev_y = y;
    }
}

fn owner_color(code: u8) -> u32 {
    match code {
        b'R' => rgba(68, 180, 190),
        b'B' => rgba(64, 118, 230),
        b'S' => rgba(212, 84, 220),
        b'D' => rgba(190, 122, 54),
        b'A' => rgba(72, 190, 96),
        b'C' => rgba(238, 206, 72),
        b'L' => rgba(222, 78, 76),
        b'P' => rgba(230, 232, 224),
        _ => rgba(20, 22, 26),
    }
}

fn owner_name_for_code(code: u8) -> &'static str {
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

fn draw_outline(frame: &mut [u8], rect: Rect, color: u32, scale: usize) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: 1,
            },
            scale,
        ),
        color,
        scale,
    );
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: rect.x,
                y: rect.y + rect.h.saturating_sub(1),
                w: rect.w,
                h: 1,
            },
            scale,
        ),
        color,
        scale,
    );
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: rect.x,
                y: rect.y,
                w: 1,
                h: rect.h,
            },
            scale,
        ),
        color,
        scale,
    );
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: rect.x + rect.w.saturating_sub(1),
                y: rect.y,
                w: 1,
                h: rect.h,
            },
            scale,
        ),
        color,
        scale,
    );
}

fn clipped_rect(rect: Rect, clip: Rect) -> Option<Rect> {
    let x0 = rect.x.max(clip.x);
    let y0 = rect.y.max(clip.y);
    let x1 = rect
        .x
        .saturating_add(rect.w)
        .min(clip.x.saturating_add(clip.w));
    let y1 = rect
        .y
        .saturating_add(rect.h)
        .min(clip.y.saturating_add(clip.h));
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

fn fill_rect_clipped(frame: &mut [u8], rect: Rect, clip: Rect, color: u32, scale: usize) {
    if let Some(rect) = clipped_rect(rect, clip) {
        fill_rect(frame, scale_rect(rect, scale), color, scale);
    }
}

fn draw_outline_clipped(frame: &mut [u8], rect: Rect, clip: Rect, color: u32, scale: usize) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    fill_rect_clipped(
        frame,
        Rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: 1,
        },
        clip,
        color,
        scale,
    );
    fill_rect_clipped(
        frame,
        Rect {
            x: rect.x,
            y: rect.y + rect.h.saturating_sub(1),
            w: rect.w,
            h: 1,
        },
        clip,
        color,
        scale,
    );
    fill_rect_clipped(
        frame,
        Rect {
            x: rect.x,
            y: rect.y,
            w: 1,
            h: rect.h,
        },
        clip,
        color,
        scale,
    );
    fill_rect_clipped(
        frame,
        Rect {
            x: rect.x + rect.w.saturating_sub(1),
            y: rect.y,
            w: 1,
            h: rect.h,
        },
        clip,
        color,
        scale,
    );
}

fn trace_x(rect: Rect, hpos: usize, cols: usize) -> usize {
    rect.x + (hpos.min(cols.saturating_sub(1)) * rect.w / cols.max(1))
}

fn trace_y(rect: Rect, vpos: usize, rows: usize) -> usize {
    rect.y + (vpos.min(rows.saturating_sub(1)) * rect.h / rows.max(1))
}

/// Halve each colour channel of an RGBA pixel, keeping it opaque. Dims the
/// picture underlay so the DMA colours drawn over it stay readable.
fn dim_rgba(pix: u32) -> u32 {
    ((pix >> 1) & 0x007F_7F7F) | 0xFF00_0000
}

/// Deep-dim an RGBA pixel to an eighth, keeping it opaque: the ghost of
/// the not-yet-drawn region while beam scrubbing.
fn ghost_rgba(pix: u32) -> u32 {
    ((pix >> 3) & 0x001F_1F1F) | 0xFF00_0000
}

/// Sample the picture underlay for heatmap pixel (`x`, `vpos`): `x` is the
/// horizontal heatmap pixel (mapped at hi-res precision, four pixels per
/// colour clock) and `vpos` the already-resolved beam line.
fn underlay_sample(
    underlay: &AnalyzerUnderlayView,
    trace: &AnalyzerTraceView,
    rect: Rect,
    x: usize,
    vpos: usize,
) -> Option<u32> {
    let hires_x = x * trace.cols * 4 / rect.w.max(1);
    let fb_x = hires_x as i64 - i64::from(trace.display_hpos_start) * 4;
    let fb_y = vpos as i64 - i64::from(trace.visible_start_vpos);
    if !(0..FB_WIDTH as i64).contains(&fb_x) || !(0..underlay.rows as i64).contains(&fb_y) {
        return None;
    }
    // The underlay canvas may carry a 35 ns pixel pitch; sample at its scale.
    let canvas_scale = underlay.width / FB_WIDTH;
    underlay
        .fb
        .get(fb_y as usize * underlay.width + fb_x as usize * canvas_scale)
        .copied()
}

fn draw_owner_heatmap(
    frame: &mut [u8],
    rect: Rect,
    trace: &AnalyzerTraceView,
    underlay: Option<&AnalyzerUnderlayView>,
    scrub: bool,
    scale: usize,
) {
    fill_rect(frame, scale_rect(rect, scale), rgba(10, 12, 14), scale);
    for y in 0..rect.h {
        let vpos = y * trace.rows / rect.h.max(1);
        for x in 0..rect.w {
            let hpos = x * trace.cols / rect.w.max(1);
            let code = trace.owner_code_at(vpos, hpos);
            let mut color = owner_color(code);
            if let Some(pix) =
                underlay.and_then(|under| underlay_sample(under, trace, rect, x, vpos))
            {
                // Picture shows through idle slots; owned slots blend the
                // owner colour over the dimmed picture so both read. While
                // scrubbing, beam positions the CRT has not reached yet
                // ghost at an eighth brightness.
                let drawn = !scrub || (vpos, hpos) <= (trace.selected_vpos, trace.selected_hpos);
                let under_pix = if drawn {
                    dim_rgba(pix)
                } else {
                    ghost_rgba(pix)
                };
                color = if code == b'.' {
                    under_pix
                } else {
                    super::blend_rgba(under_pix, color, 176)
                };
            }
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: rect.x + x,
                        y: rect.y + y,
                        w: 1,
                        h: 1,
                    },
                    scale,
                ),
                color,
                scale,
            );
        }
    }

    let visible_top = trace_y(rect, trace.visible_start_vpos as usize, trace.rows);
    let visible_bottom = trace_y(
        rect,
        (trace.visible_start_vpos as usize)
            .saturating_add(trace.visible_lines)
            .min(trace.rows.saturating_sub(1)),
        trace.rows,
    )
    .max(visible_top + 1);
    let display_left = trace_x(rect, trace.display_hpos_start as usize, trace.cols);
    let display_right =
        trace_x(rect, trace.display_hpos_end as usize, trace.cols).max(display_left + 1);
    draw_outline(
        frame,
        Rect {
            x: display_left,
            y: visible_top,
            w: display_right.saturating_sub(display_left).max(1),
            h: visible_bottom.saturating_sub(visible_top).max(1),
        },
        rgba(238, 238, 232),
        scale,
    );

    // Frame-start DIW box (accent) and DDF fetch-bound verticals (cyan),
    // spanning the display window's lines. Mid-frame changes to these
    // registers show up as write markers instead.
    let diw_rows = trace.diw_v.map(|(v0, v1)| {
        (
            trace_y(rect, usize::from(v0).min(trace.rows), trace.rows),
            trace_y(rect, usize::from(v1).min(trace.rows), trace.rows),
        )
    });
    if let (Some((y0, y1)), Some((h0, h1))) = (diw_rows, trace.diw_h_cck) {
        let x0 = trace_x(rect, usize::from(h0).min(trace.cols), trace.cols);
        let x1 = trace_x(rect, usize::from(h1).min(trace.cols), trace.cols);
        draw_outline_clipped(
            frame,
            Rect {
                x: x0,
                y: y0,
                w: x1.saturating_sub(x0).max(1),
                h: y1.saturating_sub(y0).max(1),
            },
            rect,
            PANEL_TEXT_ACCENT,
            scale,
        );
    }
    if let (Some((y0, y1)), Some((d0, d1))) = (diw_rows, trace.ddf_cck) {
        for ddf in [d0, d1] {
            fill_rect_clipped(
                frame,
                Rect {
                    x: trace_x(rect, usize::from(ddf).min(trace.cols), trace.cols),
                    y: y0,
                    w: 1,
                    h: y1.saturating_sub(y0).max(1),
                },
                rect,
                DDF_LINE,
                scale,
            );
        }
    }

    for marker in trace.markers.iter() {
        let x = trace_x(rect, marker.hpos as usize, trace.cols);
        let y = trace_y(rect, marker.vpos as usize, trace.rows);
        fill_rect_clipped(
            frame,
            Rect {
                x: x.saturating_sub(1),
                y,
                w: 3,
                h: 1,
            },
            rect,
            PANEL_TEXT_ACCENT,
            scale,
        );
        fill_rect_clipped(
            frame,
            Rect {
                x,
                y: y.saturating_sub(1),
                w: 1,
                h: 3,
            },
            rect,
            PANEL_TEXT_ACCENT,
            scale,
        );
    }

    let sx = trace_x(rect, trace.selected_hpos, trace.cols);
    let sy = trace_y(rect, trace.selected_vpos, trace.rows);
    draw_outline_clipped(
        frame,
        Rect {
            x: sx.saturating_sub(3),
            y: sy.saturating_sub(3),
            w: 7,
            h: 7,
        },
        rect,
        PANEL_TEXT_HILIGHT,
        scale,
    );
    draw_outline(frame, rect, BUTTON_EDGE_LIGHT, scale);
}

fn draw_scanline_strip(frame: &mut [u8], rect: Rect, trace: &AnalyzerTraceView, scale: usize) {
    fill_rect(frame, scale_rect(rect, scale), rgba(10, 12, 14), scale);
    if let Some(row) = trace.owner_row(trace.selected_vpos) {
        for x in 0..rect.w {
            let hpos = x * trace.cols / rect.w.max(1);
            let color = owner_color(row[hpos.min(row.len().saturating_sub(1))]);
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: rect.x + x,
                        y: rect.y + 8,
                        w: 1,
                        h: rect.h.saturating_sub(14),
                    },
                    scale,
                ),
                color,
                scale,
            );
        }
    }
    let sx = trace_x(rect, trace.selected_hpos, trace.cols);
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: sx,
                y: rect.y,
                w: 1,
                h: rect.h,
            },
            scale,
        ),
        PANEL_TEXT_HILIGHT,
        scale,
    );
    draw_outline(frame, rect, BUTTON_EDGE_LIGHT, scale);
}

fn draw_owner_counters(
    frame: &mut [u8],
    x: usize,
    mut y: usize,
    trace: &AnalyzerTraceView,
    scale: usize,
) {
    let total: u64 = trace.owner_cck.iter().sum();
    draw_panel_text(frame, x, y, "Owner cck", PANEL_TEXT_HILIGHT, 1, scale);
    y += 12;
    for (idx, name) in crate::bus::CHIP_BUS_OWNER_NAMES.iter().enumerate() {
        let cck = trace.owner_cck[idx];
        if cck == 0 {
            continue;
        }
        let pct = if total == 0 {
            0.0
        } else {
            cck as f64 * 100.0 / total as f64
        };
        let code = match idx {
            0 => b'R',
            1 => b'B',
            2 => b'S',
            3 => b'D',
            4 => b'A',
            5 => b'C',
            6 => b'L',
            7 => b'P',
            _ => b'.',
        };
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x,
                    y: y + 2,
                    w: 8,
                    h: 8,
                },
                scale,
            ),
            owner_color(code),
            scale,
        );
        draw_panel_text(
            frame,
            x + 14,
            y,
            &format!("{name:<8} {cck:>5} {pct:>4.1}%"),
            PANEL_TEXT,
            1,
            scale,
        );
        y += 12;
    }
    if trace.blitter_busy_cck != 0 {
        y += 4;
        let blit_grant = trace.owner_cck[6];
        let pct = blit_grant as f64 * 100.0 / trace.blitter_busy_cck as f64;
        draw_panel_text(
            frame,
            x,
            y,
            &format!("blitter grant {pct:>4.1}%"),
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
        y += 12;
        let total_starve: u64 = trace.blitter_starve_cck.iter().sum();
        draw_panel_text(
            frame,
            x,
            y,
            &format!("blitter wait {total_starve:>5}"),
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
        y += 12;
        for (idx, name) in crate::bus::CHIP_BUS_OWNER_NAMES.iter().enumerate() {
            let cck = trace.blitter_starve_cck[idx];
            if cck == 0 {
                continue;
            }
            draw_panel_text(
                frame,
                x,
                y,
                &format!("{name:<8} {cck:>5}"),
                PANEL_TEXT_DIM,
                1,
                scale,
            );
            y += 12;
        }
    }
}

/// The picture-underlay and beam-scrub tick boxes on the analyzer's
/// button row.
fn draw_analyzer_checkboxes(
    frame: &mut [u8],
    rect: Rect,
    panel: &FrameAnalyzerPanel,
    hover: Option<UiControl>,
    scale: usize,
) {
    for (control_rect, label, checked, control) in [
        (
            analyzer_underlay_rect(rect),
            ANALYZER_UNDERLAY_LABEL,
            panel.show_underlay || panel.show_scrub,
            UiControl::AnalyzerUnderlay,
        ),
        (
            analyzer_scrub_rect(rect),
            ANALYZER_SCRUB_LABEL,
            panel.show_scrub,
            UiControl::AnalyzerScrub,
        ),
    ] {
        draw_analyzer_checkbox(
            frame,
            control_rect,
            label,
            checked,
            hover == Some(control),
            scale,
        );
    }
}

/// One tick box plus label at `control` on the analyzer's button row.
fn draw_analyzer_checkbox(
    frame: &mut [u8],
    control: Rect,
    label: &str,
    checked: bool,
    hover: bool,
    scale: usize,
) {
    let box_rect = Rect {
        x: control.x,
        y: control.y + (control.h - 12) / 2,
        w: 12,
        h: 12,
    };
    fill_rect(
        frame,
        scale_rect(box_rect, scale),
        if hover { BUTTON_FACE_HOVER } else { ENTRY_BG },
        scale,
    );
    draw_outline(frame, box_rect, BUTTON_EDGE_LIGHT, scale);
    if checked {
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x: box_rect.x + 3,
                    y: box_rect.y + 3,
                    w: 6,
                    h: 6,
                },
                scale,
            ),
            PANEL_TEXT_HILIGHT,
            scale,
        );
    }
    draw_panel_text(
        frame,
        box_rect.x + 18,
        control.y + (control.h - 8) / 2,
        label,
        if hover { BUTTON_TEXT } else { PANEL_TEXT },
        1,
        scale,
    );
}

fn draw_frame_analyzer(
    frame: &mut [u8],
    rect: Rect,
    panel: &FrameAnalyzerPanel,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let status_w = view.status.chars().count() * font::GLYPH_W;
    draw_panel_text(
        frame,
        rect.x + rect.w - TITLE_H - 8 - status_w.min(rect.w.saturating_sub(TITLE_H + 16)),
        rect.y + (TITLE_H - 8) / 2,
        &view.status,
        PANEL_TITLE_TEXT,
        1,
        scale,
    );
    draw_analyzer_tabs(frame, rect, panel.tab, hover, scale);
    // The tab dispatch comes before any "nothing captured yet" message:
    // the memory view is built from the live map, so it has something to
    // show whether or not a beam trace has ever been captured.
    match panel.tab {
        AnalyzerTab::Beam => draw_analyzer_beam_tab(frame, rect, view, hover, scale),
        AnalyzerTab::Memory => draw_analyzer_heat_tab(frame, rect, panel, view, hover, scale),
    }
    // Transport buttons (and the beam tab's checkboxes) are bottom-anchored
    // chrome under whichever tab's content sits above them.
    for (control, button_rect) in analyzer_tab_button_rects(rect, panel.tab) {
        let label = match control {
            UiControl::AnalyzerRun if view.running => "Pause",
            UiControl::AnalyzerRun => "Run",
            UiControl::AnalyzerFrame => "Frame",
            _ => "To slot",
        };
        draw_text_button(
            frame,
            button_rect,
            label,
            true,
            hover == Some(control),
            scale,
        );
    }
    if panel.tab == AnalyzerTab::Beam {
        draw_analyzer_checkboxes(frame, rect, panel, hover, scale);
    }
}

/// The tab row under the title bar, drawn like the debugger's.
fn draw_analyzer_tabs(
    frame: &mut [u8],
    rect: Rect,
    selected: AnalyzerTab,
    hover: Option<UiControl>,
    scale: usize,
) {
    for (index, tab) in ANALYZER_TABS.iter().enumerate() {
        let tab_rect = analyzer_tab_rect(rect, index);
        let active = selected == *tab;
        let hovered = hover == Some(UiControl::AnalyzerTab(*tab));
        let face = if active {
            ENTRY_BG
        } else if hovered {
            BUTTON_FACE_HOVER
        } else {
            BUTTON_FACE
        };
        let scaled = scale_rect(tab_rect, scale);
        fill_rect(frame, scaled, face, scale);
        draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
        let label = analyzer_tab_label(*tab);
        let text_w = label.chars().count() * font::GLYPH_W;
        draw_panel_text(
            frame,
            tab_rect.x + tab_rect.w.saturating_sub(text_w) / 2,
            tab_rect.y + (DEBUG_TAB_H - 8) / 2,
            label,
            if active { ENTRY_TEXT } else { BUTTON_TEXT },
            1,
            scale,
        );
    }
}

fn draw_analyzer_beam_tab(
    frame: &mut [u8],
    rect: Rect,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = analyzer_content_top(rect);
    let Some(trace) = &view.trace else {
        let mut y = content_top + 26;
        for line in [
            "No chip-bus trace captured yet.",
            "Press Frame to record one full Agnus frame, or Run to collect live frames.",
            "The analyzer records hpos/vpos ownership, including overscan and blanking.",
        ] {
            draw_panel_text(frame, rect.x + 24, y, line, PANEL_TEXT, 1, scale);
            y += 16;
        }
        return;
    };

    let header = format!(
        "frame {}  {:.3}s  {} lines x {} cck{}{}",
        trace.frame,
        trace.seconds,
        trace.rows,
        trace.line_cck,
        if trace.cols as u32 != trace.line_cck {
            " sampled"
        } else {
            ""
        },
        if trace.partial { "  partial" } else { "" }
    );
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        &header,
        PANEL_TEXT,
        1,
        scale,
    );
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top + 14,
        "x=hpos colour clocks, y=vpos lines; white=captured display, orange=DIW, cyan=DDF",
        PANEL_TEXT_DIM,
        1,
        scale,
    );

    let raster = analyzer_raster_rect(rect);
    draw_owner_heatmap(
        frame,
        raster,
        trace,
        view.underlay.as_ref(),
        view.scrub,
        scale,
    );
    let counters_x = raster.x + raster.w + 16;
    draw_owner_counters(frame, counters_x, raster.y, trace, scale);

    let mut selected = format!(
        "selected v={:03} h={:03}  owner={} ({})",
        trace.selected_vpos,
        trace.selected_hpos,
        trace.selected_owner,
        trace.selected_owner_code as char
    );
    if let Some(blit) = &trace.selected_blit {
        selected.push_str("  ");
        selected.push_str(blit);
    }
    draw_panel_text(
        frame,
        rect.x + 10,
        raster.y + raster.h + 10,
        &selected,
        PANEL_TEXT_HILIGHT,
        1,
        scale,
    );
    // Register writes near the point of interest: the hovered heatmap
    // slot while the pointer is over the raster, the selected slot
    // otherwise. Nearby means within a heatmap pixel, so markers are
    // inspectable by pointing at them rather than needing an exact
    // colour-clock hit.
    let (probe_vpos, probe_hpos) = match hover {
        Some(UiControl::AnalyzerPick {
            x,
            y,
            scanline: false,
        }) => (
            (usize::from(y) * trace.rows / 1024).min(trace.rows.saturating_sub(1)),
            (usize::from(x) * trace.cols / 1024).min(trace.cols.saturating_sub(1)),
        ),
        _ => (trace.selected_vpos, trace.selected_hpos),
    };
    let mut near = trace
        .markers
        .iter()
        .filter(|marker| marker.near(probe_vpos, probe_hpos));
    let mut marker_text = String::new();
    for marker in near.by_ref().take(2) {
        if !marker_text.is_empty() {
            marker_text.push_str("  |  ");
        }
        marker_text.push_str(&marker.label());
    }
    let extra = near.count();
    if extra > 0 {
        marker_text.push_str(&format!("  (+{extra} more)"));
    }
    if !marker_text.is_empty() {
        draw_panel_text(
            frame,
            rect.x + 10,
            raster.y + raster.h + 22,
            &marker_text,
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
    }

    let scanline = analyzer_scanline_rect(rect);
    draw_panel_text(
        frame,
        scanline.x,
        scanline.y - 14,
        "selected scanline",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_scanline_strip(frame, scanline, trace, scale);

    let mut y = scanline.y + scanline.h + 14;
    draw_panel_text(frame, rect.x + 10, y, "Legend", PANEL_TEXT_DIM, 1, scale);
    let mut x = rect.x + 66;
    for code in *b"RBSDACLP." {
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x,
                    y: y + 2,
                    w: 8,
                    h: 8,
                },
                scale,
            ),
            owner_color(code),
            scale,
        );
        draw_panel_text(
            frame,
            x + 12,
            y,
            owner_name_for_code(code),
            PANEL_TEXT,
            1,
            scale,
        );
        x += if code == b'.' { 54 } else { 82 };
    }
    y += 18;
    let marker_count = format!(
        "register writes marked: {} (hover a slot to inspect)",
        trace.markers.len()
    );
    draw_panel_text(
        frame,
        rect.x + 10,
        y,
        &marker_count,
        PANEL_TEXT_DIM,
        1,
        scale,
    );
}

/// A byte count in the units memory windows come in: powers of two, with
/// one decimal where the figure is not a whole unit ("512", "4K", "1.5M").
fn compact_bytes(bytes: u64) -> String {
    for (unit, suffix) in [(1u64 << 30, 'G'), (1 << 20, 'M'), (1 << 10, 'K')] {
        if bytes >= unit {
            let whole = bytes / unit;
            let tenths = (bytes % unit) * 10 / unit;
            return if tenths == 0 {
                format!("{whole}{suffix}")
            } else {
                format!("{whole}.{tenths}{suffix}")
            };
        }
    }
    format!("{bytes}")
}

/// Re-pack a heat map colour for the presentation texture. The map paints
/// 0xAARRGGBB; the texture takes the red channel in the low byte (see
/// [`rgba`]), so red and blue swap on the way in.
fn heat_rgba(argb: u32) -> u32 {
    rgba((argb >> 16) & 0xFF, (argb >> 8) & 0xFF, argb & 0xFF)
}

/// The address range one grid cell covers, as "$XXXXXX-$YYYYYY".
fn heat_cell_range(base: u32, bytes_per_cell: u32, cell: usize) -> String {
    let start = base.saturating_add((cell as u32).saturating_mul(bytes_per_cell));
    let end = start.saturating_add(bytes_per_cell.saturating_sub(1));
    format!("${start:06X}-${end:06X}")
}

fn draw_analyzer_heat_tab(
    frame: &mut [u8],
    rect: Rect,
    panel: &FrameAnalyzerPanel,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = analyzer_content_top(rect);
    let Some(heat) = &view.heat else {
        // Nothing to paint until the map is recording; the presets stay,
        // because picking a window is how it gets armed.
        draw_panel_text(
            frame,
            rect.x + 10,
            content_top,
            "The heat map is not armed.",
            PANEL_TEXT,
            1,
            scale,
        );
        draw_analyzer_presets(frame, rect, panel, None, hover, scale);
        return;
    };

    let per_cell = compact_bytes(u64::from(heat.bytes_per_cell));
    let last = heat.base.saturating_add(heat.span.saturating_sub(1));
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        &format!(
            "frame {}  window ${:06X}-${:06X}  {} span  {}/cell",
            heat.frame,
            heat.base,
            last,
            compact_bytes(u64::from(heat.span)),
            per_cell,
        ),
        PANEL_TEXT,
        1,
        scale,
    );
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top + 14,
        &format!(
            "one cell per {per_cell} bytes, coloured by what last touched it, \
             fading over {} frames",
            heatmap::DECAY_FRAMES
        ),
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_analyzer_presets(
        frame,
        rect,
        panel,
        Some((heat.base, heat.span)),
        hover,
        scale,
    );

    let map = analyzer_heat_map_rect(rect);
    draw_heat_map(frame, map, &heat.image, scale);
    draw_outline(frame, map, PANEL_TEXT_HILIGHT, scale);
    if let Some(cell) = panel.heat_selected {
        // One cell is under 1.5 px at this scale, so the marker is a 5x5
        // box around it rather than its own footprint.
        let (x, y) = heat_cell_origin(map, cell);
        draw_outline_clipped(
            frame,
            Rect {
                x: x.saturating_sub(2),
                y: y.saturating_sub(2),
                w: 5,
                h: 5,
            },
            map,
            rgba(238, 238, 232),
            scale,
        );
    }
    draw_heat_census(frame, rect, map, &heat.census, scale);

    // The readout describes the hovered cell while the pointer is over
    // the map and the pinned one otherwise. Only the pinned cell can name
    // its toucher: the view carries one record, read from the live map by
    // the view builder, which has no way to know where the pointer is.
    let hovered = match hover {
        Some(UiControl::AnalyzerHeatPick { x, y }) => {
            Some(usize::from(y) * heatmap::GRID + usize::from(x))
        }
        _ => None,
    };
    let readout_y = map.y + map.h + 10;
    let (text, colour, swatch) = match (hovered, panel.heat_selected) {
        (Some(cell), _) => (
            heat_cell_range(heat.base, heat.bytes_per_cell, cell),
            PANEL_TEXT,
            None,
        ),
        (None, Some(cell)) => {
            let range = heat_cell_range(heat.base, heat.bytes_per_cell, cell);
            match heat.selected.as_ref().filter(|sel| sel.cell == cell) {
                Some(sel) => {
                    let mut text = format!("{range}  {}", sel.toucher.unwrap_or("untouched"));
                    if let Some(age) = sel.age_frames {
                        text.push_str(&format!("  age {age}f"));
                    }
                    (text, PANEL_TEXT_HILIGHT, Some(sel.colour))
                }
                None => (format!("{range}  untouched"), PANEL_TEXT, None),
            }
        }
        (None, None) => ("click a cell to inspect".to_string(), PANEL_TEXT_DIM, None),
    };
    let text_x = if let Some(colour) = swatch {
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x: map.x,
                    y: readout_y,
                    w: 8,
                    h: 8,
                },
                scale,
            ),
            heat_rgba(colour),
            scale,
        );
        map.x + 12
    } else {
        map.x
    };
    draw_panel_text(frame, text_x, readout_y, &text, colour, 1, scale);
}

/// The Memory tab's window presets. `window` is the live map's
/// (base, span), so the preset naming it can read as pressed.
fn draw_analyzer_presets(
    frame: &mut [u8],
    rect: Rect,
    panel: &FrameAnalyzerPanel,
    window: Option<(u32, u32)>,
    hover: Option<UiControl>,
    scale: usize,
) {
    // The rect list is a prefix of the presets (any that would not fit are
    // dropped), so zipping pairs each button with its own label.
    for ((control, button), preset) in analyzer_preset_rects(rect, &panel.heat_presets)
        .into_iter()
        .zip(&panel.heat_presets)
    {
        // A preset's span is rounded to whole cells when the map takes it,
        // so compare what it becomes, not what it asks for.
        let active = window == Some((preset.base, heatmap::rounded_span(preset.span)));
        draw_text_button(
            frame,
            button,
            &preset.label,
            true,
            active || hover == Some(control),
            scale,
        );
    }
}

/// Top-left pixel of a grid cell's footprint inside the map rect.
fn heat_cell_origin(map: Rect, cell: usize) -> (usize, usize) {
    let cell = cell.min(heatmap::CELLS - 1);
    (
        map.x + (cell % heatmap::GRID) * map.w / heatmap::GRID,
        map.y + (cell / heatmap::GRID) * map.h / heatmap::GRID,
    )
}

/// Nearest-sample the 256x256 grid into the map rect. The image arrives
/// already faded by age, so this only re-packs the channel order.
fn draw_heat_map(frame: &mut [u8], map: Rect, image: &[u32], scale: usize) {
    for y in 0..map.h {
        let cell_y = y * heatmap::GRID / map.h.max(1);
        for x in 0..map.w {
            let cell_x = x * heatmap::GRID / map.w.max(1);
            let pixel = image
                .get(cell_y * heatmap::GRID + cell_x)
                .copied()
                .unwrap_or(0xFF00_0000);
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: map.x + x,
                        y: map.y + y,
                        w: 1,
                        h: 1,
                    },
                    scale,
                ),
                heat_rgba(pixel),
                scale,
            );
        }
    }
}

/// The census column right of the map: a swatch, the toucher's name, and
/// how much of the window it holds. Touchers with nothing draw dim, so
/// the column reads as the legend too and its rows never move.
fn draw_heat_census(
    frame: &mut [u8],
    rect: Rect,
    map: Rect,
    census: &[AnalyzerHeatCensusRow],
    scale: usize,
) {
    let x = analyzer_heat_census_x(rect);
    draw_panel_text(frame, x, map.y, "Touchers", PANEL_TEXT_DIM, 1, scale);
    for (index, row) in census.iter().enumerate() {
        let y = map.y + 16 + index * 14;
        fill_rect(
            frame,
            scale_rect(Rect { x, y, w: 8, h: 8 }, scale),
            heat_rgba(row.colour),
            scale,
        );
        draw_panel_text(
            frame,
            x + 12,
            y,
            &format!(
                "{:<9}{:>5} cells  {}",
                row.name,
                row.cells,
                compact_bytes(row.bytes)
            ),
            if row.cells == 0 {
                PANEL_TEXT_DIM
            } else {
                PANEL_TEXT
            },
            1,
            scale,
        );
    }
}

// ---------------------------------------------------------------------------
// Machine-configuration (launcher) panel
// ---------------------------------------------------------------------------

const LAUNCHER_W: usize = 700;
const LAUNCHER_H: usize = 520;
const LAUNCH_MARGIN: usize = 8;
const LAUNCH_MODEL_H: usize = 22;
const LAUNCH_MODEL_GAP: usize = 4;
/// Machines per row in the selector grid before it wraps; the grid rebalances
/// so the buttons fill the width (eight models fit one row today, more wrap to
/// two balanced rows -- room for the A3000/A4000 and beyond).
const LAUNCH_MODEL_MAX_PER_ROW: usize = 8;
/// Width of the left-hand vertical category-tab column.
const LAUNCH_SIDEBAR_W: usize = 116;
const LAUNCH_TAB_H: usize = 26;
const LAUNCH_TAB_GAP: usize = 2;
const LAUNCH_ROW_H: usize = 26;
/// Label column width inside the settings pane (before a row's control).
const LAUNCH_LABEL_W: usize = 150;
const LAUNCH_ARROW_W: usize = 24;
const LAUNCH_VALUE_W: usize = 132;
const LAUNCH_TOGGLE_W: usize = 64;
const LAUNCH_ACTION_W: usize = 84;
const LAUNCH_ACTION_H: usize = 22;
const LAUNCH_BROWSE_W: usize = 66;
const LAUNCH_CLEAR_W: usize = 54;
/// Width of the path-preview text column before a path row's Browse/Clear
/// buttons. The buttons sit just after it (near the other control widgets)
/// rather than out at the panel's right edge; a long value is clipped to fit.
const LAUNCH_PATH_VALUE_W: usize = 216;
/// Width of the editable volume-name box on a drive row.
const LAUNCH_NAME_W: usize = 96;
const LAUNCH_REMOVE_W: usize = 70;
const LAUNCH_CONTROL_H: usize = 20;

fn launcher_model_top(rect: Rect) -> usize {
    rect.y + TITLE_H + 8
}

/// (rows, columns) of the machine-selector grid, balanced so the buttons fill
/// the width evenly however many models there are.
fn launcher_model_grid() -> (usize, usize) {
    let count = launcher::MODELS.len();
    let rows = count.div_ceil(LAUNCH_MODEL_MAX_PER_ROW).max(1);
    (rows, count.div_ceil(rows))
}

fn launcher_model_rect(rect: Rect, i: usize) -> Rect {
    let (_, per_row) = launcher_model_grid();
    let avail = rect.w - 2 * LAUNCH_MARGIN;
    let w = (avail - (per_row - 1) * LAUNCH_MODEL_GAP) / per_row;
    let (row, col) = (i / per_row, i % per_row);
    Rect {
        x: rect.x + LAUNCH_MARGIN + col * (w + LAUNCH_MODEL_GAP),
        y: launcher_model_top(rect) + row * (LAUNCH_MODEL_H + LAUNCH_MODEL_GAP),
        w,
        h: LAUNCH_MODEL_H,
    }
}

fn launcher_model_strip_height() -> usize {
    let (rows, _) = launcher_model_grid();
    rows * (LAUNCH_MODEL_H + LAUNCH_MODEL_GAP)
}

/// Top of the configuration area (the vertical tab column and the settings
/// pane both start here), below the machine grid and its divider.
fn launcher_content_top(rect: Rect) -> usize {
    launcher_model_top(rect) + launcher_model_strip_height() + 12
}

/// A category tab in the left sidebar.
fn launcher_tab_rect(rect: Rect, i: usize) -> Rect {
    Rect {
        x: rect.x + LAUNCH_MARGIN,
        y: launcher_content_top(rect) + i * (LAUNCH_TAB_H + LAUNCH_TAB_GAP),
        w: LAUNCH_SIDEBAR_W,
        h: LAUNCH_TAB_H,
    }
}

/// Left edge of the settings pane (right of the tab column).
fn launcher_pane_x(rect: Rect) -> usize {
    rect.x + LAUNCH_MARGIN + LAUNCH_SIDEBAR_W + 12
}

/// X of a settings row's control column (after its label).
fn launcher_control_x(rect: Rect) -> usize {
    launcher_pane_x(rect) + LAUNCH_LABEL_W
}

fn launcher_row_y(rect: Rect, i: usize) -> usize {
    launcher_content_top(rect) + i * LAUNCH_ROW_H
}

fn launcher_action_y(rect: Rect) -> usize {
    rect.y + rect.h - LAUNCH_ACTION_H - 8
}

fn launcher_status_y(rect: Rect) -> usize {
    launcher_action_y(rect).saturating_sub(16)
}

/// (prev arrow, value field, next arrow) for a cycle row.
fn launcher_cycle_rects(rect: Rect, row_y: usize) -> (Rect, Rect, Rect) {
    let y = row_y + 2;
    let cx = launcher_control_x(rect);
    let prev = Rect {
        x: cx,
        y,
        w: LAUNCH_ARROW_W,
        h: LAUNCH_CONTROL_H,
    };
    let value = Rect {
        x: prev.x + LAUNCH_ARROW_W,
        y,
        w: LAUNCH_VALUE_W,
        h: LAUNCH_CONTROL_H,
    };
    let next = Rect {
        x: value.x + LAUNCH_VALUE_W,
        y,
        w: LAUNCH_ARROW_W,
        h: LAUNCH_CONTROL_H,
    };
    (prev, value, next)
}

fn launcher_toggle_rect(rect: Rect, row_y: usize) -> Rect {
    Rect {
        x: launcher_control_x(rect),
        y: row_y + 2,
        w: LAUNCH_TOGGLE_W,
        h: LAUNCH_CONTROL_H,
    }
}

/// A sub-page navigation button (the `slot`-th one) on the top nav row: a page's
/// sibling links, or a sub-page's Back button.
/// Sized to match the left-hand category tabs.
fn launcher_nav_button_rect(rect: Rect, slot: usize) -> Rect {
    Rect {
        x: launcher_pane_x(rect) + slot * (LAUNCH_SIDEBAR_W + 8),
        y: launcher_nav_y(rect),
        w: LAUNCH_SIDEBAR_W,
        h: LAUNCH_TAB_H,
    }
}

/// A sub-page's Back button, on the nav row.
fn launcher_back_button_rect(rect: Rect) -> Rect {
    launcher_nav_button_rect(rect, 0)
}

/// Y of the nav row (the sibling-page buttons and any Back button) at the top of
/// the settings pane, in line with the first category tab. The setting rows
/// below it are shifted down by [`LAUNCH_NAV_BLOCK_H`] to make room.
fn launcher_nav_y(rect: Rect) -> usize {
    launcher_content_top(rect)
}

/// Vertical space reserved at the top of the pane for the nav button row plus a
/// gap below it, before the settings begin, on tabs that have a nav.
const LAUNCH_NAV_BLOCK_H: usize = LAUNCH_TAB_H + 14;

/// The Status column's clickable area (the "Bootable" label plus its tick box),
/// sitting to the right of the priority stepper on a Boot Priority row.
fn launcher_bootable_rect(rect: Rect, row_y: usize) -> Rect {
    let (_, _, next) = launcher_cycle_rects(rect, row_y);
    Rect {
        x: next.x + next.w + 24,
        y: row_y + 2,
        w: BOOTABLE_LABEL.len() * font::GLYPH_W + 8 + 12,
        h: LAUNCH_CONTROL_H,
    }
}

/// The tick box within a Bootable cell, after its label.
fn launcher_bootable_box(cell: Rect) -> Rect {
    Rect {
        x: cell.x + BOOTABLE_LABEL.len() * font::GLYPH_W + 8,
        y: cell.y + (cell.h.saturating_sub(12)) / 2,
        w: 12,
        h: 12,
    }
}

const BOOTABLE_LABEL: &str = "Bootable";

/// (Browse, Clear) buttons for a path row, just after the fixed-width value
/// column ([`LAUNCH_PATH_VALUE_W`]) rather than out at the panel's right edge.
fn launcher_path_rects(rect: Rect, row_y: usize) -> (Rect, Rect) {
    let y = row_y + 2;
    let browse = Rect {
        x: launcher_control_x(rect) + LAUNCH_PATH_VALUE_W,
        y,
        w: LAUNCH_BROWSE_W,
        h: LAUNCH_CONTROL_H,
    };
    let clear = Rect {
        x: browse.x + LAUNCH_BROWSE_W + 4,
        y,
        w: LAUNCH_CLEAR_W,
        h: LAUNCH_CONTROL_H,
    };
    (browse, clear)
}

/// The editable volume-name box on a drive row: it sits just left of the
/// Browse button, with the path text filling the space before it.
fn launcher_drive_name_rect(rect: Rect, row_y: usize) -> Rect {
    let (browse, _clear) = launcher_path_rects(rect, row_y);
    Rect {
        x: browse.x.saturating_sub(6 + LAUNCH_NAME_W),
        y: browse.y,
        w: LAUNCH_NAME_W,
        h: LAUNCH_CONTROL_H,
    }
}

fn launcher_action_rects(rect: Rect) -> [(UiControl, Rect); 4] {
    let y = launcher_action_y(rect);
    let load = Rect {
        x: rect.x + LAUNCH_MARGIN,
        y,
        w: LAUNCH_ACTION_W,
        h: LAUNCH_ACTION_H,
    };
    let save = Rect {
        x: load.x + LAUNCH_ACTION_W + 6,
        y,
        w: LAUNCH_ACTION_W,
        h: LAUNCH_ACTION_H,
    };
    let run = Rect {
        x: rect.x + rect.w - LAUNCH_MARGIN - LAUNCH_ACTION_W,
        y,
        w: LAUNCH_ACTION_W,
        h: LAUNCH_ACTION_H,
    };
    let defaults = Rect {
        x: run.x - 6 - LAUNCH_ACTION_W,
        y,
        w: LAUNCH_ACTION_W,
        h: LAUNCH_ACTION_H,
    };
    [
        (UiControl::LauncherLoad, load),
        (UiControl::LauncherSave, save),
        (UiControl::LauncherDefaults, defaults),
        (UiControl::LauncherRun, run),
    ]
}

/// One drawable/clickable item in the Zorro tab. The flat layout list keeps
/// drawing and hit-testing in exact sync (immediate-mode UI).
#[derive(Clone, Copy)]
enum ZorroItem {
    Header(usize),
    Option { board: usize, opt: usize },
}

/// Flatten the Zorro boards into (content-row, item) pairs. Row 0 is the Add
/// button, pinned to the top; each board header and its option rows follow.
fn launcher_zorro_layout(setup: &launcher::MachineSetup) -> Vec<(usize, ZorroItem)> {
    let mut items = Vec::new();
    // Row 0 is the first list row; the board list is shifted below the Add button
    // by LAUNCH_NAV_BLOCK_H at draw/hit-test time.
    let mut row = 0;
    for (i, board) in setup.zorro_boards().iter().enumerate() {
        items.push((row, ZorroItem::Header(i)));
        row += 1;
        for opt in 0..board.options().len() {
            items.push((row, ZorroItem::Option { board: i, opt }));
            row += 1;
        }
    }
    items
}

/// The Remove button for a board header drawn at content `row`.
fn launcher_zorro_remove_rect(rect: Rect, row: usize) -> Rect {
    Rect {
        x: rect.x + rect.w - LAUNCH_MARGIN - LAUNCH_REMOVE_W,
        y: launcher_row_y(rect, row) + LAUNCH_NAV_BLOCK_H + 2,
        w: LAUNCH_REMOVE_W,
        h: LAUNCH_CONTROL_H,
    }
}

/// The clickable value box for a string option at `row_y` (control column to
/// the right margin).
fn launcher_board_value_rect(rect: Rect, row_y: usize) -> Rect {
    let x = launcher_control_x(rect);
    let right = rect.x + rect.w - LAUNCH_MARGIN;
    Rect {
        x,
        y: row_y + 2,
        w: right.saturating_sub(x),
        h: LAUNCH_CONTROL_H,
    }
}

/// The "Add board..." button: a nav-style button at the top of the pane, the
/// same size and position as the sibling-page buttons on other tabs, with the
/// board list below it after the same gap.
fn launcher_zorro_add_rect(rect: Rect) -> Rect {
    launcher_nav_button_rect(rect, 0)
}

fn launcher_action_label(control: UiControl) -> &'static str {
    match control {
        UiControl::LauncherLoad => "Load...",
        UiControl::LauncherSave => "Save...",
        UiControl::LauncherDefaults => "Defaults",
        UiControl::LauncherRun => "Run",
        _ => "",
    }
}

/// Hit-test the configuration panel. Returns the control under `pos`, or `None`
/// to let the caller swallow the click on the panel body.
fn launcher_control_at(rect: Rect, state: &LauncherState, pos: (i32, i32)) -> Option<UiControl> {
    for (i, &model) in launcher::MODELS.iter().enumerate() {
        if launcher_model_rect(rect, i).contains(pos) {
            return Some(UiControl::LauncherModel(model));
        }
    }
    for (i, &tab) in launcher::TABS.iter().enumerate() {
        if launcher_tab_rect(rect, i).contains(pos) {
            return Some(UiControl::LauncherTab(tab));
        }
    }
    if state.tab == LauncherTab::Zorro {
        use crate::zorro::ConfigOptionKind as K;
        for (row, item) in launcher_zorro_layout(&state.setup) {
            let row_y = launcher_row_y(rect, row) + LAUNCH_NAV_BLOCK_H;
            match item {
                ZorroItem::Header(i) => {
                    if launcher_zorro_remove_rect(rect, row).contains(pos) {
                        return Some(UiControl::LauncherZorroRemove(i));
                    }
                }
                ZorroItem::Option { board, opt } => {
                    match &state.setup.zorro_boards()[board].options()[opt].kind {
                        K::Bool => {
                            if launcher_toggle_rect(rect, row_y).contains(pos) {
                                return Some(UiControl::LauncherBoardToggle { board, opt });
                            }
                        }
                        K::Enum(_) | K::Int => {
                            let (prev, _v, next) = launcher_cycle_rects(rect, row_y);
                            if prev.contains(pos) {
                                return Some(UiControl::LauncherBoardCycle {
                                    board,
                                    opt,
                                    forward: false,
                                });
                            }
                            if next.contains(pos) {
                                return Some(UiControl::LauncherBoardCycle {
                                    board,
                                    opt,
                                    forward: true,
                                });
                            }
                        }
                        K::File => {
                            let (browse, clear) = launcher_path_rects(rect, row_y);
                            if browse.contains(pos) {
                                return Some(UiControl::LauncherBoardBrowse { board, opt });
                            }
                            if clear.contains(pos) {
                                return Some(UiControl::LauncherBoardClear { board, opt });
                            }
                        }
                        K::String => {
                            if launcher_board_value_rect(rect, row_y).contains(pos) {
                                return Some(UiControl::LauncherBoardEdit { board, opt });
                            }
                        }
                    }
                }
            }
        }
        if launcher_zorro_add_rect(rect).contains(pos) {
            return Some(UiControl::LauncherZorroAdd);
        }
    } else {
        let row_offset = if state.tab.has_top_nav() {
            LAUNCH_NAV_BLOCK_H
        } else {
            0
        };
        for (i, r) in launcher::rows(
            state.tab,
            state.setup.parallel_device(),
            state.setup.serial_mode(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .enumerate()
        {
            if !state.setup.applies(r.field) {
                continue;
            }
            let row_y = launcher_row_y(rect, i) + row_offset;
            match r.kind {
                // Non-interactive rows.
                RowKind::SectionHeader | RowKind::BootpriHeader => {}
                RowKind::Cycle => {
                    let (prev, _value, next) = launcher_cycle_rects(rect, row_y);
                    if prev.contains(pos) {
                        return Some(UiControl::LauncherCycle {
                            field: r.field,
                            forward: false,
                        });
                    }
                    if next.contains(pos) {
                        return Some(UiControl::LauncherCycle {
                            field: r.field,
                            forward: true,
                        });
                    }
                }
                RowKind::Bootpri => {
                    // No-drive / CD-image rows are skipped by the `applies` guard
                    // above, so this only runs for a drive with an image. The
                    // Bootable box is always live; the priority stepper/field is
                    // inert while the box is cleared (the priority shows greyed).
                    if launcher_bootable_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherDriveBootToggle(r.field));
                    }
                    if state.setup.drive_boot_off(r.field) {
                        continue;
                    }
                    let (prev, value, next) = launcher_cycle_rects(rect, row_y);
                    if prev.contains(pos) {
                        return Some(UiControl::LauncherCycle {
                            field: r.field,
                            forward: false,
                        });
                    }
                    if next.contains(pos) {
                        return Some(UiControl::LauncherCycle {
                            field: r.field,
                            forward: true,
                        });
                    }
                    if value.contains(pos) {
                        return Some(UiControl::LauncherDriveBootpriEdit(r.field));
                    }
                }
                RowKind::Toggle => {
                    if launcher_toggle_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherToggle(r.field));
                    }
                }
                RowKind::Path => {
                    let (browse, clear) = launcher_path_rects(rect, row_y);
                    if browse.contains(pos) {
                        return Some(UiControl::LauncherBrowse(r.field));
                    }
                    if clear.contains(pos) {
                        return Some(UiControl::LauncherClear(r.field));
                    }
                }
                RowKind::Drive => {
                    let (browse, clear) = launcher_path_rects(rect, row_y);
                    if browse.contains(pos) {
                        return Some(UiControl::LauncherBrowse(r.field));
                    }
                    if clear.contains(pos) {
                        return Some(UiControl::LauncherClear(r.field));
                    }
                    // The volume name only matters once an image is chosen
                    // (and never for a CD image).
                    if state.setup.path(r.field).is_some()
                        && state.setup.drive_name_applies(r.field)
                        && launcher_drive_name_rect(rect, row_y).contains(pos)
                    {
                        return Some(UiControl::LauncherDriveNameEdit(r.field));
                    }
                }
            }
        }
    }
    // The top nav row: a page's "Options:"/"Settings:" sibling links, or a Back
    // button.
    if let Some(parent) = state.tab.parent_tab() {
        if launcher_back_button_rect(rect).contains(pos) {
            return Some(UiControl::LauncherTab(parent));
        }
    } else {
        for (slot, &(_, target)) in state.tab.nav_options().iter().enumerate() {
            if launcher_nav_button_rect(rect, slot).contains(pos) {
                return Some(UiControl::LauncherTab(target));
            }
        }
    }
    for (control, button_rect) in launcher_action_rects(rect) {
        if button_rect.contains(pos) {
            return Some(control);
        }
    }
    None
}

/// Truncate `text` (already a short file name) to fit `avail_px`, appending a
/// `~` marker when clipped.
fn truncate_to_width(text: &str, avail_px: usize) -> String {
    let max_chars = avail_px / font::GLYPH_W;
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return String::new();
    }
    let kept: String = text.chars().take(max_chars - 1).collect();
    format!("{kept}~")
}

/// Clip a path to `avail_px`, keeping the TAIL and prefixing an ASCII "..."
/// when it does not fit -- for a host directory the meaningful end (the leaf
/// dir) stays visible. The bitmap font is ASCII-only, so a real ellipsis
/// glyph cannot be drawn; "..." is the closest it can render. Mirrors
/// [`truncate_to_width`], which keeps the head instead.
fn clip_path_tail(text: &str, avail_px: usize) -> String {
    let max_chars = avail_px / font::GLYPH_W;
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let tail: String = text.chars().skip(len - (max_chars - 3)).collect();
    format!("...{tail}")
}

/// Clip a host path to `avail_px`, always keeping the final component (the file
/// name) whole: leading directories are dropped and replaced by a "..." prefix,
/// rather than cutting into the name. Splits on both `/` and `\` so Windows and
/// Unix paths work. If even the name alone is too wide, its tail is shown.
fn clip_path_keep_name(text: &str, avail_px: usize) -> String {
    clip_path_to_chars(text, avail_px / font::GLYPH_W)
}

/// [`clip_path_keep_name`] in characters rather than pixels, shared with the
/// status line (see `window::shorten_status_paths`).
pub(super) fn clip_path_to_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut comps: Vec<&str> = text.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let name = comps.pop().unwrap_or(text);
    let sep = if text.contains('\\') { '\\' } else { '/' };
    // Grow from the name, prepending whole parent components while the result
    // (with its "..." prefix) still fits.
    let mut shown = name.to_string();
    for comp in comps.into_iter().rev() {
        let candidate = format!("{comp}{sep}{shown}");
        if 3 + 1 + candidate.chars().count() <= max_chars {
            shown = candidate;
        } else {
            break;
        }
    }
    let prefixed = format!("...{sep}{shown}");
    if prefixed.chars().count() <= max_chars {
        prefixed
    } else {
        // The file name alone does not fit; fall back to a plain tail clip.
        clip_path_tail(name, max_chars * font::GLYPH_W)
    }
}

/// A model-selector / tab button: a flat bevel that fills with the title-bar
/// blue when active/selected. Tabs label left, model buttons centred.
fn draw_launcher_chip(
    frame: &mut [u8],
    rect: Rect,
    label: &str,
    active: bool,
    hover: bool,
    align_left: bool,
    scale: usize,
) {
    let face = if active {
        PANEL_TITLE_BG
    } else if hover {
        BUTTON_FACE_HOVER
    } else {
        BUTTON_FACE
    };
    let scaled = scale_rect(rect, scale);
    fill_rect(frame, scaled, face, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
    let color = if active {
        PANEL_TITLE_TEXT
    } else {
        BUTTON_TEXT
    };
    let text_w = label.chars().count() * font::GLYPH_W;
    let x = if align_left {
        rect.x + 8
    } else {
        rect.x + rect.w.saturating_sub(text_w) / 2
    };
    let y = rect.y + rect.h.saturating_sub(font::GLYPH_H) / 2;
    draw_panel_text(frame, x, y, label, color, 1, scale);
}

fn draw_launcher_row(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    r: &launcher::Row,
    i: usize,
    y_offset: usize,
    hover: Option<UiControl>,
    scale: usize,
) {
    let setup = &state.setup;
    let row_y = launcher_row_y(rect, i) + y_offset;
    // A section heading is a greyed, non-interactive label grouping the rows
    // below it (the Serial:/Parallel: sections of the I/O Ports tab).
    if r.kind == RowKind::SectionHeader {
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            row_y + 8,
            r.label,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        return;
    }
    // The greyed column titles above the Boot Priority rows.
    if r.kind == RowKind::BootpriHeader {
        for (x, title) in [
            (launcher_pane_x(rect), "Drive"),
            (launcher_control_x(rect), "Priority"),
            (launcher_bootable_rect(rect, row_y).x, "Status"),
        ] {
            draw_panel_text(frame, x, row_y + 8, title, PANEL_TEXT_DIM, 1, scale);
        }
        return;
    }
    let reason = setup.disabled_reason(r.field);
    let label_color = if reason.is_none() {
        PANEL_TEXT
    } else {
        PANEL_TEXT_DIM
    };
    draw_panel_text(
        frame,
        launcher_pane_x(rect),
        row_y + 8,
        r.label,
        label_color,
        1,
        scale,
    );
    // Greyed: explain why instead of drawing controls (e.g. "needs 32-bit CPU").
    // The shaping rows are the exception -- channel mode, separation and mouse
    // sensitivity are merely inapplicable (audio disabled, separation in mono,
    // or no mouse in either port), so the greyed label alone says enough and
    // column 2 is left blank.
    let blank_when_greyed = matches!(
        r.field,
        LauncherField::AudioChannelMode
            | LauncherField::AudioStereoSeparation
            | LauncherField::MouseSensitivity
            | LauncherField::MouseCapture
            | LauncherField::ShaderStrength
    );
    if let Some(reason) = reason {
        if !blank_when_greyed {
            draw_panel_text(
                frame,
                launcher_control_x(rect),
                row_y + 8,
                reason,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
        return;
    }
    match r.kind {
        // Drawn above with an early return.
        RowKind::SectionHeader | RowKind::BootpriHeader => {}
        RowKind::Cycle => {
            let (prev, value, next) = launcher_cycle_rects(rect, row_y);
            draw_text_button(
                frame,
                prev,
                "<",
                true,
                hover
                    == Some(UiControl::LauncherCycle {
                        field: r.field,
                        forward: false,
                    }),
                scale,
            );
            draw_text_button(
                frame,
                next,
                ">",
                true,
                hover
                    == Some(UiControl::LauncherCycle {
                        field: r.field,
                        forward: true,
                    }),
                scale,
            );
            // Clip a long value (e.g. a wordy MIDI device name) to the box so
            // it cannot spill over the ">" stepper.
            let text = truncate_to_width(&setup.value_label(r.field), value.w);
            let tw = text.chars().count() * font::GLYPH_W;
            let tx = value.x + value.w.saturating_sub(tw) / 2;
            draw_panel_text(frame, tx, value.y + 6, &text, PANEL_TEXT_HILIGHT, 1, scale);
        }
        RowKind::Bootpri => {
            // Priority column: a `< value >` stepper whose value is also a text
            // field. Greyed and inert while the Bootable box (drawn last) is
            // cleared -- the number stays visible so re-ticking restores it.
            let disabled = setup.drive_boot_off(r.field);
            let (prev, value, next) = launcher_cycle_rects(rect, row_y);
            draw_text_button(
                frame,
                prev,
                "<",
                !disabled,
                hover
                    == Some(UiControl::LauncherCycle {
                        field: r.field,
                        forward: false,
                    }),
                scale,
            );
            draw_text_button(
                frame,
                next,
                ">",
                !disabled,
                hover
                    == Some(UiControl::LauncherCycle {
                        field: r.field,
                        forward: true,
                    }),
                scale,
            );
            draw_rect_bevel(
                frame,
                scale_rect(value, scale),
                BUTTON_EDGE_DARK,
                BUTTON_EDGE_LIGHT,
                scale,
            );
            let editing = state.editing() == Some(EditTarget::DriveBootpri(r.field));
            let text = if editing {
                format!("{}_", state.edit_buffer())
            } else {
                setup.value_label(r.field)
            };
            let text = truncate_to_width(&text, value.w.saturating_sub(8));
            let tw = text.chars().count() * font::GLYPH_W;
            let tx = value.x + value.w.saturating_sub(tw) / 2;
            let color = if disabled {
                PANEL_TEXT_DIM
            } else if editing {
                PANEL_TEXT_HILIGHT
            } else {
                PANEL_TEXT
            };
            draw_panel_text(frame, tx, value.y + 6, &text, color, 1, scale);
            // Status column: the "Bootable" label then a tick box, ticked when
            // the drive is bootable.
            let cell = launcher_bootable_rect(rect, row_y);
            draw_panel_text(
                frame,
                cell.x,
                cell.y + 6,
                BOOTABLE_LABEL,
                PANEL_TEXT,
                1,
                scale,
            );
            let box_rect = launcher_bootable_box(cell);
            let hovered = hover == Some(UiControl::LauncherDriveBootToggle(r.field));
            fill_rect(
                frame,
                scale_rect(box_rect, scale),
                if hovered { BUTTON_FACE_HOVER } else { ENTRY_BG },
                scale,
            );
            draw_outline(frame, box_rect, BUTTON_EDGE_LIGHT, scale);
            if !disabled {
                fill_rect(
                    frame,
                    scale_rect(
                        Rect {
                            x: box_rect.x + 3,
                            y: box_rect.y + 3,
                            w: 6,
                            h: 6,
                        },
                        scale,
                    ),
                    PANEL_TEXT_HILIGHT,
                    scale,
                );
            }
        }
        RowKind::Toggle => {
            let button = launcher_toggle_rect(rect, row_y);
            let label = if setup.toggle_value(r.field) {
                "On"
            } else {
                "Off"
            };
            draw_text_button(
                frame,
                button,
                label,
                true,
                hover == Some(UiControl::LauncherToggle(r.field)),
                scale,
            );
        }
        RowKind::Path => {
            let (browse, clear) = launcher_path_rects(rect, row_y);
            let value_x = launcher_control_x(rect);
            let avail = browse.x.saturating_sub(value_x + 8);
            // The printer output shows its full path (clipped to keep the file
            // name if long, so the row never overflows), "(none)" until one is
            // chosen; other path rows show the image file name.
            let text = if r.field == LauncherField::ParallelOutput {
                match setup.path(r.field) {
                    Some(p) => clip_path_keep_name(&p.to_string_lossy(), avail),
                    None => "(none)".to_string(),
                }
            } else {
                truncate_to_width(&setup.value_label(r.field), avail)
            };
            draw_panel_text(frame, value_x, browse.y + 6, &text, PANEL_TEXT, 1, scale);
            draw_text_button(
                frame,
                browse,
                "Browse",
                true,
                hover == Some(UiControl::LauncherBrowse(r.field)),
                scale,
            );
            draw_text_button(
                frame,
                clear,
                "Clear",
                true,
                hover == Some(UiControl::LauncherClear(r.field)),
                scale,
            );
        }
        RowKind::Drive => {
            let (browse, clear) = launcher_path_rects(rect, row_y);
            let value_x = launcher_control_x(rect);
            // The volume-name box only appears once an image is chosen (a name
            // has nothing to label otherwise, and never labels a CD image);
            // until then the row reads like a plain path row and the path text
            // fills the full width.
            let has_image = setup.path(r.field).is_some() && setup.drive_name_applies(r.field);
            let name_box = launcher_drive_name_rect(rect, row_y);
            let text_right = if has_image { name_box.x } else { browse.x };
            let avail = text_right.saturating_sub(value_x + 8);
            // Host FS mounts show the whole host path (clipped to keep the final
            // directory name, with a leading "..." when long), since the path is
            // meaningful; other drives show the image's file name.
            let text = match (r.field.is_filesys_dir_field(), setup.path(r.field)) {
                (true, Some(p)) => clip_path_keep_name(&p.to_string_lossy(), avail),
                _ => truncate_to_width(&setup.value_label(r.field), avail),
            };
            draw_panel_text(frame, value_x, browse.y + 6, &text, PANEL_TEXT, 1, scale);
            if has_image {
                draw_rect_bevel(
                    frame,
                    scale_rect(name_box, scale),
                    BUTTON_EDGE_DARK,
                    BUTTON_EDGE_LIGHT,
                    scale,
                );
                let editing = state.editing() == Some(EditTarget::DriveName(r.field));
                let (label, color) = if editing {
                    (format!("{}_", state.edit_buffer()), PANEL_TEXT_HILIGHT)
                } else if let Some(name) = setup.drive_name(r.field) {
                    (name.to_string(), PANEL_TEXT)
                } else {
                    ("(volume)".to_string(), PANEL_TEXT_DIM)
                };
                let shown = truncate_to_width(&label, name_box.w.saturating_sub(8));
                draw_panel_text(
                    frame,
                    name_box.x + 4,
                    name_box.y + 6,
                    &shown,
                    color,
                    1,
                    scale,
                );
            }
            draw_text_button(
                frame,
                browse,
                "Browse",
                true,
                hover == Some(UiControl::LauncherBrowse(r.field)),
                scale,
            );
            draw_text_button(
                frame,
                clear,
                "Clear",
                true,
                hover == Some(UiControl::LauncherClear(r.field)),
                scale,
            );
        }
    }
}

fn draw_launcher_zorro(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let setup = &state.setup;
    let pane_x = launcher_pane_x(rect);
    // Add button pinned to the top of the pane; the board list (or the empty
    // note) sits below it.
    draw_text_button(
        frame,
        launcher_zorro_add_rect(rect),
        "Add board...",
        true,
        hover == Some(UiControl::LauncherZorroAdd),
        scale,
    );
    if setup.zorro_boards().is_empty() {
        draw_panel_text(
            frame,
            pane_x,
            launcher_row_y(rect, 0) + LAUNCH_NAV_BLOCK_H + 8,
            "No extra Zorro boards configured.",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    for (row, item) in launcher_zorro_layout(setup) {
        let row_y = launcher_row_y(rect, row) + LAUNCH_NAV_BLOCK_H;
        match item {
            ZorroItem::Header(i) => {
                let board = &setup.zorro_boards()[i];
                let remove = launcher_zorro_remove_rect(rect, row);
                let name = truncate_to_width(&board.name(), remove.x.saturating_sub(pane_x + 8));
                draw_panel_text(frame, pane_x, row_y + 8, &name, PANEL_TEXT, 1, scale);
                draw_text_button(
                    frame,
                    remove,
                    "Remove",
                    true,
                    hover == Some(UiControl::LauncherZorroRemove(i)),
                    scale,
                );
            }
            ZorroItem::Option { board, opt } => {
                draw_launcher_board_option(frame, rect, state, board, opt, row_y, hover, scale);
            }
        }
    }
}

/// Draw one plugin config-option row (indented under its board): a label plus
/// the widget its kind calls for.
fn draw_launcher_board_option(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    board: usize,
    opt: usize,
    row_y: usize,
    hover: Option<UiControl>,
    scale: usize,
) {
    use crate::zorro::ConfigOptionKind as K;
    let setup = &state.setup;
    let option = &setup.zorro_boards()[board].options()[opt];
    // Indented label.
    let label_x = launcher_pane_x(rect) + 12;
    let label = truncate_to_width(
        &option.label,
        launcher_control_x(rect).saturating_sub(label_x + 6),
    );
    draw_panel_text(frame, label_x, row_y + 8, &label, PANEL_TEXT, 1, scale);

    let value = setup.zorro_boards()[board].value(opt);
    match &option.kind {
        K::Bool => {
            let on = value.trim().eq_ignore_ascii_case("true");
            draw_text_button(
                frame,
                launcher_toggle_rect(rect, row_y),
                if on { "On" } else { "Off" },
                true,
                hover == Some(UiControl::LauncherBoardToggle { board, opt }),
                scale,
            );
        }
        K::Enum(_) | K::Int => {
            let (prev, val, next) = launcher_cycle_rects(rect, row_y);
            draw_text_button(
                frame,
                prev,
                "<",
                true,
                hover
                    == Some(UiControl::LauncherBoardCycle {
                        board,
                        opt,
                        forward: false,
                    }),
                scale,
            );
            let shown = truncate_to_width(&value, val.w.saturating_sub(8));
            draw_panel_text(
                frame,
                val.x + 6,
                row_y + 8,
                &shown,
                PANEL_TEXT_HILIGHT,
                1,
                scale,
            );
            draw_text_button(
                frame,
                next,
                ">",
                true,
                hover
                    == Some(UiControl::LauncherBoardCycle {
                        board,
                        opt,
                        forward: true,
                    }),
                scale,
            );
        }
        K::File => {
            let (browse, clear) = launcher_path_rects(rect, row_y);
            let shown = if value.is_empty() {
                "(none)".to_string()
            } else {
                std::path::Path::new(&value)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(value.clone())
            };
            let avail = browse.x.saturating_sub(launcher_control_x(rect) + 6);
            let shown = truncate_to_width(&shown, avail);
            draw_panel_text(
                frame,
                launcher_control_x(rect),
                row_y + 8,
                &shown,
                PANEL_TEXT,
                1,
                scale,
            );
            draw_text_button(
                frame,
                browse,
                "Browse",
                true,
                hover == Some(UiControl::LauncherBoardBrowse { board, opt }),
                scale,
            );
            draw_text_button(
                frame,
                clear,
                "Clear",
                true,
                hover == Some(UiControl::LauncherBoardClear { board, opt }),
                scale,
            );
        }
        K::String => {
            let editing = state.editing() == Some(EditTarget::BoardOption { board, opt });
            let vbox = launcher_board_value_rect(rect, row_y);
            draw_rect_bevel(
                frame,
                scale_rect(vbox, scale),
                BUTTON_EDGE_DARK,
                BUTTON_EDGE_LIGHT,
                scale,
            );
            let text = if editing {
                format!("{}_", state.edit_buffer())
            } else {
                value.clone()
            };
            let color = if editing {
                PANEL_TEXT_HILIGHT
            } else {
                PANEL_TEXT
            };
            let shown = truncate_to_width(&text, vbox.w.saturating_sub(8));
            draw_panel_text(frame, vbox.x + 4, row_y + 8, &shown, color, 1, scale);
        }
    }
}

/// A thin divider line.
fn draw_launcher_divider(frame: &mut [u8], rect: Rect, scale: usize) {
    fill_rect(frame, scale_rect(rect, scale), BUTTON_EDGE_DARK, scale);
}

fn draw_launcher(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let setup = &state.setup;
    // Machine selector grid. The A500 highlights when no profile is chosen
    // (a no-profile machine is the A500 defaults).
    let selected_model = setup.selected_model();
    for (i, &model) in launcher::MODELS.iter().enumerate() {
        draw_launcher_chip(
            frame,
            launcher_model_rect(rect, i),
            launcher::model_label(model),
            selected_model == model,
            hover == Some(UiControl::LauncherModel(model)),
            false,
            scale,
        );
    }
    // Divider under the machine grid; vertical divider between the tab column
    // and the settings pane.
    let content_top = launcher_content_top(rect);
    draw_launcher_divider(
        frame,
        Rect {
            x: rect.x + LAUNCH_MARGIN,
            y: content_top - 6,
            w: rect.w - 2 * LAUNCH_MARGIN,
            h: 1,
        },
        scale,
    );
    draw_launcher_divider(
        frame,
        Rect {
            x: rect.x + LAUNCH_MARGIN + LAUNCH_SIDEBAR_W + 5,
            y: content_top,
            w: 1,
            h: launcher_status_y(rect).saturating_sub(content_top + 4),
        },
        scale,
    );
    // Vertical category-tab column.
    for (i, &tab) in launcher::TABS.iter().enumerate() {
        draw_launcher_chip(
            frame,
            launcher_tab_rect(rect, i),
            tab.label(),
            state.tab.strip_tab() == tab,
            hover == Some(UiControl::LauncherTab(tab)),
            true,
            scale,
        );
    }
    // Active tab content in the settings pane, shifted down past the top nav
    // when the tab has one.
    let row_offset = if state.tab.has_top_nav() {
        LAUNCH_NAV_BLOCK_H
    } else {
        0
    };
    if state.tab == LauncherTab::Zorro {
        draw_launcher_zorro(frame, rect, state, hover, scale);
    } else {
        for (i, r) in launcher::rows(
            state.tab,
            state.setup.parallel_device(),
            state.setup.serial_mode(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .enumerate()
        {
            draw_launcher_row(frame, rect, state, r, i, row_offset, hover, scale);
        }
    }
    // Nav row at the top of the pane: a page's sibling links, or a sub-page's
    // Back button. The A/V categories highlight the current one; Storage's
    // sub-page links are plain (no current selection).
    let back_parent = state.tab.parent_tab();
    let options = state.tab.nav_options();
    if back_parent.is_some() || !options.is_empty() {
        if let Some(parent) = back_parent {
            draw_text_button(
                frame,
                launcher_back_button_rect(rect),
                "< Back",
                true,
                hover == Some(UiControl::LauncherTab(parent)),
                scale,
            );
        } else {
            for (slot, &(label, target)) in options.iter().enumerate() {
                draw_launcher_chip(
                    frame,
                    launcher_nav_button_rect(rect, slot),
                    label,
                    target == state.tab,
                    hover == Some(UiControl::LauncherTab(target)),
                    false,
                    scale,
                );
            }
        }
    }
    // The Input tab spells out what the chosen wiring means: which host
    // input source ends up driving each port, live as the values cycle.
    if state.tab == LauncherTab::Input {
        let summary_top = launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::Input,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
            )
            .len()
                + 1,
        ) + row_offset;
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            summary_top,
            "With these settings:",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        for (i, line) in setup.input_routing_summary().iter().enumerate() {
            draw_panel_text(
                frame,
                launcher_pane_x(rect) + 8,
                summary_top + 16 + i * 14,
                line,
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
    // The Boot Priority page spells out the valid range and the floppy-drive
    // priorities its cascade defaults sort around, all greyed like a footnote.
    if state.tab == LauncherTab::BootPriority && state.setup.has_boot_priority_rows() {
        let help_top = (launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::BootPriority,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
            )
            .len()
                + 1,
        ) + row_offset)
            .saturating_sub(10);
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            help_top,
            "Info:",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        for (i, line) in [
            "Valid boot priorities are any value between 127 (highest) and",
            "-128 (disabled).",
        ]
        .iter()
        .enumerate()
        {
            draw_panel_text(
                frame,
                launcher_pane_x(rect) + 8,
                help_top + 16 + i * 14,
                line,
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
    // The NAT backend delivers inbound traffic on the host's schedule, not
    // the emulated clock, so warn that runs stop being reproducible the
    // moment packets flow (loopback and an isolated NIC stay deterministic).
    if state.tab == LauncherTab::IoPorts && setup.ethernet_breaks_determinism() {
        let note_top = launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::IoPorts,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
            )
            .len()
                + 1,
        ) + row_offset;
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            note_top,
            "Warning: NAT networking is non-deterministic.",
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
        for (i, line) in [
            "Inbound traffic follows the host clock, so input recordings",
            "and save-state replays are not byte-identical while it flows.",
        ]
        .iter()
        .enumerate()
        {
            draw_panel_text(
                frame,
                launcher_pane_x(rect) + 8,
                note_top + 16 + i * 14,
                line,
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
    // Status / error line.
    if let Some(status) = &state.status {
        let color = if status.error {
            PANEL_TEXT_ACCENT
        } else {
            PANEL_TEXT_HILIGHT
        };
        draw_panel_text(
            frame,
            rect.x + 10,
            launcher_status_y(rect),
            &status.text,
            color,
            1,
            scale,
        );
    }
    // Action bar.
    for (control, button_rect) in launcher_action_rects(rect) {
        draw_text_button(
            frame,
            button_rect,
            launcher_action_label(control),
            true,
            hover == Some(control),
            scale,
        );
    }
}

pub fn draw_panel_layer(
    frame: &mut [u8],
    texture_scale: usize,
    panel: &Panel,
    hover: Option<UiControl>,
    data: Option<&PanelViewData>,
) {
    draw_panel_chrome(frame, panel, hover, texture_scale);
    let rect = panel_rect(panel);
    match (panel, data) {
        (Panel::About, Some(PanelViewData::About(view))) => {
            draw_about(frame, rect, view, texture_scale)
        }
        (Panel::Shortcuts, _) => draw_shortcuts(frame, rect, texture_scale),
        (Panel::Calibration(session), Some(PanelViewData::Calibration(view))) => {
            draw_calibration(frame, rect, view, hover, session, texture_scale)
        }
        (Panel::Debugger(panel_state), Some(PanelViewData::Debugger(view))) => {
            draw_debugger(frame, rect, panel_state, view, hover, texture_scale)
        }
        (Panel::FrameAnalyzer(panel_state), Some(PanelViewData::FrameAnalyzer(view))) => {
            draw_frame_analyzer(frame, rect, panel_state, view, hover, texture_scale)
        }
        // The console, input-mapping and configuration panels are
        // self-contained (their state holds everything they render), so they
        // need no per-frame view-data snapshot.
        (Panel::InputMap(panel_state), _) => {
            draw_input_map(frame, rect, panel_state, hover, texture_scale)
        }
        (Panel::Console(panel_state), _) => draw_console(frame, rect, panel_state, texture_scale),
        (Panel::Launcher(state), _) => draw_launcher(frame, rect, state, hover, texture_scale),
        (Panel::DropChooser(state), _) => {
            draw_drop_chooser(frame, rect, state, hover, texture_scale)
        }
        _ => {}
    }
}

/// Draw the whole UI layer: pop-up menu and/or the open panel. Drawn after
/// the status bar and OSD so it sits on top of everything.
pub fn draw(
    frame: &mut [u8],
    texture_scale: usize,
    ui: &UiState,
    hover: Option<UiControl>,
    data: Option<&PanelViewData>,
    midi_active: bool,
    sampler_active: bool,
    labels: MenuLabels,
) {
    if let Some(panel) = &ui.panel {
        draw_panel_layer(frame, texture_scale, panel, hover, data);
    }
    if ui.menu_open {
        draw_menu(
            frame,
            hover,
            midi_active,
            sampler_active,
            ui.menu_scroll,
            labels,
            texture_scale,
        );
    }
}

// ---------------------------------------------------------------------------
// Pure formatting helpers (shared with window.rs view builders)
// ---------------------------------------------------------------------------

pub fn parse_hex_u32(s: &str) -> Option<u32> {
    // Tolerate the conventional $ prefix (console input allows it; the
    // debugger displays addresses that way).
    let s = s.trim().trim_start_matches('$');
    if s.is_empty() {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Parse a 68000 register name into the GDB-style index used by
/// `debug_set_register`: D0-D7 -> 0-7, A0-A7 -> 8-15, SR -> 16, PC -> 17,
/// with SP an alias for A7.
fn parse_reg_name(token: &str) -> Option<usize> {
    let token = token.to_ascii_uppercase();
    match token.as_str() {
        "PC" => return Some(17),
        "SR" => return Some(16),
        "SP" => return Some(15),
        _ => {}
    }
    if token.len() < 2 {
        return None;
    }
    let (kind, idx) = token.split_at(1);
    let n: usize = idx.parse().ok()?;
    match kind {
        "D" if n <= 7 => Some(n),
        "A" if n <= 7 => Some(8 + n),
        _ => None,
    }
}

/// Parse a breakpoint spec from the entry box: "ADDR [LHS OP RHS] [IGN N]".
/// Returns the address, an optional condition, and an ignore count. The
/// condition is three whitespace tokens (operand, mnemonic, operand); the
/// optional trailing "IGN N" gives a hex ignore count.
pub fn parse_break_spec(entry: &str) -> Option<(u32, Option<BreakCond>, u32)> {
    let mut tokens = entry.split_whitespace();
    let addr = parse_hex_u32(tokens.next()?)?;
    let rest: Vec<&str> = tokens.collect();
    // Split off a trailing "IGN N" clause if present.
    let (cond_tokens, ignore) = match rest.iter().position(|t| t.eq_ignore_ascii_case("IGN")) {
        Some(i) => {
            let count = parse_hex_u32(rest.get(i + 1)?)?;
            (&rest[..i], count)
        }
        None => (&rest[..], 0),
    };
    let cond = match cond_tokens {
        [] => None,
        [lhs, op, rhs] => Some(BreakCond {
            lhs: parse_cond_operand(lhs)?,
            op: parse_cond_op(op)?,
            rhs: parse_cond_operand(rhs)?,
        }),
        _ => return None,
    };
    Some((addr, cond, ignore))
}

/// Parse the Break tab's entry as a beam-trap position: decimal
/// "VPOS" or "VPOS HPOS", matching the beam coordinates the analyzer and
/// Chipset tab display. `hpos` omitted means the start of the line.
pub fn parse_beam_spec(entry: &str) -> Option<(u16, Option<u16>)> {
    let mut tokens = entry.split_whitespace();
    let vpos = tokens.next()?.parse::<u16>().ok()?;
    let hpos = match tokens.next() {
        Some(token) => Some(token.parse::<u16>().ok()?),
        None => None,
    };
    if tokens.next().is_some() {
        return None;
    }
    Some((vpos, hpos))
}

/// Parse a condition operand: a register name, `M<hex>` for a memory word, or a
/// bare hex immediate. Register names win over hex (so `D0` is the register,
/// not `$D0`); write an immediate with a leading zero (`0D0`) to disambiguate.
fn parse_cond_operand(token: &str) -> Option<CondOperand> {
    if let Some(reg) = parse_reg_name(token) {
        return Some(match reg {
            0..=7 => CondOperand::Data(reg),
            8..=15 => CondOperand::Addr(reg - 8),
            16 => CondOperand::Sr,
            _ => CondOperand::Pc,
        });
    }
    if let Some(hex) = token.strip_prefix('M').or_else(|| token.strip_prefix('m')) {
        return Some(CondOperand::Mem(parse_hex_u32(hex)?));
    }
    Some(CondOperand::Imm(parse_hex_u32(token)?))
}

fn parse_cond_op(token: &str) -> Option<CondOp> {
    Some(match token.to_ascii_uppercase().as_str() {
        "EQ" => CondOp::Eq,
        "NE" => CondOp::Ne,
        "LT" => CondOp::Lt,
        "GT" => CondOp::Gt,
        "LE" => CondOp::Le,
        "GE" => CondOp::Ge,
        "AND" => CondOp::And,
        _ => return None,
    })
}

const DMACON_BITS: [(u16, &str); 15] = [
    (1 << 14, "BBUSY"),
    (1 << 13, "BZERO"),
    (1 << 10, "BLTPRI"),
    (1 << 9, "DMAEN"),
    (1 << 8, "BPLEN"),
    (1 << 7, "COPEN"),
    (1 << 6, "BLTEN"),
    (1 << 5, "SPREN"),
    (1 << 4, "DSKEN"),
    (1 << 3, "AUD3"),
    (1 << 2, "AUD2"),
    (1 << 1, "AUD1"),
    (1 << 0, "AUD0"),
    (1 << 12, "B12"),
    (1 << 11, "B11"),
];

const INT_BITS: [(u16, &str); 15] = [
    (1 << 14, "INTEN"),
    (1 << 13, "EXTER"),
    (1 << 12, "DSKSYN"),
    (1 << 11, "RBF"),
    (1 << 10, "AUD3"),
    (1 << 9, "AUD2"),
    (1 << 8, "AUD1"),
    (1 << 7, "AUD0"),
    (1 << 6, "BLIT"),
    (1 << 5, "VERTB"),
    (1 << 4, "COPER"),
    (1 << 3, "PORTS"),
    (1 << 2, "SOFT"),
    (1 << 1, "DSKBLK"),
    (1 << 0, "TBE"),
];

fn decode_bits(value: u16, names: &[(u16, &str)]) -> String {
    let set: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| value & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if set.is_empty() {
        "-".to_string()
    } else {
        set.join(" ")
    }
}

/// The set DMACON bit names, most significant first.
pub fn dmacon_flags(value: u16) -> String {
    decode_bits(value, &DMACON_BITS)
}

/// The set INTENA/INTREQ bit names, most significant first.
pub fn int_flags(value: u16) -> String {
    decode_bits(value, &INT_BITS)
}

/// A compact status-register summary: supervisor/user, interrupt mask,
/// trace, and the CCR flags (uppercase = set).
pub fn sr_flags(sr: u16) -> String {
    let mode = if sr & 0x2000 != 0 { 'S' } else { 'U' };
    let trace = if sr & 0x8000 != 0 { "T " } else { "" };
    let ipl = (sr >> 8) & 7;
    let ccr: String = [(4, 'X'), (3, 'N'), (2, 'Z'), (1, 'V'), (0, 'C')]
        .iter()
        .map(|&(bit, ch)| {
            if sr & (1 << bit) != 0 {
                ch
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect();
    format!("{trace}{mode} IPL{ipl} {ccr}")
}

/// ADKCON audio-modulation attach bits (bits 0-7). Vx = the channel's
/// volume modulates the next channel; Px = its period modulates the next.
const ADKCON_AUDIO_BITS: [(u16, &str); 8] = [
    (1 << 7, "3PN"),
    (1 << 6, "2P3"),
    (1 << 5, "1P2"),
    (1 << 4, "0P1"),
    (1 << 3, "3VN"),
    (1 << 2, "2V3"),
    (1 << 1, "1V2"),
    (1 << 0, "0V1"),
];

/// The set ADKCON audio attach bits, or "-" when no channels are attached.
pub fn adkcon_audio_flags(value: u16) -> String {
    decode_bits(value, &ADKCON_AUDIO_BITS)
}

/// One hex-dump row: address, 16 bytes as hex, then printable ASCII.
pub fn hex_dump_row(addr: u32, bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let ascii: String = bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{addr:06X}: {}  {ascii}", hex.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_path_keeps_the_file_name() {
        let g = font::GLYPH_W;
        // Fits: returned unchanged.
        assert_eq!(clip_path_keep_name("/a/b.txt", 100 * g), "/a/b.txt");

        // A long Unix path keeps the whole file name after a "..." prefix.
        let unix = "/Users/me/Documents/amiga/captures/printer-output.txt";
        let out = clip_path_keep_name(unix, 24 * g);
        assert!(out.starts_with("..."), "{out}");
        assert!(out.ends_with("printer-output.txt"), "{out}");
        assert!(out.chars().count() <= 24, "{out}");

        // A long Windows path: backslash separators, file name preserved.
        let win = r"C:\Users\me\Documents\amiga\captures\printer-output.txt";
        let out = clip_path_keep_name(win, 24 * g);
        assert!(out.contains('\\'), "{out}");
        assert!(out.ends_with("printer-output.txt"), "{out}");
        assert!(out.chars().count() <= 24, "{out}");
    }

    #[test]
    fn menu_sits_above_the_status_bar_and_hit_tests_items() {
        let n = menu_items(false, false).len();
        let rect = menu_rect(n);
        assert!(rect.y + rect.h <= present_height());
        assert!(rect.x + rect.w <= FB_WIDTH);

        let ui = UiState {
            menu_open: true,
            menu_scroll: 0,
            panel: None,
        };
        let first = menu_item_rect(0, n, 0).unwrap();
        let pos = (first.x as i32 + 4, first.y as i32 + 4);
        assert_eq!(
            ui.control_at(pos, false, false),
            Some(UiControl::MenuItem(MenuItem::MachineConfig))
        );
        // Leading block is MachineConfig, FrameAnalyzer, Debugger, Console,
        // AudioOutput, AudioFilter, Calibration, so Joystick Input sits at
        // index 7.
        let joystick = menu_item_rect(7, n, 0).unwrap();
        let pos = (joystick.x as i32 + 4, joystick.y as i32 + 4);
        assert_eq!(
            ui.control_at(pos, false, false),
            Some(UiControl::MenuItem(MenuItem::JoystickInput))
        );
        // Outside the menu: nothing (the click closes the menu).
        assert_eq!(ui.control_at((0, 0), false, false), None);
    }

    /// The menu grows upward from the bottom edge, so every optional item
    /// (MIDI, sampler) that can appear has to still fit. Rows tighten rather
    /// than the first items falling off the top, and only once even the
    /// tightest rows overflow does the list scroll. The fullest session --
    /// MIDI endpoints and a sampler both live -- reaches that point: at the
    /// 16px floor its items want more than the display opening holds.
    #[test]
    fn the_menu_fits_on_screen_with_every_optional_item_shown() {
        for (midi, sampler) in [(false, false), (true, false), (false, true), (true, true)] {
            let n = menu_items(midi, sampler).len();
            let rect = menu_rect(n);
            assert!(
                rect.y + rect.h <= present_height(),
                "menu of {n} items overflows the display"
            );
            let layout = menu_layout(n);
            assert!(
                layout.item_h >= MENU_TEXT_PX * font::GLYPH_H,
                "menu rows must stay at least as tall as their text"
            );
            // Only the everything-live session is allowed to reach the
            // scrolling fallback; every ordinary session still shows the
            // whole list at once.
            if !(midi && sampler) {
                assert!(
                    !layout.scrolling,
                    "menu of {n} items must not scroll without both MIDI and a sampler"
                );
            }
            // Every drawn row lands inside the menu background.
            let last = menu_item_rect(layout.visible - 1, n, 0).expect("last visible item");
            assert!(last.y + last.h <= rect.y + rect.h);
            assert!(menu_item_rect(0, n, 0).unwrap().y >= rect.y);
        }
    }

    /// Past the point where even the tightest rows fit, the menu scrolls: it
    /// shows a window into the list with a scroll row at each end.
    #[test]
    fn an_over_long_menu_scrolls_instead_of_overflowing() {
        // Deliberately more items than any real session builds, so the
        // fallback is exercised whatever the menu grows to next.
        let n = 200;
        let layout = menu_layout(n);
        assert!(layout.scrolling);
        assert!(layout.visible < n);
        let rect = menu_rect(n);
        assert!(
            rect.y + rect.h <= present_height(),
            "a scrolling menu still fits the display"
        );

        // At the top: the first items are visible, the last are not, and only
        // the down arrow is live.
        assert!(menu_item_rect(0, n, 0).is_some());
        assert!(menu_item_rect(layout.visible, n, 0).is_none());
        let rows = menu_scroll_rows(n, 0).expect("scroll rows");
        assert_eq!(rows[0].0, UiControl::MenuScrollUp);
        assert!(!rows[0].2, "nothing above the first item");
        assert!(rows[1].2, "there is more below");

        // Scrolled by one: the window has moved by exactly one item and both
        // arrows are live.
        assert!(menu_item_rect(0, n, 1).is_none());
        assert_eq!(menu_item_rect(1, n, 1), menu_item_rect(0, n, 0));
        let rows = menu_scroll_rows(n, 1).expect("scroll rows");
        assert!(rows[0].2 && rows[1].2);

        // At the end: the last item is visible and the down arrow is spent.
        let end = layout.max_scroll(n);
        assert!(menu_item_rect(n - 1, n, end).is_some());
        let rows = menu_scroll_rows(n, end).expect("scroll rows");
        assert!(rows[0].2);
        assert!(!rows[1].2, "nothing below the last item");

        // An out-of-range scroll clamps rather than emptying the menu.
        assert_eq!(clamp_menu_scroll(n * 2, n), end);
        assert!(menu_item_rect(n - 1, n, n * 2).is_some());
        // A menu that fits has nothing to scroll.
        assert_eq!(clamp_menu_scroll(5, 3), 0);
        assert!(menu_scroll_rows(3, 0).is_none());
    }

    #[test]
    fn scroll_rows_hit_test_ahead_of_the_items_they_sit_over() {
        let items = menu_items(false, false);
        let n = items.len();
        // Force the scrolling layout by asking for a list the display cannot
        // hold, then hit-test through a UiState carrying a scroll position.
        let big = 200;
        let ui = UiState {
            menu_open: true,
            menu_scroll: 1,
            panel: None,
        };
        let rows = menu_scroll_rows(big, 1).expect("scroll rows");
        for (control, rect, _) in rows {
            let pos = (rect.x as i32 + 4, rect.y as i32 + 2);
            // The real menu is shorter than `big`, so drive the geometry
            // directly: the point is that the row rect wins over an item.
            assert!(rect.contains(pos), "{control:?} row contains its own point");
        }

        // On the real (non-scrolling) menu, item hit-testing is unchanged and
        // the scroll position is ignored.
        let first = menu_item_rect(0, n, 0).unwrap();
        assert_eq!(
            ui.control_at((first.x as i32 + 4, first.y as i32 + 4), false, false),
            Some(UiControl::MenuItem(items[0]))
        );
    }

    /// The shortcuts panel is sized from its row count, so adding a row must
    /// not push the table (or the notes under it) off the display.
    #[test]
    fn the_shortcuts_panel_fits_on_screen() {
        let h = shortcuts_panel_height();
        assert!(
            h <= present_height(),
            "shortcuts panel is {h}px tall, taller than the {}px display",
            present_height()
        );
        // Sized to exactly hold header + rows + notes.
        assert!(
            h >= TITLE_H
                + SHORTCUT_ROWS.len() * SHORTCUT_ROW_H
                + SHORTCUT_NOTES.len() * SHORTCUT_NOTE_H
        );
    }

    /// The launcher panel is a fixed box with no row scrolling, so a tab's
    /// rows have to fit between the content top and the chrome below them:
    /// the footer nav row on the Storage tab and its sub-pages, the status
    /// line everywhere else. Nothing may reach the action buttons or hang
    /// off the panel. Adding one row too many to a tab fails here rather
    /// than silently drawing over the Save button.
    #[test]
    fn every_launcher_tab_row_fits_inside_the_panel() {
        use crate::config::{ParallelDevice, SerialMode};
        let rect = panel_rect(&Panel::Launcher(Box::new(LauncherState::new(
            launcher::MachineSetup::default(),
        ))));
        let devices = [
            ParallelDevice::None,
            ParallelDevice::Printer,
            ParallelDevice::Sampler,
        ];
        // Every serial mode, so a future one that grows its own rows is
        // swept here the day it is added.
        let modes = [
            SerialMode::Off,
            SerialMode::Stdout,
            SerialMode::Midi,
            SerialMode::Tcp,
            SerialMode::TcpConnect,
            SerialMode::Pty,
        ];
        // The strip tabs, plus the sub-pages and A/V categories reached from a
        // nav row rather than the strip.
        let off_strip = [
            LauncherTab::Cd,
            LauncherTab::HostFs,
            LauncherTab::BootPriority,
            LauncherTab::AvVideo,
            LauncherTab::AvEmulation,
        ];
        for &tab in launcher::TABS.iter().chain(off_strip.iter()) {
            // The row grid always ends above the status line; on tabs with a top
            // nav it starts a nav block lower, leaving it less room.
            let bound = launcher_status_y(rect);
            let row_offset = if tab.has_top_nav() {
                LAUNCH_NAV_BLOCK_H
            } else {
                0
            };
            for &device in &devices {
                for &mode in &modes {
                    let rows = launcher::rows(tab, device, mode);
                    for (i, r) in rows.iter().enumerate() {
                        let row_y = launcher_row_y(rect, i) + row_offset;
                        let (prev, value, next) = launcher_cycle_rects(rect, row_y);
                        let (browse, clear) = launcher_path_rects(rect, row_y);
                        // Every control a row can draw, whatever its kind:
                        // the widest and lowest of them must still fit.
                        let boxes = [
                            prev,
                            value,
                            next,
                            browse,
                            clear,
                            launcher_toggle_rect(rect, row_y),
                            launcher_drive_name_rect(rect, row_y),
                            launcher_bootable_rect(rect, row_y),
                        ];
                        for b in boxes {
                            let label = r.label;
                            assert!(
                                b.y >= launcher_content_top(rect),
                                "{tab:?} row {i} ({label:?}) starts above the content area"
                            );
                            assert!(
                                b.y + b.h <= bound,
                                "{tab:?} row {i} ({label:?}) reaches {}, past the {bound} limit",
                                b.y + b.h
                            );
                            assert!(
                                b.x >= rect.x && b.x + b.w <= rect.x + rect.w,
                                "{tab:?} row {i} ({label:?}) spills outside the panel"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fullscreen_menu_item_label_tracks_window_state() {
        assert!(menu_items(false, false).contains(&MenuItem::Fullscreen));
        let labels = |fullscreen| MenuLabels {
            warp: false,
            warp_speed: WarpSpeed::Max,
            fullscreen,
            status_bar_hidden: false,
            recording: false,
            input_recording: false,
            rewind: false,
            save_slot: 1,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [
                crate::bus::PortDevice::Mouse,
                crate::bus::PortDevice::Joystick,
            ],
            pixel_aspect: PixelAspect::Tv,
            floppy_speed: 100,
            midi_in: "",
            midi_out: "",
            audio_output: "",
            audio_filter: crate::config::AudioFilterMode::Auto,
            sampler_input: "",
            sampler_gain: "",
            shader: crate::config::ShaderKind::None,
        };
        assert_eq!(
            menu_item_label(MenuItem::Fullscreen, labels(false)),
            "Fullscreen     [off]"
        );
        assert_eq!(
            menu_item_label(MenuItem::Fullscreen, labels(true)),
            "Fullscreen      [on]"
        );
    }

    #[test]
    fn status_bar_menu_item_label_tracks_state() {
        assert!(menu_items(false, false).contains(&MenuItem::StatusBar));
        let labels = |status_bar_hidden| MenuLabels {
            warp: false,
            warp_speed: WarpSpeed::Max,
            fullscreen: false,
            status_bar_hidden,
            recording: false,
            input_recording: false,
            rewind: false,
            save_slot: 1,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [
                crate::bus::PortDevice::Mouse,
                crate::bus::PortDevice::Joystick,
            ],
            pixel_aspect: PixelAspect::Tv,
            floppy_speed: 100,
            midi_in: "",
            midi_out: "",
            audio_output: "",
            audio_filter: crate::config::AudioFilterMode::Auto,
            sampler_input: "",
            sampler_gain: "",
            shader: crate::config::ShaderKind::None,
        };
        // "on" means the bar is shown.
        assert_eq!(
            menu_item_label(MenuItem::StatusBar, labels(false)),
            "Status Bar      [on]"
        );
        assert_eq!(
            menu_item_label(MenuItem::StatusBar, labels(true)),
            "Status Bar     [off]"
        );
    }

    #[test]
    fn every_menu_label_fits_inside_the_popup() {
        // The label is drawn at `item_rect.x + MENU_TEXT_INSET`; its glyphs
        // must end before the popup's right edge or the trailing "~" clips.
        let items = menu_items(true, true);
        let menu = menu_rect(items.len());
        let limit = menu.x + menu.w;
        let modes = [JoystickInputMode::Gamepad, JoystickInputMode::Keyboard];
        let speeds = [WarpSpeed::X2, WarpSpeed::X8, WarpSpeed::X16, WarpSpeed::Max];
        // A deliberately over-long device name exercises the clip path.
        let long = "Extremely Long USB MIDI Interface Name 9000";
        let aspects = [PixelAspect::Tv, PixelAspect::Square];
        let shaders = [
            crate::config::ShaderKind::None,
            crate::config::ShaderKind::Scanlines,
            crate::config::ShaderKind::Mask,
            crate::config::ShaderKind::Crt,
            crate::config::ShaderKind::Custom,
        ];
        // The width check itself, out of the sweep below: one combination per
        // label-bearing field nests deeply enough without it.
        let check = |item: MenuItem, labels: MenuLabels| {
            let label = menu_item_label(item, labels);
            let text_w = label.chars().count() * font::GLYPH_W * MENU_TEXT_PX;
            let right = menu_item_rect(0, items.len(), 0).unwrap().x + MENU_TEXT_INSET + text_w;
            assert!(
                right <= limit,
                "label {label:?} ({text_w}px) overflows the menu by {}px",
                right.saturating_sub(limit)
            );
        };
        for &item in &items {
            for warp in [false, true] {
                for recording in [false, true] {
                    for input_recording in [false, true] {
                        for &mode in &modes {
                            for &speed in &speeds {
                                for &aspect in &aspects {
                                    for &shader in &shaders {
                                        let labels = MenuLabels {
                                            warp,
                                            warp_speed: speed,
                                            // Rides warp's sweep so both label
                                            // states are width-checked.
                                            fullscreen: warp,
                                            status_bar_hidden: warp,
                                            recording,
                                            input_recording,
                                            // Rides warp's sweep so both label
                                            // states are width-checked.
                                            rewind: warp,
                                            save_slot: 1,
                                            autofire_hz: 0,
                                            joystick_input_mode: mode,
                                            // The longest device label.
                                            port_devices: [crate::bus::PortDevice::Analogue; 2],
                                            pixel_aspect: aspect,
                                            // Rides warp's sweep: "turbo" is the
                                            // widest value, "100%" the tallest
                                            // percent form.
                                            floppy_speed: if warp {
                                                crate::floppy::SPEED_TURBO
                                            } else {
                                                100
                                            },
                                            midi_in: long,
                                            midi_out: long,
                                            audio_output: long,
                                            audio_filter: crate::config::AudioFilterMode::Auto,
                                            sampler_input: long,
                                            sampler_gain: "-24 dB",
                                            shader,
                                        };
                                        check(item, labels);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn frame_analyzer_controls_hit_test() {
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::FrameAnalyzer(FrameAnalyzerPanel::new())),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let raster = analyzer_raster_rect(rect);
        assert_eq!(
            ui.control_at(
                (raster.x as i32 + raster.w as i32 / 2, raster.y as i32 + 2),
                false,
                false
            ),
            Some(UiControl::AnalyzerPick {
                x: 511,
                y: 8,
                scanline: false,
            })
        );
        let scanline = analyzer_scanline_rect(rect);
        assert_eq!(
            ui.control_at(
                (
                    scanline.x as i32 + scanline.w as i32 / 2,
                    scanline.y as i32 + 2
                ),
                false,
                false
            ),
            Some(UiControl::AnalyzerPick {
                x: 511,
                y: 60,
                scanline: true,
            })
        );
        let (control, button) = analyzer_button_rects(rect)[1];
        assert_eq!(control, UiControl::AnalyzerFrame);
        assert_eq!(
            ui.control_at((button.x as i32 + 2, button.y as i32 + 2), false, false),
            Some(UiControl::AnalyzerFrame)
        );
        let underlay = analyzer_underlay_rect(rect);
        assert_eq!(
            ui.control_at((underlay.x as i32 + 2, underlay.y as i32 + 2), false, false),
            Some(UiControl::AnalyzerUnderlay)
        );
        // The checkbox must not overlap the transport buttons.
        for (_, button) in analyzer_button_rects(rect) {
            assert!(button.x + button.w <= underlay.x || underlay.x + underlay.w <= button.x);
        }
    }

    /// A Frame Analyzer panel in `tab`, with `presets` on the Memory tab.
    fn analyzer_ui(tab: AnalyzerTab, presets: Vec<HeatPreset>) -> UiState {
        let mut panel = FrameAnalyzerPanel::new();
        panel.tab = tab;
        panel.heat_presets = presets;
        UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::FrameAnalyzer(panel)),
        }
    }

    fn heat_preset(label: &str, base: u32, span: u32) -> HeatPreset {
        HeatPreset {
            label: label.to_string(),
            base,
            span,
        }
    }

    /// A synthetic Memory-tab view: a black window with `lit` cells set to
    /// their toucher's colour, and a full census.
    fn heat_view(lit: &[(usize, crate::heatmap::Toucher)]) -> AnalyzerHeatView {
        use crate::heatmap::Toucher;
        let mut image = vec![0xFF00_0000u32; heatmap::CELLS];
        for (cell, toucher) in lit {
            image[*cell] = toucher.colour();
        }
        let touchers = [
            Toucher::CpuRead,
            Toucher::CpuWrite,
            Toucher::Blitter,
            Toucher::Copper,
            Toucher::Disk,
            Toucher::Bitplane,
            Toucher::Sprite,
            Toucher::Audio,
        ];
        AnalyzerHeatView {
            image,
            base: 0,
            span: heatmap::DEFAULT_SPAN,
            bytes_per_cell: heatmap::DEFAULT_SPAN / heatmap::CELLS as u32,
            frame: 4321,
            census: touchers
                .iter()
                .map(|toucher| {
                    let cells = lit.iter().filter(|(_, t)| t == toucher).count();
                    AnalyzerHeatCensusRow {
                        name: toucher.name(),
                        colour: toucher.colour(),
                        cells,
                        bytes: cells as u64
                            * u64::from(heatmap::DEFAULT_SPAN / heatmap::CELLS as u32),
                    }
                })
                .collect(),
            selected: None,
        }
    }

    fn analyzer_view(
        trace: Option<AnalyzerTraceView>,
        heat: Option<AnalyzerHeatView>,
    ) -> Box<FrameAnalyzerView> {
        Box::new(FrameAnalyzerView {
            running: false,
            status: "paused frame 4321".to_string(),
            trace,
            underlay: None,
            scrub: false,
            heat,
        })
    }

    #[test]
    fn analyzer_tabs_hit_test_and_gate_their_tab_controls() {
        for (index, tab) in ANALYZER_TABS.iter().enumerate() {
            let ui = analyzer_ui(AnalyzerTab::Beam, Vec::new());
            let rect = panel_rect(ui.panel.as_ref().unwrap());
            let button = analyzer_tab_rect(rect, index);
            assert_eq!(
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2), false, false),
                Some(UiControl::AnalyzerTab(*tab))
            );
        }

        // Beam tab: the beam controls hit, the map does not exist.
        let ui = analyzer_ui(AnalyzerTab::Beam, Vec::new());
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let map = analyzer_heat_map_rect(rect);
        let underlay = analyzer_underlay_rect(rect);
        assert_eq!(
            ui.control_at((underlay.x as i32 + 2, underlay.y as i32 + 2), false, false),
            Some(UiControl::AnalyzerUnderlay)
        );
        let in_map = (map.x as i32 + map.w as i32 / 2, map.y as i32 + 4);
        assert!(
            !matches!(
                ui.control_at(in_map, false, false),
                Some(UiControl::AnalyzerHeatPick { .. })
            ),
            "the heat map is not drawn on the Beam tab, so it must not be clickable"
        );

        // Memory tab: the map hits, and none of the beam-only controls do.
        let ui = analyzer_ui(AnalyzerTab::Memory, Vec::new());
        assert!(matches!(
            ui.control_at(in_map, false, false),
            Some(UiControl::AnalyzerHeatPick { .. })
        ));
        for beam_only in [
            (underlay.x as i32 + 2, underlay.y as i32 + 2),
            (
                analyzer_scrub_rect(rect).x as i32 + 2,
                analyzer_scrub_rect(rect).y as i32 + 2,
            ),
            (
                analyzer_button_rects(rect)[2].1.x as i32 + 2,
                analyzer_button_rects(rect)[2].1.y as i32 + 2,
            ),
        ] {
            assert_eq!(
                ui.control_at(beam_only, false, false),
                Some(UiControl::PanelBody),
                "beam-only controls are inert on the Memory tab"
            );
        }
        // Run and Frame stay on both tabs.
        for slot in 0..2 {
            let (control, button) = analyzer_button_rects(rect)[slot];
            assert_eq!(
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2), false, false),
                Some(control)
            );
        }
        // The scanline strip belongs to the beam view too.
        let scanline = analyzer_scanline_rect(rect);
        assert!(!matches!(
            ui.control_at((scanline.x as i32 + 4, scanline.y as i32 + 4), false, false),
            Some(UiControl::AnalyzerPick { .. })
        ));
    }

    #[test]
    fn heat_map_clicks_map_to_grid_cells() {
        let ui = analyzer_ui(AnalyzerTab::Memory, Vec::new());
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let map = analyzer_heat_map_rect(rect);
        let last = (heatmap::GRID - 1) as u8;
        let pick = |dx: usize, dy: usize| {
            ui.control_at(
                (map.x as i32 + dx as i32, map.y as i32 + dy as i32),
                false,
                false,
            )
        };
        assert_eq!(pick(0, 0), Some(UiControl::AnalyzerHeatPick { x: 0, y: 0 }));
        assert_eq!(
            pick(map.w - 1, map.h - 1),
            Some(UiControl::AnalyzerHeatPick { x: last, y: last }),
            "the map's last pixel is the grid's last cell"
        );
        assert_eq!(
            pick(map.w - 1, 0),
            Some(UiControl::AnalyzerHeatPick { x: last, y: 0 })
        );
        assert_eq!(
            pick(map.w / 2, map.h / 2),
            Some(UiControl::AnalyzerHeatPick { x: 128, y: 128 })
        );
        // One pixel past the map is not a pick.
        assert_ne!(
            ui.control_at(
                (map.x as i32 + map.w as i32, map.y as i32 + 2),
                false,
                false
            ),
            Some(UiControl::AnalyzerHeatPick { x: last, y: 0 })
        );
    }

    #[test]
    fn heat_presets_hit_by_index_and_vanish_when_there_are_none() {
        let presets = vec![
            heat_preset("Chip", 0, 0x0020_0000),
            heat_preset("24-bit", 0, heatmap::DEFAULT_SPAN),
        ];
        let ui = analyzer_ui(AnalyzerTab::Memory, presets.clone());
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let rects = analyzer_preset_rects(rect, &presets);
        assert_eq!(rects.len(), 2);
        for (index, (control, button)) in rects.iter().enumerate() {
            assert_eq!(*control, UiControl::AnalyzerHeatPreset(index as u8));
            assert_eq!(
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2), false, false),
                Some(*control)
            );
            // Presets sit above the map, never over it.
            assert!(button.y + button.h <= analyzer_heat_map_rect(rect).y);
        }
        // With no presets the row is empty: the same points are panel body.
        let empty = analyzer_ui(AnalyzerTab::Memory, Vec::new());
        for (_, button) in rects {
            assert_eq!(
                empty.control_at((button.x as i32 + 2, button.y as i32 + 2), false, false),
                Some(UiControl::PanelBody)
            );
        }
        assert!(analyzer_preset_rects(rect, &[]).is_empty());
    }

    /// The tab row shifted the beam layout down; the content it pushed
    /// down must still clear the bottom-anchored transport row.
    #[test]
    fn analyzer_tab_row_leaves_both_tabs_room_above_the_buttons() {
        let ui = analyzer_ui(AnalyzerTab::Beam, Vec::new());
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let tabs = analyzer_tab_rect(rect, ANALYZER_TABS.len() - 1);
        assert!(tabs.y >= rect.y + TITLE_H, "tabs sit under the title bar");
        assert!(tabs.x + tabs.w < rect.x + rect.w);
        let content_top = analyzer_content_top(rect);
        assert!(content_top >= tabs.y + tabs.h);
        let buttons_top = analyzer_button_rects(rect)[0].1.y;
        // Beam: the legend and marker-count lines follow the scanline
        // strip (strip bottom + 14 for the legend, + 18 for the count).
        let scanline = analyzer_scanline_rect(rect);
        assert!(
            scanline.y + scanline.h + 14 + 18 + font::GLYPH_H <= buttons_top,
            "beam tab content runs into the transport row"
        );
        // Memory: the map plus its readout line.
        let map = analyzer_heat_map_rect(rect);
        assert!(
            map.y + map.h + 10 + font::GLYPH_H <= buttons_top,
            "the heat map runs into the transport row"
        );
        assert_eq!(map.w, map.h, "the map is square");
        // The census column fits between the map and the panel edge.
        let census_x = analyzer_heat_census_x(rect);
        assert!(census_x >= map.x + map.w);
        assert!(census_x + 12 + 27 * font::GLYPH_W <= rect.x + rect.w);
    }

    #[test]
    fn heat_map_draws_its_cells_and_leaves_the_rest_black() {
        use super::super::window::{texture_height, texture_width};
        use crate::heatmap::Toucher;

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let mut frame = vec![0u8; w * h * 4];
        let mut panel = FrameAnalyzerPanel::new();
        panel.tab = AnalyzerTab::Memory;
        panel.heat_presets = vec![heat_preset("Chip", 0, 0x0020_0000)];
        let rect = panel_rect(&Panel::FrameAnalyzer(panel.clone()));
        let map = analyzer_heat_map_rect(rect);
        // One lit cell in the middle of the grid, away from the outline.
        let cell = 128 * heatmap::GRID + 128;
        let view = analyzer_view(None, Some(heat_view(&[(cell, Toucher::Blitter)])));
        draw_frame_analyzer(&mut frame, rect, &panel, &view, None, scale);

        let pixel = |x: usize, y: usize| -> [u8; 4] {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4]
                .try_into()
                .unwrap()
        };
        // Sample where the map's own nearest mapping puts that cell.
        let lit = (0..map.w)
            .find(|x| x * heatmap::GRID / map.w == 128)
            .unwrap();
        assert_eq!(
            pixel(map.x + lit, map.y + lit),
            heat_rgba(Toucher::Blitter.colour()).to_le_bytes(),
            "the lit cell is painted in its toucher's colour"
        );
        // A cell nothing touched stays black (not the untouched-frame zero).
        assert_eq!(
            pixel(map.x + 40, map.y + 40),
            rgba(0, 0, 0).to_le_bytes(),
            "cold cells are black"
        );
    }

    #[test]
    fn the_beam_tab_draws_below_the_tab_row() {
        use super::super::window::{texture_height, texture_width};

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let mut frame = vec![0u8; w * h * 4];
        let panel = FrameAnalyzerPanel::new();
        assert_eq!(panel.tab, AnalyzerTab::Beam, "the beam view opens first");
        let rect = panel_rect(&Panel::FrameAnalyzer(panel.clone()));
        let trace = AnalyzerTraceView {
            frame: 1,
            seconds: 0.0,
            rows: 4,
            cols: 4,
            line_cck: 4,
            visible_start_vpos: 0,
            visible_lines: 2,
            display_hpos_start: 0,
            display_hpos_end: 4,
            owner_cck: [0; 9],
            blitter_busy_cck: 0,
            blitter_starve_cck: [0; 9],
            partial: false,
            selected_vpos: 0,
            selected_hpos: 0,
            selected_owner: "idle",
            selected_owner_code: b'.',
            owners: vec![b'.'; 16],
            markers: Vec::new(),
            selected_blit: None,
            diw_v: None,
            diw_h_cck: None,
            ddf_cck: None,
        };
        let view = analyzer_view(Some(trace), None);
        draw_frame_analyzer(&mut frame, rect, &panel, &view, None, scale);

        let pixel = |x: usize, y: usize| -> [u8; 4] {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4]
                .try_into()
                .unwrap()
        };
        // The open tab reads as pressed, the other as a plain button.
        let beam = analyzer_tab_rect(rect, 0);
        let memory = analyzer_tab_rect(rect, 1);
        // Sampled inside the bevel but left of the centred label.
        assert_eq!(pixel(beam.x + 3, beam.y + 3), ENTRY_BG.to_le_bytes());
        assert_eq!(pixel(memory.x + 3, memory.y + 3), BUTTON_FACE.to_le_bytes());
        // The raster moved down with the rest of the content and is still
        // painted (idle slots, not the untouched frame).
        let raster = analyzer_raster_rect(rect);
        assert!(raster.y > beam.y + beam.h);
        assert_eq!(
            pixel(raster.x + 4, raster.y + raster.h / 2),
            owner_color(b'.').to_le_bytes()
        );
    }

    #[test]
    fn the_memory_tab_draws_without_a_beam_trace_or_a_map() {
        use super::super::window::{texture_height, texture_width};
        use crate::heatmap::Toucher;

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let mut panel = FrameAnalyzerPanel::new();
        panel.tab = AnalyzerTab::Memory;
        panel.heat_presets = vec![
            heat_preset("Chip", 0, 0x0020_0000),
            heat_preset("24-bit", 0, heatmap::DEFAULT_SPAN),
        ];
        panel.heat_selected = Some(7 * heatmap::GRID + 9);
        let rect = panel_rect(&Panel::FrameAnalyzer(panel.clone()));
        let map = analyzer_heat_map_rect(rect);
        let pixel = |frame: &[u8], x: usize, y: usize| -> [u8; 4] {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4]
                .try_into()
                .unwrap()
        };

        // No beam trace at all: the memory view still paints its map.
        let mut frame = vec![0u8; w * h * 4];
        let mut heat = heat_view(&[(0, Toucher::CpuWrite)]);
        heat.selected = Some(AnalyzerHeatCell {
            cell: 7 * heatmap::GRID + 9,
            toucher: Some(Toucher::Sprite.name()),
            colour: Toucher::Sprite.colour(),
            age_frames: Some(3),
        });
        let view = analyzer_view(None, Some(heat));
        draw_frame_analyzer(&mut frame, rect, &panel, &view, None, scale);
        assert_eq!(
            pixel(&frame, map.x + map.w / 2, map.y + map.h / 2),
            rgba(0, 0, 0).to_le_bytes()
        );
        assert_ne!(
            pixel(&frame, map.x, map.y),
            [0, 0, 0, 0],
            "the map is outlined even when every cell is cold"
        );

        // No map either: the not-armed line and the presets, nothing else.
        let mut bare = vec![0u8; w * h * 4];
        let view = analyzer_view(None, None);
        draw_frame_analyzer(&mut bare, rect, &panel, &view, None, scale);
        for y in map.y..map.y + map.h {
            for x in map.x..map.x + map.w {
                assert_eq!(
                    pixel(&bare, x, y),
                    [0, 0, 0, 0],
                    "an unarmed map paints nothing at ({x}, {y})"
                );
            }
        }
        let presets = analyzer_preset_rects(rect, &panel.heat_presets);
        assert_eq!(presets.len(), 2);
        assert_ne!(
            pixel(&bare, presets[0].1.x + 2, presets[0].1.y + 2),
            [0, 0, 0, 0],
            "the presets are how an unarmed map gets armed, so they stay"
        );
    }

    #[test]
    fn memory_entry_parsers_find_and_region() {
        let mut panel = DebuggerPanel::new();
        panel.entry = "C0 FFEE".into();
        assert_eq!(panel.find_pattern(), Some(vec![0xC0, 0xFF, 0xEE]));
        panel.entry = "C0FFE".into(); // odd number of hex digits
        assert_eq!(panel.find_pattern(), None);
        panel.entry = String::new();
        assert_eq!(panel.find_pattern(), None);

        panel.entry = "C00000 1000".into();
        assert_eq!(panel.region_spec(), Some((0xC0_0000, 0x1000)));
        panel.entry = "C00000".into(); // missing length
        assert_eq!(panel.region_spec(), None);
        panel.entry = "C00000 0".into(); // empty region
        assert_eq!(panel.region_spec(), None);
    }

    #[test]
    fn catch_spec_parses_irq_trap_and_vector_forms() {
        assert_eq!(parse_catch_spec("irq 3"), Some(27));
        assert_eq!(parse_catch_spec("IRQ 7"), Some(31));
        assert_eq!(parse_catch_spec("trap 0"), Some(32));
        assert_eq!(parse_catch_spec("trap 15"), Some(47));
        assert_eq!(parse_catch_spec("vec 4"), Some(4));
        assert_eq!(parse_catch_spec("irq 0"), None); // no level-0 interrupt
        assert_eq!(parse_catch_spec("irq 8"), None);
        assert_eq!(parse_catch_spec("trap 16"), None);
        assert_eq!(parse_catch_spec("vec 1"), None); // reset vectors excluded
        assert_eq!(parse_catch_spec("C033C2"), None); // plain address is not a catch
        assert_eq!(parse_catch_spec("irq 3 4"), None);
    }

    #[test]
    fn analyzer_marker_radius_and_label() {
        let marker = AnalyzerMarker {
            vpos: 100,
            hpos: 50,
            offset: 0x180,
            value: 0x0F00,
            source: "copper",
        };
        // Within a line and two colour clocks counts as near.
        assert!(marker.near(100, 50));
        assert!(marker.near(101, 52));
        assert!(marker.near(99, 48));
        assert!(!marker.near(102, 50));
        assert!(!marker.near(100, 53));
        assert_eq!(marker.label(), "copper COLOR00=$0F00 v100 h50");
    }

    #[test]
    fn analyzer_underlay_sample_maps_display_box_to_framebuffer() {
        // A trace shaped like a standard PAL frame: 312 lines of 227 cck,
        // display box starting at the framebuffer anchor.
        let trace = AnalyzerTraceView {
            frame: 1,
            seconds: 0.0,
            rows: 312,
            cols: 227,
            line_cck: 227,
            visible_start_vpos: 0x1A,
            visible_lines: 285,
            display_hpos_start: 0x30,
            display_hpos_end: 0x30 + (FB_WIDTH as u32 / 4),
            owner_cck: [0; 9],
            blitter_busy_cck: 0,
            blitter_starve_cck: [0; 9],
            partial: false,
            selected_vpos: 0,
            selected_hpos: 0,
            selected_owner: "idle",
            selected_owner_code: b'.',
            owners: vec![b'.'; 312 * 227],
            markers: Vec::new(),
            selected_blit: None,
            diw_v: None,
            diw_h_cck: None,
            ddf_cck: None,
        };
        let mut fb = vec![0u32; FB_WIDTH * 285];
        fb[0] = 0xFF11_2233; // beam (0x1A, 0x30): framebuffer origin
        let underlay = AnalyzerUnderlayView {
            fb: std::rc::Rc::new(fb),
            rows: 285,
            width: FB_WIDTH,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            w: 448,
            h: 246,
        };
        // Heatmap pixel exactly at the display box origin lands on fb[0]:
        // the first x whose hi-res mapping reaches display_hpos_start * 4.
        let x0 = (0..rect.w)
            .find(|x| x * trace.cols * 4 / rect.w >= 0x30 * 4)
            .unwrap();
        assert_eq!(
            underlay_sample(&underlay, &trace, rect, x0, 0x1A),
            Some(0xFF11_2233)
        );
        // Left of the display box or above the visible window: no sample.
        assert_eq!(underlay_sample(&underlay, &trace, rect, 0, 0x1A), None);
        assert_eq!(underlay_sample(&underlay, &trace, rect, x0, 0), None);
    }

    #[test]
    fn panel_close_button_hit_tests() {
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::About),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let close = close_button_rect(rect);
        let pos = (close.x as i32 + 2, close.y as i32 + 2);
        assert_eq!(
            ui.control_at(pos, false, false),
            Some(UiControl::PanelClose)
        );
        // Panel body swallows clicks.
        let body = (rect.x as i32 + 5, (rect.y + TITLE_H + 5) as i32);
        assert_eq!(
            ui.control_at(body, false, false),
            Some(UiControl::PanelBody)
        );
        // Outside the panel: nothing.
        assert_eq!(ui.control_at((0, 0), false, false), None);
    }

    #[test]
    fn debugger_controls_hit_test_and_entry_edits() {
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(DebuggerPanel::new())),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let tab = debug_tab_rect(rect, 3);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2), false, false),
            Some(UiControl::DebugTab(DebugTab::Video))
        );
        let tab = debug_tab_rect(rect, 4);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2), false, false),
            Some(UiControl::DebugTab(DebugTab::Audio))
        );
        let tab = debug_tab_rect(rect, 6);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2), false, false),
            Some(UiControl::DebugTab(DebugTab::IoMap))
        );
        let tab = debug_tab_rect(rect, 7);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2), false, false),
            Some(UiControl::DebugTab(DebugTab::Break))
        );
        // All eight tabs fit inside the panel.
        let last = debug_tab_rect(rect, 7);
        assert!(last.x + last.w <= rect.x + rect.w);
        let (control, step) = debug_button_rects(rect)[1];
        assert_eq!(control, UiControl::DebugStep);
        assert_eq!(
            ui.control_at((step.x as i32 + 2, step.y as i32 + 2), false, false),
            Some(UiControl::DebugStep)
        );

        // Break-tab toggle buttons hit-test only while that tab is active.
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Break;
        let ui_break = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        let (control, toggle) = break_tab_button_rects(rect)[0];
        assert_eq!(control, UiControl::DebugBreakToggle);
        let pos = (toggle.x as i32 + 2, toggle.y as i32 + 2);
        assert_eq!(
            ui_break.control_at(pos, false, false),
            Some(UiControl::DebugBreakToggle)
        );
        // On another tab the same position is just panel body.
        assert_eq!(ui.control_at(pos, false, false), Some(UiControl::PanelBody));

        // Audio-tab mute buttons hit-test only while the Audio tab is active.
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Audio;
        let ui_audio = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        let (control, mute0) = audio_tab_button_rects(rect)[0];
        assert_eq!(control, UiControl::DebugAudioMute(0));
        let pos = (mute0.x as i32 + 2, mute0.y as i32 + 2);
        assert_eq!(
            ui_audio.control_at(pos, false, false),
            Some(UiControl::DebugAudioMute(0))
        );
        // The CD mute is the fifth (index 4) button.
        let (cd_control, cd_mute) = audio_tab_button_rects(rect)[4];
        assert_eq!(cd_control, UiControl::DebugAudioMute(4));
        let cd_pos = (cd_mute.x as i32 + 2, cd_mute.y as i32 + 2);
        assert_eq!(
            ui_audio.control_at(cd_pos, false, false),
            Some(UiControl::DebugAudioMute(4))
        );
        // On another tab that position does not resolve to a mute.
        assert_eq!(ui.control_at(pos, false, false), Some(UiControl::PanelBody));

        let mut panel = DebuggerPanel::new();
        for ch in ['c', '0', '0', '3', 'C'] {
            panel.push_entry_char(ch);
        }
        assert_eq!(panel.entry, "C003C");
        assert_eq!(panel.entry_addr(), Some(0xC003C));
        // Punctuation is rejected (letters/digits/space are kept for spec
        // mnemonics).
        panel.push_entry_char('!');
        assert_eq!(panel.entry, "C003C");
        panel.backspace_entry();
        assert_eq!(panel.entry, "C003");
        // Capped at the entry length (room for a conditional breakpoint spec).
        for _ in 0..50 {
            panel.push_entry_char('F');
        }
        assert_eq!(panel.entry.len(), 40);
    }

    #[test]
    fn flag_decoders_name_set_bits() {
        assert_eq!(dmacon_flags(0), "-");
        let flags = dmacon_flags(0x8390 & 0x7FFF);
        assert!(flags.contains("DMAEN"));
        assert!(flags.contains("BPLEN"));
        assert!(flags.contains("COPEN"));
        assert!(flags.contains("DSKEN"));
        assert!(!flags.contains("BLTEN"));

        let ints = int_flags((1 << 5) | (1 << 6) | (1 << 14));
        assert_eq!(ints, "INTEN BLIT VERTB");

        assert_eq!(sr_flags(0x2700), "S IPL7 xnzvc");
        assert_eq!(sr_flags(0x0015), "U IPL0 XnZvC");
        assert_eq!(sr_flags(0xA01F), "T S IPL0 XNZVC");
    }

    #[test]
    fn hex_dump_row_formats_address_hex_and_ascii() {
        let bytes: Vec<u8> = (0x41..0x51).collect();
        let row = hex_dump_row(0xC00000, &bytes);
        assert!(row.starts_with("C00000: 41 42 43"));
        assert!(row.ends_with("ABCDEFGHIJKLMNOP"));
    }

    #[test]
    fn parse_hex_entry() {
        assert_eq!(parse_hex_u32("C00000"), Some(0xC00000));
        assert_eq!(parse_hex_u32(""), None);
        assert_eq!(parse_hex_u32("xyz"), None);
    }

    #[test]
    fn entry_box_parses_address_and_poke_tokens() {
        let mut panel = DebuggerPanel::new();
        // The entry only accepts hex, space, and the P/S/R register letters.
        for ch in "C00000 DEAD".chars() {
            panel.push_entry_char(ch);
        }
        assert_eq!(panel.entry, "C00000 DEAD");
        // The address consumers see just the first token.
        assert_eq!(panel.entry_addr(), Some(0xC00000));
        // Memory poke takes both tokens; the address is forced even.
        assert_eq!(panel.poke_target(), Some((0xC00000, 0xDEAD)));

        // Leading/doubled spaces are collapsed, and punctuation never makes it
        // in (letters are allowed now, for register names and condition
        // mnemonics).
        let mut panel = DebuggerPanel::new();
        for ch in "  D0  1234!".chars() {
            panel.push_entry_char(ch);
        }
        assert_eq!(panel.entry, "D0 1234");
        assert_eq!(panel.reg_poke(), Some((0, 0x1234)));
    }

    #[test]
    fn break_spec_parses_address_condition_and_ignore() {
        // Bare address: plain breakpoint.
        assert_eq!(parse_break_spec("C033C2"), Some((0xC033C2, None, 0)));

        // Address plus a register/immediate condition.
        let (addr, cond, ignore) = parse_break_spec("C033C2 D0 EQ 5").unwrap();
        assert_eq!(addr, 0xC033C2);
        assert_eq!(ignore, 0);
        assert_eq!(
            cond,
            Some(BreakCond {
                lhs: CondOperand::Data(0),
                op: CondOp::Eq,
                rhs: CondOperand::Imm(5),
            })
        );

        // Memory operand, bit-test op, and a trailing ignore count.
        let (_, cond, ignore) = parse_break_spec("40 MC00002 AND 4000 IGN A").unwrap();
        assert_eq!(ignore, 0xA);
        assert_eq!(
            cond,
            Some(BreakCond {
                lhs: CondOperand::Mem(0xC00002),
                op: CondOp::And,
                rhs: CondOperand::Imm(0x4000),
            })
        );

        // Ignore count with no condition.
        assert_eq!(parse_break_spec("1234 IGN 3"), Some((0x1234, None, 3)));

        // Malformed condition and bad address are rejected.
        assert!(parse_break_spec("C033C2 D0 EQ").is_none());
        assert!(parse_break_spec("C033C2 D0 XX 5").is_none());
        assert!(parse_break_spec("xyz").is_none());
    }

    #[test]
    fn register_names_map_to_gdb_indices() {
        assert_eq!(parse_reg_name("D0"), Some(0));
        assert_eq!(parse_reg_name("d7"), Some(7));
        assert_eq!(parse_reg_name("A0"), Some(8));
        assert_eq!(parse_reg_name("A7"), Some(15));
        assert_eq!(parse_reg_name("SP"), Some(15));
        assert_eq!(parse_reg_name("SR"), Some(16));
        assert_eq!(parse_reg_name("PC"), Some(17));
        assert_eq!(parse_reg_name("D8"), None);
        assert_eq!(parse_reg_name("A8"), None);
        assert_eq!(parse_reg_name("Z0"), None);
        assert_eq!(parse_reg_name(""), None);
    }

    /// Render each panel and the menu into a presentation-sized frame.
    /// Always asserts the drawing landed inside the right region; with
    /// COPPERLINE_UI_PREVIEW=1 also saves PNGs for eyeballing layout.
    #[test]
    fn wrap_text_keeps_long_lines_whole() {
        // Short lines pass through untouched.
        assert_eq!(wrap_text("Machine: A1200", 32, 31), vec!["Machine: A1200"]);
        // Long lines wrap at word boundaries with nothing dropped.
        let rom = "ROM: system v3.1 a1200 release image path rom";
        let wrapped = wrap_text(rom, 32, 31);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|l| l.chars().count() <= 32));
        assert_eq!(wrapped.join(" "), rom);
        // Words longer than a whole line are hard-split, not dropped.
        let long_word = "a".repeat(70);
        let wrapped = wrap_text(&long_word, 32, 31);
        assert_eq!(wrapped.concat(), long_word);
        // Empty input still yields one (blank) line.
        assert_eq!(wrap_text("", 32, 31), vec![String::new()]);
    }

    #[test]
    fn frame_analyzer_top_edge_overlays_clip_to_raster() {
        use super::super::window::{texture_height, texture_width};

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let mut frame = vec![0u8; w * h * 4];
        let raster = Rect {
            x: 20,
            y: 20,
            w: 40,
            h: 20,
        };
        let trace = AnalyzerTraceView {
            frame: 1,
            seconds: 0.0,
            rows: 4,
            cols: 4,
            line_cck: 4,
            visible_start_vpos: 0,
            visible_lines: 2,
            display_hpos_start: 0,
            display_hpos_end: 4,
            owner_cck: [0; 9],
            blitter_busy_cck: 0,
            blitter_starve_cck: [0; 9],
            partial: false,
            selected_vpos: 0,
            selected_hpos: 0,
            selected_owner: "idle",
            selected_owner_code: b'.',
            owners: vec![b'.'; 16],
            markers: vec![AnalyzerMarker {
                vpos: 0,
                hpos: 1,
                offset: 0x096,
                value: 0x0000,
                source: "copper",
            }],
            selected_blit: None,
            diw_v: None,
            diw_h_cck: None,
            ddf_cck: None,
        };

        draw_owner_heatmap(&mut frame, raster, &trace, None, false, scale);

        let pixel = |frame: &[u8], x: usize, y: usize| -> [u8; 4] {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4]
                .try_into()
                .unwrap()
        };
        for x in raster.x - 4..raster.x + raster.w + 4 {
            assert_eq!(pixel(&frame, x, raster.y - 1), [0, 0, 0, 0]);
        }
        for y in raster.y..raster.y + raster.h {
            assert_eq!(pixel(&frame, raster.x - 1, y), [0, 0, 0, 0]);
        }
        assert_eq!(
            pixel(&frame, raster.x, raster.y),
            BUTTON_EDGE_LIGHT.to_le_bytes()
        );
    }

    #[test]
    fn panels_render_into_their_rects() {
        use super::super::window::{texture_height, texture_width};

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let save = |frame: &[u8], name: &str| {
            if !crate::envcfg::flag("COPPERLINE_UI_PREVIEW") {
                return;
            }
            let path = format!("target/ui-preview-{name}.png");
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(frame).unwrap();
            eprintln!("saved {path}");
        };
        // Neutral labels for panels whose draw does not depend on them.
        let menu_labels = || MenuLabels {
            warp: false,
            warp_speed: WarpSpeed::Max,
            fullscreen: false,
            status_bar_hidden: false,
            recording: false,
            input_recording: false,
            rewind: false,
            save_slot: 1,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [
                crate::bus::PortDevice::Mouse,
                crate::bus::PortDevice::Joystick,
            ],
            pixel_aspect: PixelAspect::Tv,
            floppy_speed: 100,
            midi_in: "",
            midi_out: "",
            audio_output: "",
            audio_filter: crate::config::AudioFilterMode::Auto,
            sampler_input: "",
            sampler_gain: "",
            shader: crate::config::ShaderKind::None,
        };
        let panel_has_title_bar = |frame: &[u8], panel: &Panel| {
            let rect = panel_rect(panel);
            let probe = ((rect.y + 10) * w + rect.x + 4) * 4;
            let pixel = &frame[probe..probe + 4];
            pixel == PANEL_TITLE_BG.to_le_bytes()
        };

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: true,
            menu_scroll: 0,
            panel: None,
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            None,
            false,
            false,
            MenuLabels {
                warp: true,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        let menu = menu_rect(menu_items(false, false).len());
        let probe = ((menu.y + MENU_PAD + 2) * w + menu.x + 4) * 4;
        assert_eq!(&frame[probe..probe + 4], &MENU_BG.to_le_bytes());
        save(&frame, "menu");

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::About),
        };
        let data = PanelViewData::About(AboutView {
            machine_lines: vec![
                "Machine: A1200".to_string(),
                "CPU: M68EC020 @ 14 MHz".to_string(),
                "Chipset: AGA (Alice/Lisa, PAL)".to_string(),
                "RAM: 2048K chip, 4096K fast".to_string(),
                "ROM: system v3.1 a1200 release image path rom".to_string(),
                "Floppy drives: 1".to_string(),
            ],
        });
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "about");

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Shortcuts),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            Some(&PanelViewData::Shortcuts),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "shortcuts");

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::DropChooser(DropChooserState {
                disks: vec![
                    std::path::PathBuf::from("turrican2-disk1.adf"),
                    std::path::PathBuf::from("turrican2-disk2.adf"),
                ],
                disk_label: "turrican2-disk1.adf".to_string(),
                drives: vec![
                    DropDriveEntry {
                        drive: 0,
                        label: "DF0: workbench.adf".to_string(),
                    },
                    DropDriveEntry {
                        drive: 1,
                        label: "DF1 (empty)".to_string(),
                    },
                ],
            })),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DropDrive(1)),
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        // The hovered drive button renders inside the panel rect.
        let panel = ui.panel.as_ref().unwrap();
        if let Panel::DropChooser(state) = panel {
            let rect = panel_rect(panel);
            let buttons = drop_chooser_button_rects(rect, state);
            assert_eq!(buttons.len(), 2);
            assert_eq!(buttons[1].0, UiControl::DropDrive(1));
            let button = buttons[1].1;
            assert!(button.x >= rect.x && button.x + button.w <= rect.x + rect.w);
            assert!(button.y >= rect.y && button.y + button.h <= rect.y + rect.h);
            let probe = ((button.y + 2) * w + button.x + 2) * 4;
            assert_eq!(&frame[probe..probe + 4], &BUTTON_FACE_HOVER.to_le_bytes());
        } else {
            unreachable!();
        }
        save(&frame, "drop-chooser");

        // The pre-drop hover hint dims the display without opening a panel.
        let mut frame = vec![0xFFu8; w * h * 4];
        draw_drop_hint(&mut frame, scale);
        // The scrim darkens the display area but not the status bar below.
        assert!(frame[0] < 0xFF);
        assert_eq!(frame[present_height() * w * 4], 0xFF);
        save(&frame, "drop-hint");

        let mut frame = vec![0u8; w * h * 4];
        let session = crate::gamepad::CalibrationSession::new();
        let rows = (0..crate::gamepad::CalibrationSession::step_count())
            .map(|index| CalRow {
                label: crate::gamepad::CalibrationSession::step_label(index),
                binding: if index == 0 {
                    "axis 10031-".to_string()
                } else {
                    String::new()
                },
                current: index == 1,
            })
            .collect();
        let data = PanelViewData::Calibration(CalibrationView {
            pad_line: "Controller: USB Retro Pad".to_string(),
            rows,
            status: "Push and hold the control on the pad.".to_string(),
        });
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Calibration(session)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::CalCancel),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "calibration");

        // Input Mapping: self-contained, with a row armed for capture so the
        // highlighted state is drawn too.
        let mut frame = vec![0u8; w * h * 4];
        let mut map_panel = InputMapPanel::new(crate::keymap::KeyMap::default());
        map_panel.capturing = Some(crate::keymap::JoyControl::Fire);
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::InputMap(Box::new(map_panel))),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::RemapSave),
            None,
            false,
            false,
            menu_labels(),
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "input-mapping");

        let mut frame = vec![0u8; w * h * 4];
        let mut lines = vec![
            DbgLine::plain("PC 00FC0E44   SR 2700 [S IPL7 xnzvc]"),
            DbgLine::plain(""),
            DbgLine::plain("D0 00000000   D1 00000001   D2 00C00FFC   D3 DEADBEEF"),
            DbgLine::plain("A0 00DFF000   A1 00C00000   A2 00000000   A3 00FC0000"),
            DbgLine::plain(""),
        ];
        for i in 0..20 {
            let line = format!("00FC{:04X}  MOVE.W #$4000,(A0)", 0x0E44 + i * 4);
            lines.push(if i == 0 {
                DbgLine::hilit(line)
            } else {
                DbgLine::plain(line)
            });
        }
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: false,
            reverse_available: true,
            status: "paused frame 1234 24.68s".to_string(),
            lines,
            bitmap: None,
            video: None,
            audio: None,
        }));
        let mut panel = DebuggerPanel::new();
        panel.entry = "C00000".to_string();
        panel.entry_active = true;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugStep),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "debugger");

        // Break tab: toggle buttons plus the breakpoint/watch listing.
        let mut frame = vec![0u8; w * h * 4];
        let mut lines: Vec<DbgLine> = (0..BREAK_TAB_HEADER_LINES)
            .map(|_| DbgLine::plain(""))
            .collect();
        lines.push(DbgLine::hilit("Breakpoint at $C033C2"));
        lines.push(DbgLine::plain(""));
        lines.push(DbgLine::plain("Breakpoints:"));
        lines.push(DbgLine::plain("  $C033C2"));
        lines.push(DbgLine::plain("Watchpoints (word):"));
        lines.push(DbgLine::plain("  $C09580  now 0012"));
        lines.push(DbgLine::plain("Register watches (stop on write):"));
        lines.push(DbgLine::plain("  DMACON ($096)"));
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: false,
            reverse_available: true,
            status: "paused frame 1234 24.68s".to_string(),
            lines,
            bitmap: None,
            video: None,
            audio: None,
        }));
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Break;
        panel.entry = "DFF096".to_string();
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugRegToggle),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "debugger-break");

        // Waveform tab: Arm/Stop buttons plus a capture status listing.
        let mut frame = vec![0u8; w * h * 4];
        let mut lines: Vec<DbgLine> = (0..WAVEFORM_TAB_HEADER_LINES)
            .map(|_| DbgLine::plain(""))
            .collect();
        lines.push(DbgLine::hilit(
            "waveform capturing: trigger pc=0xC033C2, duration 2f, signals all",
        ));
        lines.push(DbgLine::plain("  -> out.vcd"));
        lines.push(DbgLine::plain("  14204 / 141748 cck, 35872 samples"));
        lines.push(DbgLine::plain(""));
        lines.push(DbgLine::plain(
            "Trigger:  NOW  PC=ADDR  BEAM=VPOS[:HPOS]  REG=OFF  TIME=SECS",
        ));
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: true,
            reverse_available: false,
            status: "running frame 1234 24.68s".to_string(),
            lines,
            bitmap: None,
            video: None,
            audio: None,
        }));
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Waveform;
        panel.entry = "PC=C033C2 2F".to_string();
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugWaveArm),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        // The Arm button must be enabled: its entry spec parses.
        assert!(crate::waveform::parse_wave_args("PC=C033C2 2F".split_whitespace()).is_ok());
        save(&frame, "debugger-waveform");

        // Audio tab: the four Paula channels plus CD, with representative
        // state, mute buttons (AUD2 shown muted), and synthetic scope traces.
        let mut frame = vec![0u8; w * h * 4];
        let wave = |amp: f32, cycles: f32| -> Vec<i8> {
            (0..220)
                .map(|i| {
                    let t = i as f32 / 220.0 * cycles * std::f32::consts::TAU;
                    (amp * t.sin()) as i8
                })
                .collect()
        };
        let header = "DMACON 8203  DMAEN on  AUDEN 1 1 . .   ADKCON 0000  -".to_string();
        let channels = vec![
            AudioRowView {
                text: vec![
                    DbgLine::hilit("AUD0 [Running]  DMA on  IRQ -"),
                    DbgLine::plain("  LC 021A3C  LEN 0140  PER 01B0  VOL 40"),
                    DbgLine::plain("  PTR 021B1C  words 00E2  acc 00A4  ph1  out -12"),
                    DbgLine::plain("  pending: next-word"),
                ],
                muted: false,
                scope: wave(96.0, 3.0),
            },
            AudioRowView {
                text: vec![
                    DbgLine::plain("AUD1 [StartPending]  DMA on  IRQ pend"),
                    DbgLine::plain("  LC 030000  LEN 0080  PER 00F0  VOL 3F"),
                    DbgLine::plain("  PTR 030000  words 0080  acc 0000  ph0  out 0"),
                    DbgLine::plain("  pending: dma-req"),
                ],
                muted: false,
                scope: wave(60.0, 6.0),
            },
            AudioRowView {
                text: vec![
                    DbgLine::plain("AUD2 [Off]  DMA off  IRQ -"),
                    DbgLine::plain("  LC 000000  LEN 0000  PER 0000  VOL 00"),
                    DbgLine::plain("  PTR 000000  words 0000  acc 0000  ph0  out 0"),
                ],
                muted: true,
                scope: wave(40.0, 2.0),
            },
            AudioRowView {
                text: vec![
                    DbgLine::plain("AUD3 [Manual]  DMA off  IRQ -"),
                    DbgLine::plain("  LC 000000  LEN 0000  PER 0140  VOL 20"),
                    DbgLine::plain("  PTR 000000  words 0000  acc 0050  ph1  out 7"),
                    DbgLine::plain("  pending: dma-disable manual"),
                ],
                muted: false,
                scope: wave(48.0, 9.0),
            },
        ];
        let cd = AudioRowView {
            text: vec![
                DbgLine::hilit("CD-DA  playing"),
                DbgLine::plain("  peak  72"),
            ],
            muted: false,
            scope: wave(72.0, 4.0),
        };
        let audio = AudioScopeView {
            header,
            channels,
            cd,
        };
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: false,
            reverse_available: true,
            status: "paused frame 1234 24.68s".to_string(),
            lines: Vec::new(),
            bitmap: None,
            video: None,
            audio: Some(audio),
        }));
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Audio;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugAudioMute(0)),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "debugger-audio");

        // IO Map tab: the register grid with a selection and decode pane.
        let mut frame = vec![0u8; w * h * 4];
        let mut lines: Vec<DbgLine> = Vec::new();
        lines.push(DbgLine::plain(
            "custom registers $DFF000-$DFF1FE  (page 2/4; arrows/wheel move, $ box jumps)",
        ));
        lines.push(DbgLine::plain(""));
        for row in 0..26 {
            let mut text = String::new();
            for col in 0..3 {
                let off = 0x0A0 + (col * 26 + row) * 2;
                let cursor = if off == 0x0100 { '>' } else { ' ' };
                text.push_str(&format!(
                    "{cursor}{off:03X} {:<8} {:04X}   ",
                    crate::debugger::custom_reg_name(off as u16),
                    0x2200 + off
                ));
            }
            lines.push(if row == 16 {
                DbgLine::hilit(text.trim_end().to_string())
            } else {
                DbgLine::plain(text.trim_end().to_string())
            });
        }
        lines.push(DbgLine::plain(""));
        lines.push(DbgLine::hilit("$100 BPLCON0 = $5A00".to_string()));
        lines.push(DbgLine::plain("  HAM COLOR".to_string()));
        lines.push(DbgLine::plain("  BPU=5".to_string()));
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: false,
            reverse_available: true,
            status: "paused frame 1234 24.68s".to_string(),
            lines,
            bitmap: None,
            video: None,
            audio: None,
        }));
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::IoMap;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "debugger-iomap");

        // Video tab: layer-isolation toggles (plane 2 and sprite 5 hidden),
        // sprite rows with synthetic thumbnails, and an AGA palette grid.
        let mut frame = vec![0u8; w * h * 4];
        let sprites = (0..8)
            .map(|sprite| {
                let rows = 16 + sprite;
                let mut thumb = vec![0u32; rows * 16];
                for row in 0..rows {
                    for x in 0..16usize {
                        if (x + row) % 4 == sprite % 4 {
                            thumb[row * 16 + x] =
                                rgba(80 + 20 * sprite as u32, 200 - 20 * sprite as u32, 160);
                        }
                    }
                }
                SpriteRowView {
                    text: format!(
                        "SPR{sprite} v44-{} h{} dma lines {rows}",
                        60 + sprite,
                        128 + sprite * 16
                    ),
                    thumb,
                    thumb_rows: rows,
                }
            })
            .collect();
        let palette = (0..256)
            .map(|idx| {
                let idx = idx as u32;
                rgba((idx * 5) & 0xFF, (idx * 3) & 0xFF, 255 - (idx & 0xFF))
            })
            .collect();
        let data = PanelViewData::Debugger(Box::new(DebuggerView {
            running: false,
            reverse_available: true,
            status: "paused frame 1234 24.68s".to_string(),
            lines: Vec::new(),
            bitmap: None,
            video: Some(VideoView {
                header: "BPLCON0 5200: 5 planes lores  HAM   DMACON: BPLEN on SPREN on".to_string(),
                plane_mask: 0xFD,
                nplanes: 5,
                sprite_mask: 0xDF,
                sprites,
                palette,
            }),
            audio: None,
        }));
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Video;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugPlaneToggle(0)),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "debugger-video");

        // Frame analyzer with the picture underlay ticked: a synthetic PAL
        // frame trace (refresh/bitplane/copper/blitter stripes) over a
        // gradient picture, to eyeball the beam-grid alignment of the
        // underlay against the white display box.
        let mut frame = vec![0u8; w * h * 4];
        let (rows, cols) = (312usize, 227usize);
        let mut owners = vec![b'.'; rows * cols];
        for vpos in 0..rows {
            for hpos in 0..cols {
                let owner = if hpos < 4 {
                    b'R'
                } else if (60..260).contains(&vpos) && (0x38..0xD0).contains(&hpos) && hpos % 2 == 0
                {
                    b'B'
                } else if hpos == 0x28 && vpos % 8 == 0 {
                    b'C'
                } else if (100..140).contains(&vpos) && (0x10..0x28).contains(&hpos) {
                    b'L'
                } else if (0x0D..0x11).contains(&hpos) && vpos % 2 == 0 {
                    b'A'
                } else {
                    b'.'
                };
                owners[vpos * cols + hpos] = owner;
            }
        }
        let underlay_rows = 285usize;
        let mut under_fb = vec![0u32; FB_WIDTH * underlay_rows];
        for (i, pix) in under_fb.iter_mut().enumerate() {
            let (x, y) = (i % FB_WIDTH, i / FB_WIDTH);
            // Gradient plus vertical bars so structure is visible through
            // the dimming.
            let bar = if (x / 64) % 2 == 0 { 96 } else { 0 };
            *pix = rgba(
                (x * 255 / FB_WIDTH) as u32,
                (y * 255 / underlay_rows) as u32 / 2 + bar,
                160,
            );
        }
        let trace = AnalyzerTraceView {
            frame: 1234,
            seconds: 24.68,
            rows,
            cols,
            line_cck: 227,
            visible_start_vpos: 0x1A,
            visible_lines: underlay_rows,
            display_hpos_start: 0x30,
            display_hpos_end: 227,
            owner_cck: [4400, 19000, 0, 0, 1600, 900, 2400, 6200, 36000],
            blitter_busy_cck: 3000,
            blitter_starve_cck: [0, 400, 0, 0, 0, 0, 0, 200, 0],
            partial: false,
            selected_vpos: 120,
            selected_hpos: 0x40,
            selected_owner: "bitplane",
            selected_owner_code: b'B',
            owners,
            markers: vec![AnalyzerMarker {
                vpos: 0x40,
                hpos: 0x28,
                offset: 0x180,
                value: 0x0F00,
                source: "copper",
            }],
            selected_blit: Some("in blit #2 (20x100 D $060000)".to_string()),
            // A standard PAL display window and fetch bounds, so the
            // preview shows the DIW box and DDF verticals.
            diw_v: Some((0x2C, 0x12C)),
            diw_h_cck: Some((0x81 / 2, 0x1C1 / 2)),
            ddf_cck: Some((0x38, 0xD0)),
        };
        let data = PanelViewData::FrameAnalyzer(Box::new(FrameAnalyzerView {
            running: false,
            status: "paused frame 1234 24.68s".to_string(),
            scrub: true,
            heat: None,
            trace: Some(trace),
            underlay: Some(AnalyzerUnderlayView {
                fb: std::rc::Rc::new(under_fb),
                rows: underlay_rows,
                width: FB_WIDTH,
            }),
        }));
        let mut panel = FrameAnalyzerPanel::new();
        panel.show_underlay = true;
        panel.show_scrub = true;
        panel.selected_vpos = 120;
        panel.selected_hpos = 0x40;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::FrameAnalyzer(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::AnalyzerUnderlay),
            Some(&data),
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "frame-analyzer");

        // Frame analyzer, Memory tab: the address space instead of the
        // beam. A window with a few busy regions so the map, the census
        // column and the selected-cell readout all have something to say.
        let mut frame = vec![0u8; w * h * 4];
        let mut lit = Vec::new();
        for cell in 0..heatmap::CELLS {
            let (cx, cy) = (cell % heatmap::GRID, cell / heatmap::GRID);
            // A bitplane buffer as a solid block, a copper list as a
            // column, blitter and CPU traffic scattered through the heap.
            let toucher = if (24..56).contains(&cy) && cx < 200 {
                Some(crate::heatmap::Toucher::Bitplane)
            } else if cx == 12 && (8..40).contains(&cy) {
                Some(crate::heatmap::Toucher::Copper)
            } else if (60..70).contains(&cy) && (cx / 8) % 3 == 0 {
                Some(crate::heatmap::Toucher::Blitter)
            } else if cy > 200 && (cx * cy) % 97 == 0 {
                Some(crate::heatmap::Toucher::CpuWrite)
            } else if cy == 3 && cx % 5 == 0 {
                Some(crate::heatmap::Toucher::Audio)
            } else {
                None
            };
            if let Some(toucher) = toucher {
                lit.push((cell, toucher));
            }
        }
        let mut heat = heat_view(&lit);
        let selected = 40 * heatmap::GRID + 100;
        heat.selected = Some(AnalyzerHeatCell {
            cell: selected,
            toucher: Some(crate::heatmap::Toucher::Bitplane.name()),
            colour: crate::heatmap::Toucher::Bitplane.colour(),
            age_frames: Some(1),
        });
        let data = PanelViewData::FrameAnalyzer(analyzer_view(None, Some(heat)));
        let mut panel = FrameAnalyzerPanel::new();
        panel.tab = AnalyzerTab::Memory;
        panel.heat_selected = Some(selected);
        panel.heat_presets = vec![
            heat_preset("Chip", 0, 0x0020_0000),
            heat_preset("Slow", 0x00C0_0000, 0x0010_0000),
            heat_preset("Fast", 0x0020_0000, 0x0080_0000),
            heat_preset("24-bit", 0, heatmap::DEFAULT_SPAN),
        ];
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::FrameAnalyzer(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::AnalyzerTab(AnalyzerTab::Memory)),
            Some(&data),
            false,
            false,
            menu_labels(),
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "frame-analyzer-memory");

        // Console: a session transcript over the prompt line.
        let mut frame = vec![0u8; w * h * 4];
        let mut console = ConsolePanel::default();
        console.push_output("Copperline debugger console. Type HELP for commands.");
        console.push_output("> B C033C2");
        console.push_output("breakpoint $C033C2 set");
        console.push_output("> RUN");
        console.push_output("running (PAUSE stops; breakpoints report here or on stop)");
        console.push_output("> PAUSE");
        console.push_output("!Breakpoint at $C033C2");
        console.push_output(
            "pc $C033C2  MOVE.W #$4000,$00DFF09A   sr 2300  beam v44 h101  frame 1234",
        );
        console.push_output("> D");
        console.push_output("C033C2  MOVE.W #$4000,$00DFF09A");
        console.push_output("C033C8  RTS");
        console.input = "MEM C00000 40".to_string();
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Console(console)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "console");

        // Configuration screen: an A1200 on the Memory tab.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.setup.select_model(Some(MachineModel::A1200));
        state
            .setup
            .set_path(LauncherField::Rom, std::path::PathBuf::from("kick31.rom"));
        state.tab = LauncherTab::Memory;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::LauncherRun),
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "launcher");

        // Configuration screen: the Zorro tab with a WASM plugin board whose
        // config-option schema renders an editable field per option.
        let manifest_path = std::env::temp_dir().join(format!(
            "copperline-ui-preview-board-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &manifest_path,
            r#"
            name = "Demo NIC"
            zorro = 2
            type = "wasm"
            size = "64K"
            manufacturer = 5192
            product = 16
            wasm = "demo.wasm"
            [config]
            mode = "bridged"
            [[option]]
            key = "mode"
            label = "Mode"
            type = "enum"
            choices = ["bridged", "nat"]
            [[option]]
            key = "verbose"
            label = "Verbose"
            type = "bool"
            [[option]]
            key = "mtu"
            label = "MTU"
            type = "int"
            default = 1500
            [[option]]
            key = "rom"
            label = "Boot ROM"
            type = "file"
            [[option]]
            key = "mac"
            label = "MAC address"
            type = "string"
            default = "02:00:10:00:00:01"
        "#,
        )
        .unwrap();
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.setup.add_zorro(manifest_path.clone());
        state.tab = LauncherTab::Zorro;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "launcher-zorro");
        let _ = std::fs::remove_file(&manifest_path);

        // Configuration screen: the Storage tab on an A1200, with an IDE
        // master mounted from a host directory and given a volume-name
        // override (the editable box beside Browse).
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.setup.select_model(Some(MachineModel::A1200));
        state.setup.set_path(
            LauncherField::IdeMaster,
            std::path::PathBuf::from("/host/games"),
        );
        state
            .setup
            .set_drive_name(LauncherField::IdeMaster, "Games".to_string());
        state.tab = LauncherTab::Storage;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "launcher-storage");

        // Configuration screen: the Input tab, with the live routing
        // summary spelled out under the rows (two joysticks, so the
        // numpad stand-in line shows).
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        // Port 1: Mouse -> Joystick, making a two-stick setup.
        state.setup.cycle(LauncherField::Port1Device, true);
        state.tab = LauncherTab::Input;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            None,
            false,
            false,
            MenuLabels {
                warp: false,
                warp_speed: WarpSpeed::Max,
                fullscreen: false,
                status_bar_hidden: false,
                recording: false,
                input_recording: false,
                rewind: false,
                save_slot: 1,
                autofire_hz: 0,
                joystick_input_mode: JoystickInputMode::Gamepad,
                port_devices: [
                    crate::bus::PortDevice::Mouse,
                    crate::bus::PortDevice::Joystick,
                ],
                pixel_aspect: PixelAspect::Tv,
                floppy_speed: 100,
                midi_in: "",
                midi_out: "",
                audio_output: "",
                audio_filter: crate::config::AudioFilterMode::Auto,
                sampler_input: "",
                sampler_gain: "",
                shader: crate::config::ShaderKind::None,
            },
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        // The summary header landed below the rows: some text pixel is lit
        // on its line inside the settings pane.
        let rect = panel_rect(&Panel::Launcher(Box::new(LauncherState::new(
            launcher::MachineSetup::default(),
        ))));
        let header_y = launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::Input,
                crate::config::ParallelDevice::None,
                crate::config::SerialMode::default(),
            )
            .len()
                + 1,
        );
        let row = &frame[(header_y * w + launcher_pane_x(rect)) * 4
            ..(header_y * w + launcher_pane_x(rect) + 200) * 4];
        assert!(
            row.chunks_exact(4)
                .any(|px| px == PANEL_TEXT_DIM.to_le_bytes()),
            "routing summary header not drawn"
        );
        save(&frame, "launcher-input");

        // Configuration screen: the I/O Ports tab, with the sampler selected so
        // both the Serial: and Parallel: sections and the sampler rows show.
        let labels = || MenuLabels {
            warp: false,
            warp_speed: WarpSpeed::Max,
            fullscreen: false,
            status_bar_hidden: false,
            recording: false,
            input_recording: false,
            rewind: false,
            save_slot: 1,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [
                crate::bus::PortDevice::Mouse,
                crate::bus::PortDevice::Joystick,
            ],
            pixel_aspect: PixelAspect::Tv,
            floppy_speed: 100,
            midi_in: "",
            midi_out: "",
            audio_output: "",
            audio_filter: crate::config::AudioFilterMode::Auto,
            sampler_input: "",
            sampler_gain: "",
            shader: crate::config::ShaderKind::None,
        };
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        state.setup.cycle(LauncherField::ParallelDevice, true); // None -> Printer
        state.setup.cycle(LauncherField::ParallelDevice, true); // Printer -> Sampler
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-io-ports");

        // I/O Ports with the A2065 on the NAT backend, to check the
        // non-determinism warning under the rows.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        // Not fitted -> Isolated -> Loopback -> NAT (where the NAT cannot
        // come up this wraps back to Not fitted and no warning is shown).
        for _ in 0..3 {
            state.setup.cycle(LauncherField::Ethernet, true);
        }
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-ethernet-warning");

        // I/O Ports with the printer selected and a long output path set, to
        // check the "Output file" value and the Browse/Clear placement.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        state.setup.cycle(LauncherField::ParallelDevice, true); // None -> Printer
        state.setup.set_path(
            LauncherField::ParallelOutput,
            std::path::PathBuf::from("/Users/me/Documents/amiga/captures/printer-output.txt"),
        );
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-printer");

        // The Host Mounts sub-page reached from the Storage tab.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::HostFs;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-host-mounts");

        // The Boot Priority sub-page: an A1200 with two IDE drives -- the master
        // bootable at 5, the slave with its Bootable box cleared (priority
        // greyed) -- and the empty SCSI slots greyed "no drive".
        let mut frame = vec![0u8; w * h * 4];
        let mut setup = launcher::MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        setup.set_path(LauncherField::IdeMaster, std::path::PathBuf::from("wb.hdf"));
        setup.set_drive_bootpri(LauncherField::IdeMasterBoot, Some(5));
        setup.set_path(
            LauncherField::IdeSlave,
            std::path::PathBuf::from("games.hdf"),
        );
        setup.toggle_drive_boot(LauncherField::IdeSlaveBoot);
        let mut state = LauncherState::new(setup);
        state.tab = LauncherTab::BootPriority;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-boot-priority");

        // A/V & Emu: the Audio category (the default landing), with the
        // Audio / Video / Emulation nav buttons at the top, Audio highlighted.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::AvAudio;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-av-audio");

        // The Video category, reached from the same nav row.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::AvVideo;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-av-video");

        // The Floppy tab with two drives wired in: each drive is a greyed "DFn:"
        // heading with indented settings; DF2/DF3 are hidden until enabled.
        let mut frame = vec![0u8; w * h * 4];
        let mut setup = launcher::MachineSetup::default();
        while setup.value_label(LauncherField::FloppyDrives) != "2" {
            setup.cycle(LauncherField::FloppyDrives, true);
        }
        setup.set_path(
            LauncherField::Df0Image,
            std::path::PathBuf::from("workbench.adf"),
        );
        let mut state = LauncherState::new(setup);
        state.tab = LauncherTab::Floppy;
        let ui = UiState {
            menu_open: false,
            menu_scroll: 0,
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None, false, false, labels());
        save(&frame, "launcher-floppy");
    }
}

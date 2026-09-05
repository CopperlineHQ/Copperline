// SPDX-License-Identifier: GPL-3.0-or-later

//! In-window menu and overlay sub-windows (about, keyboard shortcuts,
//! gamepad calibration, debugger). Everything is drawn into the
//! presentation texture over the emulated display, styled after the
//! classic Amiga look: white menus with inverted highlights and blue
//! window title bars. This module owns layout, hit-testing and drawing;
//! `window.rs` routes events to it and builds the per-frame view data
//! (register snapshots, disassembly text) the panels render.

use super::launcher::{self, EditTarget, LauncherField, LauncherState, LauncherTab, RowKind};
use super::menu;
use super::window::{
    draw_rect_bevel, fill_rect, fill_rect_blend, rgba, scale_rect, texture_width, Rect,
    BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE, BUTTON_FACE_HOVER,
};
use super::{font, present_height, FB_WIDTH, HOST_SHORTCUT_MODIFIER_LABEL};
use crate::config::MachineModel;
use crate::debugger::{BreakCond, CondOp, CondOperand};
use crate::heatmap;

mod configuration;
pub(crate) use configuration::HOST_DISK_VISIBLE_ROWS;
use configuration::*;
pub(in crate::video) use configuration::{clip_path_to_chars, control_live, SAVE_ACTIONS};
#[cfg(feature = "game-library")]
pub(in crate::video) use configuration::{
    library_favourite_rows, library_version_max, library_visible_rows,
};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

const MENU_HILIGHT_BG: u32 = rgba(0, 85, 170);
const MENU_HILIGHT_TEXT: u32 = rgba(255, 255, 255);
const PANEL_BG: u32 = rgba(30, 32, 36);
const PANEL_TITLE_BG: u32 = rgba(0, 85, 170);
const PANEL_TITLE_TEXT: u32 = rgba(255, 255, 255);
pub(in crate::video) const PANEL_TEXT: u32 = rgba(214, 216, 208);
pub(in crate::video) const PANEL_TEXT_DIM: u32 = rgba(136, 138, 130);
pub(in crate::video) const PANEL_TEXT_HILIGHT: u32 = rgba(120, 255, 150);
const PANEL_TEXT_ACCENT: u32 = rgba(255, 184, 80);
const BUTTON_TEXT: u32 = rgba(220, 222, 214);
const BUTTON_TEXT_DISABLED: u32 = rgba(120, 120, 112);
/// DDF fetch-bound verticals on the Frame Analyzer heatmap.
const DDF_LINE: u32 = rgba(80, 200, 220);
const ENTRY_BG: u32 = rgba(8, 10, 8);
/// The mark inside a ticked box.
const TICK_GREEN: u32 = rgba(72, 214, 96);

/// The variants the second filesystem row offers. Plain is not among them:
/// it is what none of these being ticked means.
const FS_VARIANTS: [crate::diskimage::Variant; 3] = [
    crate::diskimage::Variant::Intl,
    crate::diskimage::Variant::DirCache,
    crate::diskimage::Variant::LongName,
];
const ENTRY_TEXT: u32 = rgba(27, 220, 71);
/// The veil an overlay draws over what it covers: enough to throw the
/// overlay forward and to say the machine is not listening, while
/// leaving what is behind readable. One tint for the menu and every
/// dialog alike -- two of them differing was a step you could see.
const SCRIM: u32 = rgba(8, 9, 11);
const SCRIM_ALPHA: f32 = 0.45;
// Audio-tab oscilloscope trace colours for the four Paula channels.
const AUDIO_SCOPE_COLORS: [u32; 4] = [
    rgba(120, 255, 150), // ch0 green
    rgba(96, 200, 255),  // ch1 cyan
    rgba(230, 130, 245), // ch2 magenta
    rgba(240, 214, 96),  // ch3 yellow
];

/// Trace colour for a line-mixed source row (CD-DA, MIDI synth, Toccata,
/// MHI).
fn audio_extra_color(kind: AudioExtraKind) -> u32 {
    match kind {
        AudioExtraKind::Cd => rgba(255, 170, 90),       // amber
        AudioExtraKind::Synth => rgba(160, 160, 255),   // lavender
        AudioExtraKind::Toccata => rgba(120, 235, 235), // teal
        AudioExtraKind::Mhi => rgba(255, 130, 150),     // coral
    }
}
const AUDIO_MUTE_FACE: u32 = rgba(96, 44, 44);

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

/// Status-bar anchor for the menu button; the pop-up opens above it.
pub const MENU_BUTTON_X: usize = FB_WIDTH - 220;
pub const MENU_BUTTON_W: usize = 22;

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
    /// Recorded blits with exact source/result reconstruction.
    Blits,
    Memory,
    /// The debug resources the guest registered through the uaelib trap
    /// (crate::uaelib): a table plus a decoded preview of the selection.
    Resources,
}

pub const ANALYZER_TABS: [AnalyzerTab; 4] = [
    AnalyzerTab::Beam,
    AnalyzerTab::Blits,
    AnalyzerTab::Memory,
    AnalyzerTab::Resources,
];

fn analyzer_tab_label(tab: AnalyzerTab) -> &'static str {
    match tab {
        AnalyzerTab::Beam => "Beam",
        AnalyzerTab::Blits => "Blits",
        AnalyzerTab::Memory => "Memory",
        AnalyzerTab::Resources => "Resources",
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
    /// CPU wait view: paint the slots the CPU waited through in the colour
    /// of whoever denied them, dim everything else, and swap the counters
    /// column for the wait breakdown and the top stalled PCs.
    pub show_cpu_wait: bool,
    /// Memory tab: the address-space windows offered as buttons. Empty
    /// until window.rs builds them from the machine's memory map.
    pub heat_presets: Vec<HeatPreset>,
    /// Memory tab: the pinned cell (an index into the 256x256 grid) whose
    /// address range and last toucher are reported under the map.
    pub heat_selected: Option<usize>,
    /// Resources tab: the selected resource, keyed by its guest address so
    /// the selection survives registry churn between draw and click.
    pub resource_selected: Option<u32>,
    /// Resources tab: the first listed registry entry (cursor keys
    /// scroll), so a registry larger than the table stays reachable.
    pub resource_scroll: usize,
    /// Blits tab selection, stable across the same cross-frame record.
    pub blit_selected: Option<u64>,
    /// Blits tab: first listed record, kept in step with the keyboard
    /// selection so every captured blit remains reachable.
    pub blit_scroll: usize,
}

impl FrameAnalyzerPanel {
    pub fn new() -> Self {
        Self {
            tab: AnalyzerTab::Beam,
            selected_vpos: 0x2C,
            selected_hpos: 0x28,
            show_underlay: false,
            show_scrub: false,
            show_cpu_wait: false,
            heat_presets: Vec::new(),
            heat_selected: None,
            resource_selected: None,
            resource_scroll: 0,
            blit_selected: None,
            blit_scroll: 0,
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
    /// The menu as it stood when it was opened, and how far into it the
    /// cursor has gone. Built once per open, from the machine at that
    /// moment, so nothing it offers can change under the pointer.
    pub menu_rows: Vec<menu::MenuRow>,
    pub menu_nav: menu::MenuNav,
    pub panel: Option<Panel>,
}

impl UiState {
    /// Whether the UI is consuming pointer/keyboard input.
    pub fn active(&self) -> bool {
        self.menu_open || self.panel.is_some()
    }

    /// The UI control under `pos`, if any. `PanelBody` swallows clicks on a
    /// panel's background so they never reach the emulated display.
    pub fn control_at(&self, pos: (i32, i32)) -> Option<UiControl> {
        if self.menu_open {
            // The menu answers for itself: a level, and a row in it.
            let pos = (pos.0.max(0) as usize, pos.1.max(0) as usize);
            return menu_hit(&self.menu_rows, &self.menu_nav, pos)
                .map(|(depth, row)| UiControl::MenuRow { depth, row });
        }
        self.panel
            .as_ref()
            .and_then(|panel| panel_control_at(panel, pos))
    }
}

pub fn panel_control_at(panel: &Panel, pos: (i32, i32)) -> Option<UiControl> {
    let rect = panel_rect(panel);
    // A dialog over the panel answers first, its own close gadget
    // included; the panel's must not close the launcher out from under it.
    #[cfg(feature = "game-library")]
    let modal =
        matches!(panel, Panel::Launcher(state) if state.login.is_some() || state.meta.is_some());
    #[cfg(not(feature = "game-library"))]
    let modal = false;
    // The confirm, then the Save menu: each answers before anything under
    // it, including the close gadget, because while one is up it is the
    // only thing being asked.
    if let Panel::Launcher(state) = panel {
        if state.confirm_reset {
            let (yes, _) = launcher_confirm_button_rects(rect);
            if yes.contains(pos) {
                return Some(UiControl::LauncherConfirmReset);
            }
            if close_button_rect(launcher_confirm_rect(rect)).contains(pos) {
                return Some(UiControl::LauncherDialogClose);
            }
            // Anywhere else, the dialog's own frame included, is the
            // answer that changes nothing. A question about deleting
            // something should not be answerable by a stray click.
            return Some(UiControl::LauncherCancelReset);
        }
        if state.save_dialog {
            if let Some(control) = launcher_save_dialog_hit(rect, pos) {
                return Some(control);
            }
            if close_button_rect(launcher_save_dialog_rect(rect)).contains(pos) {
                return Some(UiControl::LauncherDialogClose);
            }
            return Some(UiControl::LauncherSave);
        }
    }
    if !modal && close_button_rect(rect).contains(pos) {
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
                    if analyzer_cpu_wait_rect(rect).contains(pos) {
                        return Some(UiControl::AnalyzerCpuWait);
                    }
                }
                AnalyzerTab::Blits => {
                    for (control, row_rect) in analyzer_blit_row_rects(rect) {
                        if row_rect.contains(pos) {
                            return Some(control);
                        }
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
                AnalyzerTab::Resources => {
                    // Rows are hit-tested by position alone; a click past
                    // the listed resources maps to an index the selection
                    // handler bounds-checks into a no-op.
                    for (control, row_rect) in analyzer_resource_row_rects(rect) {
                        if row_rect.contains(pos) {
                            return Some(control);
                        }
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
    /// A row of the menu: which open level, and which row of it.
    MenuRow {
        depth: usize,
        row: usize,
    },
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
    /// Audio tab: toggle mute for a row (0..3 = Paula channels, 4.. = the
    /// line-mixed source rows in `AudioScopeView::extras` order, CD-DA
    /// first).
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
    /// Frame analyzer: toggle the CPU wait view (slots the CPU was denied,
    /// coloured by the denier, with the wait breakdown in the counters).
    AnalyzerCpuWait,
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
    /// A row of the Resources tab's table (index into the displayed rows).
    AnalyzerResourceRow(u8),
    /// Save the selected bitmap or palette resource as PNG.
    AnalyzerResourceSave,
    /// A row of the Blits tab's recorded-transfer table.
    AnalyzerBlitRow(u8),
    AnalyzerHeatPick {
        x: u8,
        y: u8,
    },
    /// Configuration screen: pick a machine model.
    LauncherModel(MachineModel),
    /// Configuration screen: switch the category tab.
    LauncherTab(LauncherTab),
    /// The same page, reached from the row of sibling pages above the
    /// settings rather than from the category column. It is a button
    /// of its own -- somewhere else on the screen, lighting on its own
    /// -- even though pressing it goes where the category button goes.
    LauncherNavTab(LauncherTab),
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
    /// Configuration screen: flip a directory-mount drive between FFS and OFS.
    LauncherDriveFilesystemToggle(LauncherField),
    /// A free-text box on a Create Image page (a volume or device name).
    LauncherNewImageEdit(LauncherField),
    /// A serial TCP address box on the I/O Ports tab (Connect or Listen).
    LauncherSerialHostEdit(LauncherField),
    LauncherSerialPortEdit(LauncherField),
    /// The fixed RAM power-on word on the Memory tab.
    LauncherRamPatternEdit,
    /// The Create button on a Create Image page.
    LauncherNewImageCreate(LauncherField),
    /// The MB/GB written beside the hard-drive size, which swaps on click.
    LauncherNewImageUnit,
    /// Fetch a WHDLoad support archive into the default place.
    #[cfg(feature = "game-library")]
    LauncherWhdloadDownload(LauncherField),
    /// Scroll the Library list, by rows: negative up, positive down.
    #[cfg(feature = "game-library")]
    LauncherLibraryScroll(isize),
    /// Choose the game on that drawn row of the Library list.
    #[cfg(feature = "game-library")]
    LauncherLibraryPick(usize),
    /// Mark or unmark that drawn row of the Library list.
    #[cfg(feature = "game-library")]
    LauncherLibraryFavourite(usize),
    /// Choose the game on that row of the favourites list.
    #[cfg(feature = "game-library")]
    LauncherLibraryFavouritePick(usize),
    /// Take that row off the favourites list.
    #[cfg(feature = "game-library")]
    LauncherLibraryFavouriteRemove(usize),
    /// Scroll the favourites list by that many rows.
    #[cfg(feature = "game-library")]
    LauncherLibraryFavouriteScroll(isize),
    /// Jump the games list to the first game in that A-Z bucket.
    #[cfg(feature = "game-library")]
    LauncherLibraryJump(usize),
    /// Open the OpenRetro sign-in dialog.
    #[cfg(feature = "game-library")]
    LauncherOpenRetroLogin,
    /// Re-read the game folder, without touching metadata.
    #[cfg(feature = "game-library")]
    LauncherLibraryRefresh,
    /// Resolve metadata and art for everything in the game folder.
    #[cfg(feature = "game-library")]
    LauncherLibraryUpdate,
    /// Open the metadata editor on the selected game.
    #[cfg(feature = "game-library")]
    LauncherLibraryEdit,
    /// A field of the metadata editor, its art box, or one of its buttons.
    #[cfg(feature = "game-library")]
    MetaField(launcher::MetaField),
    #[cfg(feature = "game-library")]
    MetaArt,
    #[cfg(feature = "game-library")]
    MetaSave,
    #[cfg(feature = "game-library")]
    MetaClear,
    #[cfg(feature = "game-library")]
    MetaCancel,
    /// A field of the sign-in dialog, or one of its two buttons.
    #[cfg(feature = "game-library")]
    LoginField(launcher::LoginField),
    #[cfg(feature = "game-library")]
    LoginOk,
    #[cfg(feature = "game-library")]
    LoginCancel,
    /// A filesystem family tick box on a Create Image page.
    LauncherFsFamily {
        field: LauncherField,
        family: launcher::FsFamily,
    },
    /// A filesystem variant tick box, on the row under the family.
    LauncherFsVariant {
        field: LauncherField,
        variant: crate::diskimage::Variant,
    },
    /// Let the hard-disk geometry follow the size.
    LauncherGeometryAuto,
    /// Set the hard-disk geometry by hand.
    LauncherGeometryCustom,
    /// Boot Priority page: focus a drive's boot-priority field for typing.
    LauncherDriveBootpriEdit(LauncherField),
    /// Boot Priority page: toggle a drive's Bootable box.
    LauncherDriveBootToggle(LauncherField),
    /// Floppy tab: turn a bay over to a real drive, or back to images.
    LauncherDriveBridgeToggle(usize),
    /// Floppy tab: open the FluxBridge settings for a bay.
    LauncherBridgeConfigure(usize),
    /// Configuration screen: add a Zorro metadata board file.
    LauncherZorroAdd,
    /// Configuration screen: remove the Zorro board at this index.
    LauncherZorroRemove(usize),
    /// Tick one disk in the Host Disk table.
    LauncherHostDiskSelect(usize),
    /// Flip one disk between writable and protected.
    LauncherHostDiskWritable(usize),
    /// Step one disk through the attachment points.
    LauncherHostDiskAttach(usize),
    /// Give a real disk back to the host, from the drive row holding it.
    LauncherHostDiskUnmount(LauncherField),
    /// Move the disk list's window up or down one row.
    /// The Enable tick at the end of a host-disk row: the same answer
    /// as picking the row, given its own place so the focus can stand
    /// on the box it ticks rather than on the whole row.
    LauncherHostDiskEnable(usize),
    LauncherHostDiskScroll(isize),
    /// Look at the host's storage again.
    LauncherHostDiskRefresh,
    /// Attach the ticked disks to the machine.
    LauncherHostDiskMount,
    /// Take the ticked disks that the machine has back off it.
    LauncherHostDiskUnmountSelected,
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
    /// Configuration screen: open the Save menu.
    LauncherSave,
    /// Save menu: save the configuration to a .toml file of its own.
    LauncherSaveAs,
    /// Save menu: save it as the configuration Copperline starts with.
    LauncherSaveDefault,
    /// Save menu: delete the saved default, so Copperline starts from
    /// factory settings again.
    LauncherResetDefault,
    /// The "are you sure" over Reset default: go ahead.
    LauncherConfirmReset,
    /// The "are you sure" over Reset default: leave it alone.
    LauncherCancelReset,
    /// The close gadget on whichever launcher dialog is up.
    ///
    /// Its own control rather than sharing the one a click anywhere else
    /// returns. Both mean "put this away", but only one of them is the
    /// gadget, and the gadget lights up when the pointer is on it -- share
    /// the control and it lights up for every hover in the dialog.
    LauncherDialogClose,
    /// Configuration screen: reset to the selected profile's defaults.
    LauncherDefaults,
    /// Configuration screen: build and run the configured machine.
    LauncherRun,
    /// Drop chooser: insert the dropped disk(s) into this drive.
    DropDrive(usize),
}

fn panel_dims(panel: &Panel) -> (usize, usize) {
    match panel {
        Panel::About => (560, 450),
        Panel::Shortcuts => (600, shortcuts_panel_height()),
        Panel::Calibration(_) => (620, calibration_panel_height()),
        Panel::InputMap(_) => (INPUT_MAP_W, input_map_panel_height()),
        Panel::Debugger(_) => (684, 520),
        Panel::FrameAnalyzer(_) => (700, 526),
        Panel::Console(_) => (700, 460),
        // Clamped to the display area so the status bar below stays a
        // status bar whatever the height grows to: a taller launcher
        // gives up height rather than pixels, because its bottom row is
        // its buttons, and buttons drawn off the canvas cannot be
        // clicked.
        Panel::Launcher(_) => (LAUNCHER_W, LAUNCHER_H.min(present_height())),
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

/// The rect the launcher panel occupies, for the parts of the window that
/// have to measure against it.
#[cfg(feature = "game-library")]
pub(in crate::video) fn launcher_panel_rect(ui: &UiState) -> Option<Rect> {
    match &ui.panel {
        Some(panel @ Panel::Launcher(_)) => Some(panel_rect(panel)),
        _ => None,
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

pub(in crate::video) const TITLE_H: usize = 22;

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
/// Vertical pitch of one calibration step row.
const CAL_ROW_H: usize = 18;
/// What the calibration panel holds besides its step rows: title bar, the
/// controller line, the prompt, and the button row.
const CAL_FIXED_H: usize = 138;

/// Panel height that exactly holds every calibration step, so adding a
/// step never pushes the prompt or the buttons off the bottom.
fn calibration_panel_height() -> usize {
    CAL_FIXED_H + crate::gamepad::CalibrationSession::step_count() * CAL_ROW_H
}

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

// Audio tab layout: a header line, four Paula channel blocks, then one
// shorter row per line-mixed source (CD-DA always, MIDI synth / Toccata /
// MHI while fitted). Each block has a mute button on the left, text detail
// in the middle, and an oscilloscope box on the right.
const AUDIO_HEADER_H: usize = 16;
const AUDIO_ROW_H: usize = 46;
const AUDIO_EXTRA_ROW_H: usize = 30;
const AUDIO_MUTE_W: usize = 54;
const AUDIO_TEXT_X: usize = 70;
const AUDIO_SCOPE_X: usize = 470;
/// The most rows the tab can hold: four Paula channels plus every
/// line-mixed source (CD-DA, MIDI synth, Toccata, MHI).
const AUDIO_MAX_ROWS: usize = 8;

/// Geometry of one Audio-tab row: (mute button rect, scope box rect). `idx`
/// 0..3 are the Paula channels; 4.. are the line-mixed source rows in the
/// order `AudioScopeView::extras` presents them (CD-DA first).
fn audio_row_geom(rect: Rect, idx: usize) -> (Rect, Rect) {
    let top = debug_content_top(rect)
        + AUDIO_HEADER_H
        + if idx < 4 {
            idx * AUDIO_ROW_H
        } else {
            4 * AUDIO_ROW_H + (idx - 4) * AUDIO_EXTRA_ROW_H
        };
    let row_h = if idx >= 4 {
        AUDIO_EXTRA_ROW_H
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

/// The Audio-tab mute buttons: four Paula channels, then every possible
/// line-mixed source slot. A slot with no row drawn in it still hit-tests
/// (the geometry cannot see which sources are fitted); the click dispatcher
/// rebuilds the fitted-source list and ignores clicks past its end.
fn audio_tab_button_rects(rect: Rect) -> [(UiControl, Rect); AUDIO_MAX_ROWS] {
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

/// Height of one Resources-tab table row, and how many the panel lists.
const ANALYZER_RESOURCE_ROW_H: usize = 12;
pub const ANALYZER_RESOURCE_ROWS_MAX: usize = 10;
const ANALYZER_BLIT_ROW_H: usize = 14;
pub const ANALYZER_BLIT_ROWS_MAX: usize = 8;

fn analyzer_blit_row_rects(rect: Rect) -> Vec<(UiControl, Rect)> {
    let top = analyzer_content_top(rect) + 16;
    (0..ANALYZER_BLIT_ROWS_MAX)
        .map(|index| {
            (
                UiControl::AnalyzerBlitRow(index as u8),
                Rect {
                    x: rect.x + 10,
                    y: top + index * ANALYZER_BLIT_ROW_H,
                    w: rect.w - 20,
                    h: ANALYZER_BLIT_ROW_H,
                },
            )
        })
        .collect()
}

fn analyzer_blit_detail_rect(rect: Rect) -> Rect {
    let top = analyzer_content_top(rect) + 16 + ANALYZER_BLIT_ROWS_MAX * ANALYZER_BLIT_ROW_H + 26;
    Rect {
        x: rect.x + 10,
        y: top,
        w: rect.w - 20,
        h: (rect.y + rect.h)
            .saturating_sub(DEBUG_BUTTON_H + 12)
            .saturating_sub(top),
    }
}

/// The Resources tab's table rows, top to bottom under the header line.
fn analyzer_resource_row_rects(rect: Rect) -> Vec<(UiControl, Rect)> {
    let top = analyzer_content_top(rect) + 16;
    (0..ANALYZER_RESOURCE_ROWS_MAX)
        .map(|index| {
            (
                UiControl::AnalyzerResourceRow(index as u8),
                Rect {
                    x: rect.x + 10,
                    y: top + index * ANALYZER_RESOURCE_ROW_H,
                    w: rect.w - 20,
                    h: ANALYZER_RESOURCE_ROW_H,
                },
            )
        })
        .collect()
}

/// The Resources tab's preview area, between the table and the transport
/// buttons.
fn analyzer_resource_detail_rect(rect: Rect) -> Rect {
    let top =
        analyzer_content_top(rect) + 16 + ANALYZER_RESOURCE_ROWS_MAX * ANALYZER_RESOURCE_ROW_H + 18;
    Rect {
        x: rect.x + 10,
        y: top,
        w: rect.w - 20,
        h: (rect.y + rect.h)
            .saturating_sub(DEBUG_BUTTON_H + 12 + 14)
            .saturating_sub(top),
    }
}

/// The Memory tab's map: a 368 px square nearest-sampled from the 256x256
/// grid (not an integral scale, so a cell lands on 1-2 px).
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
        AnalyzerTab::Blits => all[..2].to_vec(),
        AnalyzerTab::Memory => all[..2].to_vec(),
        AnalyzerTab::Resources => vec![all[0], all[1], (UiControl::AnalyzerResourceSave, all[2].1)],
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

/// Label of the CPU-wait checkbox, right of the scrub one.
const ANALYZER_CPU_WAIT_LABEL: &str = "CPU wait";

/// Hit/draw rect of the CPU-wait checkbox.
fn analyzer_cpu_wait_rect(rect: Rect) -> Rect {
    let scrub = analyzer_scrub_rect(rect);
    Rect {
        x: scrub.x + scrub.w + 16,
        y: scrub.y,
        w: 12 + 6 + ANALYZER_CPU_WAIT_LABEL.len() * font::GLYPH_W,
        h: DEBUG_BUTTON_H,
    }
}

/// The per-line CPU stall gutter beside the raster: one texel row per
/// heat-map row, a bar as long as the share of the line's colour clocks the
/// CPU spent waiting, in the colour of the line's dominant denier.
const ANALYZER_GUTTER_W: usize = 20;

fn analyzer_gutter_rect(rect: Rect) -> Rect {
    let raster = analyzer_raster_rect(rect);
    Rect {
        x: raster.x + raster.w + 4,
        y: raster.y,
        w: ANALYZER_GUTTER_W,
        h: raster.h,
    }
}

/// Left edge of the counters column, right of the gutter.
fn analyzer_counters_x(rect: Rect) -> usize {
    let gutter = analyzer_gutter_rect(rect);
    gutter.x + gutter.w + 12
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

/// Per-channel and line-mixed-source state for the debugger Audio tab.
pub struct AudioScopeView {
    /// Header line (DMACON / AUDEN / ADKCON summary).
    pub header: String,
    /// The four Paula channels, in order.
    pub channels: Vec<AudioRowView>,
    /// The line-mixed source rows drawn under the channels, in order:
    /// CD-DA first (always present), then one row per fitted source
    /// (MIDI synth, Toccata, MHI). Row `4 + i` of the tab is `extras[i]`,
    /// and the mute-click dispatcher maps clicks back through the same
    /// order.
    pub extras: Vec<AudioExtraRow>,
}

/// Which line-mixed source an extra Audio-tab row shows; picks the row's
/// trace colour and the mute's OSD label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioExtraKind {
    Cd,
    Synth,
    Toccata,
    Mhi,
}

/// One line-mixed source row of the Audio tab.
pub struct AudioExtraRow {
    pub kind: AudioExtraKind,
    pub row: AudioRowView,
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
    /// MOVE instruction address for reciprocal slot/list navigation.
    pub copper_addr: Option<u32>,
    /// Register-class colour for Copper MOVE markers.
    pub colour: u32,
}

impl AnalyzerMarker {
    fn label(&self) -> String {
        format!(
            "{} {}=${:04X} v{} h{}{}",
            self.source,
            crate::debugger::custom_reg_name(self.offset & 0x01FE),
            self.value,
            self.vpos,
            self.hpos,
            self.copper_addr
                .map(|addr| format!(" @${addr:06X}"))
                .unwrap_or_default(),
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
    pub records: Option<std::sync::Arc<Vec<crate::bus::BusSlotRecord>>>,
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
    /// Per-slot CPU wait grid parallel to `owners`
    /// (`crate::bus::cpu_wait_class_code`, `.` where the CPU was not
    /// waiting), with its totals by denier class
    /// (`crate::bus::CPU_WAIT_CLASS_NAMES` order) and by access kind
    /// (`crate::bus::CPU_BUS_ACCESS_KIND_NAMES` order).
    pub cpu_waits: Vec<u8>,
    pub cpu_wait_cck: u64,
    pub cpu_wait_by_class: [u64; 9],
    pub cpu_wait_by_kind: [u64; 4],
    /// The instructions that waited longest, `(pc, cck, live ROM symbol)`,
    /// longest first. The optional name keeps the view useful before Exec
    /// has initialised its lists and for ordinary program PCs.
    pub top_stalled_pcs: Vec<(u32, u32, Option<String>)>,
    pub selected_cpu_wait_code: u8,
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

    fn cpu_wait_code_at(&self, vpos: usize, hpos: usize) -> u8 {
        if vpos >= self.rows || hpos >= self.cols {
            return b'.';
        }
        self.cpu_waits
            .get(vpos * self.cols + hpos)
            .copied()
            .unwrap_or(b'.')
    }

    fn record_at(&self, vpos: usize, hpos: usize) -> Option<&crate::bus::BusSlotRecord> {
        if vpos >= self.rows || hpos >= self.cols {
            return None;
        }
        self.records.as_ref()?.get(vpos * self.cols + hpos)
    }

    fn cpu_wait_row(&self, vpos: usize) -> Option<&[u8]> {
        if vpos >= self.rows || self.cols == 0 {
            return None;
        }
        let start = vpos * self.cols;
        self.cpu_waits.get(start..start + self.cols)
    }

    /// The share of `total` (the CPU's granted plus waited clocks) it
    /// spent waiting, as a percentage.
    fn cpu_wait_percent(&self) -> f64 {
        let granted = self.owner_cck[7];
        let total = granted.saturating_add(self.cpu_wait_cck);
        if total == 0 {
            0.0
        } else {
            self.cpu_wait_cck as f64 * 100.0 / total as f64
        }
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
    /// Guest-registered debug resources (crate::uaelib), sorted by start,
    /// so cells and presets can be named after what the program says
    /// lives there.
    pub resources: Vec<AnalyzerHeatResource>,
}

/// One guest-registered resource as the Memory tab names it.
pub struct AnalyzerHeatResource {
    pub start: u32,
    /// Exclusive end.
    pub end: u32,
    pub name: String,
    pub kind: &'static str,
}

/// The Resources tab's table and the selected resource's decoded preview.
pub struct AnalyzerResourcesView {
    pub rows: Vec<AnalyzerResourceRowView>,
    /// Registry entries scrolled off either end of the table, reported
    /// under it with the scroll hint.
    pub hidden_above: usize,
    pub hidden_below: usize,
    pub detail: Option<AnalyzerResourceDetail>,
    pub exportable: bool,
}

/// The Blits tab's table plus the selected source/result pair.
pub struct AnalyzerBlitsView {
    pub rows: Vec<AnalyzerBlitRowView>,
    pub hidden_above: usize,
    pub hidden_below: usize,
    pub source_label: &'static str,
    pub source: Option<crate::video::resource_preview::BitmapPreview>,
    pub destination: Option<crate::video::resource_preview::BitmapPreview>,
    pub formula: String,
    pub detail: String,
}

pub struct AnalyzerBlitRowView {
    pub text: String,
    pub selected: bool,
}

pub struct AnalyzerResourceRowView {
    pub text: String,
    pub selected: bool,
}

pub enum AnalyzerResourceDetail {
    Bitmap(crate::video::resource_preview::BitmapPreview),
    Palette { colours: Vec<u32> },
    Copperlist { lines: Vec<String> },
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
    /// The Resources tab's data; built only while that tab is up.
    pub resources: Option<AnalyzerResourcesView>,
    /// The Blits tab's data; built only while that tab is up.
    pub blits: Option<AnalyzerBlitsView>,
}

pub enum PanelViewData {
    About(super::about::AboutView),
    Shortcuts,
    Calibration(CalibrationView),
    Debugger(Box<DebuggerView>),
    FrameAnalyzer(Box<FrameAnalyzerView>),
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

pub(in crate::video) fn draw_panel_text(
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
    hover: f32,
    texture_scale: usize,
) {
    let face = if enabled {
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover)
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
    draw_title_bar(
        frame,
        rect,
        panel_title(panel),
        lit(hover, UiControl::PanelClose),
        scale,
    );
}

/// A panel's blue title bar, with its name and its close gadget.
fn draw_title_bar(frame: &mut [u8], rect: Rect, title: &str, close_hover: f32, scale: usize) {
    let bar = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        w: rect.w - 2,
        h: TITLE_H - 1,
    };
    fill_rect(frame, scale_rect(bar, scale), PANEL_TITLE_BG, scale);
    draw_panel_text(
        frame,
        rect.x + 10,
        rect.y + (TITLE_H - 16) / 2,
        title,
        PANEL_TITLE_TEXT,
        2,
        scale,
    );
    draw_close_gadget(frame, rect, close_hover, scale);
}

/// The close gadget: a classic square with an inner square.
fn draw_close_gadget(frame: &mut [u8], rect: Rect, close_hover: f32, scale: usize) {
    let close = close_button_rect(rect);
    // The gadget already wears the interface's blue, so the focus lifts
    // it to the paler one rather than painting it the colour it is.
    let face = light_face_to(PANEL_TITLE_BG, BUTTON_FACE_HOVER, NAV_FACE_ON, close_hover);
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

// Where the focus stands while a surface is being drawn, and how far
// through its breath it is.
//
// The focus lights a control the way the pointer lights one -- there
// is no second language to learn -- but in the interface's own blue
// rather than the pointer's grey, and breathing rather than steady,
// so the two hands never say the same thing. Drawing is one pass on
// one thread, so the surface reads where the focus is from here
// rather than every drawing function in the file carrying it through.
thread_local! {
    static NAV_LIGHT: std::cell::Cell<(Option<UiControl>, f32)> =
        const { std::cell::Cell::new((None, 0.0)) };
    /// Whether that control stands open for changing.
    static NAV_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the marker is up on the *other* surface -- the status bar
    /// while this is a panel, or the other way about. The keyboard is in
    /// charge either way, so the pointer lights nothing here either.
    static NAV_ELSEWHERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Say where the focus is, and how far through its breath, for the
/// drawing about to happen.
pub(in crate::video) fn set_nav_light(
    target: Option<UiControl>,
    mix: f32,
    open: bool,
    elsewhere: bool,
) {
    NAV_LIGHT.with(|light| light.set((target, mix.clamp(0.0, 1.0))));
    NAV_OPEN.with(|flag| flag.set(open));
    NAV_ELSEWHERE.with(|flag| flag.set(elsewhere));
}

/// How lit a control is: all the way under the pointer, and as far as
/// the breath has come when the focus is standing on it.
fn lit(hover: Option<UiControl>, control: UiControl) -> f32 {
    // Negative says the focus has it rather than the pointer: the two
    // light the same control differently, and one number carries both
    // how far through the breath it is and whose it is. The focus is
    // asked first, so a control the mouse happens to be resting on
    // still breathes when the keyboard walks onto it.
    let focused = nav_lit(control);
    if focused != 0.0 {
        return -focused;
    }
    // And while the marker is up at all, the keyboard is in charge: the
    // pointer lights nothing, or a hand left resting on the mouse marks
    // a second control wherever it happens to sit. Moving the mouse
    // puts the marker away, and the pointer has it back.
    if nav_showing() {
        return 0.0;
    }
    if hover == Some(control) {
        1.0
    } else {
        0.0
    }
}

/// Whether the focus is being shown at all, whatever it is standing on.
fn nav_showing() -> bool {
    nav_target().is_some() || NAV_ELSEWHERE.with(std::cell::Cell::get)
}

/// What the focus is standing on, if it is being shown.
fn nav_target() -> Option<UiControl> {
    NAV_LIGHT.with(|light| light.get().0)
}

/// The face a control wears, given how lit it is: the pointer's grey,
/// or the focus's blue. The status bar draws its own buttons and uses
/// this too, so the two surfaces cannot drift apart.
pub(in crate::video) fn light_face(resting: u32, hovered: u32, light: f32) -> u32 {
    light_face_to(resting, hovered, NAV_FACE, light)
}

/// The same, saying which blue the focus lifts a control toward.
pub(in crate::video) fn light_face_to(resting: u32, hovered: u32, focused: u32, light: f32) -> u32 {
    if light < 0.0 {
        mix_colour(resting, focused, -light)
    } else {
        mix_colour(resting, hovered, light)
    }
}

/// The face the focus lights a control with: the blue the interface
/// already wears for a chosen page. The two hands say different
/// things, so the focus takes the blue and the pointer keeps its grey.
pub(in crate::video) const NAV_FACE: u32 = PANEL_TITLE_BG;
/// A control already wearing that blue lifts toward this instead --
/// the same colour again would say nothing about where the focus is.
pub(in crate::video) const NAV_FACE_ON: u32 = rgba(120, 176, 236);

/// How lit the focus alone has a control -- the pointer does not count.
/// The value a stepper is about to change reads this: it should say
/// which setting the focus is on, not which arrow the mouse is over.
fn nav_lit(control: UiControl) -> f32 {
    NAV_LIGHT.with(|light| {
        let (target, mix) = light.get();
        if target == Some(control) {
            mix
        } else {
            0.0
        }
    })
}

/// What a tick box's outline says: green under the pointer, the
/// focus's blue while it stands there, and nothing at all otherwise.
/// A tick box is a box: filling its middle would read as a tick.
/// The fill a row of a list takes while it is under the pointer or the
/// focus, over `resting`.
///
/// A list is the one place the two hands would otherwise look alike: a
/// whole row filled grey reads as a selection rather than as a marker,
/// and the row that really is selected is already filled. So the
/// keyboard keeps its blue here as everywhere else.
#[cfg(feature = "game-library")]
fn row_light(resting: u32, light: f32) -> Option<u32> {
    (light != 0.0).then(|| light_face(resting, BUTTON_FACE_HOVER, light))
}

fn tick_outline(light: f32) -> Option<u32> {
    if light == 0.0 {
        return None;
    }
    Some(if light < 0.0 {
        // Green, breathing up out of the box's own edge. A tick box is
        // the one control small enough that the marker has to be its
        // outline, and an outline in the focus's blue over a green tick
        // read as a second state of the box rather than as a marker.
        mix_colour(BUTTON_EDGE_LIGHT, PANEL_TEXT_HILIGHT, -light)
    } else {
        PANEL_TEXT_HILIGHT
    })
}

/// How lit one end of a stepper is: the pointer's own light if it is
/// over that end, and otherwise the focus's, which both ends share.
fn stepper_light(hover: Option<UiControl>, end: UiControl, stepper: f32) -> f32 {
    if hover == Some(end) {
        1.0
    } else {
        -stepper
    }
}

/// Whether the setting the focus is on stands open for changing.
fn nav_open() -> bool {
    NAV_OPEN.with(std::cell::Cell::get)
}

/// A colour part of the way to another.
pub(in crate::video) fn mix_colour(from: u32, to: u32, t: f32) -> u32 {
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as f32;
        let b = ((to >> shift) & 0xFF) as f32;
        ((a + (b - a) * t) as u32) << shift
    };
    channel(0) | channel(8) | channel(16) | (from & 0xFF00_0000)
}

/// Word-wrap `text` so no panel line is cropped: the first line holds up to
/// `first_width` characters, continuations up to `rest_width` (they are drawn
/// indented). Words longer than a whole line are hard-split.
pub(in crate::video) fn wrap_text(
    text: &str,
    first_width: usize,
    rest_width: usize,
) -> Vec<String> {
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
        draw_text_button(frame, button_rect, &label, true, lit(hover, control), scale);
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
const SHORTCUT_ROW_H: usize = 18;
/// Trailing note lines under the shortcut table, and their pitch.
const SHORTCUT_NOTES: [&str; 3] = [
    "Shortcuts: Cmd on macOS, Alt on Linux/Windows",
    "Amiga modifiers: Alt, Cmd/Super=Amiga, Ctrl",
    "In the debugger: S step, O over, U out, F frame, R run/pause",
];
const SHORTCUT_NOTE_H: usize = 12;
/// Space between the last table row and the first note line.
const SHORTCUT_NOTES_GAP: usize = 6;

/// Panel height that exactly holds the table plus the notes, so adding a row
/// does not silently push the last one off the bottom. The gap above the
/// notes and the bottom margin are what a 25-row table leaves within the
/// display.
fn shortcuts_panel_height() -> usize {
    TITLE_H
        + 14
        + SHORTCUT_ROWS.len() * SHORTCUT_ROW_H
        + SHORTCUT_NOTES_GAP
        + SHORTCUT_NOTES.len() * SHORTCUT_NOTE_H
        + 8
}

const SHORTCUT_ROWS: [(&str, &str, bool); 25] = [
    ("Q", "Quit", true),
    ("E", "Open the menu", true),
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
    ("Shift+B", "Freeze (HRTMon)", true),
    ("K", "Console", true),
    ("J", "Joystick input mode", true),
    ("M", "Monitor bezel off/on", true),
    ("Shift+A", "Cycle audio output", true),
    ("F", "Fullscreen on/off", true),
    ("Shift+F", "Status bar on/off", true),
    ("P", "Performance overlay on/off", true),
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
    y += SHORTCUT_NOTES_GAP;
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
                    lit(hover, *control),
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
                    lit(hover, *control),
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
                    lit(hover, *control),
                    scale,
                );
            }
            UiControl::RemapDefaults => draw_text_button(
                frame,
                *button_rect,
                "Defaults",
                true,
                lit(hover, *control),
                scale,
            ),
            UiControl::RemapSave => draw_text_button(
                frame,
                *button_rect,
                "Save",
                true,
                lit(hover, *control),
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
        y += CAL_ROW_H;
    }
    y += 6;
    // Wrapped to the panel: the prompt says what to do next and a line
    // that ran off the edge would be saying it to nobody.
    let chars = (rect.w.saturating_sub(32)) / font::GLYPH_W;
    for line in wrap_text(&view.status, chars, chars) {
        draw_panel_text(frame, rect.x + 16, y, &line, PANEL_TEXT_ACCENT, 1, scale);
        y += font::GLYPH_H + 2;
    }
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
            lit(hover, control),
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
        let hovered = lit(hover, UiControl::DebugTab(*tab));
        let face = if selected {
            light_face_to(ENTRY_BG, ENTRY_BG, NAV_FACE_ON, hovered)
        } else {
            light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hovered)
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
                lit(hover, control),
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
                lit(hover, control),
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
                lit(hover, control),
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
                lit(hover, control),
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
                    lit(hover, control),
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
    // The same caret every other box in the UI draws, at the end of the
    // line because that is the only place this one can type: the console
    // appends and backspaces, and keeps its arrow keys for the history.
    // (It also clips to the box, where the old fixed truncation could cut
    // a multi-byte character in half and panic.)
    let prompt = format!("> {}", panel.input);
    draw_edit_line(
        frame,
        entry.x + 6,
        entry.y + (CONSOLE_INPUT_H - 8) / 2,
        &prompt,
        prompt.chars().count(),
        ENTRY_TEXT,
        ENTRY_BG,
        entry.w.saturating_sub(12),
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
            lit(hover, control),
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

/// Draw the Audio tab: a header line, four Paula channel blocks, and one
/// row per line-mixed source, each with a mute button, text detail, and an
/// output oscilloscope.
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
    // Channels occupy rows 0..3 and the line-mixed sources rows 4.. --
    // fixed slots, so the extras stay where the mute hit-test expects them
    // even if a channel row were ever absent.
    let rows = audio
        .channels
        .iter()
        .enumerate()
        .take(4)
        .map(|(idx, row)| (idx, row, AUDIO_SCOPE_COLORS[idx.min(3)]))
        .chain(
            audio
                .extras
                .iter()
                .enumerate()
                .map(|(i, extra)| (4 + i, &extra.row, audio_extra_color(extra.kind))),
        );
    for (idx, row, color) in rows.filter(|(idx, ..)| *idx < AUDIO_MAX_ROWS) {
        let (mute_rect, scope_rect) = audio_row_geom(rect, idx);
        let control = UiControl::DebugAudioMute(idx);
        draw_mute_button(frame, mute_rect, row.muted, lit(hover, control), scale);
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
        draw_audio_scope(frame, scope_rect, &row.scope, color, row.muted, scale);
    }
}

/// A single mute toggle button: red-tinted face and "Muted" label when active.
fn draw_mute_button(frame: &mut [u8], rect: Rect, muted: bool, hover: f32, scale: usize) {
    let face = if muted {
        AUDIO_MUTE_FACE
    } else {
        light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hover)
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

/// Colour of a CPU wait code (`crate::bus::cpu_wait_class_code`): the
/// denier's owner colour, a hotter red for the BLTPRI-set blitter, grey for
/// the 020+ port turnaround.
fn cpu_wait_color(code: u8) -> u32 {
    match code {
        b'N' => rgba(255, 40, 40),
        b'p' => rgba(150, 150, 150),
        b'.' => rgba(20, 22, 26),
        other => owner_color(other),
    }
}

/// Legend name of a CPU wait code, short enough for the legend row.
fn cpu_wait_name_for_code(code: u8) -> &'static str {
    match code {
        b'N' => "bltpri",
        b'p' => "port",
        b'.' => "none",
        other => owner_name_for_code(other),
    }
}

/// Quarter the brightness of an RGBA pixel, keeping it opaque: the rest of
/// the grid while the CPU wait view is on.
fn quarter_rgba(pix: u32) -> u32 {
    ((pix >> 2) & 0x003F_3F3F) | 0xFF00_0000
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
    cpu_wait: bool,
    scale: usize,
) {
    fill_rect(frame, scale_rect(rect, scale), rgba(10, 12, 14), scale);
    for y in 0..rect.h {
        let vpos = y * trace.rows / rect.h.max(1);
        for x in 0..rect.w {
            let hpos = x * trace.cols / rect.w.max(1);
            let owner_code = trace.owner_code_at(vpos, hpos);
            // The CPU wait view keeps the owner grid faintly visible under
            // the slots the CPU was denied, so a stall reads against the
            // DMA pattern that caused it.
            let (code, mut color) = if cpu_wait {
                let wait_code = trace.cpu_wait_code_at(vpos, hpos);
                if wait_code != b'.' {
                    (wait_code, cpu_wait_color(wait_code))
                } else {
                    (owner_code, quarter_rgba(owner_color(owner_code)))
                }
            } else {
                (owner_code, owner_color(owner_code))
            };
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
            marker.colour,
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
            marker.colour,
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

fn draw_scanline_strip(
    frame: &mut [u8],
    rect: Rect,
    trace: &AnalyzerTraceView,
    cpu_wait: bool,
    scale: usize,
) {
    fill_rect(frame, scale_rect(rect, scale), rgba(10, 12, 14), scale);
    if let Some(row) = trace.owner_row(trace.selected_vpos) {
        let waits = trace.cpu_wait_row(trace.selected_vpos);
        for x in 0..rect.w {
            let hpos = x * trace.cols / rect.w.max(1);
            let slot = hpos.min(row.len().saturating_sub(1));
            // The wait view overlays the denied slots on the dimmed owner
            // pattern, exactly as the raster does.
            let wait_code = if cpu_wait {
                waits.map_or(b'.', |waits| waits[slot.min(waits.len().saturating_sub(1))])
            } else {
                b'.'
            };
            let color = if wait_code != b'.' {
                cpu_wait_color(wait_code)
            } else if cpu_wait {
                quarter_rgba(owner_color(row[slot]))
            } else {
                owner_color(row[slot])
            };
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

/// The per-line CPU stall gutter: for each heat-map row, a bar as long as
/// the share of that beam line's colour clocks the CPU spent waiting, in
/// the colour of the class that denied it most on that line. Reads as a
/// profile of where the frame chokes the CPU, whichever view is on.
fn draw_cpu_wait_gutter(frame: &mut [u8], rect: Rect, trace: &AnalyzerTraceView, scale: usize) {
    // No outline: the bars share the raster's row mapping, and a frame
    // would hide the first and last lines' bars.
    fill_rect(frame, scale_rect(rect, scale), ANALYZER_GUTTER_BG, scale);
    let inner_w = rect.w.saturating_sub(2);
    for y in 0..rect.h {
        let vpos = y * trace.rows / rect.h.max(1);
        let Some(row) = trace.cpu_wait_row(vpos) else {
            continue;
        };
        let mut waited = 0usize;
        let mut by_code: [(u8, usize); 9] = [
            (b'R', 0),
            (b'B', 0),
            (b'S', 0),
            (b'D', 0),
            (b'A', 0),
            (b'C', 0),
            (b'L', 0),
            (b'N', 0),
            (b'p', 0),
        ];
        for &code in row {
            if code == b'.' {
                continue;
            }
            waited += 1;
            if let Some(entry) = by_code.iter_mut().find(|(c, _)| *c == code) {
                entry.1 += 1;
            }
        }
        if waited == 0 {
            continue;
        }
        let dominant = by_code
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(code, _)| *code)
            .unwrap_or(b'.');
        let w = (waited * inner_w / trace.cols.max(1)).clamp(1, inner_w);
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x: rect.x + 1,
                    y: rect.y + y,
                    w,
                    h: 1,
                },
                scale,
            ),
            cpu_wait_color(dominant),
            scale,
        );
    }
}

/// Background of the stall gutter, a shade lighter than the raster so the
/// strip reads as its own column.
const ANALYZER_GUTTER_BG: u32 = rgba(16, 18, 22);

/// The CPU wait view's counters column: what the CPU waited for, by
/// denier and by access kind, and the instructions that waited longest.
/// Pitch of one counters-column row.
const ANALYZER_COUNTER_ROW_H: usize = 12;

/// A row cursor for the counters column: rows are drawn top down and
/// none may start past `bottom`, so a busy trace (every class and access
/// kind non-zero, a full PC list) truncates its lower sections instead of
/// running into the selected-slot line under the raster.
struct CounterRows {
    y: usize,
    bottom: usize,
}

impl CounterRows {
    /// The y of the next row if it fits, advancing the cursor; None once
    /// the column is full.
    fn next(&mut self) -> Option<usize> {
        if self.y + ANALYZER_COUNTER_ROW_H > self.bottom {
            return None;
        }
        let y = self.y;
        self.y += ANALYZER_COUNTER_ROW_H;
        Some(y)
    }

    /// A small gap before a new section, only when a row still fits.
    fn gap(&mut self) {
        if self.y + 4 + ANALYZER_COUNTER_ROW_H <= self.bottom {
            self.y += 4;
        }
    }
}

/// The CPU wait view's counters column: what the CPU waited for, by
/// denier and by access kind, and the instructions that waited longest.
/// Sections are drawn in that order of importance and each stops at
/// `bottom`.
fn draw_cpu_wait_counters(
    frame: &mut [u8],
    x: usize,
    y: usize,
    bottom: usize,
    trace: &AnalyzerTraceView,
    scale: usize,
) {
    let mut rows = CounterRows { y, bottom };
    if let Some(y) = rows.next() {
        draw_panel_text(frame, x, y, "CPU wait cck", PANEL_TEXT_HILIGHT, 1, scale);
    }
    if let Some(y) = rows.next() {
        draw_panel_text(
            frame,
            x,
            y,
            &format!(
                "waited {:>6} {:>4.1}%",
                trace.cpu_wait_cck,
                trace.cpu_wait_percent()
            ),
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
    }
    if let Some(y) = rows.next() {
        draw_panel_text(
            frame,
            x,
            y,
            &format!("granted {:>5}", trace.owner_cck[7]),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    for (idx, name) in crate::bus::CPU_WAIT_CLASS_NAMES.iter().enumerate() {
        let cck = trace.cpu_wait_by_class[idx];
        if cck == 0 {
            continue;
        }
        let Some(y) = rows.next() else {
            break;
        };
        let code = *b"RBSDACLNp".get(idx).unwrap_or(&b'.');
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
            cpu_wait_color(code),
            scale,
        );
        let pct = if trace.cpu_wait_cck == 0 {
            0.0
        } else {
            cck as f64 * 100.0 / trace.cpu_wait_cck as f64
        };
        // "blitter_nasty" is wider than the column's name field.
        let label = if code == b'N' { "bltpri" } else { name };
        draw_panel_text(
            frame,
            x + 14,
            y,
            &format!("{label:<8} {cck:>5} {pct:>4.1}%"),
            PANEL_TEXT,
            1,
            scale,
        );
    }
    let kinds: Vec<String> = crate::bus::CPU_BUS_ACCESS_KIND_NAMES
        .iter()
        .zip(trace.cpu_wait_by_kind.iter())
        .filter(|(_, cck)| **cck != 0)
        .map(|(name, cck)| format!("{name} {cck}"))
        .collect();
    if !kinds.is_empty() {
        rows.gap();
        if let Some(y) = rows.next() {
            draw_panel_text(frame, x, y, "by access", PANEL_TEXT_DIM, 1, scale);
        }
        for kind in kinds {
            let Some(y) = rows.next() else {
                break;
            };
            draw_panel_text(frame, x, y, &kind, PANEL_TEXT_DIM, 1, scale);
        }
    }
    if !trace.top_stalled_pcs.is_empty() {
        rows.gap();
        if let Some(y) = rows.next() {
            draw_panel_text(frame, x, y, "Top stalled PCs", PANEL_TEXT_HILIGHT, 1, scale);
        }
        for (pc, cck, symbol) in trace.top_stalled_pcs.iter().take(ANALYZER_TOP_STALLED_PCS) {
            let Some(y) = rows.next() else {
                break;
            };
            let address = format!("${pc:08X}");
            draw_panel_text(
                frame,
                x,
                y,
                &format!("{} {cck:>6}", symbol.as_deref().unwrap_or(&address)),
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
}

/// Stalled-PC rows the counters column lists.
pub const ANALYZER_TOP_STALLED_PCS: usize = 6;

/// The picture-underlay, beam-scrub and CPU-wait tick boxes on the
/// analyzer's button row.
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
        (
            analyzer_cpu_wait_rect(rect),
            ANALYZER_CPU_WAIT_LABEL,
            panel.show_cpu_wait,
            UiControl::AnalyzerCpuWait,
        ),
    ] {
        draw_analyzer_checkbox(
            frame,
            control_rect,
            label,
            checked,
            lit(hover, control),
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
    hover: f32,
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
        light_face(ENTRY_BG, BUTTON_FACE_HOVER, hover),
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
        light_face(PANEL_TEXT, BUTTON_TEXT, hover),
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
        AnalyzerTab::Beam => draw_analyzer_beam_tab(frame, rect, panel, view, hover, scale),
        AnalyzerTab::Blits => draw_analyzer_blits_tab(frame, rect, view, hover, scale),
        AnalyzerTab::Memory => draw_analyzer_heat_tab(frame, rect, panel, view, hover, scale),
        AnalyzerTab::Resources => draw_analyzer_resources_tab(frame, rect, view, hover, scale),
    }
    // Transport buttons (and the beam tab's checkboxes) are bottom-anchored
    // chrome under whichever tab's content sits above them.
    for (control, button_rect) in analyzer_tab_button_rects(rect, panel.tab) {
        let label = match control {
            UiControl::AnalyzerRun if view.running => "Pause",
            UiControl::AnalyzerRun => "Run",
            UiControl::AnalyzerFrame => "Frame",
            UiControl::AnalyzerResourceSave => "Save...",
            _ => "To slot",
        };
        let enabled = control != UiControl::AnalyzerResourceSave
            || view
                .resources
                .as_ref()
                .is_some_and(|resources| resources.exportable);
        draw_text_button(
            frame,
            button_rect,
            label,
            enabled,
            lit(hover, control),
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
        let hovered = lit(hover, UiControl::AnalyzerTab(*tab));
        let face = if active {
            light_face_to(ENTRY_BG, ENTRY_BG, NAV_FACE_ON, hovered)
        } else {
            light_face(BUTTON_FACE, BUTTON_FACE_HOVER, hovered)
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
    panel: &FrameAnalyzerPanel,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let cpu_wait = panel.show_cpu_wait;
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
        if cpu_wait {
            "CPU wait: slots the CPU was denied, coloured by the denier; gutter=stall share per line"
        } else {
            "x=hpos colour clocks, y=vpos lines; white=captured display, orange=DIW, cyan=DDF"
        },
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
        cpu_wait,
        scale,
    );
    draw_cpu_wait_gutter(frame, analyzer_gutter_rect(rect), trace, scale);
    let counters_x = analyzer_counters_x(rect);
    if cpu_wait {
        // The column ends with the raster: the selected-slot line sits
        // just under it.
        draw_cpu_wait_counters(
            frame,
            counters_x,
            raster.y,
            raster.y + raster.h,
            trace,
            scale,
        );
    } else {
        draw_owner_counters(frame, counters_x, raster.y, trace, scale);
    }

    let mut selected = format!(
        "selected v={:03} h={:03}  owner={} ({})",
        trace.selected_vpos,
        trace.selected_hpos,
        trace.selected_owner,
        trace.selected_owner_code as char
    );
    // Short legend names keep the line inside the panel with a blit suffix.
    if trace.selected_cpu_wait_code != b'.' {
        selected.push_str(&format!(
            "  wait={}",
            cpu_wait_name_for_code(trace.selected_cpu_wait_code)
        ));
    }
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
    let slot_detail_drawn = if let Some(record) = trace.record_at(probe_vpos, probe_hpos) {
        let event_names = crate::bus::bus_event_names(record.events).join("|");
        let copper_instruction = if record.kind == crate::bus::BUS_RECORD_COPPER {
            let addr = if record.flags & 1 != 0 || record.subtype != 0 {
                record.addr.saturating_sub(2)
            } else {
                record.addr
            };
            format!(" copper=@${addr:06X}")
        } else {
            String::new()
        };
        let detail = format!(
            "reg=${:04X} addr=${:08X} data=${:016X}/{} kind={}:{} ipl={} events={}{}",
            record.reg,
            record.addr,
            record.data,
            record.size,
            record.kind,
            record.subtype,
            record.ipl,
            if event_names.is_empty() {
                "-"
            } else {
                &event_names
            },
            copper_instruction,
        );
        draw_panel_text(
            frame,
            rect.x + 10,
            raster.y + raster.h + 22,
            &detail,
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
        true
    } else {
        false
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
            raster.y + raster.h + if slot_detail_drawn { 34 } else { 22 },
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
    draw_scanline_strip(frame, scanline, trace, cpu_wait, scale);

    let mut y = scanline.y + scanline.h + 14;
    draw_panel_text(frame, rect.x + 10, y, "Legend", PANEL_TEXT_DIM, 1, scale);
    let mut x = rect.x + 66;
    let codes: &[u8] = if cpu_wait { b"RBSDACLNp" } else { b"RBSDACLP." };
    for &code in codes {
        let (color, name) = if cpu_wait {
            (cpu_wait_color(code), cpu_wait_name_for_code(code))
        } else {
            (owner_color(code), owner_name_for_code(code))
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
            color,
            scale,
        );
        draw_panel_text(frame, x + 12, y, name, PANEL_TEXT, 1, scale);
        // Entries pack by label so the wait legend's extra row fits.
        x += 12 + name.len() * font::GLYPH_W + 10;
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

/// The guest-registered resource covering any byte of `cell`, if one does.
fn heat_resource_at(view: &AnalyzerHeatView, cell: usize) -> Option<&AnalyzerHeatResource> {
    let start = view
        .base
        .saturating_add((cell as u32).saturating_mul(view.bytes_per_cell));
    let end = start.saturating_add(view.bytes_per_cell.saturating_sub(1));
    view.resources
        .iter()
        .find(|resource| resource.start <= end && start < resource.end)
}

/// `  in 'name' (kind)` when a registered resource covers the cell.
fn heat_resource_suffix(view: &AnalyzerHeatView, cell: usize) -> String {
    heat_resource_at(view, cell)
        .map(|resource| format!("  in '{}' ({})", resource.name, resource.kind))
        .unwrap_or_default()
}

fn draw_analyzer_blits_tab(
    frame: &mut [u8],
    rect: Rect,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = analyzer_content_top(rect);
    let Some(blits) = &view.blits else {
        draw_panel_text(
            frame,
            rect.x + 10,
            content_top,
            "No blits captured in the current frame trace.",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        return;
    };
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        "#  beam span        mode/channels    size       used/stall   destination",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    for ((control, row_rect), row) in analyzer_blit_row_rects(rect).into_iter().zip(&blits.rows) {
        if row.selected {
            fill_rect(frame, scale_rect(row_rect, scale), ENTRY_BG, scale);
        }
        draw_panel_text(
            frame,
            row_rect.x,
            row_rect.y + 3,
            &row.text,
            if row.selected {
                PANEL_TEXT_HILIGHT
            } else if hover == Some(control) {
                PANEL_TEXT_ACCENT
            } else {
                PANEL_TEXT
            },
            1,
            scale,
        );
    }
    if blits.hidden_above > 0 || blits.hidden_below > 0 {
        draw_panel_text(
            frame,
            rect.x + rect.w.saturating_sub(214),
            content_top,
            &format!(
                "up/down: {} above, {} below",
                blits.hidden_above, blits.hidden_below
            ),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    let detail = analyzer_blit_detail_rect(rect);
    draw_panel_text(
        frame,
        detail.x,
        detail.y - 20,
        &format!("{}    {}", blits.detail, blits.formula),
        PANEL_TEXT_ACCENT,
        1,
        scale,
    );
    let gap = 8;
    let half = detail.w.saturating_sub(gap) / 2;
    let source_rect = Rect {
        x: detail.x,
        y: detail.y,
        w: half,
        h: detail.h,
    };
    let dest_rect = Rect {
        x: detail.x + half + gap,
        y: detail.y,
        w: half,
        h: detail.h,
    };
    draw_panel_text(
        frame,
        source_rect.x,
        source_rect.y,
        blits.source_label,
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_panel_text(
        frame,
        dest_rect.x,
        dest_rect.y,
        "result / D",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    let inset = |area: Rect| Rect {
        x: area.x,
        y: area.y + 14,
        w: area.w,
        h: area.h.saturating_sub(14),
    };
    if let Some(preview) = &blits.source {
        draw_resource_bitmap_preview(frame, inset(source_rect), preview, scale);
    }
    if let Some(preview) = &blits.destination {
        draw_resource_bitmap_preview(frame, inset(dest_rect), preview, scale);
    }
}

fn draw_analyzer_resources_tab(
    frame: &mut [u8],
    rect: Rect,
    view: &FrameAnalyzerView,
    hover: Option<UiControl>,
    scale: usize,
) {
    let content_top = analyzer_content_top(rect);
    let Some(resources) = &view.resources else {
        return;
    };
    if resources.rows.is_empty() {
        draw_panel_text(
            frame,
            rect.x + 10,
            content_top,
            "no resources registered (uaelib debug_register_*)",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        return;
    }
    draw_panel_text(
        frame,
        rect.x + 10,
        content_top,
        "name         type        address    size      geometry",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    for ((control, row_rect), row) in analyzer_resource_row_rects(rect)
        .into_iter()
        .zip(&resources.rows)
    {
        if row.selected {
            fill_rect(frame, scale_rect(row_rect, scale), ENTRY_BG, scale);
        }
        let colour = if row.selected {
            PANEL_TEXT_HILIGHT
        } else if hover == Some(control) {
            PANEL_TEXT_ACCENT
        } else {
            PANEL_TEXT
        };
        draw_panel_text(
            frame,
            row_rect.x,
            row_rect.y + 2,
            &row.text,
            colour,
            1,
            scale,
        );
    }
    let more_y = analyzer_content_top(rect) + 16 + resources.rows.len() * ANALYZER_RESOURCE_ROW_H;
    if resources.hidden_above > 0 || resources.hidden_below > 0 {
        draw_panel_text(
            frame,
            rect.x + 10,
            more_y,
            &format!(
                "{} above / {} below (cursor keys scroll)",
                resources.hidden_above, resources.hidden_below
            ),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }

    let detail_rect = analyzer_resource_detail_rect(rect);
    match &resources.detail {
        None => draw_panel_text(
            frame,
            detail_rect.x,
            detail_rect.y,
            "click a resource to preview it",
            PANEL_TEXT_DIM,
            1,
            scale,
        ),
        Some(AnalyzerResourceDetail::Bitmap(preview)) => {
            draw_resource_bitmap_preview(frame, detail_rect, preview, scale);
        }
        Some(AnalyzerResourceDetail::Palette { colours }) => {
            for (idx, colour) in colours.iter().enumerate().take(256) {
                fill_rect(
                    frame,
                    scale_rect(
                        Rect {
                            x: detail_rect.x + (idx % 32) * VIDEO_PALETTE_CELL_W,
                            y: detail_rect.y + (idx / 32) * VIDEO_PALETTE_CELL_H,
                            w: VIDEO_PALETTE_CELL_W - 1,
                            h: VIDEO_PALETTE_CELL_H - 1,
                        },
                        scale,
                    ),
                    *colour,
                    scale,
                );
            }
        }
        Some(AnalyzerResourceDetail::Copperlist { lines }) => {
            for (idx, line) in lines.iter().enumerate() {
                let y = detail_rect.y + idx * 10;
                if y + 8 > detail_rect.y + detail_rect.h {
                    break;
                }
                draw_panel_text(frame, detail_rect.x, y, line, PANEL_TEXT, 1, scale);
            }
        }
    }
}

/// Nearest-sample a decoded bitmap preview into the detail area,
/// preserving its aspect (the [`draw_heat_map`] sampling pattern), with
/// the decoder's note underneath.
fn draw_resource_bitmap_preview(
    frame: &mut [u8],
    detail_rect: Rect,
    preview: &crate::video::resource_preview::BitmapPreview,
    scale: usize,
) {
    let note_h = 12;
    let box_h = detail_rect.h.saturating_sub(note_h);
    if preview.width == 0 || preview.height == 0 || box_h < 8 {
        draw_panel_text(
            frame,
            detail_rect.x,
            detail_rect.y,
            preview.note.as_deref().unwrap_or("nothing to preview"),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        return;
    }
    // Fit the picture into the box: integer upscale when it is small,
    // proportional downsample when it is big.
    let fit = |avail: usize, src: usize| -> usize {
        if src <= avail {
            (src * (avail / src).max(1)).min(avail)
        } else {
            avail
        }
    };
    let scale_num = usize::min(
        fit(detail_rect.w, preview.width) * 1000 / preview.width,
        fit(box_h, preview.height) * 1000 / preview.height,
    );
    let out_w = (preview.width * scale_num / 1000).max(1);
    let out_h = (preview.height * scale_num / 1000).max(1);
    let shown = Rect {
        x: detail_rect.x,
        y: detail_rect.y,
        w: out_w,
        h: out_h,
    };
    for y in 0..out_h {
        let src_y = y * preview.height / out_h;
        for x in 0..out_w {
            let src_x = x * preview.width / out_w;
            let pixel = preview
                .pixels
                .get(src_y * preview.width + src_x)
                .copied()
                .unwrap_or(0xFF00_0000);
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: shown.x + x,
                        y: shown.y + y,
                        w: 1,
                        h: 1,
                    },
                    scale,
                ),
                pixel,
                scale,
            );
        }
    }
    draw_outline(frame, shown, BUTTON_EDGE_LIGHT, scale);
    let caption = match &preview.note {
        Some(note) => format!("{}x{}  {}", preview.width, preview.height, note),
        None => format!("{}x{}", preview.width, preview.height),
    };
    draw_panel_text(
        frame,
        detail_rect.x,
        shown.y + out_h + 4,
        &caption,
        PANEL_TEXT_DIM,
        1,
        scale,
    );
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
            format!(
                "{}{}",
                heat_cell_range(heat.base, heat.bytes_per_cell, cell),
                heat_resource_suffix(heat, cell)
            ),
            PANEL_TEXT,
            None,
        ),
        (None, Some(cell)) => {
            let range = heat_cell_range(heat.base, heat.bytes_per_cell, cell);
            let in_resource = heat_resource_suffix(heat, cell);
            match heat.selected.as_ref().filter(|sel| sel.cell == cell) {
                Some(sel) => {
                    let mut text = format!("{range}  {}", sel.toucher.unwrap_or("untouched"));
                    if let Some(age) = sel.age_frames {
                        text.push_str(&format!("  age {age}f"));
                    }
                    text.push_str(&in_resource);
                    (text, PANEL_TEXT_HILIGHT, Some(sel.colour))
                }
                None => (format!("{range}  untouched{in_resource}"), PANEL_TEXT, None),
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
            lit(hover, control).max(f32::from(u8::from(active))),
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
            super::about::draw(frame, rect, view, texture_scale)
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
) {
    if let Some(panel) = &ui.panel {
        draw_panel_layer(frame, texture_scale, panel, hover, data);
    }
    if ui.menu_open {
        draw_menu(frame, &ui.menu_rows, &ui.menu_nav, texture_scale);
    }
}

// ---------------------------------------------------------------------------
// The pop-up menu
// ---------------------------------------------------------------------------

/// The menu's own ground: the status bar's colour, since the menu is
/// the bar's.
const MENU_BG: u32 = super::window::STATUS_BG;

/// The open menu wears the same veil as every other overlay.
const MENU_VEIL: u32 = SCRIM;
const MENU_VEIL_ALPHA: f32 = SCRIM_ALPHA;

/// A tick, drawn rather than typed: the font stops at ASCII, and a mark built
/// from the text scale grows with it.
fn draw_check(frame: &mut [u8], x: usize, y: usize, color: u32, px: usize, scale: usize) {
    // Two strokes of a check: a short one down-right, a long one up-right.
    let dot = |frame: &mut [u8], cx: usize, cy: usize| {
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x: cx,
                    y: cy,
                    w: px,
                    h: px,
                },
                scale,
            ),
            color,
            scale,
        );
    };
    for i in 0..3 {
        dot(frame, x + i * px, y + (2 + i) * px);
    }
    for i in 0..4 {
        dot(frame, x + (2 + i) * px, y + (4 - i) * px);
    }
}

/// Draw the menu: a veil over everything behind, then one column per open
/// level from the hamburger button upward.
fn draw_menu(frame: &mut [u8], rows: &[menu::MenuRow], nav: &menu::MenuNav, scale: usize) {
    // The veil goes over the display, panels included: the menu takes
    // precedence over anything it is opened on top of. The status bar
    // below is left alight -- it is still live while the menu is up,
    // and a dialog does not dim it either. It is painted into the
    // presentation texture, so it never reaches a recording.
    fill_rect_blend(
        frame,
        Rect {
            x: 0,
            y: 0,
            w: texture_width(scale),
            h: super::present_height() * scale,
        },
        MENU_VEIL,
        MENU_VEIL_ALPHA,
        scale,
    );

    let px = super::menu_scale().factor();
    let levels = nav.levels(rows);
    let columns = menu_columns(&levels, nav);
    let deepest = columns.len().saturating_sub(1);
    let inset = menu::MENU_TEXT_INSET * px;
    let glyph_w = font::GLYPH_W * px;
    for (depth, (column, level)) in columns.iter().zip(levels.iter()).enumerate() {
        let panel = Rect {
            x: column.x,
            y: column.y,
            w: column.w,
            h: column.h,
        };
        // The menu wears the status bar's own colour: it belongs to the
        // bar it hangs from, not to the panels it opens over.
        fill_rect(frame, scale_rect(panel, scale), MENU_BG, scale);
        draw_rect_bevel(
            frame,
            scale_rect(panel, scale),
            BUTTON_EDGE_LIGHT,
            BUTTON_EDGE_DARK,
            scale,
        );

        // A level that marks one of its rows indents them all, so the labels
        // stay in a line whether or not they carry the tick.
        let ticked = level.iter().any(menu::MenuRow::marks_state);
        let indent = inset + usize::from(ticked) * 2 * glyph_w;

        for n in 0..column.visible {
            let index = column.first + n;
            let Some(row) = level.get(index) else {
                continue;
            };
            let (rx, ry, rw, rh) = column.row_rect(n);
            // The cursor marks the deepest level; above it, the row that was
            // opened stays lit so the trail back is visible.
            let lit = if depth == deepest {
                nav.cursor() == Some(index)
            } else {
                nav.open_at(depth) == Some(index)
            };
            if lit && row.enabled {
                fill_rect(
                    frame,
                    scale_rect(
                        Rect {
                            x: rx,
                            y: ry,
                            w: rw,
                            h: rh,
                        },
                        scale,
                    ),
                    MENU_HILIGHT_BG,
                    scale,
                );
            }
            let text_y = ry + rh.saturating_sub(font::GLYPH_H * px) / 2;
            let color = if matches!(row.kind, menu::MenuRowKind::Caption) {
                // A caption is not a row that has been taken away, so it does
                // not read as one: it takes the colour a value carries.
                PANEL_TEXT_HILIGHT
            } else if !row.enabled {
                PANEL_TEXT_DIM
            } else if lit {
                MENU_HILIGHT_TEXT
            } else {
                PANEL_TEXT
            };
            if row.marked() {
                draw_check(frame, rx + inset, text_y, color, px, scale);
            }
            draw_panel_text(frame, rx + indent, text_y, &row.label, color, px, scale);

            // The value sits against the right edge, before the marker a
            // submenu ends with.
            let marker_w = usize::from(row.is_submenu()) * 2 * glyph_w;
            if let Some(value) = &row.value {
                let vw = value.chars().count() * glyph_w;
                let vx = rx + rw.saturating_sub(inset + marker_w + vw);
                let vcolor = if lit {
                    MENU_HILIGHT_TEXT
                } else {
                    PANEL_TEXT_HILIGHT
                };
                draw_panel_text(frame, vx, text_y, value, vcolor, px, scale);
            }
            if row.is_submenu() {
                let mx = rx + rw.saturating_sub(inset + glyph_w);
                draw_panel_text(frame, mx, text_y, ">", color, px, scale);
            }
        }
    }
}

/// Where each open level sits. Drawing and hit-testing both come through
/// here, so the menu cannot be clicked anywhere but where it is drawn.
fn menu_columns(levels: &[&[menu::MenuRow]], nav: &menu::MenuNav) -> Vec<menu::layout::Column> {
    let opened: Vec<Option<usize>> = (0..levels.len()).map(|d| nav.open_at(d)).collect();
    menu::layout::columns(
        levels,
        &opened,
        MENU_BUTTON_X + MENU_BUTTON_W,
        present_height(),
        super::menu_scale().factor(),
    )
}

/// Which level and row the pointer is over, if any.
pub fn menu_hit(
    rows: &[menu::MenuRow],
    nav: &menu::MenuNav,
    pos: (usize, usize),
) -> Option<(usize, usize)> {
    let levels = nav.levels(rows);
    let columns = menu_columns(&levels, nav);
    // Innermost first: a child overlapping its parent takes the pointer.
    for (depth, column) in columns.iter().enumerate().rev() {
        if let Some(row) = column.row_at(pos.0, pos.1) {
            return Some((depth, row));
        }
    }
    None
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
mod tests;

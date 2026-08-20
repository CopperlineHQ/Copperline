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
    draw_rect_bevel, fill_rect, fill_rect_blend, rgba, scale_rect, texture_height, texture_width,
    Rect, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE, BUTTON_FACE_HOVER,
};
use super::{font, present_height, FB_WIDTH, HOST_SHORTCUT_MODIFIER_LABEL};
use crate::config::MachineModel;
use crate::debugger::{BreakCond, CondOp, CondOperand};
use crate::heatmap;

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
const SCRIM: u32 = rgba(0, 0, 0);
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
    /// Configuration screen: flip a directory-mount drive between FFS and OFS.
    LauncherDriveFilesystemToggle(LauncherField),
    /// A free-text box on a Create Image page (a volume or device name).
    LauncherNewImageEdit(LauncherField),
    /// A serial TCP address box on the I/O Ports tab (Connect or Listen).
    LauncherSerialAddrEdit(LauncherField),
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
        Panel::Calibration(_) => (620, 372),
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
    draw_title_bar(
        frame,
        rect,
        panel_title(panel),
        hover == Some(UiControl::PanelClose),
        scale,
    );
}

/// A panel's blue title bar, with its name and its close gadget.
fn draw_title_bar(frame: &mut [u8], rect: Rect, title: &str, close_hover: bool, scale: usize) {
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
fn draw_close_gadget(frame: &mut [u8], rect: Rect, close_hover: bool, scale: usize) {
    let close = close_button_rect(rect);
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
const SHORTCUT_ROW_H: usize = 18;
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

const SHORTCUT_ROWS: [(&str, &str, bool); 24] = [
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

// Full canvas width: the panel's edges line up with the status bar
// below it rather than leaving gutters of display either side.
const LAUNCHER_W: usize = FB_WIDTH;
const LAUNCHER_H: usize = 520;
const LAUNCH_MARGIN: usize = 8;
const LAUNCH_MODEL_H: usize = 22;
const LAUNCH_MODEL_GAP: usize = 4;
/// Machines per row in the selector grid before it wraps; the grid rebalances
/// so the buttons fill the width (eight fit one row; the current ten models
/// wrap to two balanced rows).
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
/// The priority column's value box. Narrower than the general one, which is
/// sized for device names: the widest thing here is "No drive", against a
/// priority otherwise (down to the "-128" that a cleared Bootable box
/// stores), and this leaves a clear margin either side.
const LAUNCH_BOOTPRI_VALUE_W: usize = 96;
const LAUNCH_TOGGLE_W: usize = 64;
const LAUNCH_ACTION_W: usize = 84;
const LAUNCH_ACTION_H: usize = 22;
const LAUNCH_BROWSE_W: usize = 66;
const LAUNCH_CLEAR_W: usize = LAUNCH_BROWSE_W;
/// Width of the path-preview text column before a path row's Browse/Clear
/// buttons. The buttons sit just after it (near the other control widgets)
/// rather than out at the panel's right edge; a long value is clipped to fit.
const LAUNCH_PATH_VALUE_W: usize = 216;
/// Width of the editable volume-name box on a drive row.
const LAUNCH_NAME_W: usize = 96;
/// Width of the FFS/OFS toggle button on a drive row (just "FFS"/"OFS").
const LAUNCH_FS_W: usize = 40;
/// Width of the serial TCP address box. Far wider than a volume name's,
/// because a host name and a port together are a long string and the port
/// is at the far end of it -- the part a reader most needs to see.
const LAUNCH_ADDR_W: usize = LAUNCH_PATH_VALUE_W;
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
    launcher_stepper_rects(rect, row_y, LAUNCH_VALUE_W)
}

/// The geometry figures' `< value >`, on the same run as every other
/// stepper in the launcher.
fn launcher_geometry_stepper_rects(rect: Rect, row_y: usize) -> (Rect, Rect, Rect) {
    launcher_stepper_rects(rect, row_y, 64)
}

/// The priority column's `< value >`, on its own narrower value box.
fn launcher_bootpri_rects(rect: Rect, row_y: usize) -> (Rect, Rect, Rect) {
    launcher_stepper_rects(rect, row_y, LAUNCH_BOOTPRI_VALUE_W)
}

fn launcher_stepper_rects(rect: Rect, row_y: usize, value_w: usize) -> (Rect, Rect, Rect) {
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
        w: value_w,
        h: LAUNCH_CONTROL_H,
    };
    let next = Rect {
        x: value.x + value_w,
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

/// Nav-row buttons per row before they wrap.
const LAUNCH_NAV_PER_ROW: usize = 4;

/// A nav-row button, in the machine selector's own size and rhythm: same
/// width, same height, same gap, and the first one sits in the machine
/// grid's second column so it lines up with the button above it. Four to a
/// row, wrapping after that.
fn launcher_nav_button_rect(rect: Rect, slot: usize) -> Rect {
    // Column 1 of the machine grid, which is where the pane's own left
    // edge very nearly falls: taking the grid's column exactly is what
    // makes the two rows read as one column of buttons.
    let above = launcher_model_rect(rect, 1);
    let (row, col) = (slot / LAUNCH_NAV_PER_ROW, slot % LAUNCH_NAV_PER_ROW);
    Rect {
        x: above.x + col * (above.w + LAUNCH_MODEL_GAP),
        y: launcher_nav_y(rect) + row * (LAUNCH_MODEL_H + LAUNCH_MODEL_GAP),
        w: above.w,
        h: LAUNCH_MODEL_H,
    }
}

/// How tall the nav row block is for a given number of buttons, so the
/// settings below it start clear of a wrapped second row.
fn launcher_nav_rows(slots: usize) -> usize {
    slots.max(1).div_ceil(LAUNCH_NAV_PER_ROW)
}

/// A free-text value box: where a value would sit, at the width its content
/// needs -- a volume or device name on a Create Image row, or the longer
/// `host:port` of a serial address on the I/O Ports tab.
fn launcher_text_rect(rect: Rect, row_y: usize, field: LauncherField) -> Rect {
    Rect {
        x: launcher_pane_x(rect) + LAUNCH_LABEL_W,
        y: row_y + (LAUNCH_ROW_H - LAUNCH_CONTROL_H) / 2,
        w: if LauncherState::is_serial_addr(field) {
            LAUNCH_ADDR_W
        } else {
            LAUNCH_NAME_W
        },
        h: LAUNCH_CONTROL_H,
    }
}

/// The button on a Create Image action row: the page's one commitment,
/// rather than another little control.
fn launcher_action_rect(rect: Rect, row_y: usize) -> Rect {
    Rect {
        // At the pane's own left edge, under the labels rather than out in
        // the value column: it acts on the page, not on a row.
        x: launcher_pane_x(rect),
        // Pushed down a little, so it is not mistaken for another setting.
        y: row_y + (LAUNCH_ROW_H - LAUNCH_TAB_H) / 2 + 10,
        // Sized like the category buttons down the left: the same shape the
        // launcher uses everywhere for "go and do this".
        w: LAUNCH_SIDEBAR_W,
        h: LAUNCH_TAB_H,
    }
}

/// The geometry editor's second button, beside its Save.
fn launcher_action2_rect(rect: Rect, row_y: usize) -> Rect {
    let first = launcher_action_rect(rect, row_y);
    Rect {
        x: first.x + first.w + LAUNCH_TAB_GAP,
        ..first
    }
}

/// The typed number on the hard-drive size row. Lines up with the value
/// boxes on the rows below it, so the column reads straight down.
fn launcher_size_box_rect(rect: Rect, row_y: usize) -> Rect {
    Rect {
        x: launcher_pane_x(rect) + LAUNCH_LABEL_W,
        y: row_y + (LAUNCH_ROW_H - LAUNCH_CONTROL_H) / 2,
        w: 64,
        h: LAUNCH_CONTROL_H,
    }
}

/// The Auto / Custom pair on the geometry row, and the Configure button
/// that joins them once the geometry is set by hand. Sized like the
/// Browse/Clear buttons the path rows use.
fn launcher_geometry_rects(rect: Rect, row_y: usize) -> (Rect, Rect, Rect) {
    let y = row_y + (LAUNCH_ROW_H - LAUNCH_CONTROL_H) / 2;
    let auto = Rect {
        x: launcher_pane_x(rect) + LAUNCH_LABEL_W,
        y,
        w: LAUNCH_CLEAR_W,
        h: LAUNCH_CONTROL_H,
    };
    let custom = Rect {
        x: auto.x + auto.w + 4,
        w: LAUNCH_BROWSE_W,
        ..auto
    };
    let configure = Rect {
        x: custom.x + custom.w + 4,
        w: LAUNCH_ACTION_W,
        ..auto
    };
    (auto, custom, configure)
}

/// "a" or "an" for a size like `64M`, which is read aloud as a number:
/// anything beginning with an eight takes "an", as do eleven and eighteen
/// themselves. (Eighteen *thousand* would too, but no size box reaches it.)
fn indefinite_article(size: &str) -> &'static str {
    let leading: String = size.chars().take_while(char::is_ascii_digit).collect();
    let vowel = leading.starts_with('8') || leading == "11" || leading == "18";
    if vowel {
        "an"
    } else {
        "a"
    }
}

/// Draw a free-text/number value box: what the setting holds, or what is
/// being typed into it, with a caret while it has the focus. Used by the
/// Create Image pages and by the Serial section's TCP address boxes, so it
/// reads the value through `row_value` rather than from either store.
fn draw_launcher_value_box(
    frame: &mut [u8],
    box_rect: Rect,
    state: &LauncherState,
    field: LauncherField,
    disabled: bool,
    centred: bool,
    scale: usize,
) {
    draw_rect_bevel(
        frame,
        scale_rect(box_rect, scale),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );
    let avail = box_rect.w.saturating_sub(8);
    if state.typing_in_value_box(field) {
        draw_edit_line(
            frame,
            box_rect.x + 4,
            box_rect.y + 6,
            state.edit_buffer(),
            state.edit_caret().at(),
            PANEL_TEXT_HILIGHT,
            PANEL_BG,
            avail,
            scale,
        );
        return;
    }
    let (text, color) = match disabled {
        true => (state.row_value(field), PANEL_TEXT_DIM),
        false => (state.row_value(field), PANEL_TEXT),
    };
    // A value too long for the box loses its tail: the head is the part
    // that says which of several it is.
    let shown = truncate_to_width(&text, avail);
    // A short figure between two arrows reads as belonging to them when it
    // is centred, and as a stray left-aligned word when it is not.
    let x = if centred {
        let text_w = shown.chars().count() * font::GLYPH_W;
        box_rect.x + box_rect.w.saturating_sub(text_w) / 2
    } else {
        box_rect.x + 4
    };
    draw_panel_text(frame, x, box_rect.y + 6, &shown, color, 1, scale);
}

/// Gap between one tick box's label and the next box along.
const LAUNCH_TICK_GAP: usize = 14;
/// A tick box's own side, and the gap between it and its label.
const LAUNCH_TICK_BOX: usize = 10;
const LAUNCH_TICK_LABEL_GAP: usize = 5;

/// Lay a row of labelled tick boxes across the value column, left to right,
/// and hand back each one's clickable rect (box and label together, so the
/// word is as easy to hit as the square).
fn launcher_tick_strip(rect: Rect, row_y: usize, labels: &[&str]) -> Vec<Rect> {
    let mut x = launcher_pane_x(rect) + LAUNCH_LABEL_W;
    let y = row_y + (LAUNCH_ROW_H - LAUNCH_TICK_BOX) / 2;
    labels
        .iter()
        .map(|label| {
            let w = LAUNCH_TICK_BOX + LAUNCH_TICK_LABEL_GAP + label.len() * font::GLYPH_W;
            let at = Rect {
                x,
                y,
                w,
                h: LAUNCH_TICK_BOX,
            };
            x += w + LAUNCH_TICK_GAP;
            at
        })
        .collect()
}

/// Draw one entry of a tick strip: the box, then its word.
fn draw_launcher_tick_choice(
    frame: &mut [u8],
    at: Rect,
    label: &str,
    set: bool,
    disabled: bool,
    hot: bool,
    scale: usize,
) {
    let colour = if disabled { PANEL_TEXT_DIM } else { TICK_GREEN };
    draw_tick_box(frame, at.x, at.y, set, colour, scale);
    if hot && !disabled {
        draw_outline(
            frame,
            Rect {
                w: LAUNCH_TICK_BOX,
                ..at
            },
            PANEL_TEXT_HILIGHT,
            scale,
        );
    }
    draw_panel_text(
        frame,
        at.x + LAUNCH_TICK_BOX + LAUNCH_TICK_LABEL_GAP,
        at.y + 1,
        label,
        if disabled { PANEL_TEXT_DIM } else { PANEL_TEXT },
        1,
        scale,
    );
}

/// A typed whole number, lined up with the value column beside it.
fn launcher_number_rect(rect: Rect, row_y: usize) -> Rect {
    Rect {
        x: launcher_pane_x(rect) + LAUNCH_LABEL_W,
        y: row_y + (LAUNCH_ROW_H - LAUNCH_CONTROL_H) / 2,
        w: 64,
        h: LAUNCH_CONTROL_H,
    }
}

/// The unit written beside that number. Text, not a button -- but clicking
/// it swaps MB and GB, so it is a control all the same.
fn launcher_size_unit_rect(rect: Rect, row_y: usize) -> Rect {
    let box_rect = launcher_size_box_rect(rect, row_y);
    Rect {
        x: box_rect.x + box_rect.w + 8,
        y: box_rect.y,
        w: 2 * font::GLYPH_W,
        h: LAUNCH_CONTROL_H,
    }
}

/// Geometry of the Host Disk table: the framed box listing what the host has.
const HOST_DISK_ROW_H: usize = 14;
const HOST_DISK_HEADER_H: usize = 16;
/// Rows drawn inside the box at once. A longer list scrolls.
pub(crate) const HOST_DISK_VISIBLE_ROWS: usize = 8;
/// Column starts, as offsets from the inside edge of the box. Volume gets
/// the widest cell -- model strings are the longest text on the page -- and
/// every cell clips at the next column, so a Windows `PhysicalDrive11` reads
/// truncated in Disk rather than running into its neighbour.
const HOST_DISK_COL_DISK: usize = 8;
const HOST_DISK_COL_VOLUME: usize = 112;
const HOST_DISK_COL_SIZE: usize = 272;
const HOST_DISK_COL_ATTACH: usize = 344;
const HOST_DISK_COL_WRITABLE: usize = 440;
/// The last column ends before the scroll arrows, which sit inside the frame.
const HOST_DISK_COL_TICK: usize = 472;

fn host_disk_table_rect(rect: Rect) -> Rect {
    let x = launcher_pane_x(rect);
    Rect {
        x,
        y: launcher_content_top(rect) + LAUNCH_NAV_BLOCK_H + 18,
        w: rect.w.saturating_sub(x - rect.x + 16),
        h: HOST_DISK_HEADER_H + HOST_DISK_VISIBLE_ROWS * HOST_DISK_ROW_H + 4,
    }
}

/// One row inside the table, by index.
fn host_disk_row_rect(rect: Rect, index: usize) -> Rect {
    let table = host_disk_table_rect(rect);
    Rect {
        x: table.x + 2,
        y: table.y + HOST_DISK_HEADER_H + index * HOST_DISK_ROW_H,
        w: table.w.saturating_sub(4),
        h: HOST_DISK_ROW_H,
    }
}

// --- the WHDLoad Library page ---------------------------------------------

/// The games list starts level with the top of the Memory tab and is as
/// tall as the art frame beside it; the favourites list fills what is left
/// below, down to the status line. Both are worked out from the panel
/// rather than from a row count, so these are what that comes to -- and
/// what the scrolling and hit-testing count in.
///
/// `whdload_entry` is whether the strip carries the WHDLoad entry -- see
/// [`launcher::tabs`] -- since the strip is a row longer when it does, and
/// every rect on this page is measured against it. Every layout function
/// here takes it for the same reason.
#[cfg(feature = "game-library")]
pub(in crate::video) fn library_visible_rows(rect: Rect, whdload_entry: bool) -> usize {
    library_table_rect(rect, whdload_entry)
        .h
        .saturating_sub(LIBRARY_HEADER_H + 4)
        / LIBRARY_ROW_H
}

#[cfg(feature = "game-library")]
pub(in crate::video) fn library_favourite_rows(rect: Rect, whdload_entry: bool) -> usize {
    library_favourites_rect(rect, whdload_entry)
        .h
        .saturating_sub(LIBRARY_HEADER_H + 4)
        / LIBRARY_ROW_H
}
#[cfg(feature = "game-library")]
const LIBRARY_ROW_H: usize = 14;
#[cfg(feature = "game-library")]
const LIBRARY_HEADER_H: usize = 16;
/// The widest a cover is drawn. The gap either side of its frame is
/// [`LIBRARY_COVER_GAP`], and the frame around it [`LIBRARY_COVER_BEZEL`].
#[cfg(feature = "game-library")]
const LIBRARY_COVER: usize = 128;
/// How much taller than wide the art frame is. Amiga box art is portrait:
/// measured across the catalogue it runs between 0.75 and 0.82 wide-to-tall
/// with the odd square compilation, so 4:5 sits in the middle of what is
/// really there and a picture of any of those shapes only has to give up a
/// thin margin to fit.
#[cfg(feature = "game-library")]
const LIBRARY_COVER_TALL: (usize, usize) = (5, 4);
/// The frame around the art: thicker than the list's hairline outline and
/// bevelled, so the picture reads as mounted in the panel rather than
/// pasted onto it.
#[cfg(feature = "game-library")]
const LIBRARY_COVER_BEZEL: usize = 5;
/// Between the game list and the row of buttons under it.
#[cfg(feature = "game-library")]
const LIBRARY_BUTTON_GAP: usize = 8;
/// How many lines the version under the cover runs to.
#[cfg(feature = "game-library")]
const LIBRARY_VERSION_LINES: usize = 2;
/// How many lines each catalogue field under the cover runs to. A
/// developer is sometimes credited to nine people, and without a limit
/// that one field pushes everything under it off the panel.
#[cfg(feature = "game-library")]
const LIBRARY_FIELD_LINES: usize = 2;

/// The most a version may be, in characters: what fits the column it is
/// drawn in, over [`LIBRARY_VERSION_LINES`] lines. The editor stops there
/// too, since there is no use in typing what the page cannot show.
#[cfg(feature = "game-library")]
pub(in crate::video) fn library_version_max() -> usize {
    LIBRARY_VERSION_LINES * (LIBRARY_COVER + 2 * LIBRARY_COVER_BEZEL) / font::GLYPH_W
}
#[cfg(feature = "game-library")]
const LIBRARY_COVER_GAP: usize = 12;
/// Where each column starts, from the inside edge of the box. Two columns:
/// the game, and whether it is a favourite. Year and publisher moved under
/// the cover art, where there is room to read them.
#[cfg(feature = "game-library")]
const LIBRARY_COL_NAME: usize = 6;
/// The Favourite column, far enough right that a long title clips before
/// it rather than running into it.
/// Where the tick column starts, as an offset into the box.
///
/// Measured back from the right-hand edge rather than fixed, so the
/// heading and the ticks under it stay clear of the scroll arrows inside
/// the frame -- which they did not when the art column beside them grew.
#[cfg(feature = "game-library")]
fn library_col_favourite(rect: Rect, whdload_entry: bool) -> usize {
    let table = library_table_rect(rect, whdload_entry);
    let heading = "Favourite".len() * font::GLYPH_W;
    table
        .w
        .saturating_sub(HOST_DISK_ARROW + 12 + heading + 6)
        .max(LIBRARY_COL_NAME + 40)
}

/// Where a tab sits in the strip, whichever strip is showing.
#[cfg(feature = "game-library")]
fn strip_rect(rect: Rect, tab: launcher::LauncherTab, whdload_entry: bool) -> Rect {
    let at = launcher::tabs(whdload_entry)
        .iter()
        .position(|&t| t == tab)
        .unwrap_or(0);
    launcher_tab_rect(rect, at)
}

/// The games list, squared off against the strip beside it: its top level
/// with the top of Memory, its bottom with the bottom of I/O Ports. Tying
/// it to the strip rather than to a row count keeps the page looking
/// deliberate when the strip changes -- which it does, since WHDLoad can
/// join it.
#[cfg(feature = "game-library")]
fn library_table_rect(rect: Rect, whdload_entry: bool) -> Rect {
    let top = strip_rect(rect, launcher::LauncherTab::Memory, whdload_entry);
    let x = launcher_pane_x(rect);
    let right = rect.x + rect.w - 16;
    Rect {
        x,
        y: top.y,
        w: right
            .saturating_sub(x)
            .saturating_sub(library_cover_column()),
        // The art frame's height, so the two boxes end on one line. Its
        // top stays level with the top of Memory in the strip; whatever it
        // no longer reaches down to, the favourites list below it takes.
        h: library_cover_size().1,
    }
}

/// The favourites list, under the games with the button row between them.
/// It stops short of the bottom so the panel's own status line, which
/// reports what just happened, is never drawn over.
#[cfg(feature = "game-library")]
fn library_favourites_rect(rect: Rect, whdload_entry: bool) -> Rect {
    let games = library_table_rect(rect, whdload_entry);
    // The gap: the button row, then the "Favourites:" label above the box.
    let y = games.y + games.h + LIBRARY_BUTTON_GAP + LAUNCH_MODEL_H + 10 + 14;
    let bottom = launcher_status_y(rect).saturating_sub(10);
    Rect {
        y,
        h: bottom.saturating_sub(y),
        ..games
    }
}

/// One row of the favourites list.
#[cfg(feature = "game-library")]
fn library_favourite_row_rect(rect: Rect, whdload_entry: bool, drawn: usize) -> Rect {
    let table = library_favourites_rect(rect, whdload_entry);
    Rect {
        x: table.x + 2,
        y: table.y + LIBRARY_HEADER_H + drawn * LIBRARY_ROW_H,
        w: table.w.saturating_sub(4),
        h: LIBRARY_ROW_H,
    }
}

/// The Favourite tick on one drawn row: centred under its heading rather
/// than tucked against the left of the column.
#[cfg(feature = "game-library")]
fn library_favourite_box(rect: Rect, whdload_entry: bool, drawn: usize) -> Rect {
    centred_tick(
        library_row_rect(rect, whdload_entry, drawn),
        library_col_favourite(rect, whdload_entry),
        "Favourite",
    )
}

/// The Remove tick on one row of the favourites list.
#[cfg(feature = "game-library")]
fn library_remove_box(rect: Rect, whdload_entry: bool, drawn: usize) -> Rect {
    // On the same line as the Favourite tick in the list above it, not
    // centred under its own shorter heading: two columns of the same tick
    // that do not line up read as a mistake.
    centred_tick(
        library_favourite_row_rect(rect, whdload_entry, drawn),
        library_col_favourite(rect, whdload_entry),
        "Favourite",
    )
}

/// Where the "Remove" heading goes: centred over its own ticks.
#[cfg(feature = "game-library")]
fn library_remove_heading_x(rect: Rect, whdload_entry: bool) -> usize {
    let tick = centred_tick(
        library_favourites_rect(rect, whdload_entry),
        library_col_favourite(rect, whdload_entry),
        "Favourite",
    );
    (tick.x + 5).saturating_sub("Remove".len() * font::GLYPH_W / 2)
}

/// A tick in the second column, centred under a heading of that width
/// rather than tucked against the left of the column.
#[cfg(feature = "game-library")]
fn centred_tick(row: Rect, column: usize, heading: &str) -> Rect {
    let width = heading.len() * font::GLYPH_W;
    Rect {
        x: row.x + 4 + column + width.saturating_sub(10) / 2,
        y: row.y + (row.h - 10) / 2,
        w: 10,
        h: 10,
    }
}

/// One row of the list, by drawn position rather than by index into the
/// library: the list scrolls, so the two differ.
#[cfg(feature = "game-library")]
fn library_row_rect(rect: Rect, whdload_entry: bool, drawn: usize) -> Rect {
    let table = library_table_rect(rect, whdload_entry);
    Rect {
        x: table.x + 2,
        y: table.y + LIBRARY_HEADER_H + drawn * LIBRARY_ROW_H,
        w: table.w.saturating_sub(4),
        h: LIBRARY_ROW_H,
    }
}

/// How much of the panel's width the art column takes: the widest frame,
/// with a gap either side of it, so the frame is centred in a space rather
/// than pressed against the list.
#[cfg(feature = "game-library")]
fn library_cover_column() -> usize {
    library_cover_size().0 + 2 * LIBRARY_COVER_GAP
}

/// The art frame at its widest, which is also the game list's height: the
/// two boxes end on the same line, and it is the frame -- sized from the
/// shape of a cover -- that decides where that line is. The frame is never
/// stretched to reach anything.
#[cfg(feature = "game-library")]
fn library_cover_size() -> (usize, usize) {
    (
        LIBRARY_COVER + 2 * LIBRARY_COVER_BEZEL,
        LIBRARY_COVER * LIBRARY_COVER_TALL.0 / LIBRARY_COVER_TALL.1 + 2 * LIBRARY_COVER_BEZEL,
    )
}

/// The art frame: the size the layout reserves, whatever is in it. A
/// picture that is not this shape is fitted inside and letterboxed, rather
/// than the frame being cut to the picture -- a frame that changed shape
/// per game would drag the metadata under it up and down the page.
#[cfg(feature = "game-library")]
fn library_cover_rect(rect: Rect, whdload_entry: bool) -> Rect {
    let table = library_table_rect(rect, whdload_entry);
    let column = table.x + table.w;
    let right = rect.x + rect.w - 16;
    let (w, h) = library_cover_size();
    Rect {
        x: column + right.saturating_sub(column).saturating_sub(w) / 2,
        y: table.y,
        w,
        h,
    }
}

/// The most a picture may be, inside the widest frame.
#[cfg(feature = "game-library")]
fn library_art_rect(rect: Rect, whdload_entry: bool) -> Rect {
    let frame = library_cover_rect(rect, whdload_entry);
    Rect {
        x: frame.x + LIBRARY_COVER_BEZEL,
        y: frame.y + LIBRARY_COVER_BEZEL,
        w: frame.w - 2 * LIBRARY_COVER_BEZEL,
        h: frame.h - 2 * LIBRARY_COVER_BEZEL,
    }
}

/// The three buttons under the game list: as thin as the ones along the
/// top, and sized so a third fits beside the two there are.
#[cfg(feature = "game-library")]
fn library_button_rects(rect: Rect, whdload_entry: bool) -> [Rect; 3] {
    let table = library_table_rect(rect, whdload_entry);
    let gap = 6;
    let w = (table.w + gap) / 3 - gap;
    std::array::from_fn(|i| Rect {
        x: table.x + i * (w + gap),
        y: table.y + table.h + LIBRARY_BUTTON_GAP,
        w,
        h: LAUNCH_MODEL_H,
    })
}

/// The sign-in dialog: a small box in the middle of the panel.
#[cfg(feature = "game-library")]
fn login_rect(rect: Rect) -> Rect {
    // Wide enough that the title clears its own close gadget.
    let (w, h) = (380, 128 + TITLE_H);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// Its two value boxes, and its two buttons.
#[cfg(feature = "game-library")]
fn login_field_rect(rect: Rect, field: launcher::LoginField) -> Rect {
    let dialog = login_rect(rect);
    let label = 10 * font::GLYPH_W;
    Rect {
        x: dialog.x + 12 + label,
        y: dialog.y + TITLE_H + 20 + usize::from(field == launcher::LoginField::Pass) * 26,
        w: dialog.w.saturating_sub(24 + label),
        h: 18,
    }
}

/// The metadata editor: the art on the left at the shape a cover is, the
/// fields down the right, the buttons along the bottom.
#[cfg(feature = "game-library")]
fn meta_rect(rect: Rect) -> Rect {
    let (w, h) = (440, TITLE_H + META_ART.1 + 56);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// The art box inside it, at the same 4:5 the Library page uses.
#[cfg(feature = "game-library")]
const META_ART: (usize, usize) = (112, 140);

#[cfg(feature = "game-library")]
fn meta_art_rect(rect: Rect) -> Rect {
    let dialog = meta_rect(rect);
    Rect {
        x: dialog.x + 14,
        y: dialog.y + TITLE_H + 14,
        w: META_ART.0,
        h: META_ART.1,
    }
}

#[cfg(feature = "game-library")]
fn meta_field_rect(rect: Rect, field: launcher::MetaField) -> Rect {
    let dialog = meta_rect(rect);
    let art = meta_art_rect(rect);
    let label = 10 * font::GLYPH_W;
    let x = art.x + art.w + 12 + label;
    let at = launcher::MetaField::ALL
        .iter()
        .position(|&f| f == field)
        .unwrap_or(0);
    Rect {
        x,
        y: art.y + at * 24,
        w: (dialog.x + dialog.w).saturating_sub(x + 14),
        h: 18,
    }
}

/// Save, Clear and Cancel, in that order.
#[cfg(feature = "game-library")]
fn meta_button_rects(rect: Rect) -> [Rect; 3] {
    let dialog = meta_rect(rect);
    let (w, h, gap) = (66, 20, 8);
    let y = dialog.y + dialog.h - h - 12;
    std::array::from_fn(|i| Rect {
        x: dialog.x + dialog.w - 14 - (3 - i) * (w + gap) + gap,
        y,
        w,
        h,
    })
}

#[cfg(feature = "game-library")]
fn login_button_rects(rect: Rect) -> (Rect, Rect) {
    let dialog = login_rect(rect);
    let (w, h) = (66, 20);
    let y = dialog.y + dialog.h - h - 12;
    (
        Rect {
            x: dialog.x + dialog.w - 2 * w - 12 - 8,
            y,
            w,
            h,
        },
        Rect {
            x: dialog.x + dialog.w - w - 12,
            y,
            w,
            h,
        },
    )
}

/// One A-Z shortcut button.
///
/// Its own drawer rather than [`draw_text_button`], for the hover: the lift
/// a button's face gets is a couple of shades across seven visible pixels,
/// which at this size is no answer to the pointer at all. Hovered, the
/// whole face goes to the blue the chosen list row uses, so there is no
/// mistaking which letter is under it. A letter with nothing behind it
/// does not answer.
#[cfg(feature = "game-library")]
fn draw_az_button(
    frame: &mut [u8],
    rect: Rect,
    label: &str,
    live: bool,
    hovered: bool,
    scale: usize,
) {
    let scaled = scale_rect(rect, scale);
    fill_rect(
        frame,
        scaled,
        if hovered {
            MENU_HILIGHT_BG
        } else {
            BUTTON_FACE
        },
        scale,
    );
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
    let colour = match (live, hovered) {
        (_, true) => MENU_HILIGHT_TEXT,
        (true, false) => BUTTON_TEXT,
        (false, false) => BUTTON_TEXT_DISABLED,
    };
    let text_w = label.chars().count() * font::GLYPH_W;
    let x = rect.x + rect.w.saturating_sub(text_w) / 2;
    let y = rect.y + rect.h.saturating_sub(font::GLYPH_H) / 2;
    draw_panel_text(frame, x, y, label, colour, 1, scale);
}

/// The A-Z shortcut buttons, from just after the "Games:" label to the
/// right edge of the list below them.
///
/// Each is barely wider than the character on it -- the row has to hold
/// twenty-eight of them across the width of the list -- except the digits
/// bucket, which carries three characters and is given the room for them.
/// The leftover pixels are spread one apiece from the left, so the last
/// button ends exactly on the list's right edge rather than a few pixels
/// short of it.
#[cfg(feature = "game-library")]
fn library_az_rects(rect: Rect, whdload_entry: bool) -> Vec<Rect> {
    use launcher::AZ_BUCKETS;
    let table = library_table_rect(rect, whdload_entry);
    let label = "Games:".len() * font::GLYPH_W;
    let x = table.x + label + LIBRARY_AZ_GAP;
    let width = (table.x + table.w).saturating_sub(x);
    let wide = 3 * font::GLYPH_W + 2;
    let narrow_count = AZ_BUCKETS - 1;
    let narrow = width.saturating_sub(wide) / narrow_count;
    // What the division left over, one pixel to each of the first buttons.
    let mut spare = width.saturating_sub(wide + narrow * narrow_count);
    let mut at = x;
    (0..AZ_BUCKETS)
        .map(|bucket| {
            let mut w = if bucket == 0 { wide } else { narrow };
            if spare > 0 {
                w += 1;
                spare -= 1;
            }
            let r = Rect {
                x: at,
                y: table.y.saturating_sub(15),
                w: w.saturating_sub(1),
                h: LIBRARY_AZ_H,
            };
            at += w;
            r
        })
        .collect()
}

/// How many games a list needs before the A-Z row appears.
///
/// A short list is read rather than navigated: with a screenful or so in
/// front of you, twenty-eight buttons to reach one of them is in the way.
#[cfg(feature = "game-library")]
const LIBRARY_AZ_MIN_GAMES: usize = 20;

/// How far the shortcut row starts after the "Games:" label, and how tall
/// its buttons are: the label's own line, so the row costs no height.
#[cfg(feature = "game-library")]
const LIBRARY_AZ_GAP: usize = 6;
#[cfg(feature = "game-library")]
const LIBRARY_AZ_H: usize = 11;

/// The scroll arrows for a list, inside its own frame: up in the top right
/// corner, down in the bottom right. Both Library lists use it, each with
/// its own pair of controls.
#[cfg(feature = "game-library")]
fn library_arrows_in(table: Rect, control: fn(isize) -> UiControl) -> [(UiControl, Rect); 2] {
    let x = table.x + table.w - HOST_DISK_ARROW - 3;
    let arrow = |y| Rect {
        x,
        y,
        w: HOST_DISK_ARROW,
        h: HOST_DISK_ARROW,
    };
    [
        (control(-1), arrow(table.y + 2)),
        (control(1), arrow(table.y + table.h - HOST_DISK_ARROW - 2)),
    ]
}

#[cfg(feature = "game-library")]
fn library_arrow_rects(rect: Rect, whdload_entry: bool) -> [(UiControl, Rect); 2] {
    library_arrows_in(
        library_table_rect(rect, whdload_entry),
        UiControl::LauncherLibraryScroll,
    )
}

#[cfg(feature = "game-library")]
fn library_favourite_arrow_rects(rect: Rect, whdload_entry: bool) -> [(UiControl, Rect); 2] {
    library_arrows_in(
        library_favourites_rect(rect, whdload_entry),
        UiControl::LauncherLibraryFavouriteScroll,
    )
}

/// The scroll arrows, up at the top right of the box and down at the bottom
/// right. Inside the frame rather than beside it, so the box keeps its shape
/// whether or not the list overflows.
const HOST_DISK_ARROW: usize = 12;

fn host_disk_arrow_rects(rect: Rect) -> [(UiControl, Rect); 2] {
    let table = host_disk_table_rect(rect);
    let x = table.x + table.w - HOST_DISK_ARROW - 3;
    [
        (
            UiControl::LauncherHostDiskScroll(-1),
            Rect {
                x,
                y: table.y + 2,
                w: HOST_DISK_ARROW,
                h: HOST_DISK_ARROW,
            },
        ),
        (
            UiControl::LauncherHostDiskScroll(1),
            Rect {
                x,
                y: table.y + table.h - HOST_DISK_ARROW - 2,
                w: HOST_DISK_ARROW,
                h: HOST_DISK_ARROW,
            },
        ),
    ]
}

/// One scroll arrow: a bevelled button with a triangle on it.
///
/// Every scrolling list in the launcher draws its pair with this, so they
/// look and behave alike -- lit while there is somewhere to go that way and
/// greyed at the end of the list, brightened under the pointer.
///
/// The triangle is stacked runs rather than a glyph: the 8x8 font has no
/// arrow in it, and a "^" is a caret, which reads as punctuation next to a
/// list rather than as a direction.
fn draw_scroll_arrow(
    frame: &mut [u8],
    arrow: Rect,
    up: bool,
    live: bool,
    hovered: bool,
    scale: usize,
) {
    let scaled = scale_rect(arrow, scale);
    fill_rect(frame, scaled, BUTTON_FACE, scale);
    draw_rect_bevel(frame, scaled, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK, scale);
    let colour = match (live, hovered) {
        (false, _) => BUTTON_TEXT_DISABLED,
        (true, true) => PANEL_TEXT_HILIGHT,
        (true, false) => BUTTON_TEXT,
    };
    // Three rows is enough to read as an arrow at this size. Widening
    // downwards is an up arrow (narrow tip at the top); widening upwards is
    // a down arrow.
    for step in 0..3usize {
        let width = 1 + step * 2;
        let y = match up {
            true => arrow.y + 4 + step,
            false => arrow.y + 4 + (2 - step),
        };
        fill_rect(
            frame,
            scale_rect(
                Rect {
                    x: arrow.x + HOST_DISK_ARROW / 2 - width / 2 - 1,
                    y,
                    w: width,
                    h: 1,
                },
                scale,
            ),
            colour,
            scale,
        );
    }
}

/// The Attach cell of one row: clicked to step through where the machine
/// would see the disk.
fn host_disk_attach_cell(rect: Rect, index: usize) -> Rect {
    let row = host_disk_row_rect(rect, index);
    Rect {
        x: row.x + HOST_DISK_COL_ATTACH,
        y: row.y,
        // Up to the next column, so a click near the edge cannot land on
        // both this cell and the one beside it.
        w: HOST_DISK_COL_WRITABLE - HOST_DISK_COL_ATTACH,
        h: row.h,
    }
}

/// The R/W cell of one row.
fn host_disk_writable_cell(rect: Rect, index: usize) -> Rect {
    let row = host_disk_row_rect(rect, index);
    Rect {
        x: row.x + HOST_DISK_COL_WRITABLE,
        y: row.y,
        w: HOST_DISK_COL_TICK - HOST_DISK_COL_WRITABLE,
        h: row.h,
    }
}

/// The buttons under the table, left to right: the two acts on the ticked
/// disks first, then Refresh, which only looks.
fn host_disk_button_rects(rect: Rect) -> [(UiControl, Rect); 3] {
    let table = host_disk_table_rect(rect);
    let y = table.y + table.h + 10;
    let button = |slot: usize| Rect {
        x: table.x + slot * 96,
        y,
        w: 88,
        h: LAUNCH_TAB_H,
    };
    [
        (UiControl::LauncherHostDiskMount, button(0)),
        (UiControl::LauncherHostDiskUnmountSelected, button(1)),
        (UiControl::LauncherHostDiskRefresh, button(2)),
    ]
}

/// A sub-page's Back button: the nav row's first slot, always.
fn launcher_back_button_rect(rect: Rect) -> Rect {
    launcher_nav_button_rect(rect, 0)
}

/// How many buttons the nav row holds for a tab: its sibling links, plus a
/// Back button when it is a sub-page.
fn launcher_nav_slots(tab: launcher::LauncherTab) -> usize {
    usize::from(tab.parent_tab().is_some()) + tab.nav_options().len()
}

/// Y of the nav row (the sibling-page buttons and any Back button) at the top of
/// the settings pane, in line with the first category tab. The setting rows
/// below it are shifted down by [`LAUNCH_NAV_BLOCK_H`] to make room.
fn launcher_nav_y(rect: Rect) -> usize {
    launcher_content_top(rect)
}

/// Vertical space reserved at the top of the pane for the nav button row plus a
/// gap below it, before the settings begin, on tabs that have a nav.
const LAUNCH_NAV_BLOCK_H: usize = LAUNCH_MODEL_H + 14;

/// The same, for a tab whose nav wraps onto more than one row.
fn launcher_nav_block_h(tab: launcher::LauncherTab) -> usize {
    let rows = launcher_nav_rows(launcher_nav_slots(tab));
    LAUNCH_NAV_BLOCK_H + (rows - 1) * (LAUNCH_MODEL_H + LAUNCH_MODEL_GAP)
}

/// The Status column's clickable area (the "Bootable" label plus its tick box),
/// sitting to the right of the priority stepper on a Boot Priority row.
fn launcher_bootable_rect(rect: Rect, row_y: usize) -> Rect {
    let (_, _, next) = launcher_bootpri_rects(rect, row_y);
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

/// The heading above the FluxBridge settings: upstream's own name for the
/// library, and which version of it is installed. Nothing else in the launcher
/// says which build is in use, and it is the first thing worth knowing when a
/// drive misbehaves.
fn bridge_library_heading() -> String {
    #[cfg(feature = "fluxbridge")]
    return format!("FluxBridge v{}:", crate::fluxbridge::version());
    #[cfg(not(feature = "fluxbridge"))]
    "FluxBridge:".to_string()
}

const WRITE_PROTECT_LABEL: &str = "Write protect:";
const PHYSICAL_DRIVE_LABEL: &str = "Physical drive:";

/// The two tick-box cells under a floppy drive: write protect on the left,
/// the real-drive switch level with the value column so the eye can run down
/// the tab.
fn launcher_floppy_flag_rects(rect: Rect, row_y: usize) -> (Rect, Rect) {
    let y = row_y + 2;
    let protect = Rect {
        // Indented to sit under the media row's label, which carries its own
        // two leading spaces, so the drive's two lines start together.
        x: launcher_pane_x(rect) + 2 * font::GLYPH_W,
        y,
        w: WRITE_PROTECT_LABEL.len() * font::GLYPH_W + 8 + 12,
        h: LAUNCH_CONTROL_H,
    };
    let bridge = Rect {
        x: launcher_control_x(rect) + LAUNCH_ARROW_W,
        y,
        w: PHYSICAL_DRIVE_LABEL.len() * font::GLYPH_W + 8 + 12,
        h: LAUNCH_CONTROL_H,
    };
    (protect, bridge)
}

/// The tick box inside one of those cells, after its label.
fn launcher_flag_box(cell: Rect, label: &str) -> Rect {
    Rect {
        x: cell.x + label.len() * font::GLYPH_W + 8,
        y: cell.y + (cell.h.saturating_sub(12)) / 2,
        w: 12,
        h: 12,
    }
}

/// The Configure button on a bridged drive's media row, where Browse sits on
/// an image-backed one.
fn launcher_bridge_configure_rect(rect: Rect, row_y: usize) -> Rect {
    let (browse, clear) = launcher_path_rects(rect, row_y);
    Rect {
        x: browse.x,
        y: browse.y,
        w: browse.w + 4 + clear.w,
        h: browse.h,
    }
}

/// (Browse, Clear) buttons for a path row, just after the fixed-width value
/// column ([`LAUNCH_PATH_VALUE_W`]) rather than out at the panel's right edge.
/// Which of a path row's two buttons are there, as (browse, reset).
///
/// Every path row outside the Paths page has both, always. On the Paths
/// page a row that is inheriting has nothing to reset, so it offers only
/// Browse -- and the base swaps the two rather than showing both, because
/// it is the root the others hang off and moving it is a different act
/// from picking a folder for one of them.
///
/// One function, so what is drawn and what can be clicked cannot disagree:
/// a Reset that is not there must not still answer, and a Browse that is
/// not there must not still open a dialog.
fn launcher_path_buttons(setup: &launcher::MachineSetup, field: LauncherField) -> (bool, bool) {
    // The soundfont row keeps both buttons on show; Reset greys out
    // while the bundled GeneralUser GS is already the bank in force.
    #[cfg(feature = "coppersynth")]
    if field == LauncherField::CsynthSoundfont {
        return (true, true);
    }
    if !field.is_paths_field() {
        return (true, true);
    }
    let set = setup.paths_is_set(field);
    if field == LauncherField::PathsBase {
        (!set, set)
    } else {
        (true, set)
    }
}

/// Whether a row is a Paths row that has not been given a directory of its
/// own. Its label and value are dimmed to say so: the row is showing
/// Copperline's answer rather than the person's.
///
/// Not the base. It names a real directory either way, it is the one row
/// on the page that always says something, and dimming the only line that
/// tells you where everything is would be the wrong thing to play down.
fn launcher_path_inherits(setup: &launcher::MachineSetup, field: LauncherField) -> bool {
    // The soundfont row reads the same way: unset means the bundled
    // bank, centred and dimmed as a default rather than left-aligned
    // as if it were a chosen path.
    #[cfg(feature = "coppersynth")]
    if field == LauncherField::CsynthSoundfont {
        return setup.path(field).is_none();
    }
    // The ROMs with bundled defaults read the same way: unset means the
    // bundled image, dimmed as Copperline's answer.
    if field == LauncherField::Rom {
        return setup.path(field).is_none();
    }
    if field == LauncherField::ScsiRom {
        return setup.scsi_controller_is_a4091() && setup.path(field).is_none();
    }
    field.is_paths_field() && field != LauncherField::PathsBase && !setup.paths_is_set(field)
}

/// Whether the row's second button has anything to do: a Clear with
/// nothing behind it is shown but greyed, so the pair of buttons keeps
/// its shape while saying there is nothing to take away. The Paths page
/// keeps its own arrangement -- its Reset only appears once something
/// is set, so it is always live.
fn launcher_clear_enabled(setup: &launcher::MachineSetup, field: LauncherField) -> bool {
    if field.is_paths_field() {
        return true;
    }
    setup.path(field).is_some()
}

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

/// The Download button on a support-archive row.
///
/// To the *left* of Browse rather than after Clear, where the row's value
/// would be. There is room because the button and the value are never both
/// there: it is only offered while nothing has been chosen, and the value
/// then reads "(none)".
#[cfg(feature = "game-library")]
const LAUNCH_DOWNLOAD_W: usize = 78;

#[cfg(feature = "game-library")]
fn launcher_download_rect(rect: Rect, row_y: usize) -> Rect {
    let (browse, _) = launcher_path_rects(rect, row_y);
    Rect {
        x: browse.x.saturating_sub(LAUNCH_DOWNLOAD_W + 6),
        w: LAUNCH_DOWNLOAD_W,
        ..browse
    }
}

/// Which archive a row is for, if it is one of the two.
#[cfg(feature = "game-library")]
fn row_archive(field: LauncherField) -> Option<crate::gamelib::support::Archive> {
    use crate::gamelib::support::Archive;
    match field {
        LauncherField::WhdloadWhdPackage => Some(Archive::Whdload),
        LauncherField::WhdloadSkickPackage => Some(Archive::Skick),
        _ => None,
    }
}

/// The editable volume-name box on a drive row: it sits just left of the
/// Browse button, with the path text filling the space before it.
/// Whether a drive row's FFS/OFS toggle applies: only a directory mount on
/// one of the disk-backed drive fields (IDE/SCSI/lide) has a filesystem
/// choice to make -- an HDF/gzip image already carries its own, and a
/// `Filesys*Dir` row is a live HOSTFS mount, not a disk snapshot, so it has
/// no filesystem to choose either. `drive_is_directory` restricts to
/// exactly that field set on its own (returning `false` for anything else,
/// same as `drive_filesystem`'s fallback) and reads a cached flag rather
/// than statting the path here on every frame the row is drawn.
fn launcher_drive_fs_applies(setup: &launcher::MachineSetup, field: LauncherField) -> bool {
    setup.drive_is_directory(field)
}

fn launcher_drive_name_rect(rect: Rect, row_y: usize) -> Rect {
    let (browse, _clear) = launcher_path_rects(rect, row_y);
    Rect {
        x: browse.x.saturating_sub(6 + LAUNCH_NAME_W),
        y: browse.y,
        w: LAUNCH_NAME_W,
        h: LAUNCH_CONTROL_H,
    }
}

/// The FFS/OFS toggle button on a drive row: just left of the volume-name
/// box, shown under the same condition as `launcher_drive_fs_applies`
/// above (a directory mount on a disk-backed drive field).
fn launcher_drive_fs_rect(rect: Rect, row_y: usize) -> Rect {
    let name_box = launcher_drive_name_rect(rect, row_y);
    Rect {
        x: name_box.x.saturating_sub(6 + LAUNCH_FS_W),
        y: name_box.y,
        w: LAUNCH_FS_W,
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

/// Flatten the Zorro boards into (content-row, item) pairs: each board header
/// and its option rows, with row 0 the first board header. The Add button is
/// drawn above the list, outside these rows.
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

/// The "Add board..." button. It stands where every other tab's nav row
/// stands and takes that row's first slot, so the top of the pane keeps one
/// shape whichever tab is open; the board list follows below it after the
/// same gap.
fn launcher_zorro_add_rect(rect: Rect) -> Rect {
    launcher_nav_button_rect(rect, 0)
}

/// The "are you sure" over Reset default, centred on the panel.
fn launcher_confirm_rect(rect: Rect) -> Rect {
    // Its own width, which its title bar and two buttons decide, but the
    // Save dialog's height exactly: they are the same window asking two
    // things, and one being shorter than the other made them look like two
    // unrelated boxes that happened to open in the same place.
    let (w, h) = (268, launcher_save_dialog_rect(rect).h);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// Its two buttons, as (yes, cancel). Cancel is the rightmost, where a
/// dialog's least destructive answer usually sits.
fn launcher_confirm_button_rects(rect: Rect) -> (Rect, Rect) {
    let dialog = launcher_confirm_rect(rect);
    let (w, h) = (66, SAVE_DIALOG_BUTTON.1);
    let y = dialog.y + dialog.h - SAVE_DIALOG_MARGIN - h;
    (
        Rect {
            x: dialog.x + dialog.w - 2 * w - 20,
            y,
            w,
            h,
        },
        Rect {
            x: dialog.x + dialog.w - w - 12,
            y,
            w,
            h,
        },
    )
}

fn draw_launcher_confirm(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    if !state.confirm_reset {
        return;
    }
    fill_rect_blend(frame, scale_rect(rect, scale), SCRIM, SCRIM_ALPHA, scale);
    let dialog = launcher_confirm_rect(rect);
    fill_rect(frame, scale_rect(dialog, scale), PANEL_BG, scale);
    draw_rect_bevel(
        frame,
        scale_rect(dialog, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    draw_title_bar(
        frame,
        dialog,
        "Reset default",
        hover == Some(UiControl::LauncherDialogClose),
        scale,
    );
    // The title bar has already said which default, and the buttons say
    // what the answers are. Anything more here is a paragraph nobody
    // reads standing between somebody and a decision they have made.
    draw_panel_text(
        frame,
        dialog.x + SAVE_DIALOG_MARGIN,
        dialog.y + TITLE_H + SAVE_DIALOG_MARGIN,
        "Are you sure?",
        PANEL_TEXT,
        1,
        scale,
    );
    let (yes, cancel) = launcher_confirm_button_rects(rect);
    draw_text_button(
        frame,
        yes,
        "Yes",
        true,
        hover == Some(UiControl::LauncherConfirmReset),
        scale,
    );
    draw_text_button(
        frame,
        cancel,
        "Cancel",
        true,
        hover == Some(UiControl::LauncherCancelReset),
        scale,
    );
}

fn launcher_action_label(control: UiControl) -> &'static str {
    match control {
        UiControl::LauncherLoad => "Load...",
        UiControl::LauncherSave => "Save...",
        UiControl::LauncherDefaults => "Defaults",
        UiControl::LauncherRun => "Run",
        UiControl::LauncherSaveAs => "Save As",
        UiControl::LauncherSaveDefault => "Save default",
        UiControl::LauncherResetDefault => "Reset default",
        _ => "",
    }
}

/// What the Save dialog offers, left to right. The one that deletes
/// something sits furthest from where the pointer comes in.
const SAVE_ACTIONS: [UiControl; 3] = [
    UiControl::LauncherSaveAs,
    UiControl::LauncherSaveDefault,
    UiControl::LauncherResetDefault,
];

/// One button's size, and the space around them. Every button is as wide
/// as the longest label so the row is even, and the dialog is then sized
/// to the row rather than the row fitted into a dialog.
const SAVE_DIALOG_BUTTON: (usize, usize) = (116, 20);
const SAVE_DIALOG_MARGIN: usize = 12;
const SAVE_DIALOG_GAP: usize = 6;
/// Lines kept for the description above the buttons, what one costs, and
/// the space between the last of them and the row.
///
/// Always reserved, whether or not anything is being pointed at: a dialog
/// that changed size as the pointer crossed it would move the buttons out
/// from under the pointer that was crossing them.
const SAVE_DIALOG_HELP_LINES: usize = 2;
const SAVE_DIALOG_LINE_H: usize = 12;
const SAVE_DIALOG_HELP_GAP: usize = 16;

/// What each button does, said while the pointer is on it.
///
/// Anything that is not one of the three gets the Save line. This is a
/// Save dialog opened from a Save button, so with the pointer resting
/// nowhere in particular it should say what saving means rather than go
/// blank and leave a hole where a sentence was a moment ago.
fn save_dialog_help(control: UiControl) -> &'static str {
    match control {
        UiControl::LauncherSaveDefault => {
            "Sets the running configuration as the default when you launch Copperline."
        }
        UiControl::LauncherResetDefault => "Resets the current default config to factory settings.",
        _ => "Save the running configuration to a file.",
    }
}

/// The Save dialog, centred on the panel like the confirm.
fn launcher_save_dialog_rect(rect: Rect) -> Rect {
    let (bw, bh) = SAVE_DIALOG_BUTTON;
    let (w, h) = (
        2 * SAVE_DIALOG_MARGIN + 3 * bw + 2 * SAVE_DIALOG_GAP,
        TITLE_H
            + 2 * SAVE_DIALOG_MARGIN
            + SAVE_DIALOG_HELP_LINES * SAVE_DIALOG_LINE_H
            + SAVE_DIALOG_HELP_GAP
            + bh,
    );
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// The three buttons in it.
fn launcher_save_dialog_rects(rect: Rect) -> [(UiControl, Rect); 3] {
    let dialog = launcher_save_dialog_rect(rect);
    let (w, h) = SAVE_DIALOG_BUTTON;
    std::array::from_fn(|i| {
        let item = Rect {
            x: dialog.x + SAVE_DIALOG_MARGIN + i * (w + SAVE_DIALOG_GAP),
            // Along the bottom, under the line that says what they do.
            y: dialog.y + dialog.h - SAVE_DIALOG_MARGIN - h,
            w,
            h,
        };
        (SAVE_ACTIONS[i], item)
    })
}

fn draw_launcher_save_dialog(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    if !state.save_dialog {
        return;
    }
    fill_rect_blend(frame, scale_rect(rect, scale), SCRIM, SCRIM_ALPHA, scale);
    let dialog = launcher_save_dialog_rect(rect);
    fill_rect(frame, scale_rect(dialog, scale), PANEL_BG, scale);
    draw_rect_bevel(
        frame,
        scale_rect(dialog, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    // The close gadget in its title bar is how this is dismissed. There is
    // no Cancel among the three because none of them answers a question --
    // they are three things you might do, and not doing any of them is
    // closing the window rather than choosing a fourth.
    draw_title_bar(
        frame,
        dialog,
        "Save configuration...",
        hover == Some(UiControl::LauncherDialogClose),
        scale,
    );
    for (control, item) in launcher_save_dialog_rects(rect) {
        draw_text_button(
            frame,
            item,
            launcher_action_label(control),
            true,
            hover == Some(control),
            scale,
        );
    }
    // Above the row, where a dialog's own words go, and never blank: with
    // the pointer on none of the three it says what the dialog is for.
    let help = save_dialog_help(hover.unwrap_or(UiControl::LauncherSaveAs));
    let chars = (dialog.w - 2 * SAVE_DIALOG_MARGIN) / font::GLYPH_W;
    for (i, line) in wrap_text(help, chars, chars)
        .into_iter()
        .take(SAVE_DIALOG_HELP_LINES)
        .enumerate()
    {
        draw_panel_text(
            frame,
            dialog.x + SAVE_DIALOG_MARGIN,
            dialog.y + TITLE_H + SAVE_DIALOG_MARGIN + i * SAVE_DIALOG_LINE_H,
            &line,
            PANEL_TEXT,
            1,
            scale,
        );
    }
}

/// Hit-test the configuration panel. Returns the control under `pos`, or `None`
/// to let the caller swallow the click on the panel body.
fn launcher_control_at(rect: Rect, state: &LauncherState, pos: (i32, i32)) -> Option<UiControl> {
    // The dialog answers for the whole panel while it is up: nothing
    // behind it can be clicked, which is what makes it a dialog.
    #[cfg(feature = "game-library")]
    if state.meta.is_some() {
        for (at, control) in [
            UiControl::MetaSave,
            UiControl::MetaClear,
            UiControl::MetaCancel,
        ]
        .into_iter()
        .enumerate()
        {
            if meta_button_rects(rect)[at].contains(pos) {
                return Some(control);
            }
        }
        if close_button_rect(meta_rect(rect)).contains(pos) {
            return Some(UiControl::MetaCancel);
        }
        if meta_art_rect(rect).contains(pos) {
            return Some(UiControl::MetaArt);
        }
        for field in launcher::MetaField::ALL {
            if meta_field_rect(rect, field).contains(pos) {
                return Some(UiControl::MetaField(field));
            }
        }
        return Some(UiControl::PanelBody);
    }
    #[cfg(feature = "game-library")]
    if state.login.is_some() {
        let (ok, cancel) = login_button_rects(rect);
        if ok.contains(pos) {
            return Some(UiControl::LoginOk);
        }
        // Its own close gadget, which is Cancel by another name. Checked
        // before the panel's, which sits behind it.
        if cancel.contains(pos) || close_button_rect(login_rect(rect)).contains(pos) {
            return Some(UiControl::LoginCancel);
        }
        for field in [launcher::LoginField::User, launcher::LoginField::Pass] {
            if login_field_rect(rect, field).contains(pos) {
                return Some(UiControl::LoginField(field));
            }
        }
        return Some(UiControl::PanelBody);
    }
    for (i, &model) in launcher::MODELS.iter().enumerate() {
        if launcher_model_rect(rect, i).contains(pos) {
            return Some(UiControl::LauncherModel(model));
        }
    }
    for (i, &tab) in launcher::tabs(state.setup.whdload_enabled())
        .iter()
        .enumerate()
    {
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
                            if !state.setup.zorro_boards()[board].value(opt).is_empty()
                                && clear.contains(pos)
                            {
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
            launcher_nav_block_h(state.tab)
        } else {
            0
        };
        for (i, r) in launcher::rows(
            state.tab,
            state.setup.parallel_device(),
            state.setup.serial_mode(),
            state.setup.midi_out_is_mt32(),
            state.setup.midi_out_is_csynth(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .enumerate()
        {
            if !state.row_applies(r.field) {
                continue;
            }
            let row_y = launcher_row_y(rect, i) + row_offset;
            match r.kind {
                // Non-interactive rows.
                RowKind::SectionHeader | RowKind::BootpriHeader | RowKind::RomInfo => {}
                RowKind::Text => {
                    if launcher_text_rect(rect, row_y, r.field).contains(pos) {
                        // The same widget serves two stores: a Create Image
                        // word, and a serial address on the machine.
                        return Some(if r.field == LauncherField::RamPattern {
                            UiControl::LauncherRamPatternEdit
                        } else if LauncherState::is_serial_addr(r.field) {
                            UiControl::LauncherSerialAddrEdit(r.field)
                        } else {
                            UiControl::LauncherNewImageEdit(r.field)
                        });
                    }
                }
                RowKind::Size => {
                    if launcher_size_box_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherNewImageEdit(r.field));
                    }
                    if launcher_size_unit_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherNewImageUnit);
                    }
                }
                RowKind::Number => {
                    if launcher_number_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherNewImageEdit(r.field));
                    }
                }
                RowKind::FsFamily => {
                    let labels: Vec<&str> =
                        launcher::FsFamily::ALL.iter().map(|f| f.label()).collect();
                    for (at, family) in launcher_tick_strip(rect, row_y, &labels)
                        .into_iter()
                        .zip(launcher::FsFamily::ALL)
                    {
                        if at.contains(pos) {
                            return Some(UiControl::LauncherFsFamily {
                                field: r.field,
                                family,
                            });
                        }
                    }
                }
                RowKind::FsVariant => {
                    let labels: Vec<&str> = FS_VARIANTS.iter().map(|v| v.label()).collect();
                    for (at, variant) in launcher_tick_strip(rect, row_y, &labels)
                        .into_iter()
                        .zip(FS_VARIANTS)
                    {
                        if state.workshop_fs_variant_enabled(r.field, variant) && at.contains(pos) {
                            return Some(UiControl::LauncherFsVariant {
                                field: r.field,
                                variant,
                            });
                        }
                    }
                }
                RowKind::Stepper => {
                    let (prev, value, next) = launcher_geometry_stepper_rects(rect, row_y);
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
                        return Some(UiControl::LauncherNewImageEdit(r.field));
                    }
                }
                RowKind::GeometryMode => {
                    let (auto, custom, configure) = launcher_geometry_rects(rect, row_y);
                    if auto.contains(pos) {
                        return Some(UiControl::LauncherGeometryAuto);
                    }
                    if custom.contains(pos) {
                        return Some(UiControl::LauncherGeometryCustom);
                    }
                    // Configure is only there once the geometry is by hand.
                    if state.workshop.geometry_custom && configure.contains(pos) {
                        return Some(UiControl::LauncherTab(LauncherTab::CreateGeometry));
                    }
                }
                RowKind::Action => {
                    if launcher_action_rect(rect, row_y).contains(pos) {
                        return Some(UiControl::LauncherNewImageCreate(r.field));
                    }
                    // The geometry editor's Auto sits beside its Save.
                    if r.field == LauncherField::NewGeomSave
                        && launcher_action2_rect(rect, row_y).contains(pos)
                    {
                        return Some(UiControl::LauncherNewImageCreate(
                            LauncherField::NewGeomAuto,
                        ));
                    }
                }
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
                    let (prev, value, next) = launcher_bootpri_rects(rect, row_y);
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
                    let (has_browse, has_clear) = launcher_path_buttons(&state.setup, r.field);
                    if has_browse && browse.contains(pos) {
                        return Some(UiControl::LauncherBrowse(r.field));
                    }
                    if has_clear
                        && launcher_clear_enabled(&state.setup, r.field)
                        && clear.contains(pos)
                    {
                        return Some(UiControl::LauncherClear(r.field));
                    }
                }
                #[cfg(feature = "game-library")]
                RowKind::Account => {
                    let (button, _) = launcher_path_rects(rect, row_y);
                    if button.contains(pos) {
                        return Some(UiControl::LauncherOpenRetroLogin);
                    }
                }
                #[cfg(not(feature = "game-library"))]
                RowKind::Account => {}
                RowKind::FloppyMedia => {
                    let drive = launcher::MachineSetup::drive_image_bay(r.field);
                    if let Some(bay) = drive {
                        if state.setup.drive_bridged(bay) {
                            // Bridged: one Configure button where Browse and
                            // Clear would be. There is no image to pick.
                            if launcher_bridge_configure_rect(rect, row_y).contains(pos) {
                                return Some(UiControl::LauncherBridgeConfigure(bay));
                            }
                            continue;
                        }
                    }
                    let (browse, clear) = launcher_path_rects(rect, row_y);
                    if browse.contains(pos) {
                        return Some(UiControl::LauncherBrowse(r.field));
                    }
                    if launcher_clear_enabled(&state.setup, r.field) && clear.contains(pos) {
                        return Some(UiControl::LauncherClear(r.field));
                    }
                }
                RowKind::FloppyFlags => {
                    let (protect, _bridge) = launcher_floppy_flag_rects(rect, row_y);
                    if protect.contains(pos) {
                        return Some(UiControl::LauncherToggle(r.field));
                    }
                    // A build without the feature has no physical-drive box to
                    // hit: the whole thing is absent rather than inert.
                    #[cfg(feature = "fluxbridge")]
                    if _bridge.contains(pos) {
                        if let Some(bay) = launcher::MachineSetup::drive_protect_bay(r.field) {
                            return Some(UiControl::LauncherDriveBridgeToggle(bay));
                        }
                    }
                }
                RowKind::Drive => {
                    let (browse, clear) = launcher_path_rects(rect, row_y);
                    // A real disk replaces both buttons with one. Only this
                    // row's buttons change: everything else on the panel must
                    // still be reachable, so nothing returns early here.
                    if state.setup.host_disk_on_row(r.field).is_some() {
                        let unmount = Rect {
                            x: browse.x,
                            y: browse.y,
                            w: clear.x + clear.w - browse.x,
                            h: browse.h,
                        };
                        if unmount.contains(pos) {
                            return Some(UiControl::LauncherHostDiskUnmount(r.field));
                        }
                    } else {
                        if browse.contains(pos) {
                            return Some(UiControl::LauncherBrowse(r.field));
                        }
                        if launcher_clear_enabled(&state.setup, r.field) && clear.contains(pos) {
                            return Some(UiControl::LauncherClear(r.field));
                        }
                        // A support archive with nothing chosen can fetch
                        // its own; once something is chosen there is
                        // nothing to fetch, and Clear brings it back.
                        #[cfg(feature = "game-library")]
                        if row_archive(r.field).is_some()
                            && state.setup.path(r.field).is_none()
                            && launcher_download_rect(rect, row_y).contains(pos)
                        {
                            return Some(UiControl::LauncherWhdloadDownload(r.field));
                        }
                    }
                    // The volume name only matters once an image is chosen
                    // (and never for a CD image).
                    if state.setup.path(r.field).is_some()
                        && state.setup.drive_name_applies(r.field)
                        && launcher_drive_name_rect(rect, row_y).contains(pos)
                    {
                        return Some(UiControl::LauncherDriveNameEdit(r.field));
                    }
                    // The filesystem toggle only matters for a directory
                    // mount: an HDF/gzip image already carries its own
                    // filesystem inside it.
                    if launcher_drive_fs_applies(&state.setup, r.field)
                        && launcher_drive_fs_rect(rect, row_y).contains(pos)
                    {
                        return Some(UiControl::LauncherDriveFilesystemToggle(r.field));
                    }
                }
            }
        }
    }
    #[cfg(feature = "game-library")]
    if state.tab == LauncherTab::WhdloadLibrary {
        let whdload_entry = state.setup.whdload_enabled();
        if state.library.games.len() > library_visible_rows(rect, whdload_entry) {
            for (control, arrow) in library_arrow_rects(rect, whdload_entry) {
                if arrow.contains(pos) {
                    return Some(control);
                }
            }
        }
        for drawn in 0..library_visible_rows(rect, whdload_entry) {
            if state.library.scroll + drawn >= state.library.games.len() {
                break;
            }
            // The tick first: it sits inside the row, and marking a
            // favourite is not the same as choosing the game.
            if library_favourite_box(rect, whdload_entry, drawn).contains(pos) {
                return Some(UiControl::LauncherLibraryFavourite(drawn));
            }
            if library_row_rect(rect, whdload_entry, drawn).contains(pos) {
                return Some(UiControl::LauncherLibraryPick(drawn));
            }
        }
        for (at, control) in [
            UiControl::LauncherLibraryRefresh,
            UiControl::LauncherLibraryUpdate,
            UiControl::LauncherLibraryEdit,
        ]
        .into_iter()
        .enumerate()
        {
            if library_button_rects(rect, whdload_entry)[at].contains(pos)
                && (at == 0 || !state.library.games.is_empty())
            {
                return Some(control);
            }
        }
        if state.library.games.len() >= LIBRARY_AZ_MIN_GAMES {
            for (bucket, at) in library_az_rects(rect, whdload_entry)
                .into_iter()
                .enumerate()
            {
                if at.contains(pos) {
                    return Some(UiControl::LauncherLibraryJump(bucket));
                }
            }
        }
        let starred = state.library.db.favourite_count();
        let rows = library_favourite_rows(rect, whdload_entry);
        if starred > rows {
            for (control, arrow) in library_favourite_arrow_rects(rect, whdload_entry) {
                if arrow.contains(pos) {
                    return Some(control);
                }
            }
        }
        for drawn in 0..starred
            .saturating_sub(state.library.favourite_scroll)
            .min(rows)
        {
            if library_remove_box(rect, whdload_entry, drawn).contains(pos) {
                return Some(UiControl::LauncherLibraryFavouriteRemove(drawn));
            }
            if library_favourite_row_rect(rect, whdload_entry, drawn).contains(pos) {
                return Some(UiControl::LauncherLibraryFavouritePick(drawn));
            }
        }
    }
    // The top nav row: a page's sibling links (the Storage and A/V sub-pages),
    // or a Back button.
    if state.tab == LauncherTab::HostDisk {
        let disks = state.setup.host_disks().len();
        if disks > HOST_DISK_VISIBLE_ROWS {
            for (control, arrow) in host_disk_arrow_rects(rect) {
                if arrow.contains(pos) {
                    return Some(control);
                }
            }
        }
        {
            let scroll = state.setup.host_disk_scroll().min(disks);
            for slot in 0..disks.saturating_sub(scroll).min(HOST_DISK_VISIBLE_ROWS) {
                let i = scroll + slot;
                // The cells that are their own answer come first: clicking
                // Attach or R/O sets that, rather than picking the row.
                if host_disk_attach_cell(rect, slot).contains(pos) {
                    return Some(UiControl::LauncherHostDiskAttach(i));
                }
                if host_disk_writable_cell(rect, slot).contains(pos) {
                    return Some(UiControl::LauncherHostDiskWritable(i));
                }
                if host_disk_row_rect(rect, slot).contains(pos) {
                    return Some(UiControl::LauncherHostDiskSelect(i));
                }
            }
        }
        for (control, button) in host_disk_button_rects(rect) {
            if button.contains(pos) {
                return Some(control);
            }
        }
    }
    // The nav row: a Back button when this is a sub-page, then whatever
    // sibling pages it offers. A page can have both -- the Create Image pages
    // say where they came from and which of the two they are.
    let mut slot = 0;
    if let Some(parent) = state.tab.parent_tab() {
        if launcher_back_button_rect(rect).contains(pos) {
            return Some(UiControl::LauncherTab(parent));
        }
        slot = 1;
    }
    for (i, &(_, target)) in state.tab.nav_options().iter().enumerate() {
        if launcher_nav_button_rect(rect, slot + i).contains(pos) {
            return Some(UiControl::LauncherTab(target));
        }
    }
    for (control, button_rect) in launcher_action_rects(rect) {
        if button_rect.contains(pos) {
            return Some(control);
        }
    }
    None
}

/// Hit-test the Save dialog, which is over everything while it is up. A
/// click anywhere else -- its close gadget, its own frame, the panel
/// behind it -- puts it away without doing anything, so it can never be a
/// mode you are stuck in.
fn launcher_save_dialog_hit(rect: Rect, pos: (i32, i32)) -> Option<UiControl> {
    launcher_save_dialog_rects(rect)
        .into_iter()
        .find_map(|(control, item)| item.contains(pos).then_some(control))
}

/// The Host Disk page: what the host has, and which of it to attach.
/// The Library page: the games found, which are favourites, and what the
/// database says about the one picked.
#[cfg(feature = "game-library")]
fn draw_library_page(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let entries = state.library.games.entries();
    let whdload_entry = state.setup.whdload_enabled();
    let games = library_table_rect(rect, whdload_entry);
    // A scan running greys both buttons: neither of them can start a
    // second one while the first is going.
    let busy = matches!(
        state.status.as_ref().map(|s| s.kind),
        Some(launcher::StatusKind::Busy)
    );

    // The shortcut row shares the label's line, so it costs no height.
    if entries.len() >= LIBRARY_AZ_MIN_GAMES {
        let present = state.az_buckets_present();
        for (bucket, at) in library_az_rects(rect, whdload_entry)
            .into_iter()
            .enumerate()
        {
            let live = present.get(bucket).copied().unwrap_or(false);
            let hovered = live && hover == Some(UiControl::LauncherLibraryJump(bucket));
            draw_az_button(frame, at, launcher::az_label(bucket), live, hovered, scale);
        }
    }

    draw_panel_text(
        frame,
        games.x,
        games.y.saturating_sub(14),
        "Games:",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_library_box(frame, games, scale);
    for (at, title) in [
        (LIBRARY_COL_NAME, "Game"),
        (library_col_favourite(rect, whdload_entry), "Favourite"),
    ] {
        draw_panel_text(
            frame,
            games.x + 4 + at,
            games.y + 5,
            title,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }

    if entries.is_empty() {
        // What is wrong, then what to do about it, broken where it reads:
        // the launcher panel is a fixed size, so these lines are the same
        // lines on every machine. Each is still put through the wrap, which
        // does nothing while they fit and catches them if the box is ever
        // made narrower than the words in it.
        let lines = [
            "No games found!",
            "",
            "Update the \"Game library\" directory",
            "under WHDLoad -> Settings...",
        ];
        let lines: Vec<String> = lines
            .into_iter()
            .flat_map(|line| match line.is_empty() {
                true => vec![String::new()],
                false => wrap_balanced(line, games.w.saturating_sub(16)),
            })
            .collect();
        for (line, text) in lines.into_iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            draw_panel_text(
                frame,
                games.x + 8,
                games.y + LIBRARY_HEADER_H + 6 + line * 14,
                &text,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
    }

    for drawn in 0..library_visible_rows(rect, whdload_entry) {
        let Some(entry) = entries.get(state.library.scroll + drawn) else {
            break;
        };
        let row = library_row_rect(rect, whdload_entry, drawn);
        let chosen = state.library.focus == launcher::LibraryFocus::Games
            && state.library.scroll + drawn == state.library.selected;
        if chosen {
            fill_rect(frame, scale_rect(row, scale), MENU_HILIGHT_BG, scale);
        } else if hover == Some(UiControl::LauncherLibraryPick(drawn)) {
            fill_rect(frame, scale_rect(row, scale), BUTTON_FACE_HOVER, scale);
        }
        let colour = if chosen {
            MENU_HILIGHT_TEXT
        } else {
            PANEL_TEXT
        };
        // Clipped at the Favourite column, so a long title stops rather
        // than running under the tick.
        draw_panel_text(
            frame,
            row.x + 4 + LIBRARY_COL_NAME,
            row.y + 3,
            &truncate_to_width(
                entry.title(),
                library_col_favourite(rect, whdload_entry).saturating_sub(LIBRARY_COL_NAME + 12),
            ),
            colour,
            1,
            scale,
        );
        let tick = library_favourite_box(rect, whdload_entry, drawn);
        draw_tick_box(
            frame,
            tick.x,
            tick.y,
            state.library.db.is_favourite(&entry.relative),
            TICK_GREEN,
            scale,
        );
        if hover == Some(UiControl::LauncherLibraryFavourite(drawn)) {
            draw_outline(frame, tick, PANEL_TEXT_HILIGHT, scale);
        }
    }

    let visible = library_visible_rows(rect, whdload_entry);
    if entries.len() > visible {
        for (control, at) in library_arrow_rects(rect, whdload_entry) {
            let up = matches!(control, UiControl::LauncherLibraryScroll(d) if d < 0);
            let live = match up {
                true => state.library.scroll > 0,
                false => state.library.scroll + visible < entries.len(),
            };
            draw_scroll_arrow(frame, at, up, live, hover == Some(control), scale);
        }
    }

    // The favourites, which are the same games under a shorter heading.
    let favourites = library_favourites_rect(rect, whdload_entry);
    draw_panel_text(
        frame,
        favourites.x,
        favourites.y.saturating_sub(14),
        "Favourites:",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_library_box(frame, favourites, scale);
    for (x, title) in [
        (favourites.x + 4 + LIBRARY_COL_NAME, "Game"),
        (library_remove_heading_x(rect, whdload_entry), "Remove"),
    ] {
        draw_panel_text(frame, x, favourites.y + 5, title, PANEL_TEXT_DIM, 1, scale);
    }
    // From the database rather than from the library, so a favourite whose
    // package has been deleted is still listed -- and can still be taken
    // off, which is most of the reason its Remove tick is there.
    let favourite_rows = library_favourite_rows(rect, whdload_entry);
    for (drawn, (key, name)) in state
        .library
        .db
        .favourites()
        .skip(state.library.favourite_scroll)
        .take(favourite_rows)
        .enumerate()
    {
        let row = library_favourite_row_rect(rect, whdload_entry, drawn);
        let chosen = state.library.focus == launcher::LibraryFocus::Favourites
            && state.library.favourite_scroll + drawn == state.library.favourite_selected;
        if chosen {
            fill_rect(frame, scale_rect(row, scale), MENU_HILIGHT_BG, scale);
        } else if hover == Some(UiControl::LauncherLibraryFavouritePick(drawn)) {
            fill_rect(frame, scale_rect(row, scale), BUTTON_FACE_HOVER, scale);
        }
        // One no longer in the library is dimmed: still listed, still
        // removable, but there is nothing to launch.
        let present = entries.iter().any(|entry| entry.relative == key);
        let colour = match (chosen, present) {
            (true, _) => MENU_HILIGHT_TEXT,
            (false, true) => PANEL_TEXT,
            (false, false) => PANEL_TEXT_DIM,
        };
        draw_panel_text(
            frame,
            row.x + 4 + LIBRARY_COL_NAME,
            row.y + 3,
            &truncate_to_width(
                name,
                library_col_favourite(rect, whdload_entry).saturating_sub(LIBRARY_COL_NAME + 12),
            ),
            colour,
            1,
            scale,
        );
        let tick = library_remove_box(rect, whdload_entry, drawn);
        draw_tick_box(frame, tick.x, tick.y, false, TICK_GREEN, scale);
        if hover == Some(UiControl::LauncherLibraryFavouriteRemove(drawn)) {
            draw_outline(frame, tick, PANEL_TEXT_HILIGHT, scale);
        }
    }

    let starred = state.library.db.favourite_count();
    if starred > favourite_rows {
        for (control, at) in library_favourite_arrow_rects(rect, whdload_entry) {
            let up = matches!(control, UiControl::LauncherLibraryFavouriteScroll(d) if d < 0);
            let live = match up {
                true => state.library.favourite_scroll > 0,
                false => state.library.favourite_scroll + favourite_rows < starred,
            };
            draw_scroll_arrow(frame, at, up, live, hover == Some(control), scale);
        }
    }

    draw_library_cover(frame, rect, state, scale);

    // The two buttons that say when work happens, in the gap between the
    // lists. A third slot is left beside them: the row is sized for three
    // so gaining one later does not move the two that are here.
    let buttons = library_button_rects(rect, whdload_entry);
    for (at, (label, control, enabled)) in [
        ("Refresh", UiControl::LauncherLibraryRefresh, !busy),
        // Nothing to look up until the folder has been read, so Scan waits
        // for a Refresh that found something.
        (
            "Scan",
            UiControl::LauncherLibraryUpdate,
            !busy && !entries.is_empty(),
        ),
        (
            "Update",
            UiControl::LauncherLibraryEdit,
            !busy && state.library_selection().is_some(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        draw_text_button(
            frame,
            buttons[at],
            label,
            enabled,
            hover == Some(control),
            scale,
        );
    }
}

/// A list box, sunk into the panel like an entry field so it reads as
/// something to look into rather than a raised control.
#[cfg(feature = "game-library")]
fn draw_library_box(frame: &mut [u8], at: Rect, scale: usize) {
    fill_rect(frame, scale_rect(at, scale), ENTRY_BG, scale);
    draw_outline(frame, at, BUTTON_EDGE_LIGHT, scale);
    draw_rect_bevel(
        frame,
        scale_rect(
            Rect {
                x: at.x + 1,
                y: at.y + 1,
                w: at.w.saturating_sub(2),
                h: at.h.saturating_sub(2),
            },
            scale,
        ),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );
}

/// The cover art box, and what the database says under it.
#[cfg(feature = "game-library")]
fn draw_library_cover(frame: &mut [u8], rect: Rect, state: &LauncherState, scale: usize) {
    let whdload_entry = state.setup.whdload_enabled();
    // The frame is the size the layout reserves, whatever shape the picture
    // in it turns out to be: it is where the eye expects the art to be, and
    // the writing under it starts on the same line for every game.
    // Amiga box art is portrait almost without exception, so the frame is
    // cut for that and the rare landscape scan is letterboxed into it --
    // black above and below beats a frame that changes shape and drags the
    // metadata down the page with it.
    let widest = library_cover_rect(rect, whdload_entry);
    let entry = state.library_selection();
    let art = entry
        .and_then(|entry| entry.game.as_ref())
        .and_then(|game| game.front_sha1.as_deref())
        .and_then(|sha1| state.library.covers.get(sha1));
    let (frame_rect, box_rect) = (widest, library_art_rect(rect, whdload_entry));

    // The mount: a button-faced border raised out of the panel, with the
    // picture recessed into it. Two bevels facing opposite ways is what
    // makes the frame read as having thickness.
    fill_rect(frame, scale_rect(frame_rect, scale), BUTTON_FACE, scale);
    draw_rect_bevel(
        frame,
        scale_rect(frame_rect, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    fill_rect(frame, scale_rect(box_rect, scale), ENTRY_BG, scale);
    if let Some(art) = art {
        draw_cover_art(frame, box_rect, art, scale);
    }
    draw_rect_bevel(
        frame,
        scale_rect(box_rect, scale),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );

    let Some(entry) = entry else {
        return;
    };
    // Two different nothings: a package the catalogue has never heard of,
    // and one it knows but has no picture for.
    let missing: &[&str] = match (&entry.game, art.is_some()) {
        (_, true) => &[],
        (None, _) => &["not in the", "database"],
        (Some(_), _) => &["No cover art"],
    };
    {
        for (line, text) in missing.iter().copied().enumerate() {
            let w = text.chars().count() * font::GLYPH_W;
            draw_panel_text(
                frame,
                box_rect.x + box_rect.w.saturating_sub(w) / 2,
                box_rect.y + box_rect.h / 2 - 8 * missing.len() + line * 12,
                text,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
    }

    // Under the art: what the database knows, each label dimmed above its
    // value, and a value too long for the column wrapped rather than cut.
    // It starts under the frame, which is one size for every game, so it
    // starts on the same line each time whatever shape the picture is.
    // The block stops above the action bar: a developer credited to nine
    // people would otherwise run down over the Run button and off the
    // panel. Each value is held to two lines as well, so one long field
    // cannot crowd out the ones under it -- what is cut is only what is
    // drawn, never what is stored.
    let floor = launcher_action_y(rect).saturating_sub(6);
    let mut y = widest.y + widest.h + 8;
    let game = entry.game.as_ref();
    let mut show = |label: &str, value: Option<&str>, y: &mut usize| {
        let Some(value) = value.filter(|v| !v.is_empty()) else {
            return;
        };
        // Label and one line at least, or there is no point starting.
        if *y + 24 > floor {
            return;
        }
        draw_panel_text(frame, widest.x, *y, label, PANEL_TEXT_DIM, 1, scale);
        *y += 12;
        let mut lines = wrap_to_width(value, widest.w);
        let over = lines.len() > LIBRARY_FIELD_LINES;
        lines.truncate(LIBRARY_FIELD_LINES);
        let last = lines.len().saturating_sub(1);
        for (at, line) in lines.into_iter().enumerate() {
            if *y + 12 > floor {
                break;
            }
            // The panel marks a cut with a tilde, so a credit that goes
            // on does not read as one that stopped. Room is made for the
            // mark rather than hoping the line is short enough.
            let line = match over && at == last {
                true => {
                    let mut cut = line;
                    while cut.chars().count() * font::GLYPH_W + font::GLYPH_W > widest.w {
                        cut.pop();
                    }
                    format!("{cut}~")
                }
                false => line,
            };
            draw_panel_text(frame, widest.x, *y, &line, PANEL_TEXT, 1, scale);
            *y += 12;
        }
        *y += 4;
    };
    show("Year", game.and_then(|g| g.year.as_deref()), &mut y);
    show(
        "Publisher",
        game.and_then(|g| g.publisher.as_deref()),
        &mut y,
    );
    show(
        "Developer",
        game.and_then(|g| g.developer.as_deref()),
        &mut y,
    );
    show("Players", game.and_then(|g| g.players.as_deref()), &mut y);

    // Which release this is. Shown only when there is something to say:
    // what somebody typed, or -- where the library holds this game under
    // one title more than once -- the package's own name, since nothing
    // else separates `CannonFodder2_v1.11_0104` from `_v1.12_Fr_2578`.
    // Without the extension: it is the same on both and says nothing about
    // which release either is.
    //
    // A game held once and never edited has no version and no row, and
    // neither has one the catalogue has never heard of -- two packages the
    // scan could not name are two rows that say nothing already, and a
    // file name under them is not the answer to which release they are.
    let version = game
        .and_then(|g| g.version.as_deref())
        .filter(|v| !v.is_empty())
        .or_else(|| (entry.duplicated && game.is_some()).then_some(entry.file_name.as_str()));
    if let Some(version) = version.filter(|_| y + 24 <= floor) {
        draw_panel_text(frame, widest.x, y, "Version", PANEL_TEXT_DIM, 1, scale);
        y += 12;
        // Two lines, because a package name is longer than the column and
        // both ends of it matter: `CannonFodder2_v1.11_0104.lha` says
        // which game at the front and which release at the back. Anything
        // past that is cut, which nothing typed here should reach -- the
        // editor stops at what these two lines hold.
        for line in wrap_to_width(version, widest.w)
            .into_iter()
            .take(LIBRARY_VERSION_LINES)
        {
            if y + 12 > floor {
                break;
            }
            draw_panel_text(frame, widest.x, y, &line, PANEL_TEXT, 1, scale);
            y += 12;
        }
    }
}

/// Draw a cover into `into`, scaled to fit and centred, keeping its shape.
///
/// Nearest-neighbour, like everything else the panel draws: the launcher
/// renders at one scale and is blown up whole, so smoothing here would be
/// undone by the magnification above it anyway.
#[cfg(feature = "game-library")]
fn draw_cover_art(frame: &mut [u8], into: Rect, art: &crate::gamelib::cover::Image, scale: usize) {
    let Some(at) = fit_within(art.width, art.height, into) else {
        return;
    };
    for y in 0..at.h {
        let from_y = y * art.height / at.h;
        for x in 0..at.w {
            let from = (from_y * art.width + x * art.width / at.w) * 4;
            let Some(px) = art.pixels.get(from..from + 4) else {
                continue;
            };
            // Drawn opaque: cover art has no transparency worth honouring,
            // and one that does reads better over the box's own fill.
            let colour = rgba(px[0] as u32, px[1] as u32, px[2] as u32);
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: at.x + x,
                        y: at.y + y,
                        w: 1,
                        h: 1,
                    },
                    scale,
                ),
                colour,
                scale,
            );
        }
    }
}

/// The largest rectangle of `w` by `h`'s shape that fits inside `into`,
/// centred. `None` if either is empty, which is nothing to draw.
///
/// Covers are portrait and the box is close to square, so a picture drawn
/// to the box's own shape would be visibly stretched.
#[cfg(feature = "game-library")]
fn fit_within(w: usize, h: usize, into: Rect) -> Option<Rect> {
    if w == 0 || h == 0 || into.w == 0 || into.h == 0 {
        return None;
    }
    // Whichever side runs out first sets the scale; the other is left a
    // margin, split evenly.
    let (fit_w, fit_h) = if w * into.h >= h * into.w {
        (into.w, (into.w * h / w).clamp(1, into.h))
    } else {
        ((into.h * w / h).clamp(1, into.w), into.h)
    };
    Some(Rect {
        x: into.x + (into.w - fit_w) / 2,
        y: into.y + (into.h - fit_h) / 2,
        w: fit_w,
        h: fit_h,
    })
}

/// The same as [`wrap_to_width`], but with the lines evened up.
///
/// A greedy wrap fills each line to the brim and leaves whatever is left
/// on the last one, which for a sentence a little wider than its box means
/// a full line and a single trailing word. It takes the same number of
/// lines to say it with the break in a sensible place, so the column is
/// narrowed a character at a time for as long as the line count holds.
#[cfg(feature = "game-library")]
fn wrap_balanced(text: &str, width: usize) -> Vec<String> {
    let mut best = wrap_to_width(text, width);
    if best.len() < 2 {
        return best;
    }
    let lines = best.len();
    let mut narrow = width;
    while narrow > font::GLYPH_W {
        narrow -= font::GLYPH_W;
        let tried = wrap_to_width(text, narrow);
        if tried.len() != lines {
            break;
        }
        best = tried;
    }
    best
}

/// Break text into lines that fit `width`, at spaces where there are any.
/// A single word longer than the column is broken across lines rather than
/// left to run off the panel.
#[cfg(feature = "game-library")]
fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    let per_line = (width / font::GLYPH_W).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let would_be = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if would_be > per_line && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if word.chars().count() > per_line {
            // Nothing to break at, so it is broken anyway -- across as
            // many lines as it needs. A package name is one long word and
            // taking only its first line would drop the part that says
            // which release it is.
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut rest: Vec<char> = word.chars().collect();
            while rest.len() > per_line {
                lines.push(rest.drain(..per_line).collect());
            }
            line = rest.into_iter().collect();
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn draw_host_disk_page(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let setup = &state.setup;
    let table = host_disk_table_rect(rect);

    // The box, sunk into the panel like an entry field so it reads as
    // something to look into rather than a raised control. The outline comes
    // first and goes all the way round: the inset shading alone is nearly the
    // panel's own colour, so on its own only the lit edges show and the box
    // looks bevelled on two sides rather than recessed.
    let scaled = scale_rect(table, scale);
    fill_rect(frame, scaled, ENTRY_BG, scale);
    draw_outline(frame, table, BUTTON_EDGE_LIGHT, scale);
    draw_rect_bevel(
        frame,
        scale_rect(
            Rect {
                x: table.x + 1,
                y: table.y + 1,
                w: table.w.saturating_sub(2),
                h: table.h.saturating_sub(2),
            },
            scale,
        ),
        BUTTON_EDGE_DARK,
        ENTRY_BG,
        scale,
    );

    // Column headings, then a rule under them.
    let head_y = table.y + 4;
    for (offset, title) in [
        (HOST_DISK_COL_DISK, "Disk"),
        (HOST_DISK_COL_VOLUME, "Volume"),
        (HOST_DISK_COL_SIZE, "Size"),
        (HOST_DISK_COL_ATTACH, "Attach"),
        (HOST_DISK_COL_WRITABLE, "R/W"),
        (HOST_DISK_COL_TICK, "Enable"),
    ] {
        draw_panel_text(
            frame,
            table.x + offset,
            head_y,
            title,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    fill_rect(
        frame,
        scale_rect(
            Rect {
                x: table.x + 2,
                y: table.y + HOST_DISK_HEADER_H - 2,
                w: table.w.saturating_sub(4),
                h: 1,
            },
            scale,
        ),
        BUTTON_EDGE_DARK,
        scale,
    );

    let disks = setup.host_disks();
    let scroll = setup.host_disk_scroll().min(disks.len());
    if disks.is_empty() {
        draw_panel_text(
            frame,
            table.x + HOST_DISK_COL_DISK,
            table.y + HOST_DISK_HEADER_H + 4,
            "No supported disks found on the host system.",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    for (slot, disk) in disks
        .iter()
        .skip(scroll)
        .take(HOST_DISK_VISIBLE_ROWS)
        .enumerate()
    {
        // The list index, not the row on screen: everything that acts on a
        // disk names the disk, so a scrolled list still ticks the right one.
        let i = scroll + slot;
        let row = host_disk_row_rect(rect, slot);
        let ticked = setup.host_disk_is_selected(&disk.id);
        // A disk the machine has keeps the highlight whether or not it is
        // ticked right now: in a long list, what is in use should be
        // findable at a glance.
        if ticked
            || setup.host_disk_is_attached(&disk.id)
            || hover == Some(UiControl::LauncherHostDiskSelect(i))
        {
            fill_rect(frame, scale_rect(row, scale), BUTTON_FACE, scale);
        }
        let text_y = row.y + (HOST_DISK_ROW_H - font::GLYPH_H) / 2;
        // A disk the host has mounted is not dimmed: mounting takes it from
        // the host first, so being in use is not a reason it cannot be had.
        // Every column is clipped to the space before the next one: a long
        // device name or volume must not run into its neighbour.
        for (offset, next, text) in [
            (HOST_DISK_COL_DISK, HOST_DISK_COL_VOLUME, disk.id.clone()),
            (
                HOST_DISK_COL_VOLUME,
                HOST_DISK_COL_SIZE,
                disk.volume.clone(),
            ),
            (HOST_DISK_COL_SIZE, HOST_DISK_COL_ATTACH, disk.size.clone()),
            (
                HOST_DISK_COL_ATTACH,
                HOST_DISK_COL_WRITABLE,
                // Blank until the disk is ticked: an unticked disk is going
                // nowhere, and ticking is what gives it a place.
                disk.attach.map(|attach| attach.label()).unwrap_or_default(),
            ),
        ] {
            let text = truncate_to_width(&text, next - offset - 8);
            draw_panel_text(frame, row.x + offset, text_y, &text, PANEL_TEXT, 1, scale);
        }
        // Two ticks, the same kind of answer either way: may the guest write
        // to this disk, and is it going to the machine at all. Writing is on
        // by default -- a disk given to a machine is normally meant to be
        // used -- so unticking R/W is what protects it.
        draw_tick_box(
            frame,
            row.x + HOST_DISK_COL_WRITABLE + 6,
            row.y + 2,
            disk.writable,
            PANEL_TEXT,
            scale,
        );
        draw_tick_box(
            frame,
            row.x + HOST_DISK_COL_TICK + 12,
            row.y + 2,
            ticked,
            PANEL_TEXT_HILIGHT,
            scale,
        );
    }

    // Arrows only when there is somewhere to go, and each greys at its end
    // of the list so the box says where the window is.
    if disks.len() > HOST_DISK_VISIBLE_ROWS {
        for (control, arrow) in host_disk_arrow_rects(rect) {
            let up = control == UiControl::LauncherHostDiskScroll(-1);
            let live = if up {
                scroll > 0
            } else {
                scroll + HOST_DISK_VISIBLE_ROWS < disks.len()
            };
            draw_scroll_arrow(frame, arrow, up, live, hover == Some(control), scale);
        }
    }

    for (control, button) in host_disk_button_rects(rect) {
        let label = match control {
            UiControl::LauncherHostDiskMount => "Mount",
            UiControl::LauncherHostDiskUnmountSelected => "Unmount",
            _ => "Refresh",
        };
        // Mount needs a disk to mount; Unmount a ticked disk the machine
        // actually has; Refresh only ever looks, so it stays live.
        let enabled = match control {
            UiControl::LauncherHostDiskMount => !setup.host_disks_selected().is_empty(),
            UiControl::LauncherHostDiskUnmountSelected => setup
                .host_disks_selected()
                .iter()
                .any(|id| setup.host_disk_is_attached(id)),
            _ => true,
        };
        draw_text_button(
            frame,
            button,
            label,
            enabled,
            enabled && hover == Some(control),
            scale,
        );
    }

    // What Mount will do, one line per ticked disk, under the buttons so the
    // greyed Mount button is never a mystery and two ticks are never a
    // surprise about where the second disk went. Same shape as the Input
    // page's summary: a dimmed heading over the lines it introduces. With
    // nothing ticked the block instead says the one thing worth knowing
    // before ticking anything -- on hosts where attaching will raise the
    // system's privilege prompt, that it will; elsewhere, what to do next.
    let summary_top = host_disk_button_rects(rect)[0].1.y + LAUNCH_TAB_H + 10;
    let chosen: Vec<&crate::video::launcher::HostDiskRow> = setup
        .host_disks()
        .iter()
        .filter(|d| setup.host_disk_is_selected(&d.id))
        .collect();
    let warn_privilege = chosen.is_empty() && crate::blockdev::attaching_needs_privilege();
    draw_panel_text(
        frame,
        table.x,
        summary_top,
        if warn_privilege {
            "Warning:"
        } else {
            "With these settings:"
        },
        if warn_privilege {
            PANEL_TEXT_ACCENT
        } else {
            PANEL_TEXT_DIM
        },
        1,
        scale,
    );
    if chosen.is_empty() {
        if warn_privilege {
            draw_panel_text(
                frame,
                table.x + 8,
                summary_top + 16,
                "Attaching a host drive requires elevated privileges.",
                PANEL_TEXT_ACCENT,
                1,
                scale,
            );
        } else {
            draw_panel_text(
                frame,
                table.x + 8,
                summary_top + 16,
                "Select a disk to attach it to the machine",
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
    // A disk gets two lines if it needs them. The sentence ends with where the
    // disk is going, and on a long model name that is exactly the half a
    // single clipped line loses -- leaving a summary that says everything
    // except the thing it was written to say. Past two lines it is truncated,
    // because the summary cannot keep growing without reaching the panel edge.
    // Both lines start at the same edge: the wrap is one sentence running on,
    // not a list with sub-items, and stepping the second line in makes it read
    // as something subordinate to the first.
    let width_px = table.w.saturating_sub(8);
    let bottom = rect.y + rect.h;
    let mut y = summary_top + 16;
    for disk in &chosen {
        let access = if disk.writable {
            "read/write"
        } else {
            "read only"
        };
        let place = disk
            .attach
            .expect("a ticked disk has an attachment point")
            .label();
        let text = format!(
            "{} ({}): attached {access} to {place}",
            disk.id, disk.volume
        );
        let chars = width_px / font::GLYPH_W;
        let mut lines = wrap_text(&text, chars, chars);
        if lines.len() > 2 {
            let overflow = lines[1..].join(" ");
            lines.truncate(1);
            lines.push(truncate_to_width(&overflow, width_px));
        }
        for line in &lines {
            // Out of panel is not somewhere to draw: the rest of the page is
            // below this and would be written over.
            if y + HOST_DISK_ROW_H > bottom {
                return;
            }
            draw_panel_text(frame, table.x + 8, y, line, PANEL_TEXT, 1, scale);
            y += HOST_DISK_ROW_H;
        }
    }
}

/// A small square box, filled when set. The fill colour distinguishes what
/// is being answered: one page can carry more than one kind of tick.
fn draw_tick_box(frame: &mut [u8], x: usize, y: usize, set: bool, colour: u32, scale: usize) {
    let outer = Rect { x, y, w: 10, h: 10 };
    fill_rect(frame, scale_rect(outer, scale), ENTRY_BG, scale);
    draw_outline(frame, outer, BUTTON_EDGE_LIGHT, scale);
    if set {
        let inner = Rect {
            x: x + 2,
            y: y + 2,
            w: 6,
            h: 6,
        };
        fill_rect(frame, scale_rect(inner, scale), colour, scale);
    }
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

/// Which slice of a line a box shows, and where in it the caret lands.
///
/// Answers `(first character shown, cell the caret is on)` for a line of
/// `len` characters in a box `cells` wide. The caret is kept off the edges
/// where there is text either side of it -- half a box of lead means
/// stepping through a long value moves the block, and only shifts the text
/// once the block reaches an end.
fn edit_window(len: usize, caret: usize, cells: usize) -> (usize, usize) {
    let first = caret
        .saturating_sub(cells / 2)
        .min(len.saturating_sub(cells));
    (first, caret - first)
}

/// Draw a line that is being typed into, with a block over the caret.
///
/// Every editable box in the launcher goes through here -- the value boxes
/// on the configuration pages and both WHDLoad dialogs -- so a caret means
/// the same thing wherever it is seen. A block rather than a bar: the font
/// is an 8x8 cell grid with no sub-pixel anywhere in it, and a one-pixel
/// line between two cells is easy to miss on a scaled-up panel.
///
/// The window on the text slides to keep the caret in view, so typing at
/// the end of something longer than the box pushes the head off the left
/// and stepping back to the front brings it home. An "..." marks a head
/// that has been scrolled past, and the caret cell is left free at the
/// right so a caret past the last character has somewhere to sit.
#[allow(clippy::too_many_arguments)]
fn draw_edit_line(
    frame: &mut [u8],
    x: usize,
    y: usize,
    text: &str,
    caret: usize,
    color: u32,
    bg: u32,
    avail_px: usize,
    scale: usize,
) {
    let chars: Vec<char> = text.chars().collect();
    // One cell is the caret's, so a full box still shows where typing goes.
    let cells = (avail_px / font::GLYPH_W).saturating_sub(1).max(1);
    let caret = caret.min(chars.len());
    let (first, cell) = edit_window(chars.len(), caret, cells);
    let mut shown: Vec<char> = chars.iter().skip(first).take(cells + 1).copied().collect();
    // Say the head was scrolled past, where the three cells it takes do not
    // land under the block: a caret sitting on a dot would be a lie about
    // what deleting there would remove.
    if first > 0 && cell >= 3 {
        shown[..3].fill('.');
    }
    let shown: String = shown.into_iter().collect();
    draw_panel_text(frame, x, y, &shown, color, 1, scale);
    // Half a cell wide: enough to be seen against the text at any scale,
    // narrow enough to leave most of the character it stands on legible.
    // It blinks, so it is also read as a caret rather than as a mark in
    // the value; out of phase, nothing is drawn and the character shows
    // whole.
    if !crate::video::caret_lit() {
        return;
    }
    let block = Rect {
        x: x + cell * font::GLYPH_W,
        y,
        w: (font::GLYPH_W / 2).max(1),
        h: font::GLYPH_H,
    };
    fill_rect(frame, scale_rect(block, scale), color, scale);
    let _ = bg;
}

/// Clip `text` to `avail_px`, keeping the TAIL and prefixing an ASCII "..."
/// when it does not fit, so a host directory's meaningful end (the leaf
/// dir) stays visible. The bitmap font is ASCII-only, so a real ellipsis
/// glyph cannot be drawn; "..." is the closest it can render. Mirrors
/// [`truncate_to_width`], which keeps the head instead, and
/// [`draw_edit_line`], which shows a window around the caret.
fn clip_text_tail(text: &str, avail_px: usize) -> String {
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
        clip_text_tail(name, max_chars * font::GLYPH_W)
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

/// How a row's second column presents when the setting does not apply.
///
/// Greying is the signal that a row cannot be reached; what stands in its
/// place is a per-row judgement, so it is made once, here, rather than spread
/// across the drawing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GreyedAs {
    /// Say why, as text where the control would be: the machine-shaped
    /// constraints worth explaining ("needs 32-bit CPU").
    Reason,
    /// Nothing. The greyed label says enough, and one phrase repeated down a
    /// page of drive bays or bridge settings says less than silence.
    Blank,
    /// The control, dimmed, still showing the setting's own value -- which
    /// still means something, and will be used again once the row applies.
    DimmedValue,
    /// The control, dimmed, with the reason in its value box: there is no
    /// mouse to be sensitive, and no shader to be strong.
    DimmedReason,
}

fn greyed_presentation(r: &launcher::Row, setup: &launcher::MachineSetup) -> GreyedAs {
    use LauncherField as F;
    // The workshop's rows stay put when they stop applying: an unformatted
    // disk still remembers the volume name it would have had.
    if LauncherState::is_workshop(r.field) {
        return GreyedAs::DimmedValue;
    }
    // A priority the machine has no drive to apply.
    if r.kind == RowKind::Bootpri {
        return GreyedAs::DimmedReason;
    }
    match r.field {
        F::MouseSensitivity | F::MouseCapture | F::ShaderStrength => GreyedAs::DimmedReason,
        F::RamPattern
        | F::FloppySpeed
        | F::AudioChannelMode
        | F::AudioFilter
        | F::AudioStereoSeparation => GreyedAs::DimmedValue,
        // Drive select is shaped by the interface, so it only shows a
        // selection while there is one to shape it: an attached DrawBridge
        // has no drive-select line, but with no interface at all there is
        // nothing to say.
        F::BridgeCable if setup.bridge_interface_selected() => GreyedAs::DimmedValue,
        F::ScsiUnit0
        | F::ScsiUnit1
        | F::ScsiUnit2
        | F::ScsiUnit3
        | F::ScsiUnit4
        | F::ScsiUnit5
        | F::ScsiUnit6
        | F::BridgeDevice
        | F::BridgePort
        | F::BridgeCable
        | F::BridgeDensity
        | F::BridgeReadMode
        | F::BridgeReplaySpeed => GreyedAs::Blank,
        _ => GreyedAs::Reason,
    }
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
        // The FluxBridge page's heading names the installed library, so its
        // text is not the one in the row table.
        let heading;
        let text = if r.field == LauncherField::BridgeLibrary {
            heading = bridge_library_heading();
            &heading
        } else {
            r.label
        };
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            row_y + 8,
            text,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        return;
    }
    // The ROM tab's identification lines: one greyed fact per row --
    // Name, Version, Revision -- indented two spaces under the indented
    // path row, the value following its label. The prefix stands even
    // when an unrecognised image leaves the value blank.
    if r.kind == RowKind::RomInfo {
        let (name, version, revision) = state.rom_note_cells(r.field);
        let value = match r.label {
            "Version" => version,
            "Revision" => revision,
            _ => name,
        };
        // The prefix in grey, the fact itself in full text colour.
        let x = launcher_pane_x(rect) + 4 * font::GLYPH_W;
        let prefix = format!("{}: ", r.label);
        draw_panel_text(frame, x, row_y + 4, &prefix, PANEL_TEXT_DIM, 1, scale);
        draw_panel_text(
            frame,
            x + prefix.chars().count() * font::GLYPH_W,
            row_y + 4,
            &value,
            PANEL_TEXT,
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
    // A workshop row greys on its own terms -- there is no machine setting
    // behind it to explain itself -- so it is asked directly.
    let reason = if LauncherState::is_workshop(r.field) {
        (!state.workshop_applies(r.field)).then_some("")
    } else {
        setup.disabled_reason(r.field)
    };
    // The SoundFont row's label stays lit even while the value shows
    // the bundled default -- the setting is present either way; only
    // the value is Copperline's answer rather than the person's.
    let label_keeps_colour = matches!(r.field, LauncherField::Rom | LauncherField::ScsiRom) || {
        #[cfg(feature = "coppersynth")]
        {
            r.field == LauncherField::CsynthSoundfont
        }
        #[cfg(not(feature = "coppersynth"))]
        {
            false
        }
    };
    let label_inherits = !label_keeps_colour && launcher_path_inherits(setup, r.field);
    let label_color = if reason.is_none() && !label_inherits {
        PANEL_TEXT
    } else {
        PANEL_TEXT_DIM
    };
    // A bay on a real drive says so in place of "Disk image": there is no
    // image, and the row's value is the interface rather than a file.
    let label = if r.kind == RowKind::FloppyMedia
        && launcher::MachineSetup::drive_image_bay(r.field)
            .is_some_and(|bay| setup.drive_bridged(bay))
    {
        // Matches the tick box that turned it on. Which version of the
        // library is linked in is named on the Configure page, where there is
        // room for it.
        "  FluxBridge"
    } else {
        r.label
    };
    draw_panel_text(
        frame,
        launcher_pane_x(rect),
        row_y + 8,
        label,
        label_color,
        1,
        scale,
    );
    let greyed_as = reason.map(|_| greyed_presentation(r, setup));
    let greyed_shows_reason = greyed_as == Some(GreyedAs::DimmedReason);
    let disabled = reason.is_some();
    if let Some(reason) = reason {
        if !matches!(
            greyed_as,
            Some(GreyedAs::DimmedValue | GreyedAs::DimmedReason)
        ) {
            if greyed_as != Some(GreyedAs::Blank) {
                // Where the value the row cannot have would sit: flush
                // against the right edge of the stepper's left arrow, so
                // every reason shares one margin whatever its length.
                let (prev, _, _) = launcher_cycle_rects(rect, row_y);
                draw_panel_text(
                    frame,
                    prev.x + prev.w,
                    row_y + 8,
                    reason,
                    PANEL_TEXT_DIM,
                    1,
                    scale,
                );
            }
            return;
        }
    }
    match r.kind {
        // Drawn above with an early return.
        RowKind::SectionHeader | RowKind::BootpriHeader | RowKind::RomInfo => {}
        RowKind::Text => {
            draw_launcher_value_box(
                frame,
                launcher_text_rect(rect, row_y, r.field),
                state,
                r.field,
                disabled,
                false,
                scale,
            );
        }
        RowKind::Size => {
            // A number to type, with the unit written beside it. The unit
            // is text rather than a button: clicking it swaps MB and GB.
            draw_launcher_value_box(
                frame,
                launcher_size_box_rect(rect, row_y),
                state,
                r.field,
                disabled,
                false,
                scale,
            );
            let unit = launcher_size_unit_rect(rect, row_y);
            draw_panel_text(
                frame,
                unit.x,
                unit.y + 6,
                state.workshop.size_unit.label(),
                if hover == Some(UiControl::LauncherNewImageUnit) {
                    PANEL_TEXT_HILIGHT
                } else {
                    PANEL_TEXT
                },
                1,
                scale,
            );
        }
        RowKind::Number => {
            draw_launcher_value_box(
                frame,
                launcher_number_rect(rect, row_y),
                state,
                r.field,
                disabled,
                false,
                scale,
            );
        }
        RowKind::FsFamily => {
            let labels: Vec<&str> = launcher::FsFamily::ALL.iter().map(|f| f.label()).collect();
            for (at, family) in launcher_tick_strip(rect, row_y, &labels)
                .into_iter()
                .zip(launcher::FsFamily::ALL)
            {
                draw_launcher_tick_choice(
                    frame,
                    at,
                    family.label(),
                    state.workshop_fs_family_set(r.field, family),
                    disabled,
                    hover
                        == Some(UiControl::LauncherFsFamily {
                            field: r.field,
                            family,
                        }),
                    scale,
                );
            }
        }
        RowKind::FsVariant => {
            // On an unformatted volume the row greys whole -- label, boxes
            // and all -- rather than disappearing, so the page keeps its
            // shape as the family above it changes.
            let labels: Vec<&str> = FS_VARIANTS.iter().map(|v| v.label()).collect();
            for (at, variant) in launcher_tick_strip(rect, row_y, &labels)
                .into_iter()
                .zip(FS_VARIANTS)
            {
                draw_launcher_tick_choice(
                    frame,
                    at,
                    variant.label(),
                    state.workshop_fs_variant_set(r.field, variant),
                    disabled || !state.workshop_fs_variant_enabled(r.field, variant),
                    hover
                        == Some(UiControl::LauncherFsVariant {
                            field: r.field,
                            variant,
                        }),
                    scale,
                );
            }
        }
        RowKind::Stepper => {
            let (prev, value, next) = launcher_geometry_stepper_rects(rect, row_y);
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
            draw_launcher_value_box(frame, value, state, r.field, disabled, true, scale);
        }
        RowKind::GeometryMode => {
            // Auto and Custom sit together as one choice, the chosen one
            // lit; Configure only appears once there is something to
            // configure.
            let (auto, custom, configure) = launcher_geometry_rects(rect, row_y);
            let by_hand = state.workshop.geometry_custom;
            draw_launcher_chip(
                frame,
                auto,
                "Auto",
                !by_hand,
                hover == Some(UiControl::LauncherGeometryAuto),
                false,
                scale,
            );
            draw_launcher_chip(
                frame,
                custom,
                "Custom",
                by_hand,
                hover == Some(UiControl::LauncherGeometryCustom),
                false,
                scale,
            );
            if by_hand {
                draw_text_button(
                    frame,
                    configure,
                    "Configure",
                    true,
                    hover == Some(UiControl::LauncherTab(LauncherTab::CreateGeometry)),
                    scale,
                );
            }
        }
        RowKind::Action => {
            let label = state.workshop_action_label(r.field);
            draw_text_button(
                frame,
                launcher_action_rect(rect, row_y),
                &label,
                !disabled,
                hover == Some(UiControl::LauncherNewImageCreate(r.field)),
                scale,
            );
            // The geometry editor commits with Save, and fills itself in
            // from the size with Auto beside it.
            if r.field == LauncherField::NewGeomSave {
                let auto = state.workshop_action_label(LauncherField::NewGeomAuto);
                draw_text_button(
                    frame,
                    launcher_action2_rect(rect, row_y),
                    &auto,
                    true,
                    hover
                        == Some(UiControl::LauncherNewImageCreate(
                            LauncherField::NewGeomAuto,
                        )),
                    scale,
                );
            }
        }
        RowKind::Cycle => {
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
            // Clip a long value (e.g. a wordy MIDI device name) to the box so
            // it cannot spill over the ">" stepper.
            let shown = match reason {
                Some(reason) if greyed_shows_reason => reason.to_string(),
                _ => state.row_value(r.field),
            };
            let text = truncate_to_width(&shown, value.w);
            let tw = text.chars().count() * font::GLYPH_W;
            let tx = value.x + value.w.saturating_sub(tw) / 2;
            let color = if disabled {
                PANEL_TEXT_DIM
            } else {
                PANEL_TEXT_HILIGHT
            };
            draw_panel_text(frame, tx, value.y + 6, &text, color, 1, scale);
        }
        RowKind::Bootpri => {
            // Priority column: a `< value >` stepper whose value is also a text
            // field. Greyed and inert while the Bootable box (drawn last) is
            // cleared, where it shows the -128 the config will store.
            //
            // A row with no drive to order has no priority to step through, so
            // the stepper goes entirely and only the reason is left, sitting
            // where the value would: a priority that could be changed, and one
            // that does not exist, should not look alike.
            let no_drive = reason.is_some();
            let disabled = disabled || setup.drive_boot_off(r.field);
            let (prev, value, next) = launcher_bootpri_rects(rect, row_y);
            if !no_drive {
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
            }
            let editing = state.editing() == Some(EditTarget::DriveBootpri(r.field));
            let text = if let Some(reason) = reason.filter(|_| greyed_shows_reason) {
                reason.to_string()
            } else {
                setup.value_label(r.field)
            };
            // A priority sits centred in its box; a row with no drive has no
            // box, so its reason starts where the column does -- under the
            // "Priority" heading, like every other greyed row on the page.
            let (text, tx) = if no_drive {
                (
                    truncate_to_width(&text, next.x + next.w - launcher_control_x(rect)),
                    launcher_control_x(rect),
                )
            } else {
                let text = truncate_to_width(&text, value.w.saturating_sub(8));
                let tw = text.chars().count() * font::GLYPH_W;
                (text, value.x + value.w.saturating_sub(tw) / 2)
            };
            let color = if disabled {
                PANEL_TEXT_DIM
            } else if editing {
                PANEL_TEXT_HILIGHT
            } else {
                PANEL_TEXT
            };
            if editing {
                draw_edit_line(
                    frame,
                    value.x + 4,
                    value.y + 6,
                    state.edit_buffer(),
                    state.edit_caret().at(),
                    PANEL_TEXT_HILIGHT,
                    PANEL_BG,
                    value.w.saturating_sub(8),
                    scale,
                );
            } else {
                draw_panel_text(frame, tx, value.y + 6, &text, color, 1, scale);
            }
            // Status column: the "Bootable" label then a tick box, ticked when
            // the drive is bootable.
            let cell = launcher_bootable_rect(rect, row_y);
            draw_panel_text(
                frame,
                cell.x,
                cell.y + 6,
                BOOTABLE_LABEL,
                if reason.is_some() {
                    PANEL_TEXT_DIM
                } else {
                    PANEL_TEXT
                },
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
        RowKind::FloppyMedia => {
            let bay = launcher::MachineSetup::drive_image_bay(r.field);
            let bridged = bay.is_some_and(|b| setup.drive_bridged(b));
            let value_x = launcher_control_x(rect);
            let (browse, clear) = launcher_path_rects(rect, row_y);
            if bridged {
                let bay = bay.expect("bridged implies a bay");
                let text = setup.drive_bridge_label(bay);
                draw_panel_text(frame, value_x, browse.y + 6, &text, PANEL_TEXT, 1, scale);
                let button = launcher_bridge_configure_rect(rect, row_y);
                draw_text_button(
                    frame,
                    button,
                    "Configure",
                    true,
                    hover == Some(UiControl::LauncherBridgeConfigure(bay)),
                    scale,
                );
            } else {
                let avail = browse.x.saturating_sub(value_x + 8);
                let text = truncate_to_width(&setup.value_label(r.field), avail);
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
                    launcher_clear_enabled(setup, r.field),
                    hover == Some(UiControl::LauncherClear(r.field)),
                    scale,
                );
            }
        }
        RowKind::FloppyFlags => {
            #[cfg_attr(not(feature = "fluxbridge"), allow(unused_variables))]
            let bay = launcher::MachineSetup::drive_protect_bay(r.field);
            #[cfg_attr(not(feature = "fluxbridge"), allow(unused_variables))]
            let (protect_cell, bridge_cell) = launcher_floppy_flag_rects(rect, row_y);
            let mut tick = |cell: Rect, label: &str, on: bool, hot: bool| {
                draw_panel_text(frame, cell.x, cell.y + 6, label, PANEL_TEXT, 1, scale);
                let box_rect = launcher_flag_box(cell, label);
                fill_rect(
                    frame,
                    scale_rect(box_rect, scale),
                    if hot { BUTTON_FACE_HOVER } else { ENTRY_BG },
                    scale,
                );
                draw_outline(frame, box_rect, BUTTON_EDGE_LIGHT, scale);
                if on {
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
            };
            tick(
                protect_cell,
                WRITE_PROTECT_LABEL,
                setup.toggle_value(r.field),
                hover == Some(UiControl::LauncherToggle(r.field)),
            );
            // Only drawn where a physical drive can actually be attached; a
            // build without the feature leaves the write-protect box alone on
            // the row rather than offering a switch that does nothing.
            #[cfg(feature = "fluxbridge")]
            if let Some(bay) = bay {
                tick(
                    bridge_cell,
                    PHYSICAL_DRIVE_LABEL,
                    setup.drive_bridged(bay),
                    hover == Some(UiControl::LauncherDriveBridgeToggle(bay)),
                );
            }
        }
        RowKind::Toggle if LauncherState::is_workshop(r.field) => {
            // A tick box rather than an On/Off button: these pages are a
            // list of choices about one thing, and ticks read as a list.
            let button = launcher_toggle_rect(rect, row_y);
            let on = state.row_toggle(r.field);
            let hot = hover == Some(UiControl::LauncherToggle(r.field));
            let box_rect = Rect {
                x: button.x,
                y: row_y + (LAUNCH_ROW_H - 10) / 2,
                w: 10,
                h: 10,
            };
            draw_tick_box(
                frame,
                box_rect.x,
                box_rect.y,
                // A setting that does not apply is not in force: showing a
                // tick on a row that cannot boot would promise a boot.
                on && !disabled,
                if disabled { PANEL_TEXT_DIM } else { TICK_GREEN },
                scale,
            );
            if hot && !disabled {
                draw_outline(frame, box_rect, PANEL_TEXT_HILIGHT, scale);
            }
        }
        RowKind::Toggle => {
            let button = launcher_toggle_rect(rect, row_y);
            let label = if state.row_toggle(r.field) {
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
            // The printer output and the Paths rows show a whole path
            // (clipped from the front if long, so the row never overflows
            // and the end -- which is the part that identifies it -- stays);
            // other path rows show the image file name.
            let text = match setup.full_path_label(r.field) {
                Some(full) => clip_path_keep_name(&full, avail),
                None => truncate_to_width(&setup.value_label(r.field), avail),
            };
            // An inheriting row centres its `(default)`, which reads as a
            // column of its own rather than as eleven short strings
            // pretending to be paths. A row with a real path keeps the
            // left edge every other path on the page is read from.
            let inherits = launcher_path_inherits(setup, r.field);
            let (value_color, text_x) = if inherits {
                let text_w = text.chars().count() * font::GLYPH_W;
                // The bundled-ROM defaults read from the left like a
                // chosen path would, just dimmed; the SoundFont default
                // sits in line with the cycle value column, centred
                // under the arrows of the rows around it; the Paths
                // page's inherited rows keep their centred `(default)`.
                if matches!(r.field, LauncherField::Rom | LauncherField::ScsiRom) {
                    (PANEL_TEXT_DIM, value_x)
                } else {
                    #[cfg(feature = "coppersynth")]
                    if r.field == LauncherField::CsynthSoundfont {
                        let (_, value_box, _) = launcher_cycle_rects(rect, row_y);
                        let x = value_box.x + value_box.w.saturating_sub(text_w) / 2;
                        (PANEL_TEXT_DIM, x)
                    } else {
                        (PANEL_TEXT_DIM, value_x + avail.saturating_sub(text_w) / 2)
                    }
                    #[cfg(not(feature = "coppersynth"))]
                    (PANEL_TEXT_DIM, value_x + avail.saturating_sub(text_w) / 2)
                }
            } else {
                (PANEL_TEXT, value_x)
            };
            draw_panel_text(frame, text_x, browse.y + 6, &text, value_color, 1, scale);
            let (has_browse, has_clear) = launcher_path_buttons(setup, r.field);
            if has_browse {
                draw_text_button(
                    frame,
                    browse,
                    "Browse",
                    true,
                    hover == Some(UiControl::LauncherBrowse(r.field)),
                    scale,
                );
            }
            if has_clear {
                // "Reset" where the row goes back to a default rather
                // than being emptied -- the Paths page. Everywhere else
                // the button clears, and says so (the SoundFont row's
                // clear also lands on the bundled default, but it wears
                // the same word as its neighbours).
                let resets = r.field.is_paths_field();
                let label = if resets { "Reset" } else { "Clear" };
                let enabled = launcher_clear_enabled(setup, r.field);
                draw_text_button(
                    frame,
                    clear,
                    label,
                    enabled,
                    hover == Some(UiControl::LauncherClear(r.field)),
                    scale,
                );
            }
        }
        #[cfg(feature = "game-library")]
        RowKind::Account => {
            // Where Browse would be, because it is the same shape of thing:
            // the button that fills the column in. The column itself says
            // whether this session is signed in, and is empty when it is
            // not -- there is no account setting to report, only a session.
            let (button, _) = launcher_path_rects(rect, row_y);
            let signed_in = state.openretro.is_some();
            if signed_in {
                draw_panel_text(
                    frame,
                    launcher_control_x(rect),
                    button.y + 6,
                    "logged in",
                    PANEL_TEXT,
                    1,
                    scale,
                );
            }
            draw_text_button(
                frame,
                button,
                if signed_in { "Log out" } else { "Log in" },
                true,
                hover == Some(UiControl::LauncherOpenRetroLogin),
                scale,
            );
        }
        #[cfg(not(feature = "game-library"))]
        RowKind::Account => {}
        RowKind::Drive => {
            let (browse, clear) = launcher_path_rects(rect, row_y);
            let value_x = launcher_control_x(rect);
            // A slot holding a real disk is not something to browse for: the
            // disk was chosen from what the host has, and the only thing to
            // do with it here is give it back. Browse and Clear make way for
            // one Unmount spanning both.
            if let Some(disk) = setup.host_disk_on_row(r.field) {
                // The device and the volume on it: the device name is what
                // the Host Disk page and the host itself call it, and the
                // volume is what makes it recognisable.
                let text = truncate_to_width(
                    &setup.host_disk_label(&disk.device),
                    browse.x.saturating_sub(value_x + 8),
                );
                draw_panel_text(frame, value_x, browse.y + 6, &text, PANEL_TEXT, 1, scale);
                let unmount = Rect {
                    x: browse.x,
                    y: browse.y,
                    w: clear.x + clear.w - browse.x,
                    h: browse.h,
                };
                draw_text_button(
                    frame,
                    unmount,
                    "Unmount",
                    true,
                    hover == Some(UiControl::LauncherHostDiskUnmount(r.field)),
                    scale,
                );
                return;
            }

            // The volume-name box only appears once an image is chosen (a name
            // has nothing to label otherwise, and never labels a CD image);
            // until then the row reads like a plain path row and the path text
            // fills the full width.
            let has_image = setup.path(r.field).is_some() && setup.drive_name_applies(r.field);
            let has_fs_toggle = launcher_drive_fs_applies(setup, r.field);
            let name_box = launcher_drive_name_rect(rect, row_y);
            let fs_box = launcher_drive_fs_rect(rect, row_y);
            let text_right = if has_fs_toggle {
                fs_box.x
            } else if has_image {
                name_box.x
            } else {
                browse.x
            };
            let avail = text_right.saturating_sub(value_x + 8);
            // Host FS mounts and the WHDLoad paths show the whole host path
            // (clipped to keep the final name, with a leading "..." when
            // long), since the path is meaningful; other drives show the
            // image's file name.
            let full_path = r.field.is_filesys_dir_field() || r.field.is_whdload_path_field();
            let text = match (full_path, setup.path(r.field)) {
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
                let (label, color) = if let Some(name) = setup.drive_name(r.field) {
                    (name.to_string(), PANEL_TEXT)
                } else {
                    ("(volume)".to_string(), PANEL_TEXT_DIM)
                };
                if editing {
                    draw_edit_line(
                        frame,
                        name_box.x + 4,
                        name_box.y + 6,
                        state.edit_buffer(),
                        state.edit_caret().at(),
                        PANEL_TEXT_HILIGHT,
                        PANEL_BG,
                        name_box.w.saturating_sub(8),
                        scale,
                    );
                } else {
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
            }
            if has_fs_toggle {
                let label = if setup.drive_filesystem(r.field).ffs {
                    "FFS"
                } else {
                    "OFS"
                };
                draw_text_button(
                    frame,
                    fs_box,
                    label,
                    true,
                    hover == Some(UiControl::LauncherDriveFilesystemToggle(r.field)),
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
                launcher_clear_enabled(setup, r.field),
                hover == Some(UiControl::LauncherClear(r.field)),
                scale,
            );
            // A support archive with nothing chosen offers to fetch its
            // own, from the same place and against the same digest the
            // packaging script uses.
            #[cfg(feature = "game-library")]
            if row_archive(r.field).is_some() && setup.path(r.field).is_none() {
                draw_text_button(
                    frame,
                    launcher_download_rect(rect, row_y),
                    "Download",
                    true,
                    hover == Some(UiControl::LauncherWhdloadDownload(r.field)),
                    scale,
                );
            }
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
                !value.is_empty(),
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
            if editing {
                draw_edit_line(
                    frame,
                    vbox.x + 4,
                    row_y + 8,
                    state.edit_buffer(),
                    state.edit_caret().at(),
                    PANEL_TEXT_HILIGHT,
                    PANEL_BG,
                    vbox.w.saturating_sub(8),
                    scale,
                );
            } else {
                let shown = truncate_to_width(&value, vbox.w.saturating_sub(8));
                draw_panel_text(frame, vbox.x + 4, row_y + 8, &shown, PANEL_TEXT, 1, scale);
            }
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
    let whdload_entry = state.setup.whdload_enabled();
    let strip = launcher::tabs(whdload_entry);
    for (i, &tab) in strip.iter().enumerate() {
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
        launcher_nav_block_h(state.tab)
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
            state.setup.midi_out_is_mt32(),
            state.setup.midi_out_is_csynth(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .enumerate()
        {
            draw_launcher_row(frame, rect, state, r, i, row_offset, hover, scale);
        }
    }
    // Nav row at the top of the pane: a Back button when this is a sub-page,
    // then its sibling links, with the current one highlighted. A page can
    // show both -- the Create Image pages say where they came from and which
    // of the two they are.
    let back_parent = state.tab.parent_tab();
    let options = state.tab.nav_options();
    let mut slot = 0;
    if let Some(parent) = back_parent {
        draw_text_button(
            frame,
            launcher_back_button_rect(rect),
            "< Back",
            true,
            hover == Some(UiControl::LauncherTab(parent)),
            scale,
        );
        slot = 1;
    }
    for (i, &(label, target)) in options.iter().enumerate() {
        draw_launcher_chip(
            frame,
            launcher_nav_button_rect(rect, slot + i),
            label,
            target == state.tab,
            hover == Some(UiControl::LauncherTab(target)),
            false,
            scale,
        );
    }
    #[cfg(feature = "game-library")]
    if state.tab == LauncherTab::WhdloadLibrary {
        draw_library_page(frame, rect, state, hover, scale);
    }
    if state.tab == LauncherTab::HostDisk {
        draw_host_disk_page(frame, rect, state, hover, scale);
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
                state.setup.midi_out_is_mt32(),
                state.setup.midi_out_is_csynth(),
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
    // The geometry editor says what the figures come to, because on this
    // page the geometry -- not the Size box -- decides how big the image is.
    if state.tab == LauncherTab::CreateGeometry {
        let g = state.workshop.custom_geometry;
        let note_top = launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::CreateGeometry,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
                state.setup.midi_out_is_mt32(),
                state.setup.midi_out_is_csynth(),
            )
            .len()
                + 1,
        ) + row_offset
            + 14;
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            note_top,
            "Info:",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        draw_panel_text(
            frame,
            launcher_pane_x(rect) + 2 * font::GLYPH_W,
            note_top + 16,
            &{
                let size = crate::config::format_size(g.bytes() as usize);
                format!(
                    "These values will create {} {size} disk image.",
                    indefinite_article(&size)
                )
            },
            PANEL_TEXT,
            1,
            scale,
        );
    }
    // The Boot Priority page spells out the valid priority range below the
    // rows, under a dimmed "Info:" heading.
    if state.tab == LauncherTab::BootPriority && state.setup.has_boot_priority_rows() {
        let help_top = (launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::BootPriority,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
                state.setup.midi_out_is_mt32(),
                state.setup.midi_out_is_csynth(),
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
    // NAT and bridged backends deliver inbound traffic on the host's schedule,
    // so warn that runs stop being reproducible the moment packets flow
    // (loopback and an isolated NIC stay deterministic).
    if state.tab == LauncherTab::IoNetworking && setup.ethernet_breaks_determinism() {
        let note_top = launcher_row_y(
            rect,
            launcher::rows(
                LauncherTab::IoNetworking,
                state.setup.parallel_device(),
                state.setup.serial_mode(),
                state.setup.midi_out_is_mt32(),
                state.setup.midi_out_is_csynth(),
            )
            .len()
                + 1,
        ) + row_offset;
        draw_panel_text(
            frame,
            launcher_pane_x(rect),
            note_top,
            "Warning: host networking is non-deterministic.",
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
        let color = match status.kind {
            launcher::StatusKind::Ok => PANEL_TEXT_HILIGHT,
            // Work in progress and a failure share the warning colour:
            // both are "not finished, and worth your attention", and only
            // a line that says something worked has earned the green.
            launcher::StatusKind::Busy | launcher::StatusKind::Error => PANEL_TEXT_ACCENT,
        };
        // Kept inside the panel. A failure explains itself at whatever length
        // it needs to, and one long enough to run past the edge is drawn over
        // the window either side of it -- so it is clipped here, with the log
        // holding the whole of what went wrong.
        let text = truncate_to_width(&status.text, rect.w.saturating_sub(20));
        draw_panel_text(
            frame,
            rect.x + 10,
            launcher_status_y(rect),
            &text,
            color,
            1,
            scale,
        );
    }
    // Action bar. While the Save dialog is up, every position outside
    // its three buttons answers as the Save control (a stray click puts
    // the dialog away), so pointer-lighting the button under it would
    // flash on every hover in the dialog -- the button stays unlit until
    // the dialog is gone.
    for (control, button_rect) in launcher_action_rects(rect) {
        let lit =
            hover == Some(control) && !(control == UiControl::LauncherSave && state.save_dialog);
        draw_text_button(
            frame,
            button_rect,
            launcher_action_label(control),
            true,
            lit,
            scale,
        );
    }
    draw_launcher_save_dialog(frame, rect, state, hover, scale);
    draw_launcher_confirm(frame, rect, state, hover, scale);
    // Over everything, because it is the only thing being answered while
    // it is up.
    #[cfg(feature = "game-library")]
    if state.login.is_some() {
        draw_login_dialog(frame, rect, state, hover, scale);
    }
    #[cfg(feature = "game-library")]
    if state.meta.is_some() {
        draw_meta_dialog(frame, rect, state, hover, scale);
    }
}

/// The metadata editor.
#[cfg(feature = "game-library")]
fn draw_meta_dialog(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let Some(meta) = &state.meta else { return };
    fill_rect_blend(frame, scale_rect(rect, scale), SCRIM, SCRIM_ALPHA, scale);
    let dialog = meta_rect(rect);
    fill_rect(frame, scale_rect(dialog, scale), PANEL_BG, scale);
    draw_rect_bevel(
        frame,
        scale_rect(dialog, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    draw_title_bar(
        frame,
        dialog,
        "Update metadata",
        hover == Some(UiControl::MetaCancel),
        scale,
    );

    // The art, drawn the way the Library page draws it, and clickable.
    let art = meta_art_rect(rect);
    fill_rect(frame, scale_rect(art, scale), BUTTON_FACE, scale);
    draw_rect_bevel(
        frame,
        scale_rect(art, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    let inner = Rect {
        x: art.x + LIBRARY_COVER_BEZEL,
        y: art.y + LIBRARY_COVER_BEZEL,
        w: art.w - 2 * LIBRARY_COVER_BEZEL,
        h: art.h - 2 * LIBRARY_COVER_BEZEL,
    };
    fill_rect(frame, scale_rect(inner, scale), ENTRY_BG, scale);
    let picture = meta
        .art
        .as_deref()
        .and_then(|key| state.library.covers.get(key));
    match picture {
        Some(picture) => draw_cover_art(frame, inner, picture, scale),
        None => {
            for (line, text) in ["Click to", "choose art"].into_iter().enumerate() {
                let w = text.len() * font::GLYPH_W;
                draw_panel_text(
                    frame,
                    inner.x + inner.w.saturating_sub(w) / 2,
                    inner.y + inner.h / 2 - 12 + line * 12,
                    text,
                    PANEL_TEXT_DIM,
                    1,
                    scale,
                );
            }
        }
    }
    draw_rect_bevel(
        frame,
        scale_rect(inner, scale),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );
    if hover == Some(UiControl::MetaArt) {
        draw_outline(frame, art, PANEL_TEXT_HILIGHT, scale);
    }

    for field in launcher::MetaField::ALL {
        let box_rect = meta_field_rect(rect, field);
        draw_panel_text(
            frame,
            art.x + art.w + 12,
            box_rect.y + 5,
            field.label(),
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        fill_rect(frame, scale_rect(box_rect, scale), ENTRY_BG, scale);
        draw_outline(
            frame,
            box_rect,
            if meta.focus == field {
                PANEL_TEXT_HILIGHT
            } else {
                BUTTON_EDGE_DARK
            },
            scale,
        );
        // The focused box carries the caret, and the window on the text
        // follows it: metadata is amended more often than typed fresh, so
        // the middle of a value has to be reachable.
        let value = meta.value(field);
        if meta.focus == field {
            draw_edit_line(
                frame,
                box_rect.x + 5,
                box_rect.y + 5,
                value,
                meta.caret.at(),
                PANEL_TEXT,
                ENTRY_BG,
                box_rect.w.saturating_sub(10),
                scale,
            );
        } else {
            draw_panel_text(
                frame,
                box_rect.x + 5,
                box_rect.y + 5,
                &truncate_to_width(value, box_rect.w.saturating_sub(10)),
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }

    for (at, (label, control)) in [
        ("Save", UiControl::MetaSave),
        ("Clear", UiControl::MetaClear),
        ("Cancel", UiControl::MetaCancel),
    ]
    .into_iter()
    .enumerate()
    {
        draw_text_button(
            frame,
            meta_button_rects(rect)[at],
            label,
            true,
            hover == Some(control),
            scale,
        );
    }
}

/// The OpenRetro sign-in dialog.
///
/// The password is drawn as a run of asterisks, one per character typed:
/// the [`crate::gamelib::Secret`] behind it is never turned into display
/// text, so what is on screen cannot be a second copy of it.
#[cfg(feature = "game-library")]
fn draw_login_dialog(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    use launcher::LoginField;
    let Some(login) = &state.login else { return };
    // Everything behind it is dimmed rather than merely covered: a dialog
    // that only overlaps the page still looks like part of it.
    fill_rect_blend(frame, scale_rect(rect, scale), SCRIM, SCRIM_ALPHA, scale);
    let dialog = login_rect(rect);
    fill_rect(frame, scale_rect(dialog, scale), PANEL_BG, scale);
    draw_rect_bevel(
        frame,
        scale_rect(dialog, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    // Its own title bar, the same as the panel's: a window over a window
    // should look like one, close gadget included.
    draw_title_bar(
        frame,
        dialog,
        "Log in to OpenRetro",
        hover == Some(UiControl::LoginCancel),
        scale,
    );
    for field in [LoginField::User, LoginField::Pass] {
        let box_rect = login_field_rect(rect, field);
        let (label, shown) = match field {
            LoginField::User => ("Username", login.user.clone()),
            LoginField::Pass => ("Password", "*".repeat(login.pass.chars())),
        };
        draw_panel_text(
            frame,
            dialog.x + 12,
            box_rect.y + 5,
            label,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
        fill_rect(frame, scale_rect(box_rect, scale), ENTRY_BG, scale);
        draw_outline(
            frame,
            box_rect,
            if login.focus == field {
                PANEL_TEXT_HILIGHT
            } else {
                BUTTON_EDGE_DARK
            },
            scale,
        );
        // The focused box carries the caret, and the window on the text
        // follows it. The mask is one asterisk a character, so the caret
        // steps through a password exactly as it does through a name.
        if login.focus == field {
            draw_edit_line(
                frame,
                box_rect.x + 5,
                box_rect.y + 5,
                &shown,
                login.caret.at(),
                PANEL_TEXT,
                ENTRY_BG,
                box_rect.w.saturating_sub(10),
                scale,
            );
        } else {
            draw_panel_text(
                frame,
                box_rect.x + 5,
                box_rect.y + 5,
                &truncate_to_width(&shown, box_rect.w.saturating_sub(10)),
                PANEL_TEXT,
                1,
                scale,
            );
        }
    }
    let (ok, cancel) = login_button_rects(rect);
    draw_text_button(
        frame,
        ok,
        "OK",
        !login.sending,
        hover == Some(UiControl::LoginOk),
        scale,
    );
    draw_text_button(
        frame,
        cancel,
        "Cancel",
        true,
        hover == Some(UiControl::LoginCancel),
        scale,
    );
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

/// How much of the picture the open menu veils. Enough to throw the menu
/// forward and to say the machine is not listening, while leaving what is
/// behind readable.
const MENU_VEIL: u32 = rgba(8, 9, 11);
const MENU_VEIL_ALPHA: f32 = 0.45;

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
    // The veil goes over the whole presentation, panels included: the menu
    // takes precedence over anything it is opened on top of. It is painted
    // into the presentation texture, so it never reaches a recording.
    fill_rect_blend(
        frame,
        Rect {
            x: 0,
            y: 0,
            w: texture_width(scale),
            h: texture_height(scale),
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
        fill_rect(frame, scale_rect(panel, scale), PANEL_BG, scale);
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
mod tests {
    use super::*;
    use crate::config::{JoystickInputMode, PixelAspect, WarpSpeed};

    #[cfg(feature = "game-library")]
    #[test]
    fn the_az_row_appears_only_once_the_list_is_worth_indexing() {
        use crate::gamelib::{Game, Known, Library};
        let rect = Rect {
            x: 0,
            y: 0,
            w: 716,
            h: 581,
        };
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::WhdloadLibrary;
        let stock = |n: usize| {
            (0..n)
                .map(|at| Known {
                    file: format!("game{at}.lha"),
                    game: Some(Game {
                        name: format!("Game {at}"),
                        ..Game::default()
                    }),
                    manual: false,
                    slave_sha1: None,
                })
                .collect::<Vec<_>>()
        };
        let fill = |state: &mut LauncherState, n: usize| {
            state.library.db.set_known(stock(n));
            state.library.games = Library::known(std::path::Path::new("/games"), &state.library.db);
        };
        let whdload_entry = state.setup.whdload_enabled();
        let first = library_az_rects(rect, whdload_entry)[0];
        let hit = |state: &LauncherState| {
            launcher_control_at(rect, state, (first.x as i32 + 1, first.y as i32 + 1))
        };

        // A short list is read rather than indexed, so the row is not there
        // -- and the pixels it would occupy answer to nothing.
        fill(&mut state, LIBRARY_AZ_MIN_GAMES - 1);
        assert_eq!(hit(&state), None, "the row is up on a short list");

        // One more game and it appears.
        fill(&mut state, LIBRARY_AZ_MIN_GAMES);
        assert_eq!(
            hit(&state),
            Some(UiControl::LauncherLibraryJump(0)),
            "the row is missing on a list long enough for it"
        );
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_favourites_arrows_appear_and_its_rows_follow_the_scroll() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 716,
            h: 581,
        };
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::WhdloadLibrary;
        let whdload_entry = state.setup.whdload_enabled();
        let rows = library_favourite_rows(rect, whdload_entry);
        assert!(rows > 1, "the box holds rows to scroll: {rows}");

        // A list that fits has no arrows: the corner is a row's, not a
        // button's, so a click there picks the game under it.
        for at in 0..rows {
            state
                .library
                .db
                .toggle_favourite(&format!("Game{at}.lha"), &format!("Game {at}"));
        }
        let [(up, up_at), (down, down_at)] = library_favourite_arrow_rects(rect, whdload_entry);
        let hit = |state: &LauncherState, at: Rect| {
            launcher_control_at(rect, state, (at.x as i32 + 2, at.y as i32 + 2))
        };
        assert_ne!(hit(&state, up_at), Some(up));
        assert_ne!(hit(&state, down_at), Some(down));

        // One more than fits, and they appear.
        state.library.db.toggle_favourite("GameN.lha", "Game N");
        assert_eq!(hit(&state, up_at), Some(up));
        assert_eq!(hit(&state, down_at), Some(down));

        // The rows are drawn positions: scrolled down, the top row is the
        // scroll's, so clicking it removes the right favourite.
        let first = library_favourite_row_rect(rect, whdload_entry, 0);
        let click = (first.x as i32 + 40, first.y as i32 + 2);
        assert_eq!(
            launcher_control_at(rect, &state, click),
            Some(UiControl::LauncherLibraryFavouritePick(0))
        );
        state.scroll_favourites(1, rows);
        assert_eq!(state.library.favourite_scroll, 1);
        assert_eq!(
            launcher_control_at(rect, &state, click),
            Some(UiControl::LauncherLibraryFavouritePick(0)),
            "still the top drawn row; the window under it moved"
        );

        // And the last drawn row is the last one there is: scrolled to the
        // end, the rows below it are not clickable.
        state.scroll_favourites(100, rows);
        let past = library_favourite_row_rect(rect, whdload_entry, rows - 1);
        assert_eq!(
            launcher_control_at(rect, &state, (past.x as i32 + 40, past.y as i32 + 2)),
            Some(UiControl::LauncherLibraryFavouritePick(rows - 1))
        );
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn a_balanced_wrap_does_not_leave_one_word_alone() {
        let g = font::GLYPH_W;
        let text = "Update the \"Game library\" path in the WHDLoad settings";
        // Greedy: a full line, then the one word that did not fit.
        let greedy = wrap_to_width(text, 45 * g);
        assert_eq!(greedy.len(), 2);
        assert_eq!(greedy[1], "settings");

        // Balanced: the same two lines, broken where it reads.
        let even = wrap_balanced(text, 45 * g);
        assert_eq!(even.len(), 2, "the line count is what it was");
        assert!(
            even[1].split_whitespace().count() > 1,
            "still one word alone: {even:?}"
        );
        let longest = even.iter().map(|l| l.chars().count()).max().unwrap();
        assert!(
            longest < greedy[0].chars().count(),
            "no narrower than greedy: {even:?}"
        );
        // What is said is still what was passed in.
        assert_eq!(even.join(" "), text);

        // Text that fits is left alone rather than split for the sake of it.
        assert_eq!(wrap_balanced("Short", 45 * g), vec!["Short".to_string()]);
    }

    #[test]
    fn an_edit_window_keeps_the_caret_in_the_box() {
        // Short enough to fit: the whole line is shown from the start, and
        // the caret sits where it is.
        assert_eq!(edit_window(5, 0, 10), (0, 0));
        assert_eq!(edit_window(5, 5, 10), (0, 5));

        // Longer than the box. Near the front nothing scrolls yet...
        assert_eq!(edit_window(40, 0, 10), (0, 0));
        assert_eq!(edit_window(40, 4, 10), (0, 4));
        // ...and past half a box the text starts moving under a caret that
        // stays put, rather than the caret walking into the edge.
        assert_eq!(edit_window(40, 20, 10), (15, 5));
        assert_eq!(edit_window(40, 21, 10), (16, 5));
        // At the end the window stops: the tail is shown and the caret
        // walks the last cells, which is where typing leaves it.
        assert_eq!(edit_window(40, 40, 10), (30, 10));
        assert_eq!(edit_window(40, 38, 10), (30, 8));

        // The caret is always inside the box it is drawn in.
        for len in 0..40 {
            for caret in 0..=len {
                for cells in 1..12 {
                    let (first, cell) = edit_window(len, caret, cells);
                    assert!(first <= caret, "{len}/{caret}/{cells}: {first}");
                    assert!(cell <= cells, "{len}/{caret}/{cells}: {cell}");
                }
            }
        }
    }

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

    /// The ROM tab's identification lines sit under the path row: the
    /// greyed Name/Version/Revision prefixes stand either way, and an
    /// image no checksum names leaves the values after them blank.
    #[test]
    fn the_rom_tab_draws_its_identification_line_under_the_path_row() {
        use super::super::window::{texture_height, texture_width};
        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        // Pixels painted in the info-line ink on a given ROM-tab row
        // (the Name line is index 2, under the section heading and the
        // path row). The prefixes draw dimmed, the values in full text
        // colour; both count.
        let row_pixels = |frame: &[u8], rect: Rect, row: usize| {
            let row_y = launcher_row_y(rect, row);
            let mut lit = 0;
            for y in row_y..row_y + LAUNCH_ROW_H {
                for x in launcher_pane_x(rect)..rect.x + rect.w - LAUNCH_MARGIN {
                    let p = (y * w + x) * 4;
                    if frame[p..p + 4] == PANEL_TEXT_DIM.to_le_bytes()
                        || frame[p..p + 4] == PANEL_TEXT.to_le_bytes()
                    {
                        lit += 1;
                    }
                }
            }
            lit
        };
        let note_row_pixels = |frame: &[u8], rect: Rect| row_pixels(frame, rect, 2);
        let panel_of = |state: LauncherState| Panel::Launcher(Box::new(state));

        let mut setup = launcher::MachineSetup::default();
        setup.set_path(
            LauncherField::Rom,
            std::path::PathBuf::from("mystery-dump.rom"),
        );
        let mut unknown = LauncherState::new(setup.clone());
        unknown.tab = LauncherTab::Rom;
        let mut known = LauncherState::new(setup);
        known.tab = LauncherTab::Rom;
        known.set_rom_note_for_test(LauncherField::Rom, "Kickstart 3.1 (40.68) A1200");

        let mut blank_frame = vec![0u8; w * h * 4];
        let ui = UiState {
            panel: Some(panel_of(unknown)),
            ..Default::default()
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        draw(&mut blank_frame, scale, &ui, None, None);
        let blank_ink = note_row_pixels(&blank_frame, rect);
        assert!(
            blank_ink > 0,
            "the Name: prefix stands even over an unrecognised image"
        );

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            panel: Some(panel_of(known)),
            ..Default::default()
        };
        draw(&mut frame, scale, &ui, None, None);
        assert!(
            note_row_pixels(&frame, rect) > blank_ink,
            "the identification's value adds ink beyond the prefix"
        );
    }

    /// The launcher panel is a fixed box with no row scrolling, so a tab's
    /// rows have to fit between the content top (below the nav row on pages
    /// that have one) and the status line at the bottom. Nothing may reach
    /// the action buttons or hang off the panel. Adding one row too many to
    /// a tab fails here rather than silently drawing over the Save button.
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
            LauncherTab::IoParallel,
            LauncherTab::IoNetworking,
            LauncherTab::IoAudio,
            LauncherTab::Cd,
            LauncherTab::HostFs,
            LauncherTab::Whdload,
            LauncherTab::BootPriority,
            LauncherTab::Lide,
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
                    let rows = launcher::rows(tab, device, mode, false, false);
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
                            // The widest of these: a serial address box is
                            // sized for a host name and a port.
                            launcher_text_rect(rect, row_y, r.field),
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
    fn frame_analyzer_controls_hit_test() {
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::FrameAnalyzer(FrameAnalyzerPanel::new())),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let raster = analyzer_raster_rect(rect);
        assert_eq!(
            ui.control_at((raster.x as i32 + raster.w as i32 / 2, raster.y as i32 + 2)),
            Some(UiControl::AnalyzerPick {
                x: 511,
                y: 8,
                scanline: false,
            })
        );
        let scanline = analyzer_scanline_rect(rect);
        assert_eq!(
            ui.control_at((
                scanline.x as i32 + scanline.w as i32 / 2,
                scanline.y as i32 + 2
            )),
            Some(UiControl::AnalyzerPick {
                x: 511,
                y: 60,
                scanline: true,
            })
        );
        let (control, button) = analyzer_button_rects(rect)[1];
        assert_eq!(control, UiControl::AnalyzerFrame);
        assert_eq!(
            ui.control_at((button.x as i32 + 2, button.y as i32 + 2)),
            Some(UiControl::AnalyzerFrame)
        );
        let underlay = analyzer_underlay_rect(rect);
        assert_eq!(
            ui.control_at((underlay.x as i32 + 2, underlay.y as i32 + 2)),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
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
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2)),
                Some(UiControl::AnalyzerTab(*tab))
            );
        }

        // Beam tab: the beam controls hit, the map does not exist.
        let ui = analyzer_ui(AnalyzerTab::Beam, Vec::new());
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let map = analyzer_heat_map_rect(rect);
        let underlay = analyzer_underlay_rect(rect);
        assert_eq!(
            ui.control_at((underlay.x as i32 + 2, underlay.y as i32 + 2)),
            Some(UiControl::AnalyzerUnderlay)
        );
        let in_map = (map.x as i32 + map.w as i32 / 2, map.y as i32 + 4);
        assert!(
            !matches!(
                ui.control_at(in_map),
                Some(UiControl::AnalyzerHeatPick { .. })
            ),
            "the heat map is not drawn on the Beam tab, so it must not be clickable"
        );

        // Memory tab: the map hits, and none of the beam-only controls do.
        let ui = analyzer_ui(AnalyzerTab::Memory, Vec::new());
        assert!(matches!(
            ui.control_at(in_map),
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
                ui.control_at(beam_only),
                Some(UiControl::PanelBody),
                "beam-only controls are inert on the Memory tab"
            );
        }
        // Run and Frame stay on both tabs.
        for slot in 0..2 {
            let (control, button) = analyzer_button_rects(rect)[slot];
            assert_eq!(
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2)),
                Some(control)
            );
        }
        // The scanline strip belongs to the beam view too.
        let scanline = analyzer_scanline_rect(rect);
        assert!(!matches!(
            ui.control_at((scanline.x as i32 + 4, scanline.y as i32 + 4)),
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
            ui.control_at((map.x as i32 + dx as i32, map.y as i32 + dy as i32))
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
            ui.control_at((map.x as i32 + map.w as i32, map.y as i32 + 2)),
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
                ui.control_at((button.x as i32 + 2, button.y as i32 + 2)),
                Some(*control)
            );
            // Presets sit above the map, never over it.
            assert!(button.y + button.h <= analyzer_heat_map_rect(rect).y);
        }
        // With no presets the row is empty: the same points are panel body.
        let empty = analyzer_ui(AnalyzerTab::Memory, Vec::new());
        for (_, button) in rects {
            assert_eq!(
                empty.control_at((button.x as i32 + 2, button.y as i32 + 2)),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::About),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let close = close_button_rect(rect);
        let pos = (close.x as i32 + 2, close.y as i32 + 2);
        assert_eq!(ui.control_at(pos), Some(UiControl::PanelClose));
        // Panel body swallows clicks.
        let body = (rect.x as i32 + 5, (rect.y + TITLE_H + 5) as i32);
        assert_eq!(ui.control_at(body), Some(UiControl::PanelBody));
        // Outside the panel: nothing.
        assert_eq!(ui.control_at((0, 0)), None);
    }

    /// Clicking a serial address box opens *that* edit, not the Create Image
    /// one the free-text widget was first built for.
    #[cfg(feature = "midi")]
    #[test]
    fn the_serial_address_box_hit_tests_to_its_own_edit() {
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        while state.setup.serial_mode() != crate::config::SerialMode::TcpConnect {
            state.setup.cycle(LauncherField::SerialMode, true);
        }
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let Some(Panel::Launcher(state)) = ui.panel.as_ref() else {
            unreachable!()
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let index = launcher::rows(
            state.tab,
            state.setup.parallel_device(),
            state.setup.serial_mode(),
            state.setup.midi_out_is_mt32(),
            state.setup.midi_out_is_csynth(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .position(|r| r.field == LauncherField::SerialConnect)
        .expect("no Connect row in tcp-connect mode");
        // The serial page sits under the I/O Ports nav row, so its rows
        // start a nav block lower.
        let row_y = launcher_row_y(rect, index) + LAUNCH_NAV_BLOCK_H;
        let box_rect = launcher_text_rect(rect, row_y, LauncherField::SerialConnect);
        assert_eq!(
            ui.control_at((box_rect.x as i32 + 4, box_rect.y as i32 + 4)),
            Some(UiControl::LauncherSerialAddrEdit(
                LauncherField::SerialConnect
            ))
        );
    }

    #[test]
    fn fixed_ram_pattern_box_hit_tests_to_its_own_edit() {
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::Memory;
        state.setup.cycle(LauncherField::RamInit, false); // Zero -> Fixed
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let Some(Panel::Launcher(state)) = ui.panel.as_ref() else {
            unreachable!()
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let index = launcher::rows(
            state.tab,
            state.setup.parallel_device(),
            state.setup.serial_mode(),
            state.setup.midi_out_is_mt32(),
            state.setup.midi_out_is_csynth(),
        )
        .iter()
        .filter(|r| !state.setup.row_hidden(r.field))
        .position(|r| r.field == LauncherField::RamPattern)
        .expect("no fixed RAM pattern row on Memory page");
        let row_y = launcher_row_y(rect, index);
        let box_rect = launcher_text_rect(rect, row_y, LauncherField::RamPattern);
        assert_eq!(
            ui.control_at((box_rect.x as i32 + 4, box_rect.y as i32 + 4)),
            Some(UiControl::LauncherRamPatternEdit)
        );
    }

    #[test]
    fn debugger_controls_hit_test_and_entry_edits() {
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(DebuggerPanel::new())),
        };
        let rect = panel_rect(ui.panel.as_ref().unwrap());
        let tab = debug_tab_rect(rect, 3);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2)),
            Some(UiControl::DebugTab(DebugTab::Video))
        );
        let tab = debug_tab_rect(rect, 4);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2)),
            Some(UiControl::DebugTab(DebugTab::Audio))
        );
        let tab = debug_tab_rect(rect, 6);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2)),
            Some(UiControl::DebugTab(DebugTab::IoMap))
        );
        let tab = debug_tab_rect(rect, 7);
        assert_eq!(
            ui.control_at((tab.x as i32 + 2, tab.y as i32 + 2)),
            Some(UiControl::DebugTab(DebugTab::Break))
        );
        // All eight tabs fit inside the panel.
        let last = debug_tab_rect(rect, 7);
        assert!(last.x + last.w <= rect.x + rect.w);
        let (control, step) = debug_button_rects(rect)[1];
        assert_eq!(control, UiControl::DebugStep);
        assert_eq!(
            ui.control_at((step.x as i32 + 2, step.y as i32 + 2)),
            Some(UiControl::DebugStep)
        );

        // Break-tab toggle buttons hit-test only while that tab is active.
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Break;
        let ui_break = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        let (control, toggle) = break_tab_button_rects(rect)[0];
        assert_eq!(control, UiControl::DebugBreakToggle);
        let pos = (toggle.x as i32 + 2, toggle.y as i32 + 2);
        assert_eq!(ui_break.control_at(pos), Some(UiControl::DebugBreakToggle));
        // On another tab the same position is just panel body.
        assert_eq!(ui.control_at(pos), Some(UiControl::PanelBody));

        // Audio-tab mute buttons hit-test only while the Audio tab is active.
        let mut panel = DebuggerPanel::new();
        panel.tab = DebugTab::Audio;
        let ui_audio = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        let (control, mute0) = audio_tab_button_rects(rect)[0];
        assert_eq!(control, UiControl::DebugAudioMute(0));
        let pos = (mute0.x as i32 + 2, mute0.y as i32 + 2);
        assert_eq!(ui_audio.control_at(pos), Some(UiControl::DebugAudioMute(0)));
        // The CD mute is the fifth (index 4) button.
        let (cd_control, cd_mute) = audio_tab_button_rects(rect)[4];
        assert_eq!(cd_control, UiControl::DebugAudioMute(4));
        let cd_pos = (cd_mute.x as i32 + 2, cd_mute.y as i32 + 2);
        assert_eq!(
            ui_audio.control_at(cd_pos),
            Some(UiControl::DebugAudioMute(4))
        );
        // The line-mixed source slots continue below the CD row (last slot
        // = AUDIO_MAX_ROWS - 1); the click dispatcher decides whether a
        // fitted source actually occupies one.
        let (last_control, last_mute) = audio_tab_button_rects(rect)[AUDIO_MAX_ROWS - 1];
        assert_eq!(last_control, UiControl::DebugAudioMute(AUDIO_MAX_ROWS - 1));
        let last_pos = (last_mute.x as i32 + 2, last_mute.y as i32 + 2);
        assert_eq!(
            ui_audio.control_at(last_pos),
            Some(UiControl::DebugAudioMute(AUDIO_MAX_ROWS - 1))
        );
        // On another tab that position does not resolve to a mute.
        assert_eq!(ui.control_at(pos), Some(UiControl::PanelBody));

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

    /// A Library page with a few games in it, for the previews.
    #[cfg(feature = "game-library")]
    fn library_preview_state() -> LauncherState {
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::WhdloadLibrary;

        state.library.db.set_known(vec![
            crate::gamelib::Known {
                file: "GoldenAxe_v1.5_0017.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Golden Axe".to_string(),
                    year: Some("1990".to_string()),
                    publisher: Some("Virgin".to_string()),
                    // Long enough that before the two-line cap it ran down
                    // over the Run button and off the panel.
                    developer: Some(
                        "Adventuresoft UK Ltd - Teoman Irmak, Matt Ellis, Antony M. Scott, \
                         Graham Lilley, Alan Bridgman, Brian Howarth, Richard Turner"
                            .to_string(),
                    ),
                    players: Some("1 - 2 (2)".to_string()),
                    front_sha1: std::env::var("WHDLOAD_COVER_SHA1").ok(),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "JamesPond2_v2.0_AGA_1354.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "James Pond 2: Codename RoboCod".to_string(),
                    year: Some("1991".to_string()),
                    publisher: Some("Millennium".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            // Two releases of one game, which is what the Version row is
            // for: nothing else tells them apart in the list.
            crate::gamelib::Known {
                file: "CannonFodder2_v1.12_Fr_2578.zip".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Cannon Fodder 2".to_string(),
                    year: Some("1994".to_string()),
                    publisher: Some("Virgin".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "CannonFodder2_v1.11_0104.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Cannon Fodder 2".to_string(),
                    year: Some("1994".to_string()),
                    publisher: Some("Virgin".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "SimCity_v1.0_2193.lha".to_string(),
                game: None,
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Lotus2_v1.2_0451.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Lotus Turbo Challenge 2".to_string(),
                    year: Some("1991".to_string()),
                    publisher: Some("Gremlin".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Turrican2_v2.1_1120.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Turrican II".to_string(),
                    year: Some("1991".to_string()),
                    publisher: Some("Rainbow Arts".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "SensibleSoccer_v1.1_0788.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Sensible Soccer".to_string(),
                    year: Some("1992".to_string()),
                    publisher: Some("Renegade".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Superfrog_v1.0_0233.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Superfrog".to_string(),
                    year: Some("1993".to_string()),
                    publisher: Some("Team17".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "ChaosEngine_v2.0_0912.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "The Chaos Engine".to_string(),
                    year: Some("1993".to_string()),
                    publisher: Some("Renegade".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Xenon2_v1.0_0355.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Xenon 2: Megablast".to_string(),
                    year: Some("1989".to_string()),
                    publisher: Some("Image Works".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Turrican_v1.3_0087.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Turrican".to_string(),
                    year: Some("1990".to_string()),
                    publisher: Some("Rainbow Arts".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Lemmings_v1.1_0621.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Lemmings".to_string(),
                    year: Some("1991".to_string()),
                    publisher: Some("Psygnosis".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "Pinball_Dreams_v1.0_0498.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Pinball Dreams".to_string(),
                    year: Some("1992".to_string()),
                    publisher: Some("21st Century".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
            crate::gamelib::Known {
                file: "SpeedBall2_v1.2_0733.lha".to_string(),
                game: Some(crate::gamelib::Game {
                    name: "Speedball 2".to_string(),
                    year: Some("1990".to_string()),
                    publisher: Some("Image Works".to_string()),
                    ..crate::gamelib::Game::default()
                }),
                manual: false,
                slave_sha1: None,
            },
        ]);
        state
            .library
            .db
            .toggle_favourite("GoldenAxe_v1.5_0017.lha", "Golden Axe");
        // One whose package is not in the library, which is what the
        // Remove tick beside it is for.
        state
            .library
            .db
            .toggle_favourite("Deleted_v1.0_0001.lha", "A Deleted Game");
        // More than the box holds, so the preview covers a favourites list
        // with its scroll arrows up rather than only the short case.
        for (file, name) in [
            ("Lotus2_v1.2_0451.lha", "Lotus Turbo Challenge 2"),
            ("Turrican2_v2.1_1120.lha", "Turrican II"),
            ("SensibleSoccer_v1.1_0788.lha", "Sensible Soccer"),
            ("Superfrog_v1.0_0233.lha", "Superfrog"),
            ("ChaosEngine_v2.0_0912.lha", "The Chaos Engine"),
            ("Xenon2_v1.0_0355.lha", "Xenon 2: Megablast"),
        ] {
            state.library.db.toggle_favourite(file, name);
        }
        state.library.db_loaded = true;
        let want_art = std::env::var("WHDLOAD_COVERS").is_ok_and(|covers| {
            state.library.covers = crate::gamelib::Covers::new(covers.into());
            true
        });
        // A folder of packages, so the list has something in it. The store
        // above supplies the metadata; this is only what is on the disk.
        if let Ok(games) = std::env::var("WHDLOAD_GAMES") {
            state
                .setup
                .set_path(LauncherField::WhdloadGames, games.into());
            state.refresh_library(std::path::Path::new("/nonexistent"));
        }
        // Land on the game the preview has art and a full set of metadata
        // for, rather than on whatever sorts first: an empty art frame and
        // three filled rows is the case the other previews cover.
        let golden = state
            .library
            .games
            .entries()
            .iter()
            .position(|entry| entry.game.as_ref().is_some_and(|g| g.name == "Golden Axe"));
        if let Some(at) = golden {
            state.select_library_game(at);
        }
        // The window loop asks for the selected game's art each frame
        // and draws it when it lands; a preview renders once, so it
        // waits for the picture rather than rendering the empty box.
        if want_art {
            state.poll_library_covers();
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if state.poll_library_covers() {
                    break;
                }
            }
        }
        state
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn a_version_runs_to_two_lines_and_no_further() {
        // A package name is longer than the column and both ends matter:
        // which game at the front, which release at the back.
        let name = "CannonFodder2_v1.11_0104.lha";
        let column = LIBRARY_COVER + 2 * LIBRARY_COVER_BEZEL;
        let lines = wrap_to_width(name, column);
        assert!(lines.len() <= LIBRARY_VERSION_LINES, "{lines:?}");
        assert_eq!(lines.concat(), name, "the whole name should be shown");

        // Two releases stay distinguishable across the wrap, which is the
        // point of showing it at all.
        let other = wrap_to_width("CannonFodder2_v1.12_Fr_2578.zip", column);
        assert_ne!(lines, other);

        // The editor stops where the page stops showing.
        assert_eq!(library_version_max(), 34);
        assert!(name.chars().count() <= library_version_max());
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn cover_art_keeps_its_shape_inside_the_box() {
        let box_rect = Rect {
            x: 100,
            y: 50,
            w: 120,
            h: 130,
        };

        // A cover is portrait: it fills the height and is centred across.
        let at = fit_within(600, 800, box_rect).expect("fits");
        assert_eq!((at.w, at.h), (97, 130));
        assert_eq!(at.y, 50, "a portrait cover should fill the height");
        assert!(at.x >= box_rect.x && at.x + at.w <= box_rect.x + box_rect.w);
        // Centred to the pixel the odd margin allows.
        let (left, right) = (at.x - box_rect.x, box_rect.w - at.w - (at.x - box_rect.x));
        assert!(left.abs_diff(right) <= 1, "off centre: {left} and {right}");

        // A landscape one fills the width instead, with the margin above
        // and below.
        let at = fit_within(800, 400, box_rect).expect("fits");
        assert_eq!((at.w, at.h), (120, 60));
        assert_eq!(at.x, 100);
        let (top, bottom) = (at.y - box_rect.y, box_rect.h - at.h - (at.y - box_rect.y));
        assert!(top.abs_diff(bottom) <= 1, "off centre: {top} and {bottom}");

        // Square art in a not-quite-square box still keeps its shape.
        let at = fit_within(64, 64, box_rect).expect("fits");
        assert_eq!(at.w, at.h);

        // A picture far wider than it is tall does not collapse to nothing,
        // and one with no pixels is not drawn at all.
        assert_eq!(fit_within(4000, 1, box_rect).expect("fits").h, 1);
        assert!(fit_within(0, 10, box_rect).is_none());
        assert!(fit_within(10, 0, box_rect).is_none());
        assert!(fit_within(10, 10, Rect { w: 0, ..box_rect }).is_none());
    }

    #[test]
    fn panels_render_into_their_rects() {
        use super::super::window::{texture_height, texture_width};

        let scale = 1;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let save_at = |frame: &[u8], name: &str, w: usize, h: usize| {
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
        // Almost every preview is drawn at the base scale into the same
        // frame size, so most callers need say nothing about it.
        let save = |frame: &[u8], name: &str| save_at(frame, name, w, h);
        let panel_has_title_bar = |frame: &[u8], panel: &Panel| {
            let rect = panel_rect(panel);
            let probe = ((rect.y + 10) * w + rect.x + 4) * 4;
            let pixel = &frame[probe..probe + 4];
            pixel == PANEL_TITLE_BG.to_le_bytes()
        };

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::About),
        };
        let data = PanelViewData::About(crate::video::about::AboutView {
            machine_lines: vec![
                "Machine: A1200".to_string(),
                "CPU: M68EC020 @ 14 MHz".to_string(),
                "Chipset: AGA (Alice/Lisa, PAL)".to_string(),
                "RAM: 2048K chip, 4096K fast".to_string(),
                "ROM: system v3.1 a1200 release image path rom".to_string(),
                "Floppy drives: 1".to_string(),
            ],
            // Deep into the entrance so the snapshot shows the settled page.
            elapsed_ms: 60_000,
            machine_fitted: true,
        });
        draw(&mut frame, scale, &ui, None, Some(&data));
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "about");

        // Mid-entrance: the title half printed, the wave half drawn in.
        // The entrance is a pure function of elapsed time, so comparing
        // against the just-opened page makes deterministic assertions:
        // by mid-entrance the title's first slot has its settled letter
        // and wave columns stand on the left, while the strip's right
        // edge is still untouched -- the reveal front has not reached it.
        let about_at = |elapsed_ms: u64| {
            let mut frame = vec![0u8; w * h * 4];
            let data = PanelViewData::About(crate::video::about::AboutView {
                machine_lines: vec![crate::config::ABOUT_PLACEHOLDER_LINE.to_string()],
                elapsed_ms,
                machine_fitted: false,
            });
            draw(&mut frame, scale, &ui, None, Some(&data));
            frame
        };
        let opened = about_at(0);
        let mid = about_at(2_500);
        let region_differs = |a: &[u8], b: &[u8], x0: usize, x1: usize, y0: usize, y1: usize| {
            (y0..y1).any(|y| {
                a[(y * w + x0) * 4..(y * w + x1) * 4] != b[(y * w + x0) * 4..(y * w + x1) * 4]
            })
        };
        let rect = panel_rect(&Panel::About);
        let title_y = rect.y + TITLE_H + 14;
        let slot0 = rect.x + (rect.w - 240) / 2;
        assert!(
            region_differs(&mid, &opened, slot0, slot0 + 24, title_y, title_y + 24),
            "title's first letter should have settled by mid-entrance"
        );
        let base = rect.y + rect.h - 8;
        assert!(
            region_differs(&mid, &opened, rect.x, rect.x + rect.w / 4, base - 8, base),
            "wave columns should have arrived on the left by mid-entrance"
        );
        assert!(
            !region_differs(
                &mid,
                &opened,
                rect.x + rect.w - 24,
                rect.x + rect.w,
                base - 60,
                base
            ),
            "the reveal front should not have reached the right edge yet"
        );
        save(&mid, "about-opening");

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Shortcuts),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            None,
            Some(&PanelViewData::Shortcuts),
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "shortcuts");

        let mut frame = vec![0u8; w * h * 4];
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
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
        draw(&mut frame, scale, &ui, Some(UiControl::DropDrive(1)), None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Calibration(session)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::CalCancel),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::InputMap(Box::new(map_panel))),
        };
        draw(&mut frame, scale, &ui, Some(UiControl::RemapSave), None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugStep),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugRegToggle),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugWaveArm),
            Some(&data),
        );
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        // The Arm button must be enabled: its entry spec parses.
        assert!(crate::waveform::parse_wave_args("PC=C033C2 2F".split_whitespace()).is_ok());
        save(&frame, "debugger-waveform");

        // Audio tab: the four Paula channels plus every line-mixed source
        // row (CD, MIDI synth, Toccata, MHI), with representative state,
        // mute buttons (AUD2 and MHI shown muted), and synthetic scope
        // traces.
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
        let extras = vec![
            AudioExtraRow {
                kind: AudioExtraKind::Cd,
                row: AudioRowView {
                    text: vec![
                        DbgLine::hilit("CD-DA  playing"),
                        DbgLine::plain("  peak  72"),
                    ],
                    muted: false,
                    scope: wave(72.0, 4.0),
                },
            },
            AudioExtraRow {
                kind: AudioExtraKind::Synth,
                row: AudioRowView {
                    text: vec![
                        DbgLine::hilit("MIDI  MT-32  sounding"),
                        DbgLine::plain("  peak  58"),
                    ],
                    muted: false,
                    scope: wave(58.0, 5.0),
                },
            },
            AudioExtraRow {
                kind: AudioExtraKind::Toccata,
                row: AudioRowView {
                    text: vec![
                        DbgLine::hilit("Toccata  playing"),
                        DbgLine::plain("  44100 Hz 16-bit stereo  FIFO  612/1024"),
                    ],
                    muted: false,
                    scope: wave(84.0, 7.0),
                },
            },
            AudioExtraRow {
                kind: AudioExtraKind::Mhi,
                row: AudioRowView {
                    text: vec![
                        DbgLine::hilit("MHI  playing  44100 Hz"),
                        DbgLine::plain("  queue 3  vol 100  pan 50  B/M/T 50/50/50"),
                    ],
                    muted: true,
                    scope: wave(66.0, 11.0),
                },
            },
        ];
        let audio = AudioScopeView {
            header,
            channels,
            extras,
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugAudioMute(0)),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(&mut frame, scale, &ui, None, Some(&data));
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Debugger(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::DebugPlaneToggle(0)),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::FrameAnalyzer(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::AnalyzerUnderlay),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::FrameAnalyzer(panel)),
        };
        draw(
            &mut frame,
            scale,
            &ui,
            Some(UiControl::AnalyzerTab(AnalyzerTab::Memory)),
            Some(&data),
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Console(console)),
        };
        draw(&mut frame, scale, &ui, None, None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, Some(UiControl::LauncherRun), None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
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
                false,
                false,
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

        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        state.setup.cycle(LauncherField::ParallelDevice, true); // None -> Printer
        state.setup.cycle(LauncherField::ParallelDevice, true); // Printer -> Sampler
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-io-ports");

        // The CPU tab on the default (68000) machine: the JIT accelerator
        // row is greyed with its "needs 68020+" reason instead of a toggle.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::Cpu;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-cpu");

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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-printer");

        // I/O Ports with the serial port dialling out: the Connect address
        // box the tcp-connect mode brings with it, mid-edit so the caret and
        // the highlight show too.
        #[cfg(feature = "midi")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = LauncherTab::IoPorts;
            while state.setup.serial_mode() != crate::config::SerialMode::TcpConnect {
                state.setup.cycle(LauncherField::SerialMode, true);
            }
            state.begin_edit_serial_addr(LauncherField::SerialConnect);
            for c in "bbs.example.com:1337".chars() {
                state.edit_push(c);
            }
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-serial-tcp");
        }

        // The Host Folder sub-page reached from the Storage tab.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::HostFs;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-host-mounts");

        // The WHDLoad sub-page reached from the Storage tab, with a game
        // chosen so the full host path shows.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::Whdload;
        state.setup.set_path(
            LauncherField::WhdloadGame,
            std::path::PathBuf::from("/Users/me/Amiga/whdload/Turrican.lha"),
        );
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-whdload");

        // The ROM tab: each path row with the identification of the image on
        // it greyed underneath.
        let mut frame = vec![0u8; w * h * 4];
        let mut setup = launcher::MachineSetup::default();
        setup.select_model(Some(MachineModel::Cd32));
        setup.set_path(
            LauncherField::Rom,
            std::path::PathBuf::from("kick40060.CD32"),
        );
        setup.set_path(
            LauncherField::ExtendedRom,
            std::path::PathBuf::from("cd32ext.rom"),
        );
        let mut state = LauncherState::new(setup);
        state.tab = LauncherTab::Rom;
        state.set_rom_note_for_test(LauncherField::Rom, "Kickstart 3.1 (40.60) CD32");
        state.set_rom_note_for_test(LauncherField::ExtendedRom, "CD32 extended ROM (40.60)");
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        assert!(panel_has_title_bar(&frame, ui.panel.as_ref().unwrap()));
        save(&frame, "launcher-rom");

        // The Library page with the A-Z shortcut row up, since that is the
        // part whose fit across the width is worth looking at.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = library_preview_state();
            // The row appears on the size of the list, so the preview
            // needs a list that reaches it.
            let mut more: Vec<crate::gamelib::Known> = state.library.db.known().to_vec();
            for (at, name) in [
                "Agony",
                "Battle Squadron",
                "Deluxe Galaga",
                "Elite",
                "Frontier",
                "Hired Guns",
                "It Came from the Desert",
                "Kick Off 2",
                "Moonstone",
                "North & South",
                "Obitus",
                "Rick Dangerous",
                "Utopia",
                "Walker",
                "Xenon",
                "Yo! Joe!",
            ]
            .into_iter()
            .enumerate()
            {
                more.push(crate::gamelib::Known {
                    file: format!("filler{at}.lha"),
                    game: Some(crate::gamelib::Game {
                        name: name.to_string(),
                        ..crate::gamelib::Game::default()
                    }),
                    manual: false,
                    slave_sha1: None,
                });
            }
            state.library.db.set_known(more);
            state.library.games =
                crate::gamelib::Library::known(std::path::Path::new("/games"), &state.library.db);
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            // With a letter under the pointer: the hover is the part that
            // has to read at this size.
            let over = launcher::az_bucket_of("Golden Axe");
            let hover = Some(UiControl::LauncherLibraryJump(over));
            draw(&mut frame, scale, &ui, hover, None);
            save(&frame, "launcher-whdload-library-az");
        }

        // The Library page, with WHDLoad beside it in the strip and
        // a couple of games standing in for a real folder.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let state = library_preview_state();
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-whdload-library");
        }

        // The sign-in dialog, over the Configuration page it is opened
        // from, with something typed into both boxes.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = LauncherTab::Whdload;
            let mut login = launcher::LoginDialog {
                user: "hobbo91".to_string(),
                ..Default::default()
            };
            login.focus_on(launcher::LoginField::Pass);
            for c in "not-a-real-password".chars() {
                login.insert(c);
            }
            // Part-way back through it, which is what the caret is for.
            for _ in 0..8 {
                login.caret_move(launcher::CaretMove::Left);
            }
            state.login = Some(login);
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-openretro-login");
        }

        // The metadata editor, over the Library page it is opened from.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = library_preview_state();
            state.open_meta_editor();
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-meta-editor");
        }

        // Both lists run to their far end, where the arrows swap over:
        // the one pointing back into the list lights and the one pointing
        // off it greys.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let state = library_preview_state();
            let mut ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            let rect = launcher_panel_rect(&ui).expect("the launcher is up");
            if let Some(Panel::Launcher(state)) = ui.panel.as_mut() {
                let whdload_entry = state.setup.whdload_enabled();
                state.scroll_library(isize::MAX, library_visible_rows(rect, whdload_entry));
                state.scroll_favourites(isize::MAX, library_favourite_rows(rect, whdload_entry));
            }
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-whdload-library-scrolled");
        }

        // An empty Library page: nothing scanned yet, so Scan is greyed
        // and the art frame is drawn at the size it reserves.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = LauncherTab::WhdloadLibrary;
            state.library.db_loaded = true;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-whdload-library-empty");
        }

        // One of two releases of the same game: the Version row says which,
        // since nothing else in the list does.
        #[cfg(feature = "game-library")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = library_preview_state();
            state.select_library_game(0);
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-whdload-library-version");
        }

        // The Storage tab, whose six sub-page links wrap onto a second row.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::Storage;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-storage");

        // The Create Image workshop: its two pages, and the geometry editor
        // behind the hard-drive one.
        for (tab, name) in [
            (LauncherTab::CreateFloppy, "launcher-new-floppy"),
            (LauncherTab::CreateHard, "launcher-new-hard"),
            (LauncherTab::CreateGeometry, "launcher-new-geometry"),
        ] {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = tab;
            state.workshop.geometry_custom = true;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, name);
        }

        // The Boot Priority sub-page: an A1200 with two IDE drives -- the master
        // bootable at 0, the slave with its Bootable box cleared -- and one
        // SCSI unit carrying a disk of its own.
        let mut frame = vec![0u8; w * h * 4];
        let mut setup = launcher::MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        // A controller with one unit filled: the page lists that unit and
        // leaves the empty six out.
        setup.cycle(LauncherField::ScsiController, true);
        setup.set_path(LauncherField::IdeMaster, std::path::PathBuf::from("wb.hdf"));
        setup.set_drive_bootpri(LauncherField::IdeMasterBoot, Some(0));
        setup.set_path(
            LauncherField::IdeSlave,
            std::path::PathBuf::from("games.hdf"),
        );
        setup.toggle_drive_boot(LauncherField::IdeSlaveBoot);
        setup.set_path(
            LauncherField::ScsiUnit0,
            std::path::PathBuf::from("work.hdf"),
        );
        let mut state = LauncherState::new(setup);
        state.tab = LauncherTab::BootPriority;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-boot-priority");

        // The runtime menu, opened over a running machine: the top level,
        // with a category open beside it.
        let mut frame = vec![0u8; w * h * 4];
        let slots: [Option<String>; menu::SAVE_SLOTS] =
            std::array::from_fn(|i| (i == 2).then(|| "2026/07/31 14:05".to_string()));
        let devices = ["Built-in Output".to_string()];
        let none: [String; 0] = [];
        let rows = menu::build(&menu::MenuState {
            player: false,
            player_save_states: false,
            paused: false,
            fullscreen: false,
            status_bar_hidden: false,
            bezel: crate::config::BezelStyle::None,
            perf_overlay: false,
            warp: false,
            warp_speed: WarpSpeed::Max,
            rewind: false,
            recording: false,
            input_recording: false,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            keyboard_panel: false,
            port_devices: [
                crate::bus::PortDevice::Mouse,
                crate::bus::PortDevice::Joystick,
            ],
            pixel_aspect: PixelAspect::Tv,
            scaling: crate::config::DisplayScaling::Smooth,
            tv_centre: crate::config::TvCentre::default(),
            tv_centre_applies: true,
            shader: crate::config::ShaderKind::None,
            shader_strength: 1.0,
            custom_shader_available: false,
            tint: crate::config::Tint::None,
            menu_scale: crate::config::MenuScale::Normal,
            floppy_speed: 100,
            floppy_speed_applies: true,
            audio_filter: crate::config::AudioFilterMode::Auto,
            audio_output: menu::AudioOutputChoice::Default,
            audio_devices: &devices,
            midi_in: "",
            midi_out: "",
            midi_inputs: &none,
            midi_outputs: &none,
            mt32_available: false,
            mt32_selected: false,
            mt32_attached: false,
            mt32_input: false,
            mt32_panel: false,
            mt32_control_rom: None,
            mt32_pcm_rom: None,
            csynth_available: false,
            csynth_attached: false,
            csynth_panel: false,
            csynth_mt32_mode: "auto",
            csynth_custom_font: false,
            mt32_lcd: crate::config::Mt32Lcd::Oled,
            sampler_input: "",
            sampler_inputs: &none,
            sampler_gain: 0.0,
            save_slots: &slots,
        });
        let mut nav = menu::MenuNav::default();
        nav.point_at(5);
        nav.descend(&rows);
        let ui = UiState {
            menu_open: true,
            menu_rows: rows,
            menu_nav: nav,
            ..Default::default()
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "menu-open");

        // The same menu at 2x, which has to stay inside the display.
        let mut frame = vec![0u8; w * h * 4];
        crate::video::set_menu_scale(crate::config::MenuScale::Large);
        draw(&mut frame, scale, &ui, None, None);
        let levels = ui.menu_nav.levels(&ui.menu_rows);
        for column in menu_columns(&levels, &ui.menu_nav) {
            assert!(
                column.x + column.w <= texture_width(1) && column.y + column.h <= present_height(),
                "2x menu column {column:?} leaves the display"
            );
        }
        crate::video::set_menu_scale(crate::config::MenuScale::Normal);
        save(&frame, "menu-open-2x");

        // A/V & Emu: the Audio category (the default landing), with the
        // Audio / Video / Emulation nav buttons at the top, Audio highlighted.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::AvAudio;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-av-audio");

        // The Video category, reached from the same nav row.
        let mut frame = vec![0u8; w * h * 4];
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::AvVideo;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
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
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-floppy");

        // The Floppy tab with DF1 turned over to a real drive: its media row
        // shows the interface and a Configure button, and its Physical drive box
        // is ticked while DF0 stays an ordinary image drive.
        let mut frame = vec![0u8; w * h * 4];
        let mut setup = launcher::MachineSetup::default();
        while setup.value_label(LauncherField::FloppyDrives) != "2" {
            setup.cycle(LauncherField::FloppyDrives, true);
        }
        setup.set_path(
            LauncherField::Df0Image,
            std::path::PathBuf::from("workbench.adf"),
        );
        setup.set_drive_bridged(1, true);
        let mut state = LauncherState::new(setup);
        state.tab = LauncherTab::Floppy;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        draw(&mut frame, scale, &ui, None, None);
        save(&frame, "launcher-floppy-bridge");

        // A drive row holding a real disk must not swallow the rest of the
        // panel. The Unmount button replaces that row's own two buttons and
        // nothing else: Run, Defaults, Load, Save and the page links all kept
        // working before, and an early return here quietly killed every one
        // of them until the disk was cleared.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut setup = launcher::MachineSetup::default();
            setup.select_model(Some(crate::config::MachineModel::A1200));
            setup.set_host_disks_for_test(vec![launcher::HostDiskRow {
                id: "disk4".to_string(),
                fingerprint: None,
                volume: "SanDisk".to_string(),
                size: "31.9 GB".to_string(),
                mounted: Vec::new(),
                writable: false,
                attach: None,
            }]);
            setup.select_host_disk(0);
            setup.mount_host_disks().expect("A1200 has IDE");
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::Storage;
            let panel = Panel::Launcher(Box::new(state));
            let rect = panel_rect(&panel);
            let Panel::Launcher(state) = &panel else {
                unreachable!()
            };
            for (control, button) in launcher_action_rects(rect) {
                let centre = (
                    (button.x + button.w / 2) as i32,
                    (button.y + button.h / 2) as i32,
                );
                assert_eq!(
                    launcher_control_at(rect, state, centre),
                    Some(control),
                    "a mounted disk must not block {control:?}"
                );
            }
        }

        // A preview of the Paths page and of the Save menu, for eyes.
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = LauncherTab::AvPaths;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-paths");

            // And with two rows given directories of their own, which is
            // the only state in which a Reset button exists. `set_path` on
            // a Paths row adopts into the process-wide store that
            // paths::tests assert against, hence the guard.
            let _guard = crate::paths::adopted_store_lock();
            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.tab = LauncherTab::AvPaths;
            state.setup.set_path(
                LauncherField::PathsBase,
                std::path::PathBuf::from("/Volumes/AMIGA"),
            );
            state.setup.set_path(
                LauncherField::PathsScreenshots,
                std::path::PathBuf::from("/Users/someone/Pictures/Amiga screenshots"),
            );
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-paths-set");

            // Drawn at a scale of more than one, which is where anything
            // handing an unscaled rect to a scaled draw shows up: at scale
            // one the two are the same and the mistake is invisible.
            let big = 2;
            let (bw, bh) = (texture_width(big), texture_height(big));
            let mut frame = vec![0u8; bw * bh * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.save_dialog = true;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            // With the pointer on the button whose description is longest,
            // which is the one that has to fit.
            draw(
                &mut frame,
                big,
                &ui,
                Some(UiControl::LauncherSaveDefault),
                None,
            );
            save_at(&frame, "launcher-save-menu", bw, bh);

            let mut frame = vec![0u8; w * h * 4];
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.confirm_reset = true;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: menu::MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-confirm-reset");
        }

        // A Paths row must offer exactly the buttons it draws. A Reset
        // that is not there but still answers would put a row back to
        // inheriting on a click meant for nothing at all, and on the base
        // -- where Browse and Reset swap places rather than sitting side
        // by side -- the two would land on each other's rectangles.
        {
            // `set_path` on a Paths row adopts into the process-wide store
            // that paths::tests assert against.
            let _guard = crate::paths::adopted_store_lock();
            let probe = |set: bool, field: LauncherField| {
                let mut setup = launcher::MachineSetup::default();
                if set {
                    setup.set_path(field, std::path::PathBuf::from("/probe/dir"));
                }
                let mut state = LauncherState::new(setup);
                state.tab = LauncherTab::AvPaths;
                let panel = Panel::Launcher(Box::new(state));
                let rect = panel_rect(&panel);
                let row = launcher::rows(
                    LauncherTab::AvPaths,
                    Default::default(),
                    Default::default(),
                    false,
                    false,
                )
                .iter()
                .position(|r| r.field == field)
                .expect("the field has a row");
                let row_y = launcher_row_y(rect, row) + launcher_nav_block_h(LauncherTab::AvPaths);
                let (browse, reset) = launcher_path_rects(rect, row_y);
                let at = |r: Rect| {
                    panel_control_at(&panel, ((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32))
                };
                (
                    at(browse) == Some(UiControl::LauncherBrowse(field)),
                    at(reset) == Some(UiControl::LauncherClear(field)),
                )
            };
            // An ordinary row: Browse always, Reset only once it was set.
            assert_eq!(probe(false, LauncherField::PathsScreenshots), (true, false));
            assert_eq!(probe(true, LauncherField::PathsScreenshots), (true, true));
            // The base swaps them.
            assert_eq!(probe(false, LauncherField::PathsBase), (true, false));
            assert_eq!(probe(true, LauncherField::PathsBase), (false, true));
        }

        // A Clear with nothing behind it takes no clicks. The drive rows
        // and the floppy rows draw their own buttons rather than going
        // through the Path arm, so each stands trial here: empty, the
        // button is greyed and dead; with an image chosen, it answers.
        {
            let probe = |set: bool, tab: LauncherTab, field: LauncherField, kind: RowKind| {
                let mut setup = launcher::MachineSetup::default();
                // A machine that has all the rows: the default model
                // carries no IDE port, and a row that does not apply
                // takes no clicks whatever its buttons say.
                setup.select_model(Some(MachineModel::A1200));
                if set {
                    setup.set_path(field, std::path::PathBuf::from("/probe/disk.img"));
                }
                let idx = launcher::rows(tab, Default::default(), Default::default(), false, false)
                    .iter()
                    .filter(|r| !setup.row_hidden(r.field))
                    .position(|r| r.field == field && r.kind == kind)
                    .expect("the field has a visible row");
                let mut state = LauncherState::new(setup);
                state.tab = tab;
                let panel = Panel::Launcher(Box::new(state));
                let rect = panel_rect(&panel);
                let nav = if tab.has_top_nav() {
                    launcher_nav_block_h(tab)
                } else {
                    0
                };
                let row_y = launcher_row_y(rect, idx) + nav;
                let (_, clear) = launcher_path_rects(rect, row_y);
                let got = panel_control_at(
                    &panel,
                    (
                        (clear.x + clear.w / 2) as i32,
                        (clear.y + clear.h / 2) as i32,
                    ),
                );
                got == Some(UiControl::LauncherClear(field))
            };
            for (tab, field, kind) in [
                (
                    LauncherTab::Floppy,
                    LauncherField::Df0Image,
                    RowKind::FloppyMedia,
                ),
                (
                    LauncherTab::Storage,
                    LauncherField::IdeMaster,
                    RowKind::Drive,
                ),
            ] {
                assert!(!probe(false, tab, field, kind), "{field:?} empty must grey");
                assert!(probe(true, tab, field, kind), "{field:?} set must answer");
            }
        }

        // The confirm over Reset default answers every click on the panel:
        // Yes only on its own button, and anything else -- Run included --
        // cancels. A question about deleting something must not be
        // answerable by a click that missed.
        {
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.confirm_reset = true;
            let panel = Panel::Launcher(Box::new(state));
            let rect = panel_rect(&panel);
            let centre = |r: Rect| ((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
            let (yes, cancel) = launcher_confirm_button_rects(rect);
            assert_eq!(
                panel_control_at(&panel, centre(yes)),
                Some(UiControl::LauncherConfirmReset)
            );
            // Named places again, not the panel's centre: that sits inside
            // the dialog and would land on a button if it ever resized.
            let dialog = launcher_confirm_rect(rect);
            let title = Rect {
                x: dialog.x,
                y: dialog.y,
                w: dialog.w,
                h: TITLE_H,
            };
            let [load, _, _, run] = launcher_action_rects(rect);
            for elsewhere in [cancel, title, load.1, run.1] {
                assert_eq!(
                    panel_control_at(&panel, centre(elsewhere)),
                    Some(UiControl::LauncherCancelReset),
                    "a click off Yes should cancel"
                );
            }
            // And here too the gadget answers only for itself.
            assert_eq!(
                panel_control_at(&panel, centre(close_button_rect(dialog))),
                Some(UiControl::LauncherDialogClose)
            );
            // The two dialogs are the same height, so they read as one
            // window asking two things rather than two boxes.
            assert_eq!(dialog.h, launcher_save_dialog_rect(rect).h);
        }

        // The Save dialog: every button reachable, nothing under it
        // reachable while it is up, and anything that is not one of the
        // three -- its close gadget included -- putting it away.
        {
            let mut state = LauncherState::new(launcher::MachineSetup::default());
            state.save_dialog = true;
            let panel = Panel::Launcher(Box::new(state));
            let rect = panel_rect(&panel);
            let dialog = launcher_save_dialog_rect(rect);
            let centre = |r: Rect| ((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
            let items = launcher_save_dialog_rects(rect);
            for (control, item) in items {
                assert_eq!(
                    panel_control_at(&panel, centre(item)),
                    Some(control),
                    "{control:?} is not reachable in the Save dialog"
                );
                assert!(
                    item.x >= dialog.x && item.x + item.w <= dialog.x + dialog.w,
                    "{control:?} is outside the dialog"
                );
                // Readable rather than truncated by the button it sits in,
                // which is what sizes the dialog in the first place.
                let fits = item.w.saturating_sub(8) / font::GLYPH_W;
                let label = launcher_action_label(control);
                assert!(
                    label.chars().count() <= fits,
                    "{label:?} does not fit its button"
                );
            }
            for (a, b) in items.iter().zip(items.iter().skip(1)) {
                assert!(a.1.x + a.1.w <= b.1.x, "the buttons overlap each other");
                assert_eq!(a.1.y, b.1.y, "the buttons should be one row");
            }
            // The close gadget, the dialog's own body below the buttons,
            // and the bar underneath: none of them does anything but put
            // the dialog away. Probed at named places rather than at the
            // panel's centre, which is inside the dialog and moves onto a
            // button the moment the dialog changes height.
            let close = Rect {
                x: dialog.x + dialog.w - 18,
                y: dialog.y + 4,
                w: 12,
                h: 12,
            };
            let body = Rect {
                x: dialog.x,
                y: dialog.y + TITLE_H,
                w: dialog.w,
                h: SAVE_DIALOG_MARGIN,
            };
            let [load, _, _, run] = launcher_action_rects(rect);
            for elsewhere in [body, load.1, run.1] {
                assert_eq!(
                    panel_control_at(&panel, centre(elsewhere)),
                    Some(UiControl::LauncherSave),
                    "a click off the three should only put the dialog away"
                );
            }
            // The gadget answers as itself, and nothing else does. It is
            // drawn lit when the pointer is on it, so sharing a control
            // with "anywhere else" lit it up for every hover in the dialog.
            assert_eq!(
                panel_control_at(&panel, centre(close)),
                Some(UiControl::LauncherDialogClose)
            );
            for elsewhere in [body, load.1, run.1]
                .into_iter()
                .chain(items.map(|(_, item)| item))
            {
                assert_ne!(
                    panel_control_at(&panel, centre(elsewhere)),
                    Some(UiControl::LauncherDialogClose),
                    "something that is not the gadget lights the gadget"
                );
            }
            // Every description fits the two lines reserved for it. The
            // dialog cannot grow to suit a longer one -- resizing under a
            // pointer that is crossing the buttons would move them -- so a
            // sentence that does not fit is silently cut off instead.
            let chars = (dialog.w - 2 * SAVE_DIALOG_MARGIN) / font::GLYPH_W;
            for (control, _) in items {
                let help = save_dialog_help(control);
                assert!(!help.is_empty(), "{control:?} has nothing to say");
                let lines = wrap_text(help, chars, chars);
                assert!(
                    lines.len() <= SAVE_DIALOG_HELP_LINES,
                    "{help:?} wraps to {} lines, and there is room for {}",
                    lines.len(),
                    SAVE_DIALOG_HELP_LINES
                );
            }
            // With the pointer on nothing, the dialog still says what it
            // is for rather than leaving the space empty.
            assert_eq!(
                save_dialog_help(UiControl::LauncherSave),
                save_dialog_help(UiControl::LauncherSaveAs)
            );

            // And with it closed, its buttons are not there to be hit.
            let closed = Panel::Launcher(Box::new(LauncherState::new(
                launcher::MachineSetup::default(),
            )));
            for (control, item) in items {
                assert_ne!(
                    panel_control_at(&closed, centre(item)),
                    Some(control),
                    "{control:?} answers with the dialog closed"
                );
            }
        }

        // The Host Disk page with two disks ticked: the table, the second
        // disk landing beside the first, and a line each saying where they go.
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut setup = launcher::MachineSetup::default();
            setup.set_host_disks_for_test(vec![
                launcher::HostDiskRow {
                    id: "disk4".to_string(),
                    fingerprint: None,
                    volume: "SanDisk Extreme SD".to_string(),
                    size: "31.9 GB".to_string(),
                    mounted: Vec::new(),
                    writable: true,
                    attach: None,
                },
                launcher::HostDiskRow {
                    id: "disk6".to_string(),
                    fingerprint: None,
                    volume: "Kingston DataTraveler".to_string(),
                    size: "3.9 GB".to_string(),
                    mounted: vec!["/Volumes/UNTITLED".to_string()],
                    writable: false,
                    attach: None,
                },
                launcher::HostDiskRow {
                    id: "PhysicalDrive11".to_string(),
                    fingerprint: None,
                    volume: "Generic USB3.0 CRW-SD/MS Multi-Card Reader".to_string(),
                    size: "512 MB".to_string(),
                    mounted: Vec::new(),
                    writable: true,
                    attach: None,
                },
            ]);
            setup.select_model(Some(crate::config::MachineModel::A1200));
            setup.select_host_disk(0);
            setup.select_host_disk(1);
            setup.select_host_disk(2);
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::HostDisk;
            let ui = UiState {
                menu_open: false,
                panel: Some(Panel::Launcher(Box::new(state))),
                ..Default::default()
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-host-disk");

            // The same page with nothing ticked: Mount is greyed, and the
            // page says what it is waiting for.
            let mut frame = vec![0u8; w * h * 4];
            let mut setup = launcher::MachineSetup::default();
            setup.set_host_disks_for_test(vec![launcher::HostDiskRow {
                id: "disk4".to_string(),
                fingerprint: None,
                volume: "SanDisk Extreme SD".to_string(),
                size: "31.9 GB".to_string(),
                mounted: Vec::new(),
                writable: false,
                attach: None,
            }]);
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::HostDisk;
            let ui = UiState {
                menu_open: false,
                panel: Some(Panel::Launcher(Box::new(state))),
                ..Default::default()
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-host-disk-locked");

            // A list longer than the box: the arrows appear, and the window
            // is part way down so both are live.
            let mut frame = vec![0u8; w * h * 4];
            let mut setup = launcher::MachineSetup::default();
            setup.set_host_disks_for_test(
                (0..14)
                    .map(|i| launcher::HostDiskRow {
                        id: format!("disk{i}"),
                        fingerprint: None,
                        volume: format!("Pretend Media {i}"),
                        size: format!("{}.0 GB", i % 9 + 1),
                        mounted: Vec::new(),
                        writable: true,
                        attach: None,
                    })
                    .collect(),
            );
            setup.scroll_host_disks(3, HOST_DISK_VISIBLE_ROWS);
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::HostDisk;
            let ui = UiState {
                menu_open: false,
                panel: Some(Panel::Launcher(Box::new(state))),
                ..Default::default()
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-host-disk-scrolled");

            // Storage, with a real disk on IDE master: the row names the disk
            // and offers Unmount where an image would offer Browse/Clear.
            let mut frame = vec![0u8; w * h * 4];
            let mut setup = launcher::MachineSetup::default();
            // A machine that actually has IDE, so the rows are live.
            setup.select_model(Some(crate::config::MachineModel::A1200));
            setup.set_host_disks_for_test(vec![launcher::HostDiskRow {
                id: "disk4".to_string(),
                fingerprint: None,
                volume: "SanDisk Extreme SD".to_string(),
                size: "31.9 GB".to_string(),
                mounted: Vec::new(),
                writable: false,
                attach: None,
            }]);
            setup.select_host_disk(0);
            setup
                .mount_host_disks()
                .expect("the fixture machine has IDE");
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::Storage;
            let ui = UiState {
                menu_open: false,
                panel: Some(Panel::Launcher(Box::new(state))),
                ..Default::default()
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-storage-host-disk");
        }

        // The FluxBridge settings page reached from Configure, with an
        // interface selected, which is the state its rows are drawn live in.
        // Bridging a bay samples the host, so the row starts at "None" on a
        // machine with nothing plugged in and at the interface on one that
        // has: cycle until a driver is named, since what a test machine has
        // attached is not this test's subject. Bounded because cycling is a
        // fixed-length ring. Only exists in a build with the feature --
        // without it no bay can be bridged, so there is no such page to draw.
        #[cfg(feature = "fluxbridge")]
        {
            let mut frame = vec![0u8; w * h * 4];
            let mut setup = launcher::MachineSetup::default();
            setup.set_drive_bridged(0, true);
            setup.set_bridge_edit_drive(0);
            let mut named = false;
            for _ in 0..8 {
                if setup.value_label(LauncherField::BridgeDevice) != "None" {
                    named = true;
                    break;
                }
                setup.cycle(LauncherField::BridgeDevice, true);
            }
            assert!(named, "the row can name an interface to draw");
            let mut state = LauncherState::new(setup);
            state.tab = LauncherTab::FluxBridge;
            let ui = UiState {
                menu_open: false,
                panel: Some(Panel::Launcher(Box::new(state))),
                ..Default::default()
            };
            draw(&mut frame, scale, &ui, None, None);
            save(&frame, "launcher-fluxbridge-page");
        }
    }
}

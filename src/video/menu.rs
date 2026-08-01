// SPDX-License-Identifier: GPL-3.0-or-later

//! The pop-up menu's shape: what it offers, and what picking a row does.
//!
//! The menu is a tree. Its top level holds the tools and a handful of
//! categories; a category opens a child list beside it, and a setting with
//! more than two values opens a further list of those values with the current
//! one marked. Only the leaves do anything, which keeps the question "what
//! happens when this is chosen" answerable in one place ([`MenuAction`])
//! rather than spread across the drawing code.
//!
//! The tree is rebuilt each time the menu opens, from the machine as it
//! stands: a serial port with nothing on it contributes no rows, and neither
//! does a parallel port, so a category that would be empty is never offered.

use crate::bus::PortDevice;
use crate::config::JoystickInputMode;
use crate::config::{AudioFilterMode, PixelAspect, ShaderKind, Tint, WarpSpeed};

/// What choosing a leaf does. Everything the menu can do is here, so the
/// window's handler is a single match and the tree carries no behaviour.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    // Tools. Each opens its own window and closes the menu.
    OpenMachineConfig,
    OpenFrameAnalyzer,
    OpenDebugger,
    OpenConsole,
    OpenInputMapping,
    OpenCalibration,
    OpenShortcuts,
    OpenAbout,
    LoadRom,

    // Audio.
    SetAudioOutput(AudioOutputChoice),
    SetAudioFilter(AudioFilterMode),

    // Video.
    SetPixelAspect(PixelAspect),
    SetShader(ShaderKind),
    SetTint(Tint),
    ToggleFullscreen,
    ToggleStatusBar,

    // Input.
    SetPortDevice(usize, PortDevice),
    SetJoystickInput(JoystickInputMode),
    SetAutofire(u8),

    // Serial / parallel, present only when something is on the port.
    SetMidiInput(String),
    SetMidiOutput(String),
    SetSamplerInput(String),
    /// Step the gain by one notch, up (+1) or down (-1).
    StepSamplerGain(i8),

    // Emulation.
    SetFloppySpeed(u16),
    ToggleRewind,

    // Warp.
    ToggleWarp,
    SetWarpLimit(WarpSpeed),

    // Recording.
    ToggleRecord,
    ToggleRecordInput,

    // Save states.
    SaveState,
    LoadState,
    QuickSave(usize),
    QuickLoad(usize),
}

/// Which audio output a row selects. The host's device list is dynamic, so a
/// row names one rather than holding an index that could go stale between the
/// menu being built and a row being chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutputChoice {
    /// Whatever the host calls its default.
    Default,
    Named(String),
    /// No output at all.
    Disabled,
}

/// One row of a menu.
///
/// A row is built by naming it and then adding what it needs -- a value to
/// show on the right, or a reason it cannot be picked -- so adding a setting
/// later is one line here and one arm in the window's handler.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuRow {
    pub label: String,
    /// Shown right-aligned: the value in force, so a category says what it is
    /// set to without being opened.
    pub value: Option<String>,
    /// False for a row that is there to be seen but cannot be chosen -- a
    /// shader with no file behind it, say.
    pub enabled: bool,
    pub kind: MenuRowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuRowKind {
    /// Opens a child list. Drawn with a trailing marker.
    Submenu(Vec<MenuRow>),
    /// Does something and closes the menu.
    Action(MenuAction),
    /// Does something and leaves the menu open, for the rows meant to be
    /// used more than once in a row: a toggle, or a step up or down.
    Live(MenuAction),
    /// Flips something in place. The menu stays open so a run of toggles can
    /// be set without reopening it.
    Toggle { action: MenuAction, on: bool },
    /// One value of a setting, marked when it is the one in force.
    Choice { action: MenuAction, selected: bool },
}

impl MenuRow {
    fn new(label: &str, kind: MenuRowKind) -> Self {
        Self {
            label: label.to_string(),
            value: None,
            enabled: true,
            kind,
        }
    }

    fn submenu(label: &str, children: Vec<MenuRow>) -> Self {
        Self::new(label, MenuRowKind::Submenu(children))
    }

    fn action(label: &str, action: MenuAction) -> Self {
        Self::new(label, MenuRowKind::Action(action))
    }

    fn live(label: &str, action: MenuAction) -> Self {
        Self::new(label, MenuRowKind::Live(action))
    }

    fn toggle(label: &str, action: MenuAction, on: bool) -> Self {
        Self::new(label, MenuRowKind::Toggle { action, on })
    }

    fn choice(label: &str, action: MenuAction, selected: bool) -> Self {
        Self::new(label, MenuRowKind::Choice { action, selected })
    }

    /// Show `value` on the right of the row.
    fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Leave the row visible but unpickable when `available` is false.
    fn available(mut self, available: bool) -> Self {
        self.enabled = available;
        self
    }

    /// Whether picking this row closes the menu. Rows meant to be used
    /// repeatedly leave it open.
    pub fn closes_menu(&self) -> bool {
        matches!(
            self.kind,
            MenuRowKind::Action(_) | MenuRowKind::Choice { .. }
        )
    }

    /// The action this row carries, if it does anything itself.
    pub fn menu_action(&self) -> Option<&MenuAction> {
        match &self.kind {
            MenuRowKind::Action(a) | MenuRowKind::Live(a) => Some(a),
            MenuRowKind::Toggle { action, .. } | MenuRowKind::Choice { action, .. } => Some(action),
            MenuRowKind::Submenu(_) => None,
        }
    }

    /// Whether this row leads somewhere rather than doing something.
    pub fn is_submenu(&self) -> bool {
        matches!(self.kind, MenuRowKind::Submenu(_))
    }

    pub fn children(&self) -> Option<&[MenuRow]> {
        match &self.kind {
            MenuRowKind::Submenu(rows) => Some(rows),
            _ => None,
        }
    }
}

/// Where the menu is open to, and which row the cursor is on.
///
/// One structure drives both pointers and keys: the mouse moves the cursor by
/// hovering and the keys move it by stepping, and everything downstream --
/// what is drawn, what Return picks -- reads the same place. Levels are held
/// as the row index taken at each depth, so the open path survives the tree
/// being rebuilt under it (a device appearing on a port, a slot being
/// written) as long as the shape has not changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuNav {
    /// Row index chosen at each open level, outermost first. Empty means only
    /// the top level is open.
    path: Vec<usize>,
    /// Cursor position within the deepest open level. `None` before the
    /// keyboard or pointer has picked a row.
    cursor: Option<usize>,
}

impl MenuNav {
    /// The rows of the deepest open level, and the levels above it.
    pub fn levels<'a>(&self, root: &'a [MenuRow]) -> Vec<&'a [MenuRow]> {
        let mut levels = vec![root];
        let mut rows = root;
        for &i in &self.path {
            match rows.get(i).and_then(MenuRow::children) {
                Some(children) => {
                    levels.push(children);
                    rows = children;
                }
                None => break,
            }
        }
        levels
    }

    /// The rows the cursor is moving within.
    pub fn current<'a>(&self, root: &'a [MenuRow]) -> &'a [MenuRow] {
        self.levels(root).pop().unwrap_or(root)
    }

    pub fn depth(&self) -> usize {
        self.path.len()
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Which row is open at `depth`, so the drawing code can mark the parent
    /// of the level beside it.
    pub fn open_at(&self, depth: usize) -> Option<usize> {
        self.path.get(depth).copied()
    }

    /// Put the cursor on a row of the current level, as hovering does.
    pub fn point_at(&mut self, index: usize) {
        self.cursor = Some(index);
    }

    pub fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    /// Step the cursor, skipping rows that cannot be picked and wrapping at
    /// both ends. Starting with no cursor, down lands on the first row and up
    /// on the last, so a menu just opened answers either key sensibly.
    pub fn step(&mut self, root: &[MenuRow], forward: bool) {
        let rows = self.current(root);
        if rows.is_empty() {
            self.cursor = None;
            return;
        }
        let n = rows.len();
        let start = match self.cursor {
            Some(c) => c,
            None => {
                if forward {
                    n - 1
                } else {
                    0
                }
            }
        };
        for hop in 1..=n {
            let i = if forward {
                (start + hop) % n
            } else {
                (start + n - hop % n) % n
            };
            if rows[i].enabled {
                self.cursor = Some(i);
                return;
            }
        }
        // Nothing on this level can be picked; leave the cursor alone rather
        // than parking it on a row that would refuse.
    }

    /// Open the submenu under the cursor. Returns false when there is none,
    /// so the caller can treat Right on a leaf as "no move" rather than a
    /// selection.
    pub fn descend(&mut self, root: &[MenuRow]) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let rows = self.current(root);
        let Some(row) = rows.get(cursor) else {
            return false;
        };
        if !row.enabled || !row.is_submenu() {
            return false;
        }
        self.path.push(cursor);
        self.cursor = None;
        // Land on the first pickable row of the level just opened.
        self.step(root, true);
        true
    }

    /// Close the deepest level, putting the cursor back on the row that
    /// opened it. Returns false at the top level, where the caller closes the
    /// menu instead.
    pub fn ascend(&mut self) -> bool {
        match self.path.pop() {
            Some(parent) => {
                self.cursor = Some(parent);
                true
            }
            None => false,
        }
    }

    /// Open exactly `path`, cursor on its last row. Used by the pointer,
    /// which can enter a level without stepping through its parents.
    pub fn open_path(&mut self, path: Vec<usize>, cursor: Option<usize>) {
        self.path = path;
        self.cursor = cursor;
    }

    /// Forget any open submenu, as closing and reopening the menu does.
    pub fn reset(&mut self) {
        self.path.clear();
        self.cursor = None;
    }
}

/// The machine's state, as far as the menu needs to know it. Gathered once
/// when the menu opens so building the tree touches nothing live.
pub struct MenuState<'a> {
    pub fullscreen: bool,
    pub status_bar_hidden: bool,
    pub warp: bool,
    pub warp_speed: WarpSpeed,
    pub rewind: bool,
    pub recording: bool,
    pub input_recording: bool,
    pub autofire_hz: u8,
    pub joystick_input_mode: JoystickInputMode,
    pub port_devices: [PortDevice; 2],
    pub pixel_aspect: PixelAspect,
    pub shader: ShaderKind,
    /// Whether a custom shader file is configured. Without one the Custom
    /// row is shown but cannot be chosen.
    pub custom_shader_available: bool,
    pub tint: Tint,
    pub floppy_speed: u16,
    pub audio_filter: AudioFilterMode,
    /// The output in force, and every output the host offers.
    pub audio_output: AudioOutputChoice,
    pub audio_devices: &'a [String],
    /// MIDI ports, empty unless the serial port is in MIDI mode.
    pub midi_in: &'a str,
    pub midi_out: &'a str,
    pub midi_inputs: &'a [String],
    pub midi_outputs: &'a [String],
    /// Sampler, empty unless one is on the parallel port.
    pub sampler_input: &'a str,
    pub sampler_inputs: &'a [String],
    pub sampler_gain: f32,
    /// When each save slot was written, `yyyy/mm/dd HH:MM`, or `None` when
    /// the slot is free.
    pub save_slots: &'a [Option<String>; SAVE_SLOTS],
}

/// Numbered quick-save slots, matching the 1-10 keyboard shortcuts.
pub const SAVE_SLOTS: usize = 10;

/// A gain as the on-screen overlay spells it, so the menu and the overlay
/// agree.
fn gain_label(db: f32) -> String {
    if db.abs() < 0.05 {
        "0 dB".to_string()
    } else {
        format!("{db:+.0} dB")
    }
}

/// Autofire rates the menu offers, in Hz. 0 is off.
const AUTOFIRE_RATES: [u8; 6] = [0, 5, 10, 15, 20, 30];

/// Build the menu as it stands for this machine.
pub fn build(s: &MenuState) -> Vec<MenuRow> {
    let mut rows = vec![
        MenuRow::action("Machine Configuration...", MenuAction::OpenMachineConfig),
        MenuRow::action("Frame Analyzer...", MenuAction::OpenFrameAnalyzer),
        MenuRow::action("Debugger...", MenuAction::OpenDebugger),
        MenuRow::action("Console...", MenuAction::OpenConsole),
        MenuRow::submenu("Audio Settings", audio_rows(s)),
        MenuRow::submenu("Video Settings", video_rows(s)),
        MenuRow::submenu("Input Settings", input_rows(s)),
    ];

    // A port with nothing on it has nothing to set, so it contributes no
    // category rather than one that opens onto an empty list.
    if !s.midi_inputs.is_empty() || !s.midi_outputs.is_empty() {
        rows.push(MenuRow::submenu("Serial Port", serial_rows(s)));
    }
    if !s.sampler_inputs.is_empty() {
        rows.push(MenuRow::submenu("Parallel Port", parallel_rows(s)));
    }

    rows.extend([
        MenuRow::submenu("Emulation Settings", emulation_rows(s)),
        MenuRow::submenu("Warp Settings", warp_rows(s)),
        MenuRow::submenu("Recording", recording_rows(s)),
        MenuRow::submenu("Save State", save_state_rows(s)),
        MenuRow::action("Load Kickstart ROM...", MenuAction::LoadRom),
        MenuRow::action("Keyboard Shortcuts...", MenuAction::OpenShortcuts),
        MenuRow::action("About...", MenuAction::OpenAbout),
    ]);
    rows
}

fn audio_rows(s: &MenuState) -> Vec<MenuRow> {
    let mut outputs = vec![MenuRow::choice(
        "Default",
        MenuAction::SetAudioOutput(AudioOutputChoice::Default),
        s.audio_output == AudioOutputChoice::Default,
    )];
    for name in s.audio_devices {
        outputs.push(MenuRow::choice(
            name,
            MenuAction::SetAudioOutput(AudioOutputChoice::Named(name.clone())),
            s.audio_output == AudioOutputChoice::Named(name.clone()),
        ));
    }
    outputs.push(MenuRow::choice(
        "Disabled",
        MenuAction::SetAudioOutput(AudioOutputChoice::Disabled),
        s.audio_output == AudioOutputChoice::Disabled,
    ));

    let filters = [
        ("Auto", AudioFilterMode::Auto),
        ("On", AudioFilterMode::On),
        ("Off", AudioFilterMode::Off),
    ]
    .into_iter()
    .map(|(label, mode)| {
        MenuRow::choice(
            label,
            MenuAction::SetAudioFilter(mode),
            s.audio_filter == mode,
        )
    })
    .collect();

    vec![
        MenuRow::submenu("Audio Output", outputs),
        MenuRow::submenu("Audio Filter", filters),
    ]
}

fn video_rows(s: &MenuState) -> Vec<MenuRow> {
    let aspects = [("TV", PixelAspect::Tv), ("Square", PixelAspect::Square)]
        .into_iter()
        .map(|(label, a)| {
            MenuRow::choice(label, MenuAction::SetPixelAspect(a), s.pixel_aspect == a)
        })
        .collect();

    // Custom is listed whether or not a shader file is configured: greyed,
    // it says the feature exists, where a cycle that skipped it said nothing.
    let shaders = ShaderKind::MENU_ORDER
        .iter()
        .map(|k| {
            MenuRow::choice(k.label(), MenuAction::SetShader(*k), s.shader == *k)
                .available(*k != ShaderKind::Custom || s.custom_shader_available)
        })
        .collect();

    let tints = Tint::MENU_ORDER
        .iter()
        .map(|t| MenuRow::choice(t.label(), MenuAction::SetTint(*t), s.tint == *t))
        .collect();

    vec![
        MenuRow::submenu("Pixel Aspect", aspects),
        MenuRow::submenu("CRT Shader", shaders),
        MenuRow::submenu("Screen Tint", tints),
        MenuRow::toggle("Fullscreen", MenuAction::ToggleFullscreen, s.fullscreen),
        MenuRow::toggle(
            "Status Bar",
            MenuAction::ToggleStatusBar,
            !s.status_bar_hidden,
        ),
    ]
}

fn input_rows(s: &MenuState) -> Vec<MenuRow> {
    const DEVICES: [PortDevice; 5] = [
        PortDevice::Mouse,
        PortDevice::Joystick,
        PortDevice::Cd32Pad,
        PortDevice::Analogue,
        PortDevice::None,
    ];
    let port = |n: usize| -> Vec<MenuRow> {
        DEVICES
            .iter()
            .map(|d| {
                MenuRow::choice(
                    d.label(),
                    MenuAction::SetPortDevice(n, *d),
                    s.port_devices[n] == *d,
                )
            })
            .collect()
    };

    let joystick = [JoystickInputMode::Gamepad, JoystickInputMode::Keyboard]
        .into_iter()
        .map(|m| {
            MenuRow::choice(
                m.label(),
                MenuAction::SetJoystickInput(m),
                s.joystick_input_mode == m,
            )
        })
        .collect();

    let autofire = AUTOFIRE_RATES
        .iter()
        .map(|hz| {
            MenuRow::choice(
                &crate::config::autofire_label(*hz),
                MenuAction::SetAutofire(*hz),
                s.autofire_hz == *hz,
            )
        })
        .collect();

    vec![
        MenuRow::submenu("Port 1 Device", port(0)),
        MenuRow::submenu("Port 2 Device", port(1)),
        MenuRow::submenu("Joystick Input", joystick),
        MenuRow::submenu("Autofire", autofire),
        MenuRow::action("Calibrate Gamepad...", MenuAction::OpenCalibration),
        MenuRow::action("Input Mapping...", MenuAction::OpenInputMapping),
    ]
}

fn serial_rows(s: &MenuState) -> Vec<MenuRow> {
    let mut rows = Vec::new();
    if !s.midi_inputs.is_empty() {
        rows.push(MenuRow::submenu(
            "MIDI In",
            s.midi_inputs
                .iter()
                .map(|n| MenuRow::choice(n, MenuAction::SetMidiInput(n.clone()), s.midi_in == n))
                .collect(),
        ));
    }
    if !s.midi_outputs.is_empty() {
        rows.push(MenuRow::submenu(
            "MIDI Out",
            s.midi_outputs
                .iter()
                .map(|n| MenuRow::choice(n, MenuAction::SetMidiOutput(n.clone()), s.midi_out == n))
                .collect(),
        ));
    }
    rows
}

fn parallel_rows(s: &MenuState) -> Vec<MenuRow> {
    vec![
        MenuRow::submenu(
            "Sampler In",
            s.sampler_inputs
                .iter()
                .map(|n| {
                    MenuRow::choice(
                        n,
                        MenuAction::SetSamplerInput(n.clone()),
                        s.sampler_input == n,
                    )
                })
                .collect(),
        ),
        // A gain has too many steps to list, and is usually nudged rather
        // than picked: the row carries the figure and opens onto the two
        // steps, which leave the menu up so it can be nudged again.
        MenuRow::submenu(
            "Sampler Gain",
            vec![
                MenuRow::live("Increase", MenuAction::StepSamplerGain(1))
                    .available(s.sampler_gain < crate::sampler::MAX_SAMPLER_GAIN_DB),
                MenuRow::live("Decrease", MenuAction::StepSamplerGain(-1))
                    .available(s.sampler_gain > crate::sampler::MIN_SAMPLER_GAIN_DB),
            ],
        )
        .with_value(gain_label(s.sampler_gain)),
    ]
}

fn emulation_rows(s: &MenuState) -> Vec<MenuRow> {
    let speeds = std::iter::once(crate::floppy::SPEED_TURBO)
        .chain(crate::floppy::SUPPORTED_SPEED_PERCENTS)
        .map(|p| {
            MenuRow::choice(
                &crate::floppy::speed_label(p),
                MenuAction::SetFloppySpeed(p),
                s.floppy_speed == p,
            )
        })
        .collect();
    vec![
        MenuRow::submenu("Floppy Speed", speeds),
        MenuRow::toggle("Rewind", MenuAction::ToggleRewind, s.rewind),
    ]
}

fn warp_rows(s: &MenuState) -> Vec<MenuRow> {
    let limits = WarpSpeed::MENU_ORDER
        .iter()
        .map(|w| MenuRow::choice(w.label(), MenuAction::SetWarpLimit(*w), s.warp_speed == *w))
        .collect();
    vec![
        MenuRow::toggle("Warp Speed", MenuAction::ToggleWarp, s.warp),
        MenuRow::submenu("Warp Limit", limits),
    ]
}

fn recording_rows(s: &MenuState) -> Vec<MenuRow> {
    vec![
        MenuRow::action(
            if s.recording {
                "Stop Video Recording"
            } else {
                "Record Video"
            },
            MenuAction::ToggleRecord,
        ),
        MenuRow::action(
            if s.input_recording {
                "Stop Input Recording"
            } else {
                "Record Input"
            },
            MenuAction::ToggleRecordInput,
        ),
    ]
}

fn save_state_rows(s: &MenuState) -> Vec<MenuRow> {
    // A slot names what is in it, so a save that would overwrite something
    // says so before it is chosen rather than after.
    let slots = |save: bool| -> Vec<MenuRow> {
        (0..SAVE_SLOTS)
            .map(|i| {
                let held = s.save_slots[i].as_deref().unwrap_or("empty");
                let label = format!("{}: {held}", i + 1);
                let action = if save {
                    MenuAction::QuickSave(i)
                } else {
                    MenuAction::QuickLoad(i)
                };
                MenuRow::action(&label, action)
            })
            .collect()
    };
    vec![
        MenuRow::action("Save State", MenuAction::SaveState),
        MenuRow::action("Load State...", MenuAction::LoadState),
        MenuRow::submenu("Quick Save", slots(true)),
        MenuRow::submenu("Quick Load", slots(false)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_slots() -> [Option<String>; SAVE_SLOTS] {
        std::array::from_fn(|_| None)
    }

    fn state<'a>(
        audio: &'a [String],
        midi_in: &'a [String],
        midi_out: &'a [String],
        sampler: &'a [String],
        slots: &'a [Option<String>; SAVE_SLOTS],
    ) -> MenuState<'a> {
        MenuState {
            fullscreen: false,
            status_bar_hidden: false,
            warp: false,
            warp_speed: WarpSpeed::Max,
            rewind: false,
            recording: false,
            input_recording: false,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [PortDevice::Mouse, PortDevice::Joystick],
            pixel_aspect: PixelAspect::Tv,
            shader: ShaderKind::None,
            custom_shader_available: false,
            tint: Tint::None,
            floppy_speed: 100,
            audio_filter: AudioFilterMode::Auto,
            audio_output: AudioOutputChoice::Default,
            audio_devices: audio,
            midi_in: "",
            midi_out: "",
            midi_inputs: midi_in,
            midi_outputs: midi_out,
            sampler_input: "",
            sampler_inputs: sampler,
            sampler_gain: 0.0,
            save_slots: slots,
        }
    }

    fn find<'a>(rows: &'a [MenuRow], label: &str) -> Option<&'a MenuRow> {
        rows.iter().find(|r| r.label == label)
    }

    /// A port with nothing on it contributes no category: an empty list is
    /// worse than no row at all.
    #[test]
    fn silent_ports_contribute_no_categories() {
        let slots = empty_slots();
        let none: [String; 0] = [];
        let rows = build(&state(&none, &none, &none, &none, &slots));
        assert!(find(&rows, "Serial Port").is_none());
        assert!(find(&rows, "Parallel Port").is_none());

        let midi = ["IAC Bus 1".to_string()];
        let sampler = ["BlackHole".to_string()];
        let rows = build(&state(&none, &midi, &midi, &sampler, &slots));
        assert!(find(&rows, "Serial Port").is_some());
        assert!(find(&rows, "Parallel Port").is_some());
    }

    /// About is the last thing on the list, wherever the dynamic rows land.
    #[test]
    fn about_is_always_last() {
        let slots = empty_slots();
        let none: [String; 0] = [];
        let midi = ["IAC Bus 1".to_string()];
        for (a, m) in [(&none[..], &none[..]), (&none[..], &midi[..])] {
            let rows = build(&state(a, m, m, &none, &slots));
            assert_eq!(rows.last().expect("rows").label, "About...");
        }
    }

    /// Exactly one value of a setting is marked, and it is the one in force.
    #[test]
    fn the_setting_in_force_is_the_marked_one() {
        let slots = empty_slots();
        let none: [String; 0] = [];
        let mut s = state(&none, &none, &none, &none, &slots);
        s.port_devices[0] = PortDevice::Cd32Pad;
        let rows = build(&s);
        let input = find(&rows, "Input Settings").expect("input");
        let port1 = find(input.children().expect("children"), "Port 1 Device").expect("port 1");
        let choices = port1.children().expect("choices");
        let marked: Vec<&str> = choices
            .iter()
            .filter(|r| matches!(r.kind, MenuRowKind::Choice { selected: true, .. }))
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(marked, [PortDevice::Cd32Pad.label()]);
    }

    fn nav_rows() -> Vec<MenuRow> {
        vec![
            MenuRow::action("First", MenuAction::OpenAbout),
            MenuRow::action("Blocked", MenuAction::OpenAbout).available(false),
            MenuRow::submenu(
                "Deeper",
                vec![
                    MenuRow::action("Inner one", MenuAction::OpenAbout),
                    MenuRow::action("Inner two", MenuAction::OpenAbout),
                ],
            ),
        ]
    }

    /// Stepping skips what cannot be picked and wraps, and the first step
    /// into a fresh menu lands sensibly whichever key was pressed.
    #[test]
    fn stepping_skips_unpickable_rows_and_wraps() {
        let rows = nav_rows();
        let mut nav = MenuNav::default();
        nav.step(&rows, true);
        assert_eq!(nav.cursor(), Some(0), "down lands on the first row");
        nav.step(&rows, true);
        assert_eq!(nav.cursor(), Some(2), "the blocked row is skipped");
        nav.step(&rows, true);
        assert_eq!(nav.cursor(), Some(0), "and it wraps");

        let mut nav = MenuNav::default();
        nav.step(&rows, false);
        assert_eq!(nav.cursor(), Some(2), "up lands on the last row");
        nav.step(&rows, false);
        assert_eq!(nav.cursor(), Some(0), "skipping the blocked row again");
    }

    /// Descending opens the level and lands on its first row; ascending puts
    /// the cursor back on the row that opened it.
    #[test]
    fn descending_and_ascending_keep_their_place() {
        let rows = nav_rows();
        let mut nav = MenuNav::default();
        nav.point_at(2);
        assert!(nav.descend(&rows));
        assert_eq!(nav.depth(), 1);
        assert_eq!(nav.cursor(), Some(0));
        assert_eq!(nav.current(&rows).len(), 2, "the inner level");

        assert!(nav.ascend());
        assert_eq!(nav.depth(), 0);
        assert_eq!(nav.cursor(), Some(2), "back on the row that opened it");
        assert!(!nav.ascend(), "the top level has nowhere to go");
    }

    /// A leaf, and a row that cannot be picked, do not open anything.
    #[test]
    fn only_a_pickable_submenu_opens() {
        let rows = nav_rows();
        let mut nav = MenuNav::default();
        nav.point_at(0);
        assert!(!nav.descend(&rows), "a leaf has nothing to open");
        nav.point_at(1);
        assert!(!nav.descend(&rows), "nor does a blocked row");
        assert_eq!(nav.depth(), 0);
    }

    /// A quick-save slot says what it holds, so an overwrite is visible
    /// before it happens.
    #[test]
    fn quick_save_slots_name_what_they_hold() {
        let mut slots = empty_slots();
        slots[2] = Some("2026/07/31 14:05".to_string());
        let none: [String; 0] = [];
        let rows = build(&state(&none, &none, &none, &none, &slots));
        let save = find(&rows, "Save State").expect("save state");
        let quick = find(save.children().expect("children"), "Quick Save").expect("quick save");
        let labels: Vec<&str> = quick
            .children()
            .expect("slots")
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels[0], "1: empty");
        assert_eq!(labels[2], "3: 2026/07/31 14:05");
        assert_eq!(labels.len(), SAVE_SLOTS);
    }
}

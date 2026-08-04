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
use crate::config::{
    AudioFilterMode, DisplayScaling, MenuScale, PixelAspect, ShaderKind, Tint, WarpSpeed,
};

/// What choosing a leaf does. Everything the menu can do is here, so the
/// window's handler is a single match and the tree carries no behaviour.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    // Tools.
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
    SetDisplayScaling(DisplayScaling),
    SetShader(ShaderKind),
    SetTint(Tint),
    SetMenuScale(MenuScale),
    ToggleFullscreen,
    ToggleStatusBar,
    TogglePerfOverlay,

    // Input.
    SetPortDevice(usize, PortDevice),
    SetJoystickInput(JoystickInputMode),
    SetAutofire(u8),

    // Serial / parallel, present only when something is on the port.
    /// `None` unplugs the cable: a MIDI interface with nothing connected.
    SetMidiInput(Option<String>),
    SetMidiOutput(Option<String>),
    SetSamplerInput(String),
    /// Show or hide the MT-32's front panel.
    ToggleMt32Panel,
    /// How that panel's display is lit.
    SetMt32Lcd(crate::config::Mt32Lcd),
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

impl MenuAction {
    /// Whether this takes the eye somewhere else -- a window, a panel, a file
    /// dialogue. Those close the menu behind them; changing a setting leaves
    /// it up, so a run of changes costs one open rather than one each.
    pub fn opens_context(&self) -> bool {
        matches!(
            self,
            MenuAction::OpenMachineConfig
                | MenuAction::OpenFrameAnalyzer
                | MenuAction::OpenDebugger
                | MenuAction::OpenConsole
                | MenuAction::OpenInputMapping
                | MenuAction::OpenCalibration
                | MenuAction::OpenShortcuts
                | MenuAction::OpenAbout
                | MenuAction::LoadRom
                | MenuAction::SaveState
                | MenuAction::LoadState
        )
    }
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
    /// Does something. Whether the menu closes behind it is the action's
    /// own business -- see [`MenuAction::opens_context`].
    Action(MenuAction),
    /// Flips something in place, drawn as on or off.
    Toggle { action: MenuAction, on: bool },
    /// One value of a setting, marked when it is the one in force.
    Choice { action: MenuAction, selected: bool },
    /// Says what a level is and does nothing. Two levels of numbered slots
    /// look alike until one of them says which it is.
    Caption,
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

    fn caption(label: &str) -> Self {
        Self {
            enabled: false,
            ..Self::new(label, MenuRowKind::Caption)
        }
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

    /// Whether picking this row closes the menu: only the rows that open
    /// something else do.
    pub fn closes_menu(&self) -> bool {
        self.menu_action().is_some_and(MenuAction::opens_context)
    }

    /// The action this row carries, if it does anything itself.
    pub fn menu_action(&self) -> Option<&MenuAction> {
        match &self.kind {
            MenuRowKind::Action(a) => Some(a),
            MenuRowKind::Toggle { action, .. } | MenuRowKind::Choice { action, .. } => Some(action),
            MenuRowKind::Submenu(_) | MenuRowKind::Caption => None,
        }
    }

    /// Whether this row shows the state of a setting: one value of it, or
    /// whether it is on. Those rows carry a tick when the state holds, and a
    /// level holding any of them indents them all to keep the labels in line.
    pub fn marks_state(&self) -> bool {
        matches!(
            self.kind,
            MenuRowKind::Toggle { .. } | MenuRowKind::Choice { .. }
        )
    }

    /// Whether the state this row marks is the one in force.
    pub fn marked(&self) -> bool {
        matches!(
            self.kind,
            MenuRowKind::Toggle { on: true, .. } | MenuRowKind::Choice { selected: true, .. }
        )
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

    /// The rows open at each level, outermost first.
    pub fn path(&self) -> &[usize] {
        &self.path
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
    pub perf_overlay: bool,
    pub warp: bool,
    pub warp_speed: WarpSpeed,
    pub rewind: bool,
    pub recording: bool,
    pub input_recording: bool,
    pub autofire_hz: u8,
    pub joystick_input_mode: JoystickInputMode,
    pub port_devices: [PortDevice; 2],
    pub pixel_aspect: PixelAspect,
    pub scaling: DisplayScaling,
    pub shader: ShaderKind,
    /// Whether a custom shader file is configured. Without one the Custom
    /// row is shown but cannot be chosen.
    pub custom_shader_available: bool,
    pub tint: Tint,
    /// How large the menu itself is drawn.
    pub menu_scale: MenuScale,
    pub floppy_speed: u16,
    /// Whether any fitted bay serves from an image. Drive speed shapes how
    /// fast a track is served from one; a real drive's rate is the disk's own.
    pub floppy_speed_applies: bool,
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
    /// Whether an MT-32 could be selected (its ROM pair is configured),
    /// whether it is the device being played, whether its own MIDI OUT is
    /// wired back to the machine, and whether its panel is up.
    pub mt32_available: bool,
    pub mt32_attached: bool,
    pub mt32_input: bool,
    pub mt32_panel: bool,
    pub mt32_lcd: crate::config::Mt32Lcd,
    pub sampler_input: &'a str,
    pub sampler_inputs: &'a [String],
    pub sampler_gain: f32,
    /// When each save slot was written, `yyyy/mm/dd HH:MM`, or `None` when
    /// the slot is free.
    pub save_slots: &'a [Option<String>; SAVE_SLOTS],
}

/// Row pitch, the inset text keeps from each edge, the gap between a column
/// and its child, and the empty column the menu keeps beneath its rows until
/// a list is long enough to need it. Every one is multiplied by the menu
/// scale, so the whole menu grows together with the font.
pub const MENU_ROW_H: usize = 14;
pub const MENU_TEXT_INSET: usize = 8;
/// How far a child column sits over its parent. A hair of overlap, with
/// every column bevelled, reads as one stack of panels rather than a row of
/// separate boxes -- the same trick the desktop menus use.
pub const MENU_COL_OVERLAP: usize = 2;
pub const MENU_SLACK_H: usize = 28;

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
    // category rather than one that opens onto an empty list. The MT-32
    // counts: a machine with no host MIDI devices can still play to it.
    if !s.midi_inputs.is_empty() || !s.midi_outputs.is_empty() || s.mt32_available {
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
    let aspects = [
        ("TV (4:3)", PixelAspect::Tv),
        ("Square", PixelAspect::Square),
    ]
    .into_iter()
    .map(|(label, a)| MenuRow::choice(label, MenuAction::SetPixelAspect(a), s.pixel_aspect == a))
    .collect();

    let scalings = DisplayScaling::MENU_ORDER
        .iter()
        .map(|m| {
            MenuRow::choice(
                m.label(),
                MenuAction::SetDisplayScaling(*m),
                s.scaling == *m,
            )
        })
        .collect();

    // Custom is listed whether or not a shader file is configured. Greyed,
    // it says the feature is there and wants a file; absent, it says nothing.
    let shaders = ShaderKind::MENU_ORDER
        .iter()
        .map(|k| {
            MenuRow::choice(k.menu_label(), MenuAction::SetShader(*k), s.shader == *k)
                .available(*k != ShaderKind::Custom || s.custom_shader_available)
        })
        .collect();

    let tints = Tint::MENU_ORDER
        .iter()
        .map(|t| MenuRow::choice(t.menu_label(), MenuAction::SetTint(*t), s.tint == *t))
        .collect();

    let sizes = MenuScale::MENU_ORDER
        .iter()
        .map(|m| MenuRow::choice(m.label(), MenuAction::SetMenuScale(*m), s.menu_scale == *m))
        .collect();

    vec![
        MenuRow::submenu("Menu Size", sizes).with_value(s.menu_scale.label()),
        MenuRow::submenu("Pixel Aspect", aspects),
        MenuRow::submenu("Scaling", scalings),
        MenuRow::submenu("CRT Shader", shaders),
        MenuRow::submenu("Screen Tint", tints),
        MenuRow::toggle("Fullscreen", MenuAction::ToggleFullscreen, s.fullscreen),
        MenuRow::toggle(
            "Status Bar",
            MenuAction::ToggleStatusBar,
            !s.status_bar_hidden,
        ),
        MenuRow::toggle("Performance", MenuAction::TogglePerfOverlay, s.perf_overlay),
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
                    d.menu_label(),
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
                m.menu_label(),
                MenuAction::SetJoystickInput(m),
                s.joystick_input_mode == m,
            )
        })
        .collect();

    let autofire = crate::config::AUTOFIRE_RATES
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
    // The MT-32 joins the sources only while it is the destination: it has
    // no keyboard, and what it sends is an answer to what it was sent. That
    // is also the wiring an editor or librarian on the Amiga needs.
    if !s.midi_inputs.is_empty() || s.mt32_attached {
        // None heads the list: an interface with nothing plugged into that
        // socket, which is how a real one spends most of its life.
        let mut inputs = vec![MenuRow::choice(
            "None",
            MenuAction::SetMidiInput(None),
            s.midi_in == "None",
        )];
        inputs.extend(s.midi_inputs.iter().map(|n| {
            MenuRow::choice(n, MenuAction::SetMidiInput(Some(n.clone())), s.midi_in == n)
        }));
        if s.mt32_attached {
            inputs.push(MenuRow::choice(
                MT32_LABEL,
                MenuAction::SetMidiInput(Some(MT32_ENDPOINT.to_string())),
                s.mt32_input,
            ));
        }
        rows.push(MenuRow::submenu("MIDI In", inputs));
    }
    // The MT-32 is one of the outputs, always offered once its ROMs are
    // configured -- a machine with no host MIDI devices at all can still
    // play to it.
    if !s.midi_outputs.is_empty() || s.mt32_available {
        let mut outputs = vec![MenuRow::choice(
            "None",
            MenuAction::SetMidiOutput(None),
            s.midi_out == "None",
        )];
        outputs.extend(s.midi_outputs.iter().map(|n| {
            MenuRow::choice(
                n,
                MenuAction::SetMidiOutput(Some(n.clone())),
                s.midi_out == n,
            )
        }));
        if s.mt32_available {
            outputs.push(MenuRow::choice(
                MT32_LABEL,
                MenuAction::SetMidiOutput(Some(MT32_ENDPOINT.to_string())),
                s.mt32_attached,
            ));
        }
        rows.push(MenuRow::submenu("MIDI Out", outputs));
    }
    // The synth's own settings, once it is the device being played.
    if s.mt32_attached {
        let displays = crate::config::Mt32Lcd::MENU_ORDER
            .iter()
            .map(|d| MenuRow::choice(d.menu_label(), MenuAction::SetMt32Lcd(*d), s.mt32_lcd == *d))
            .collect();
        rows.push(MenuRow::submenu(
            MT32_LABEL,
            vec![
                MenuRow::toggle("Front Panel", MenuAction::ToggleMt32Panel, s.mt32_panel),
                // The display is the panel's, so it is offered with it.
                // No value on the row: the list it opens marks the one in
                // force, which is the same answer said once.
                MenuRow::submenu("Display", displays).available(s.mt32_panel),
            ],
        ));
    }
    rows
}

/// What the MT-32 output is called in the menu, and the endpoint name that
/// selects it.
const MT32_LABEL: &str = "Munt MT-32";
const MT32_ENDPOINT: &str = crate::config::MIDI_OUT_MT32;

fn parallel_rows(s: &MenuState) -> Vec<MenuRow> {
    vec![
        MenuRow::submenu(
            "Sampler Input",
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
                MenuRow::action("Increase", MenuAction::StepSamplerGain(1))
                    .available(s.sampler_gain < crate::sampler::MAX_SAMPLER_GAIN_DB),
                MenuRow::action("Decrease", MenuAction::StepSamplerGain(-1))
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
        MenuRow::submenu("Floppy Speed", speeds).available(s.floppy_speed_applies),
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
        let caption = if save { "Quick Save" } else { "Quick Load" };
        std::iter::once(MenuRow::caption(caption))
            .chain((0..SAVE_SLOTS).map(|i| {
                let held = s.save_slots[i].as_deref().unwrap_or("empty");
                let label = format!("{}: {held}", i + 1);
                let action = if save {
                    MenuAction::QuickSave(i)
                } else {
                    MenuAction::QuickLoad(i)
                };
                MenuRow::action(&label, action)
            }))
            .collect()
    };
    vec![
        MenuRow::submenu("Quick Save", slots(true)),
        MenuRow::submenu("Quick Load", slots(false)),
        MenuRow::action("Save State...", MenuAction::SaveState),
        MenuRow::action("Load State...", MenuAction::LoadState),
    ]
}

/// Geometry for the open menu: one column per open level, laid out from the
/// hamburger button upward.
///
/// The panel palette is drawn at the launcher's own text scale, which is
/// small enough that a column of a dozen rows costs little height -- so the
/// menu is anchored at the bottom, grows upward, and keeps a margin of empty
/// column beneath the rows until a list is long enough to need it.
pub mod layout {
    use super::{MenuRow, MENU_COL_OVERLAP, MENU_ROW_H, MENU_SLACK_H, MENU_TEXT_INSET};
    use crate::video::font;

    /// One open level's column.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Column {
        pub x: usize,
        pub y: usize,
        pub w: usize,
        pub h: usize,
        /// Rows drawn in this column. Fewer than the level holds when it has
        /// been trimmed to fit the display.
        pub visible: usize,
        /// Index of the first drawn row.
        pub first: usize,
        /// Row pitch at the scale this column was laid out for.
        pub row_h: usize,
    }

    impl Column {
        /// The rect of the `n`-th drawn row.
        pub fn row_rect(&self, n: usize) -> (usize, usize, usize, usize) {
            (self.x, self.y + n * self.row_h, self.w, self.row_h)
        }

        /// Which drawn row, if any, contains `(px, py)`.
        pub fn row_at(&self, px: usize, py: usize) -> Option<usize> {
            if px < self.x || px >= self.x + self.w || py < self.y {
                return None;
            }
            let n = (py - self.y) / self.row_h;
            (n < self.visible).then_some(self.first + n)
        }
    }

    /// The width a level needs: its widest row, plus room for the value and
    /// the submenu marker, within the inset either side.
    fn column_width(rows: &[MenuRow], px: usize) -> usize {
        let widest = rows
            .iter()
            .map(|r| {
                let value = r.value.as_ref().map_or(0, |v| v.chars().count() + 2);
                let marker = usize::from(r.is_submenu()) * 2;
                r.label.chars().count() + value + marker
            })
            .max()
            .unwrap_or(0);
        // A tick sits before the label on a level that marks one, so every
        // row on it is indented to keep the labels in a line.
        let tick = usize::from(rows.iter().any(super::MenuRow::marks_state)) * 2;
        (2 * MENU_TEXT_INSET + (widest + tick) * font::GLYPH_W) * px
    }

    /// Lay the open levels out, innermost last.
    ///
    /// `anchor_right` is where the first column's right edge sits (the
    /// hamburger button's right edge), `bottom` the display height the menu
    /// is anchored to, `opened_at` the row of each level that opened the one
    /// after it, so a child can start beside the row it belongs to, and `px`
    /// the menu scale every length is multiplied by.
    pub fn columns(
        levels: &[&[MenuRow]],
        opened_at: &[Option<usize>],
        anchor_right: usize,
        bottom: usize,
        px: usize,
    ) -> Vec<Column> {
        let px = px.max(1);
        let (row_h, overlap, slack_h) = (MENU_ROW_H * px, MENU_COL_OVERLAP * px, MENU_SLACK_H * px);
        let mut out: Vec<Column> = Vec::with_capacity(levels.len());
        for (depth, rows) in levels.iter().enumerate() {
            let w = column_width(rows, px);
            // The first column hangs from the button. Each child sits to the
            // right of its parent, overlapping it by a hair, and falls back
            // to the parent's left when the display runs out.
            let x = match out.last() {
                None => anchor_right.saturating_sub(w),
                Some(prev) => {
                    let right = (prev.x + prev.w).saturating_sub(overlap);
                    if right + w <= super::super::FB_WIDTH {
                        right
                    } else if let Some(left) = (prev.x + overlap).checked_sub(w) {
                        left
                    } else {
                        // Neither side has the room. Sit against the display
                        // edge rather than sliding back across the parent:
                        // the eye keeps travelling the way it set off, and
                        // less of the level behind is buried.
                        super::super::FB_WIDTH.saturating_sub(w)
                    }
                }
            };
            // The menu meets the status bar: its slack is empty panel below
            // the last row, not a gap under the panel. A level long enough to
            // need that room gives it up, and one longer still is trimmed.
            let wanted = rows.len() * row_h;
            let slack = if depth == 0 && wanted + slack_h <= bottom {
                slack_h
            } else {
                0
            };
            let room = bottom.saturating_sub(slack);
            let (visible, rows_h) = if wanted <= room {
                (rows.len(), wanted)
            } else {
                let fits = room / row_h;
                (fits, fits * row_h)
            };
            let h = rows_h + slack;
            // A child starts level with the row that opened it, so the eye
            // follows the row across rather than dropping back to a shared
            // edge. It slides up when that would push it off the bottom.
            let y = match (
                out.last(),
                opened_at.get(depth.wrapping_sub(1)).copied().flatten(),
            ) {
                (Some(prev), Some(parent_row)) => {
                    let (_, row_y, _, _) = prev.row_rect(parent_row.saturating_sub(prev.first));
                    row_y.min(bottom.saturating_sub(h))
                }
                _ => bottom.saturating_sub(h),
            };
            out.push(Column {
                x,
                y,
                w,
                h,
                visible,
                first: rows.len().saturating_sub(visible),
                row_h,
            });
        }
        out
    }
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
            perf_overlay: false,
            warp: false,
            warp_speed: WarpSpeed::Max,
            rewind: false,
            recording: false,
            input_recording: false,
            autofire_hz: 0,
            joystick_input_mode: JoystickInputMode::Gamepad,
            port_devices: [PortDevice::Mouse, PortDevice::Joystick],
            pixel_aspect: PixelAspect::Tv,
            scaling: DisplayScaling::Smooth,
            shader: ShaderKind::None,
            custom_shader_available: false,
            tint: Tint::None,
            menu_scale: MenuScale::Normal,
            floppy_speed: 100,
            floppy_speed_applies: true,
            audio_filter: AudioFilterMode::Auto,
            audio_output: AudioOutputChoice::Default,
            audio_devices: audio,
            midi_in: "",
            midi_out: "",
            midi_inputs: midi_in,
            midi_outputs: midi_out,
            mt32_available: false,
            mt32_attached: false,
            mt32_input: false,
            mt32_panel: false,
            mt32_lcd: crate::config::Mt32Lcd::Oled,
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
        assert_eq!(marked, [PortDevice::Cd32Pad.menu_label()]);
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

    /// Columns share a bottom edge, the first keeping its slack, and a child
    /// sits beside its parent.
    #[test]
    fn columns_stack_from_the_bottom_and_cascade_sideways() {
        let root = nav_rows();
        let inner = root[2].children().expect("children").to_vec();
        let cols = layout::columns(&[&root, &inner], &[Some(2)], 600, 400, 1);
        assert_eq!(cols.len(), 2);

        assert_eq!(
            cols[0].y + cols[0].h,
            400,
            "the panel meets the bottom, with its slack inside"
        );
        assert_eq!(
            cols[0].h,
            root.len() * MENU_ROW_H + MENU_SLACK_H,
            "which is empty panel below the last row"
        );
        assert_eq!(cols[0].x + cols[0].w, 600, "hangs from the anchor");

        // The child starts level with the row that opened it (index 2).
        let (_, parent_row_y, _, _) = cols[0].row_rect(2);
        assert_eq!(cols[1].y, parent_row_y, "level with its parent row");
        assert!(
            cols[1].x < cols[0].x + cols[0].w,
            "overlapping its parent by a hair"
        );
        assert!(cols[1].x > cols[0].x, "to the right of it");
    }

    /// A level too tall for the display is trimmed to what fits, keeping its
    /// end -- the rows nearest the button.
    #[test]
    fn a_long_level_is_trimmed_to_the_display() {
        let rows: Vec<MenuRow> = (0..40)
            .map(|i| MenuRow::action(&format!("Row {i}"), MenuAction::OpenAbout))
            .collect();
        let cols = layout::columns(&[&rows], &[], 600, 200, 1);
        assert!(cols[0].visible < rows.len(), "trimmed");
        assert_eq!(
            cols[0].first + cols[0].visible,
            rows.len(),
            "keeping the end of the list"
        );
        assert!(cols[0].y + cols[0].h <= 200);
    }

    /// A pointer lands on the row it is over, and on nothing outside.
    #[test]
    fn a_column_reports_the_row_under_the_pointer() {
        let rows = nav_rows();
        let cols = layout::columns(&[&rows], &[], 600, 400, 1);
        let c = cols[0];
        let (x, y, _, _) = c.row_rect(1);
        assert_eq!(c.row_at(x + 4, y + 2), Some(1));
        assert_eq!(c.row_at(c.x.saturating_sub(4), y + 2), None, "left of it");
        assert_eq!(c.row_at(x + 4, c.y + c.h + 8), None, "below it");
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
        let rows = quick.children().expect("slots");
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        // The caption says which of the two identical-looking levels this is,
        // and cannot be picked.
        assert_eq!(labels[0], "Quick Save");
        assert!(!rows[0].enabled && rows[0].menu_action().is_none());
        assert_eq!(labels[1], "1: empty");
        assert_eq!(labels[3], "3: 2026/07/31 14:05");
        assert_eq!(labels.len(), SAVE_SLOTS + 1);

        let load = find(save.children().expect("children"), "Quick Load").expect("quick load");
        assert_eq!(load.children().expect("slots")[0].label, "Quick Load");
    }

    /// The menu scale multiplies every length, so the whole thing grows
    /// together: a row twice as tall under a font twice as large.
    #[test]
    fn the_menu_scale_multiplies_every_length() {
        let none: [String; 0] = [];
        let rows = build(&state(&none, &none, &none, &none, &empty_slots()));
        let levels: Vec<&[MenuRow]> = vec![&rows];

        let small = layout::columns(&levels, &[], 600, 800, 1);
        let large = layout::columns(&levels, &[], 600, 800, 2);
        assert_eq!(large[0].w, small[0].w * 2);
        assert_eq!(large[0].h, small[0].h * 2);
        assert_eq!(large[0].row_h, small[0].row_h * 2);
        // Both fill the same bottom edge, and neither drops a row.
        assert_eq!(small[0].y + small[0].h, large[0].y + large[0].h);
        assert_eq!(small[0].visible, large[0].visible);

        // A row is hit where it is drawn, at either size.
        for cols in [&small, &large] {
            let (x, y, _, h) = cols[0].row_rect(1);
            assert_eq!(cols[0].row_at(x + 2, y + h / 2), Some(1));
        }
    }

    /// A setting that is on is ticked, not just labelled -- the same mark a
    /// chosen value carries, so "on" reads the same way everywhere.
    #[test]
    fn a_setting_that_is_on_is_ticked() {
        let none: [String; 0] = [];
        let slots = empty_slots();
        let mut st = state(&none, &none, &none, &none, &slots);
        st.rewind = true;
        st.warp = false;
        let rows = build(&st);

        let emulation = find(&rows, "Emulation Settings").expect("emulation");
        let rewind = find(emulation.children().expect("children"), "Rewind").expect("rewind");
        assert!(rewind.marks_state() && rewind.marked());

        let warp = find(&rows, "Warp Settings").expect("warp");
        let warp = find(warp.children().expect("children"), "Warp Speed").expect("warp speed");
        assert!(warp.marks_state() && !warp.marked());
    }

    /// Only the rows that put something else on the screen close the menu.
    /// Changing a setting leaves it up, so several can be changed in one go
    /// and the row just picked can be seen to have taken.
    #[test]
    fn only_the_rows_that_open_something_close_the_menu() {
        let none: [String; 0] = [];
        let rows = build(&state(&none, &none, &none, &none, &empty_slots()));

        assert!(find(&rows, "Machine Configuration...")
            .expect("config")
            .closes_menu());
        assert!(find(&rows, "About...").expect("about").closes_menu());

        let video = find(&rows, "Video Settings").expect("video");
        let video = video.children().expect("children");
        assert!(!find(video, "Fullscreen").expect("fullscreen").closes_menu());
        let tint = find(video, "Screen Tint").expect("tint");
        for row in tint.children().expect("tints") {
            assert!(!row.closes_menu(), "picking {} closed the menu", row.label);
        }

        let save = find(&rows, "Save State").expect("save state");
        let save = save.children().expect("children");
        // Both of these put a file dialogue up; a quick slot does not.
        assert!(find(save, "Save State...").expect("save").closes_menu());
        let quick = find(save, "Quick Save").expect("quick save");
        assert!(!quick.children().expect("slots")[0].closes_menu());
    }
}

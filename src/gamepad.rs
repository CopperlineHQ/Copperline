// SPDX-License-Identifier: GPL-3.0-or-later

//! Generic USB gamepad support: bundled controller-database defaults with a
//! one-time guided calibration as the per-pad override.
//!
//! gilrs is built with its bundled SDL_GameControllerDB mappings (and the
//! SDL_GAMECONTROLLERCONFIG environment variable) enabled. A pad the
//! database or the platform driver recognises works out of the box through
//! a fixed standard layout: d-pad and left stick drive the directions,
//! South is fire/CD32 red, East blue, West green, North yellow, Start
//! play/pause, and the left/right shoulders and triggers are reverse and
//! forward. Select/Back and the guide button, which no emulated control
//! uses, open the menu; Select held is the Quit hotkey.
//!
//! A saved calibration always wins over the database. That keeps unknown
//! pads working and broken database entries fixable the same way (many
//! cheap "retro" pads have broken or missing SDL_GameControllerDB entries):
//! calibrate once, pushing each control when prompted. Calibration records
//! which raw axis/button event code drives each Amiga port-2 joystick
//! direction and button, keyed by controller UUID and persisted to a config
//! file. Because it records the actual input the user pushes for each
//! direction, it needs no axis-sign or axis-order assumptions -- inversion
//! and layout fall out automatically.
//!
//! Besides the emulated controls, calibration can optionally bind a Menu
//! button and a Quit hotkey. Neither reaches the emulated port: a press of
//! the Menu button opens the menu, and the window turns a sustained hold of
//! the Quit hotkey -- or of the Menu button, when no separate Quit control
//! was bound -- into an application exit.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// An axis must reach this magnitude (after gilrs normalisation to [-1, 1]) to
/// count as a pressed direction, both when calibrating and at runtime.
const AXIS_ACTIVE_THRESHOLD: f32 = 0.5;
/// A control must reach this stronger magnitude to be *captured* during
/// calibration, so a resting/drifting stick isn't mistaken for a deliberate
/// push.
const AXIS_CAPTURE_THRESHOLD: f32 = 0.6;

/// Format tag written into newly recorded calibrations. Format 0 (absent in
/// the file) predates the bundled SDL controller mappings being enabled;
/// those mappings can flip the sign a database-covered pad reports for a
/// named axis, so a format-0 calibration on such a pad may have reversed
/// stick directions and earns a recalibration hint.
const CALIBRATION_FORMAT: u32 = 1;

/// One raw gamepad input bound to a joystick action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawInput {
    /// An analog axis (raw `Code::into_u32`) deflected past the threshold in
    /// the recorded direction.
    Axis { code: u32, positive: bool },
    /// A button (raw `Code::into_u32`) held down.
    Button { code: u32 },
}

impl RawInput {
    /// Short human-readable form for the calibration UI.
    fn describe(&self) -> String {
        match *self {
            RawInput::Axis { code, positive } => {
                format!("axis {:X}{}", code, if positive { '+' } else { '-' })
            }
            RawInput::Button { code } => format!("button {code:X}"),
        }
    }

    fn active(&self, axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> bool {
        match *self {
            RawInput::Axis { code, positive } => {
                let v = axes.get(&code).copied().unwrap_or(0.0);
                if positive {
                    v >= AXIS_ACTIVE_THRESHOLD
                } else {
                    v <= -AXIS_ACTIVE_THRESHOLD
                }
            }
            RawInput::Button { code } => buttons.get(&code).copied().unwrap_or(false),
        }
    }
}

/// The raw input each Amiga joystick action is bound to. `None` means the
/// action was skipped during calibration (e.g. a pad with no second button).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamepadCalibration {
    up: Option<RawInput>,
    down: Option<RawInput>,
    left: Option<RawInput>,
    right: Option<RawInput>,
    fire: Option<RawInput>,
    button2: Option<RawInput>,
    // CD32 joypad extras (optional; older calibration files omit them).
    #[serde(default)]
    green: Option<RawInput>,
    #[serde(default)]
    yellow: Option<RawInput>,
    #[serde(default)]
    play: Option<RawInput>,
    #[serde(default)]
    rwd: Option<RawInput>,
    #[serde(default)]
    ffw: Option<RawInput>,
    // Host-side Quit hotkey (optional; never driven into the emulated port).
    #[serde(default)]
    quit: Option<RawInput>,
    // Host-side Menu button (optional; never driven into the emulated
    // port). Opens the pop-up menu, which the d-pad then walks.
    #[serde(default)]
    menu: Option<RawInput>,
    // A second control per direction (optional), so a pad with both a
    // stick and a d-pad steers with either, as the database layout does.
    // Each ORs with its primary; files without them lose nothing.
    #[serde(default)]
    up_alt: Option<RawInput>,
    #[serde(default)]
    down_alt: Option<RawInput>,
    #[serde(default)]
    left_alt: Option<RawInput>,
    #[serde(default)]
    right_alt: Option<RawInput>,
    /// [`CALIBRATION_FORMAT`] at recording time; 0 for files written before
    /// the bundled controller mappings were enabled.
    #[serde(default)]
    format: u32,
}

/// Resolved emulated port-2 joystick state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JoystickState {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub fire: bool,
    pub button2: bool,
    // CD32 joypad extras (red = fire, blue = button2).
    pub green: bool,
    pub yellow: bool,
    pub play: bool,
    pub rwd: bool,
    pub ffw: bool,
}

/// One poll of a calibrated pad: the emulated joystick lines plus the
/// host-side controls, which never reach the emulated port.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PadState {
    pub joystick: JoystickState,
    /// The Quit hotkey is currently held: Select on the standard layout,
    /// or a calibration's Quit control -- its Menu button when no Quit
    /// was bound. Host-side only: the window turns a sustained hold into
    /// an application exit.
    pub quit: bool,
    /// The Menu button is currently held. Host-side only: a press opens
    /// (or closes) the pop-up menu, and the pad walks it while it is up.
    pub menu: bool,
    /// The stick, each axis in [-1, 1], right and up positive.
    ///
    /// The joystick lines above are what a digital port can hear; this
    /// is the deflection behind them. The standard layout reports the
    /// left stick; a calibration reports a stick only where it can see
    /// one -- a direction pair bound to the two ends of one raw axis --
    /// and zero for a d-pad. Gamepad Mouse spends it on how fast the
    /// pointer moves, where a switch could only say whether it moves at
    /// all.
    pub stick: (f32, f32),
}

/// How long a control must be held for the hold to mean something: a
/// step skipped while capturing, and the panel's own buttons once
/// everything is captured. Long enough that pressing a control to
/// capture or to try it never trips it.
const CAL_HOLD: std::time::Duration = std::time::Duration::from_millis(700);

impl GamepadCalibration {
    fn resolve_pad(&self, axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> PadState {
        PadState {
            joystick: self.resolve(axes, buttons),
            // The Quit hotkey is whichever host-side control was bound
            // for it; with none, the Menu button stands in -- held, it
            // quits, the same as Select on the standard layout -- so a
            // pad that skipped the Quit step is not left without a way
            // out. A bound Quit control takes the hold back to itself.
            quit: self
                .quit
                .or(self.menu)
                .is_some_and(|i| i.active(axes, buttons)),
            menu: self.menu.is_some_and(|i| i.active(axes, buttons)),
            // A calibration binds raw inputs one at a time, but a stick
            // still shows through it: a direction pair bound to the two
            // ends of one raw axis is that axis, and its deflection is
            // the stick's. A pad with both pairs on sticks reports the
            // one pushed further; d-pad-only bindings report none, and
            // the d-pad moves the pointer instead.
            stick: (
                Self::deflection(
                    [(self.right, self.left), (self.right_alt, self.left_alt)],
                    axes,
                ),
                Self::deflection([(self.up, self.down), (self.up_alt, self.down_alt)], axes),
            ),
        }
    }

    /// The deflection behind the direction pairs, signed so the first
    /// direction of a pair is positive: only a pair bound to the two
    /// signs of one raw axis has one, and of several the strongest wins.
    fn deflection(
        pairs: [(Option<RawInput>, Option<RawInput>); 2],
        axes: &BTreeMap<u32, f32>,
    ) -> f32 {
        pairs
            .into_iter()
            .filter_map(|(toward, away)| match (toward?, away?) {
                (
                    RawInput::Axis { code, positive },
                    RawInput::Axis {
                        code: other,
                        positive: opposite,
                    },
                ) if code == other && positive != opposite => {
                    let value = axes.get(&code).copied().unwrap_or(0.0);
                    Some(if positive { value } else { -value })
                }
                _ => None,
            })
            .fold(0.0_f32, |best, value| {
                if value.abs() > best.abs() {
                    value
                } else {
                    best
                }
            })
    }

    fn resolve(&self, axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> JoystickState {
        let active = |input: &Option<RawInput>| input.is_some_and(|i| i.active(axes, buttons));
        JoystickState {
            up: active(&self.up) || active(&self.up_alt),
            down: active(&self.down) || active(&self.down_alt),
            left: active(&self.left) || active(&self.left_alt),
            right: active(&self.right) || active(&self.right_alt),
            fire: active(&self.fire),
            button2: active(&self.button2),
            green: active(&self.green),
            yellow: active(&self.yellow),
            play: active(&self.play),
            rwd: active(&self.rwd),
            ffw: active(&self.ffw),
        }
    }
}

/// All saved calibrations, keyed by controller UUID (hex). Persisted as TOML.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CalibrationStore {
    #[serde(default)]
    gamepads: BTreeMap<String, GamepadCalibration>,
}

impl CalibrationStore {
    fn load() -> Self {
        let Some(path) = calibration_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!(
                    "ignoring unreadable gamepad calibration {}: {e}",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save(&self) -> Result<()> {
        let path =
            calibration_path().ok_or_else(|| anyhow!("no config directory for calibration"))?;
        crate::paths::ensure_parent(&path)?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        log::info!("saved gamepad calibration to {}", path.display());
        Ok(())
    }

    fn get(&self, uuid: &str) -> Option<&GamepadCalibration> {
        self.gamepads.get(uuid)
    }
}

/// Location of the persisted calibration store.
fn calibration_path() -> Option<PathBuf> {
    crate::paths::config_file("gamepads.toml")
}

fn uuid_hex(uuid: [u8; 16]) -> String {
    uuid.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// gilrs I/O: a raw reader shared by the runtime and the calibration flow.
// ---------------------------------------------------------------------------

/// Standard-layout state for a pad the controller database or platform
/// driver recognises, accumulated from gilrs's named events. Only consulted
/// when the pad has no saved calibration.
#[derive(Clone, Copy, Debug, Default)]
struct MappedPadState {
    dpad_up: bool,
    dpad_down: bool,
    dpad_left: bool,
    dpad_right: bool,
    south: bool,
    east: bool,
    west: bool,
    north: bool,
    start: bool,
    /// Select/Back and the guide button. Unused by the standard joystick
    /// layout, so the default mapping can spend them on the host-side
    /// Menu control without robbing any emulated control.
    select: bool,
    mode: bool,
    left_shoulder: bool,
    right_shoulder: bool,
    left_trigger: bool,
    right_trigger: bool,
    /// Left stick and d-pad axes in gilrs's convention: up and right are
    /// positive. Hat-style d-pads arrive as axes on some platforms.
    left_x: f32,
    left_y: f32,
    dpad_x: f32,
    dpad_y: f32,
}

impl MappedPadState {
    fn set_button(&mut self, button: gilrs::Button, pressed: bool) {
        use gilrs::Button as B;
        match button {
            B::DPadUp => self.dpad_up = pressed,
            B::DPadDown => self.dpad_down = pressed,
            B::DPadLeft => self.dpad_left = pressed,
            B::DPadRight => self.dpad_right = pressed,
            B::South => self.south = pressed,
            B::East => self.east = pressed,
            B::West => self.west = pressed,
            B::North => self.north = pressed,
            B::Start => self.start = pressed,
            B::Select => self.select = pressed,
            B::Mode => self.mode = pressed,
            B::LeftTrigger => self.left_shoulder = pressed,
            B::RightTrigger => self.right_shoulder = pressed,
            B::LeftTrigger2 => self.left_trigger = pressed,
            B::RightTrigger2 => self.right_trigger = pressed,
            _ => {}
        }
    }

    fn set_axis(&mut self, axis: gilrs::Axis, value: f32) {
        use gilrs::Axis as A;
        match axis {
            A::LeftStickX => self.left_x = value,
            A::LeftStickY => self.left_y = value,
            A::DPadX => self.dpad_x = value,
            A::DPadY => self.dpad_y = value,
            _ => {}
        }
    }

    /// The fixed standard layout: d-pad and left stick drive the directions;
    /// South = fire/red, East = blue, West = green, North = yellow, Start =
    /// play/pause, left shoulder/trigger = reverse, right = forward.
    fn resolve(&self) -> JoystickState {
        let t = AXIS_ACTIVE_THRESHOLD;
        JoystickState {
            up: self.dpad_up || self.left_y >= t || self.dpad_y >= t,
            down: self.dpad_down || self.left_y <= -t || self.dpad_y <= -t,
            left: self.dpad_left || self.left_x <= -t || self.dpad_x <= -t,
            right: self.dpad_right || self.left_x >= t || self.dpad_x >= t,
            fire: self.south,
            button2: self.east,
            green: self.west,
            yellow: self.north,
            play: self.start,
            rwd: self.left_shoulder || self.left_trigger,
            ffw: self.right_shoulder || self.right_trigger,
        }
    }

    /// The standard layout's whole report: the emulated lines above plus
    /// the host-side controls. Select/Back and the guide button, which no
    /// emulated control uses, open the menu -- harmless and dismissible.
    /// Select is also the Quit hotkey: the window only acts on a
    /// sustained hold, with a countdown that an early release cancels,
    /// which is the guard that makes a default binding safe. The guide
    /// button is left out of that on purpose: the platform often claims
    /// it, and on several pads holding it is the controller's own
    /// power-off or system-menu gesture.
    fn resolve_pad(&self) -> PadState {
        PadState {
            joystick: self.resolve(),
            quit: self.select,
            menu: self.select || self.mode,
            stick: (self.left_x, self.left_y),
        }
    }
}

/// A gilrs instance with the bundled SDL controller mappings enabled, plus
/// the current input state accumulated from its events in two forms: raw
/// axis/button codes (what calibration records and resolves against) and the
/// named standard layout (the default for recognised, uncalibrated pads).
struct RawGamepads {
    gilrs: gilrs::Gilrs,
    axes: BTreeMap<u32, f32>,
    buttons: BTreeMap<u32, bool>,
    mapped: MappedPadState,
}

impl RawGamepads {
    fn new() -> Result<Self> {
        let gilrs = gilrs::GilrsBuilder::new()
            .add_included_mappings(true)
            .add_env_mappings(true)
            .build()
            .map_err(|e| anyhow!("gamepad init: {e}"))?;
        Ok(Self {
            gilrs,
            axes: BTreeMap::new(),
            buttons: BTreeMap::new(),
            mapped: MappedPadState::default(),
        })
    }

    /// Drain pending events into the raw and named input state.
    fn pump(&mut self) {
        while let Some(event) = self.gilrs.next_event() {
            // COPPERLINE_DIAG_GAMEPAD=1 logs every gilrs event as delivered
            // (named control, post-mapping value, raw code) plus each pad's
            // identity and mapping source on connect: the ground truth for
            // diagnosing a wrong or broken controller-database entry.
            if crate::envcfg::flag("COPPERLINE_DIAG_GAMEPAD") {
                if event.event == gilrs::EventType::Connected {
                    let pad = self.gilrs.gamepad(event.id);
                    log::info!(
                        "gamepad diag: connected \"{}\" uuid {} mapping source {:?}",
                        pad.name(),
                        uuid_hex(pad.uuid()),
                        pad.mapping_source()
                    );
                } else {
                    log::info!("gamepad diag: {:?}", event.event);
                }
            }
            // Any disconnect resets the accumulated state: gilrs has
            // already dropped the pad from its connected list, so the id
            // cannot be compared against the driven pad below, and after a
            // reset the surviving pad's state rebuilds from its next events.
            if event.event == gilrs::EventType::Disconnected {
                self.axes.clear();
                self.buttons.clear();
                self.mapped = MappedPadState::default();
                continue;
            }
            // Accumulate state only for the pad this reader drives -- the
            // first connected one, the same selection poll() and the
            // calibration flow make -- so a bystander pad's drift or
            // presses cannot leak into it.
            if self.first_gamepad() != Some(event.id) {
                continue;
            }
            match event.event {
                gilrs::EventType::AxisChanged(axis, value, code) => {
                    self.axes.insert(code.into_u32(), value);
                    self.mapped.set_axis(axis, value);
                }
                gilrs::EventType::ButtonChanged(button, value, code) => {
                    if self.collapsed_dpad_half_axis(event.id, button, code) {
                        apply_collapsed_dpad(
                            &mut self.axes,
                            &mut self.buttons,
                            &mut self.mapped,
                            button,
                            value,
                            code.into_u32(),
                        );
                    } else {
                        let pressed = value >= AXIS_ACTIVE_THRESHOLD;
                        self.buttons.insert(code.into_u32(), pressed);
                        self.mapped.set_button(button, pressed);
                    }
                }
                // A collapsed half-axis d-pad also emits Pressed/Released at
                // gilrs's threshold crossings; the ButtonChanged value stream
                // carries the direction, so the guards drop those here.
                gilrs::EventType::ButtonPressed(button, code)
                    if !self.collapsed_dpad_half_axis(event.id, button, code) =>
                {
                    self.buttons.insert(code.into_u32(), true);
                    self.mapped.set_button(button, true);
                }
                gilrs::EventType::ButtonReleased(button, code)
                    if !self.collapsed_dpad_half_axis(event.id, button, code) =>
                {
                    self.buttons.insert(code.into_u32(), false);
                    self.mapped.set_button(button, false);
                }
                _ => {}
            }
        }
    }

    /// Whether this d-pad button event is really one half of an axis-mapped
    /// d-pad that gilrs collapsed to a single button.
    ///
    /// gilrs's SDL-mapping parser drops the `+`/`-` qualifiers of half-axis
    /// d-pad entries (`dpup:-a1,dpdown:+a1`, the common shape for cheap
    /// stickless pads) and keys its table by the native axis code, so the
    /// second binding for an axis overwrites the first: exactly one button
    /// of the pair keeps a code, and every event for that axis arrives as
    /// that one button with the full axis travel in its 0..1 value (0 = the
    /// SDL-negative end = up/left on standard entries, 0.5 = rest). Left as
    /// buttons, one direction per axis is lost and the other fires from the
    /// wrong end, so [`Self::reroute_collapsed_dpad`] turns these events
    /// back into a signed axis for both the raw and the named state.
    fn collapsed_dpad_half_axis(
        &self,
        id: gilrs::GamepadId,
        button: gilrs::Button,
        code: gilrs::ev::Code,
    ) -> bool {
        use gilrs::Button as B;
        let pair = match button {
            B::DPadUp | B::DPadDown => (B::DPadUp, B::DPadDown),
            B::DPadLeft | B::DPadRight => (B::DPadLeft, B::DPadRight),
            _ => return false,
        };
        let pad = self.gilrs.gamepad(id);
        if pad.mapping_source() != gilrs::MappingSource::SdlMappings {
            return false;
        }
        match (pad.button_code(pair.0), pad.button_code(pair.1)) {
            (Some(c), None) | (None, Some(c)) => c == code,
            _ => false,
        }
    }

    fn first_gamepad(&self) -> Option<gilrs::GamepadId> {
        self.gilrs.gamepads().next().map(|(id, _)| id)
    }
}

/// Recover both directions from a collapsed half-axis d-pad event (see
/// [`RawGamepads::collapsed_dpad_half_axis`]): the 0..1 button value spans
/// the full axis, so recentre it to -1..1 and store it as the raw axis it
/// really is (which is also what the pre-database calibration format
/// recorded for these pads). The named state gets the same value on the
/// matching d-pad axis, flipped vertically because SDL entries put up at
/// the negative end while the named convention is up-positive.
fn apply_collapsed_dpad(
    axes: &mut BTreeMap<u32, f32>,
    buttons: &mut BTreeMap<u32, bool>,
    mapped: &mut MappedPadState,
    button: gilrs::Button,
    value: f32,
    code: u32,
) {
    let v = value * 2.0 - 1.0;
    axes.insert(code, v);
    buttons.remove(&code);
    use gilrs::Button as B;
    match button {
        B::DPadUp | B::DPadDown => mapped.dpad_y = -v,
        B::DPadLeft | B::DPadRight => mapped.dpad_x = v,
        _ => {}
    }
}

/// Runtime reader: maps the first connected pad to the emulated port-2
/// joystick, through its saved calibration when one exists and through the
/// standard layout when the controller database or platform driver knows the
/// pad. Held by the window and polled once per scheduler quantum.
pub struct GamepadReader {
    raw: Option<RawGamepads>,
    store: CalibrationStore,
    warned_uncalibrated: bool,
    /// UUID of the pad whose input source we last announced, so we log it
    /// once per pad (and again if a different pad is plugged in).
    logged_pad: Option<String>,
}

impl Default for GamepadReader {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadReader {
    pub fn new() -> Self {
        let raw = match RawGamepads::new() {
            Ok(raw) => Some(raw),
            Err(e) => {
                log::warn!("USB gamepad support unavailable: {e}");
                None
            }
        };
        Self {
            raw,
            store: CalibrationStore::load(),
            warned_uncalibrated: false,
            logged_pad: None,
        }
    }

    /// Advance an in-window calibration session by one tick: pump pending
    /// gamepad events and feed the current raw state to the session.
    /// Returns true when the session's visible state changed (the UI
    /// should redraw).
    pub fn calibration_tick(&mut self, session: &mut CalibrationSession) -> bool {
        let Some(raw) = self.raw.as_mut() else {
            return session.note_backend_missing();
        };
        raw.pump();
        let pad = raw.first_gamepad().map(|id| {
            let pad = raw.gilrs.gamepad(id);
            (pad.name().to_string(), uuid_hex(pad.uuid()))
        });
        session.advance(pad, &raw.axes, &raw.buttons)
    }

    /// Persist a finished session's bindings for its pad and make them
    /// live for the runtime poll immediately.
    pub fn save_calibration(&mut self, session: &CalibrationSession) -> Result<()> {
        let uuid = session
            .pad_uuid()
            .ok_or_else(|| anyhow!("no gamepad connected"))?;
        self.store
            .gamepads
            .insert(uuid.to_string(), session.to_calibration());
        self.store.save()?;
        self.warned_uncalibrated = false;
        // Re-announce on the next poll now that a (new) calibration is live.
        self.logged_pad = None;
        Ok(())
    }

    /// Whether a connected pad carries a host-side control (the Quit
    /// hotkey or the Menu button). The window keeps the event loop polling
    /// while one is present: gilrs is polled, not evented, so a paused or
    /// powered-off machine would otherwise never observe the press.
    pub fn host_hotkey_present(&mut self) -> bool {
        let Some(raw) = self.raw.as_mut() else {
            return false;
        };
        raw.pump();
        let Some(id) = raw.first_gamepad() else {
            return false;
        };
        let pad = raw.gilrs.gamepad(id);
        let uuid = uuid_hex(pad.uuid());
        match self.store.get(&uuid) {
            Some(cal) => cal.quit.is_some() || cal.menu.is_some(),
            // The database's standard layout always carries the Menu
            // control on Select/Back and the guide button.
            None => !matches!(pad.mapping_source(), gilrs::MappingSource::None),
        }
    }

    /// Poll the gamepad and return its resolved state (emulated joystick
    /// lines plus the host-side Quit hotkey), or `None` when there is no
    /// pad, or the connected one is neither calibrated nor known to the
    /// controller database.
    pub fn poll(&mut self) -> Option<PadState> {
        let raw = self.raw.as_mut()?;
        raw.pump();
        let id = raw.first_gamepad()?;
        let pad = raw.gilrs.gamepad(id);
        let uuid = uuid_hex(pad.uuid());
        let mapped = !matches!(pad.mapping_source(), gilrs::MappingSource::None);
        match self.store.get(&uuid) {
            Some(cal) => {
                if self.logged_pad.as_deref() != Some(uuid.as_str()) {
                    self.logged_pad = Some(uuid.clone());
                    log::info!("using saved calibration for gamepad \"{}\"", pad.name());
                    if cal.format < CALIBRATION_FORMAT
                        && matches!(pad.mapping_source(), gilrs::MappingSource::SdlMappings)
                    {
                        // The bundled mappings can flip named-axis signs
                        // relative to what an old-format calibration
                        // recorded for this (database-covered) pad.
                        log::warn!(
                            "gamepad \"{}\" was calibrated before the bundled controller \
                             mappings were enabled; recalibrate if a direction is reversed",
                            pad.name()
                        );
                    }
                }
                Some(cal.resolve_pad(&raw.axes, &raw.buttons))
            }
            None if mapped => {
                if self.logged_pad.as_deref() != Some(uuid.as_str()) {
                    self.logged_pad = Some(uuid.clone());
                    log::info!(
                        "gamepad \"{}\" recognised by the controller database; using the \
                         standard layout (calibrate to customise)",
                        pad.name()
                    );
                }
                Some(raw.mapped.resolve_pad())
            }
            None => {
                if !self.warned_uncalibrated {
                    self.warned_uncalibrated = true;
                    log::warn!(
                        "gamepad \"{}\" is not calibrated; run `copperline --calibrate-gamepad` to use it",
                        pad.name()
                    );
                }
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stepwise calibration session (drives both the in-window panel and the
// CLI flow's capture logic).
// ---------------------------------------------------------------------------

/// The calibration prompts in order: label and whether the step may be
/// skipped (pads without CD32 extras skip the optional ones). The
/// alternate directions come last so that the familiar flow -- and the
/// step numbers every saved file and test rely on -- stay where they
/// were; a pad with one set of directions holds through four skips.
const CAL_STEPS: [(&str, bool); 17] = [
    ("Up", true),
    ("Down", true),
    ("Left", true),
    ("Right", true),
    ("Fire / CD32 red", true),
    ("Button 2 / CD32 blue", false),
    ("CD32 green", false),
    ("CD32 yellow", false),
    ("CD32 play/pause", false),
    ("CD32 reverse", false),
    ("CD32 forward", false),
    ("Open menu", false),
    ("Quit Copperline", false),
    ("Up (alternate)", false),
    ("Down (alternate)", false),
    ("Left (alternate)", false),
    ("Right (alternate)", false),
];

/// A guided calibration in progress: which step is being prompted, what
/// has been captured so far, and the connected pad's identity. Fed raw
/// gamepad state by [`GamepadReader::calibration_tick`].
pub struct CalibrationSession {
    bindings: [Option<RawInput>; CAL_STEPS.len()],
    step: usize,
    /// Every control active on the last tick. A press is a control that
    /// was not active and now is, so a control already down when a step
    /// begins -- one held over from the step before, or one a pad
    /// reports as down at rest -- never presses, and can never be
    /// captured by a step that is not asking for it.
    active: Vec<RawInput>,
    /// The press being waited on, and when it began. Released before the
    /// hold, it is the step's binding; still down at the hold, the step
    /// is skipped -- not every pad has every control.
    pending: Option<(RawInput, std::time::Instant)>,
    /// The last pad seen, kept across disconnects so a finished session
    /// can still be saved if the pad is unplugged at the end.
    pad: Option<(String, String)>,
    connected: bool,
    backend_missing: bool,
    /// Once every step is captured, the names of the bindings currently
    /// being pressed, so the user can test the calibration before saving.
    live_test: String,
    /// Once every step is captured, whether a hold has asked for the
    /// panel's own buttons: the testing is finished, and the pad walks
    /// Save and Cancel with the bindings it has just been taught.
    handed_over: bool,
    /// The pad as the bindings just captured see it, kept while those
    /// bindings are being tested. Holding one long enough hands the
    /// panel's own buttons to the pad, which walks them with exactly the
    /// controls it has just been taught -- so reaching Save proves the
    /// calibration works rather than asking for a mouse to finish.
    live_pad: PadState,
}

impl CalibrationSession {
    pub fn new() -> Self {
        Self {
            bindings: [None; CAL_STEPS.len()],
            live_pad: PadState::default(),
            handed_over: false,
            active: Vec::new(),
            pending: None,
            step: 0,
            pad: None,
            connected: false,
            backend_missing: false,
            live_test: String::new(),
        }
    }

    pub fn step_count() -> usize {
        CAL_STEPS.len()
    }

    pub fn step_label(index: usize) -> &'static str {
        CAL_STEPS[index].0
    }

    /// The (last seen) pad's display name.
    pub fn pad_name(&self) -> Option<&str> {
        self.pad.as_ref().map(|(name, _)| name.as_str())
    }

    /// Whether a pad is currently connected.
    pub fn connected(&self) -> bool {
        self.connected
    }

    fn pad_uuid(&self) -> Option<&str> {
        self.pad.as_ref().map(|(_, uuid)| uuid.as_str())
    }

    /// True when the host has no gamepad input backend at all.
    pub fn backend_missing(&self) -> bool {
        self.backend_missing
    }

    pub fn done(&self) -> bool {
        self.step >= CAL_STEPS.len()
    }

    /// The labels of the bindings currently held (only meaningful once
    /// the session is done; used to test before saving).
    pub fn live_test(&self) -> &str {
        &self.live_test
    }

    /// The pad as the bindings under test see it. Meaningless before the
    /// last step is captured, since there is nothing to resolve against.
    pub fn live_pad(&self) -> PadState {
        self.live_pad
    }

    /// Whether a hold has asked for the panel's own buttons, the
    /// testing being finished.
    pub fn handed_over(&self) -> bool {
        self.handed_over
    }

    /// A session with every step captured, for tests of what the panel
    /// does once there is nothing left to capture.
    #[cfg(test)]
    pub(crate) fn finished_for_test() -> Self {
        let mut session = Self::new();
        session.step = CAL_STEPS.len();
        session.connected = true;
        session
    }

    /// Pretend a hold has asked for the panel's buttons, for the same
    /// tests.
    #[cfg(test)]
    pub(crate) fn hand_over_for_test(&mut self) {
        self.handed_over = true;
    }

    /// Whether the current prompt may be skipped (optional CD32 extras).
    pub fn can_skip(&self) -> bool {
        self.step < CAL_STEPS.len() && !CAL_STEPS[self.step].1
    }

    /// The index of the step currently being prompted.
    pub fn current_step(&self) -> Option<usize> {
        (!self.done()).then_some(self.step)
    }

    /// Display text for a step's captured binding: the raw input, or
    /// "skipped", or "" while still pending.
    pub fn binding_text(&self, index: usize) -> String {
        match &self.bindings[index] {
            Some(input) => input.describe(),
            None if index < self.step => "skipped".to_string(),
            None => String::new(),
        }
    }

    /// Skip the current (optional) step. The Skip button's way of doing
    /// what holding a control does.
    pub fn skip_current(&mut self) {
        if self.can_skip() {
            self.bindings[self.step] = None;
            self.step += 1;
            // Whatever is down stays down as far as the next step is
            // concerned: it never pressed, so it cannot be captured.
            self.pending = None;
        }
    }

    fn note_backend_missing(&mut self) -> bool {
        let changed = !self.backend_missing;
        self.backend_missing = true;
        changed
    }

    /// Feed one tick of raw gamepad state. Returns true when visible
    /// state (pad identity, prompt, captured bindings) changed.
    fn advance(
        &mut self,
        pad: Option<(String, String)>,
        axes: &BTreeMap<u32, f32>,
        buttons: &BTreeMap<u32, bool>,
    ) -> bool {
        let mut changed = self.connected != pad.is_some();
        self.connected = pad.is_some();
        if let Some(pad) = pad {
            if self.pad.as_ref() != Some(&pad) {
                changed = true;
                self.pad = Some(pad);
            }
        }
        if changed {
            // A (re)connected pad may still report a stale deflection.
            // Forgetting what was active makes everything it reports
            // look like it was already down, so nothing is captured
            // until something is pressed afresh.
            self.active = active_inputs(axes, buttons);
            self.pending = None;
        }
        if !self.connected {
            return changed;
        }
        let now = active_inputs(axes, buttons);
        // What was pressed this tick: active now, and not before.
        let pressed = now.iter().copied().find(|i| !self.active.contains(i));
        let held = |input: &RawInput| now.contains(input);
        if self.done() {
            // Live test: show which captured bindings are active right now
            // so the calibration can be verified before saving. A press
            // is how a binding is tried, so it is a hold that says the
            // testing is finished and asks for the panel's buttons.
            let labels: Vec<&str> = self
                .bindings
                .iter()
                .enumerate()
                .filter(|(_, binding)| binding.is_some_and(|b| b.active(axes, buttons)))
                .map(|(index, _)| CAL_STEPS[index].0)
                .collect();
            let live_test = labels.join(", ");
            if live_test != self.live_test {
                self.live_test = live_test;
                changed = true;
            }
            self.live_pad = self.to_calibration().resolve_pad(axes, buttons);
            changed |= self.watch_hold(pressed, &held, |session| {
                session.handed_over = true;
                true
            });
            self.active = now;
            return changed;
        }
        // A press captures the control it was made with; holding it says
        // the pad has not got that control, and skips the step. Which of
        // the two it was is only known when the button comes back up, so
        // nothing is captured until it does.
        let skippable = !CAL_STEPS[self.step].1;
        changed |= self.watch_hold(pressed, &held, move |session| {
            if !skippable {
                return false;
            }
            session.bindings[session.step] = None;
            session.step += 1;
            true
        });
        self.active = now;
        changed
    }

    /// Follow one press through to what it turns out to be.
    ///
    /// A press is remembered rather than acted on: released before the
    /// hold it is a press, and it binds the step; still down at the
    /// hold, `on_hold` has it instead and the press is spent, so a
    /// control kept down after that does nothing more.
    ///
    /// `on_hold` says whether it took the hold. Where it did not -- a
    /// step that cannot be skipped -- the press stays pending, so
    /// holding one of the four directions and letting go still binds it
    /// rather than quietly coming to nothing.
    fn watch_hold(
        &mut self,
        pressed: Option<RawInput>,
        held: &dyn Fn(&RawInput) -> bool,
        on_hold: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        match self.pending {
            None => {
                if let Some(input) = pressed {
                    self.pending = Some((input, std::time::Instant::now()));
                }
                false
            }
            Some((input, since)) => {
                if !held(&input) {
                    self.pending = None;
                    self.on_press(input);
                    true
                } else if since.elapsed() >= CAL_HOLD && on_hold(self) {
                    self.pending = None;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// A control pressed and released: the binding for the step being
    /// captured, and nothing at all once every step is captured -- there
    /// a press is how a binding is tried.
    fn on_press(&mut self, input: RawInput) {
        if self.done() {
            return;
        }
        self.bindings[self.step] = Some(input);
        self.step += 1;
    }

    /// The captured bindings as a persistable calibration.
    fn to_calibration(&self) -> GamepadCalibration {
        let b = &self.bindings;
        GamepadCalibration {
            up: b[0],
            down: b[1],
            left: b[2],
            right: b[3],
            fire: b[4],
            button2: b[5],
            green: b[6],
            yellow: b[7],
            play: b[8],
            rwd: b[9],
            ffw: b[10],
            menu: b[11],
            quit: b[12],
            up_alt: b[13],
            down_alt: b[14],
            left_alt: b[15],
            right_alt: b[16],
            format: CALIBRATION_FORMAT,
        }
    }
}

impl Default for CalibrationSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Every control the pad has active right now: each pressed button, and
/// each axis deflected past the capture threshold, in the direction it
/// is deflected.
///
/// Calibration works from this rather than from "the strongest one",
/// because what a step wants is the control that was *pressed* -- and a
/// pad reporting something as down at rest, which several do, would
/// otherwise be captured by every step in turn.
fn active_inputs(axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> Vec<RawInput> {
    let mut active: Vec<RawInput> = buttons
        .iter()
        .filter(|(_, &pressed)| pressed)
        .map(|(&code, _)| RawInput::Button { code })
        .collect();
    active.extend(
        axes.iter()
            .filter(|(_, v)| v.abs() >= AXIS_CAPTURE_THRESHOLD)
            .map(|(&code, &v)| RawInput::Axis {
                code,
                positive: v > 0.0,
            }),
    );
    active
}

/// The most strongly deflected axis or any pressed button, if past the
/// capture threshold; otherwise `None`.
fn strongest_input_from(
    axes: &BTreeMap<u32, f32>,
    buttons: &BTreeMap<u32, bool>,
) -> Option<RawInput> {
    if let Some((&code, _)) = buttons.iter().find(|(_, &pressed)| pressed) {
        return Some(RawInput::Button { code });
    }
    axes.iter()
        .filter(|(_, v)| v.abs() >= AXIS_CAPTURE_THRESHOLD)
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map(|(&code, &v)| RawInput::Axis {
            code,
            positive: v > 0.0,
        })
}

/// No button held and no axis meaningfully deflected.
fn raw_state_neutral(axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> bool {
    !buttons.values().any(|&p| p) && !axes.values().any(|v| v.abs() >= AXIS_ACTIVE_THRESHOLD)
}

// ---------------------------------------------------------------------------
// Guided calibration (CLI mode).
// ---------------------------------------------------------------------------

/// Run the interactive calibration: prompt for each control, record the raw
/// input the user activates, and persist it for the connected pad. Exits the
/// program afterwards; never touches the emulator.
pub fn run_calibration() -> Result<()> {
    let mut raw = RawGamepads::new()?;

    println!("Copperline gamepad calibration.");
    println!("Connect your controller; calibration will begin automatically.");
    let id = wait_for_gamepad(&mut raw)?;
    let (name, uuid) = {
        let pad = raw.gilrs.gamepad(id);
        (pad.name().to_string(), uuid_hex(pad.uuid()))
    };
    println!("Calibrating \"{name}\".\n");

    let cal = GamepadCalibration {
        up: capture(&mut raw, "UP", true)?,
        down: capture(&mut raw, "DOWN", true)?,
        left: capture(&mut raw, "LEFT", true)?,
        right: capture(&mut raw, "RIGHT", true)?,
        fire: capture(&mut raw, "FIRE / CD32 red (button 1)", true)?,
        button2: capture(
            &mut raw,
            "second button / CD32 blue (or wait to skip)",
            false,
        )?,
        green: capture(&mut raw, "CD32 green (or wait to skip)", false)?,
        yellow: capture(&mut raw, "CD32 yellow (or wait to skip)", false)?,
        play: capture(&mut raw, "CD32 play/pause (or wait to skip)", false)?,
        rwd: capture(&mut raw, "CD32 reverse (or wait to skip)", false)?,
        ffw: capture(&mut raw, "CD32 forward (or wait to skip)", false)?,
        menu: capture(&mut raw, "the Menu button (or wait to skip)", false)?,
        quit: capture(
            &mut raw,
            "the Quit Copperline hotkey (or wait to skip)",
            false,
        )?,
        up_alt: capture(
            &mut raw,
            "UP on the other stick or d-pad (or wait to skip)",
            false,
        )?,
        down_alt: capture(
            &mut raw,
            "DOWN on the other stick or d-pad (or wait to skip)",
            false,
        )?,
        left_alt: capture(
            &mut raw,
            "LEFT on the other stick or d-pad (or wait to skip)",
            false,
        )?,
        right_alt: capture(
            &mut raw,
            "RIGHT on the other stick or d-pad (or wait to skip)",
            false,
        )?,
        format: CALIBRATION_FORMAT,
    };

    let mut store = CalibrationStore::load();
    store.gamepads.insert(uuid, cal);
    store.save()?;
    println!("\nCalibration saved. Start Copperline normally to use the gamepad.");
    Ok(())
}

/// Block until a gamepad is connected and an input is seen, returning its id.
fn wait_for_gamepad(raw: &mut RawGamepads) -> Result<gilrs::GamepadId> {
    loop {
        raw.pump();
        if let Some(id) = raw.first_gamepad() {
            return Ok(id);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Prompt for one control and return the raw input the user deflects past the
/// capture threshold. `required` controls whether the prompt can time out and
/// be skipped (returning `None`).
fn capture(raw: &mut RawGamepads, label: &str, required: bool) -> Result<Option<RawInput>> {
    // Make sure everything is at rest before sampling so a held control from
    // the previous step isn't recaptured.
    wait_for_neutral(raw);
    print!("Push {label} and hold... ");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let skip_after = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let captured = loop {
        raw.pump();
        if let Some(input) = strongest_input(raw) {
            break Some(input);
        }
        if !required && std::time::Instant::now() >= skip_after {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    match captured {
        Some(input) => {
            println!("got it.");
            // Wait for release so the next prompt starts clean.
            wait_for_neutral(raw);
            Ok(Some(input))
        }
        None => {
            println!("skipped.");
            Ok(None)
        }
    }
}

/// The most strongly deflected axis or any pressed button, if past the capture
/// threshold; otherwise `None`.
fn strongest_input(raw: &RawGamepads) -> Option<RawInput> {
    strongest_input_from(&raw.axes, &raw.buttons)
}

/// Spin until no button is held and no axis is meaningfully deflected.
fn wait_for_neutral(raw: &mut RawGamepads) {
    loop {
        raw.pump();
        if raw_state_neutral(&raw.axes, &raw.buttons) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(code: u32, positive: bool) -> Option<RawInput> {
        Some(RawInput::Axis { code, positive })
    }
    fn button(code: u32) -> Option<RawInput> {
        Some(RawInput::Button { code })
    }

    /// A press captures the control it was made with, and a control
    /// already down when a step begins is not a press: several pads
    /// report something as held at rest, and every step in turn was
    /// capturing it.
    #[test]
    fn a_step_captures_the_control_that_was_pressed() {
        let pad = Some(("Pad".to_string(), "uuid".to_string()));
        let mut session = CalibrationSession::new();
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        // Something the pad reports as down from the start.
        buttons.insert(0x90001, true);
        session.advance(pad.clone(), &axes, &buttons);
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(0), "nothing was pressed");

        // A press: down, then up. Nothing is captured until it comes up,
        // since until then it could still turn out to be a hold.
        buttons.insert(0x90002, true);
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(0), "still down");
        buttons.insert(0x90002, false);
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(1), "captured on release");
        assert_eq!(session.binding_text(0), "button 90002");

        // The control that was down all along still is, and still has
        // not pressed: the next step is untouched by it.
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(1));
    }

    /// Holding a control skips the step, for a pad that lacks it -- but
    /// only where the step may be skipped at all.
    #[test]
    fn holding_a_control_skips_the_step_it_can_skip() {
        let pad = Some(("Pad".to_string(), "uuid".to_string()));
        let mut session = CalibrationSession::new();
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();

        // Up is required: holding it through the hold skips nothing, and
        // the press is still there to bind it when the control comes up.
        buttons.insert(0x90001, true);
        session.advance(pad.clone(), &axes, &buttons);
        session.pending = Some((RawInput::Button { code: 0x90001 }, held_long_enough()));
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(0), "a required step stands");
        buttons.insert(0x90001, false);
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(1), "and the release binds it");
        assert_eq!(session.binding_text(0), "button 90001");
        buttons.remove(&0x90001);

        // Walk to the first step that may be skipped, then hold.
        while !session.can_skip() && !session.done() {
            let code = 0x90010 + session.step as u32;
            buttons.insert(code, true);
            session.advance(pad.clone(), &axes, &buttons);
            buttons.insert(code, false);
            session.advance(pad.clone(), &axes, &buttons);
        }
        let at = session.step;
        assert!(session.can_skip(), "there is a step to skip");
        buttons.insert(0x90020, true);
        session.advance(pad.clone(), &axes, &buttons);
        session.pending = Some((RawInput::Button { code: 0x90020 }, held_long_enough()));
        session.advance(pad.clone(), &axes, &buttons);
        assert_eq!(session.current_step(), Some(at + 1), "the step was skipped");
        assert_eq!(session.binding_text(at), "skipped");
    }

    /// Once every step is captured, a press tries a binding and a hold
    /// asks for the panel's own buttons.
    #[test]
    fn a_hold_at_the_end_asks_for_the_panels_buttons() {
        let pad = Some(("Pad".to_string(), "uuid".to_string()));
        let mut session = CalibrationSession::finished_for_test();
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        buttons.insert(0x90001, true);
        session.advance(pad.clone(), &axes, &buttons);
        assert!(!session.handed_over(), "a press is how a binding is tried");
        session.pending = Some((RawInput::Button { code: 0x90001 }, held_long_enough()));
        session.advance(pad.clone(), &axes, &buttons);
        assert!(session.handed_over(), "and a hold asks for the buttons");
    }

    /// A moment far enough in the past that a press begun then has
    /// become a hold.
    fn held_long_enough() -> std::time::Instant {
        std::time::Instant::now() - CAL_HOLD
    }

    #[test]
    fn resolve_reads_calibrated_axes_and_buttons_with_recorded_signs() {
        // Up/down share one axis with opposite signs (a typical retro pad
        // whose D-pad is a single X/Y axis pair), left/right another, and the
        // buttons are digital. The recorded sign captures the pad's physical
        // direction, so no inversion assumption is needed.
        let cal = GamepadCalibration {
            up: axis(0x10031, false),
            down: axis(0x10031, true),
            left: axis(0x10030, false),
            right: axis(0x10030, true),
            fire: button(0x90001),
            button2: button(0x90002),
            ..Default::default()
        };

        let mut axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();

        // Push "up" (Y axis to -1) and hold fire.
        axes.insert(0x10031, -1.0);
        buttons.insert(0x90001, true);
        let s = cal.resolve(&axes, &buttons);
        assert!(s.up && !s.down && s.fire && !s.button2);
        assert!(!s.left && !s.right);

        // Push "right" (X axis to +1) and second button.
        axes.clear();
        buttons.clear();
        axes.insert(0x10030, 1.0);
        buttons.insert(0x90002, true);
        let s = cal.resolve(&axes, &buttons);
        assert!(s.right && !s.left && s.button2 && !s.fire);
        assert!(!s.up && !s.down);
    }

    #[test]
    fn default_layout_maps_standard_controls_to_cd32_actions() {
        // A database-covered pad with no calibration resolves through the
        // fixed standard layout: face buttons onto the CD32 colours, Start
        // onto play/pause, shoulders and triggers onto the transport keys.
        let mut mapped = MappedPadState::default();
        mapped.set_button(gilrs::Button::South, true);
        mapped.set_button(gilrs::Button::Start, true);
        mapped.set_button(gilrs::Button::LeftTrigger, true);
        mapped.set_button(gilrs::Button::RightTrigger2, true);
        mapped.set_button(gilrs::Button::DPadUp, true);
        let s = mapped.resolve();
        assert!(s.fire && s.play && s.rwd && s.ffw && s.up);
        assert!(!s.button2 && !s.green && !s.yellow && !s.down);

        mapped = MappedPadState::default();
        mapped.set_button(gilrs::Button::East, true);
        mapped.set_button(gilrs::Button::West, true);
        mapped.set_button(gilrs::Button::North, true);
        let s = mapped.resolve();
        assert!(s.button2 && s.green && s.yellow);
        assert!(!s.fire && !s.play && !s.rwd && !s.ffw);
    }

    #[test]
    fn default_layout_host_controls_ride_on_select_and_guide() {
        // Select opens the menu and, held, is the Quit hotkey; the guide
        // button opens the menu only (the platform often claims it, and
        // holding it is several pads' own power gesture). Neither reaches
        // the emulated lines, and nothing else arms a quit.
        let mut mapped = MappedPadState::default();
        mapped.set_button(gilrs::Button::Select, true);
        let pad = mapped.resolve_pad();
        assert!(pad.menu && pad.quit);
        assert_eq!(pad.joystick, JoystickState::default());

        mapped = MappedPadState::default();
        mapped.set_button(gilrs::Button::Mode, true);
        let pad = mapped.resolve_pad();
        assert!(pad.menu && !pad.quit);

        mapped = MappedPadState::default();
        mapped.set_button(gilrs::Button::Start, true);
        mapped.set_button(gilrs::Button::South, true);
        let pad = mapped.resolve_pad();
        assert!(!pad.menu && !pad.quit);
        assert!(pad.joystick.play && pad.joystick.fire);
    }

    #[test]
    fn default_layout_directions_from_stick_dpad_buttons_and_hat_axes() {
        // gilrs's named-axis convention is up/right positive; a deflection
        // must pass the activity threshold, and d-pad buttons, hat-style
        // d-pad axes, and the left stick all drive the same four lines.
        let mut mapped = MappedPadState::default();
        mapped.set_axis(gilrs::Axis::LeftStickY, 0.9);
        mapped.set_axis(gilrs::Axis::LeftStickX, -0.9);
        let s = mapped.resolve();
        assert!(s.up && s.left && !s.down && !s.right);

        mapped.set_axis(gilrs::Axis::LeftStickY, 0.3); // below threshold
        mapped.set_axis(gilrs::Axis::LeftStickX, 0.0);
        assert_eq!(mapped.resolve(), JoystickState::default());

        mapped = MappedPadState::default();
        mapped.set_axis(gilrs::Axis::DPadY, -1.0);
        mapped.set_button(gilrs::Button::DPadRight, true);
        let s = mapped.resolve();
        assert!(s.down && s.right && !s.up && !s.left);
    }

    #[test]
    fn collapsed_dpad_reroute_recentres_and_replaces_the_button() {
        use gilrs::Button as B;
        let mut axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        let mut mapped = MappedPadState::default();
        // A stale pressed entry from before the collapse was detected must
        // not linger once the code is rerouted as an axis.
        buttons.insert(7, true);

        // Physical up: button value 0.0 is the SDL-negative axis end. Raw
        // recentres to -1 (the shape legacy calibrations recorded); the
        // named state flips to the up-positive convention.
        apply_collapsed_dpad(&mut axes, &mut buttons, &mut mapped, B::DPadUp, 0.0, 7);
        assert_eq!(axes.get(&7), Some(&-1.0));
        assert!(!buttons.contains_key(&7));
        assert_eq!(mapped.dpad_y, 1.0);
        let s = mapped.resolve();
        assert!(s.up && !s.down);

        // Rest sits at mid-travel and recentres to exactly 0.
        apply_collapsed_dpad(&mut axes, &mut buttons, &mut mapped, B::DPadUp, 0.5, 7);
        assert_eq!(axes.get(&7), Some(&0.0));
        assert_eq!(mapped.resolve(), JoystickState::default());

        // Physical down arrives through the same surviving button name.
        apply_collapsed_dpad(&mut axes, &mut buttons, &mut mapped, B::DPadUp, 1.0, 7);
        assert_eq!(axes.get(&7), Some(&1.0));
        assert_eq!(mapped.dpad_y, -1.0);
        let s = mapped.resolve();
        assert!(s.down && !s.up);

        // Horizontal has no flip: the SDL-negative end is left.
        apply_collapsed_dpad(&mut axes, &mut buttons, &mut mapped, B::DPadRight, 0.0, 8);
        assert_eq!(axes.get(&8), Some(&-1.0));
        assert_eq!(mapped.dpad_x, -1.0);
        assert!(mapped.resolve().left && !mapped.resolve().right);
    }

    #[test]
    fn calibration_format_tags_new_recordings_and_defaults_to_legacy() {
        // Files written before the bundled mappings were enabled carry no
        // format key and must load as format 0 (legacy); anything recorded
        // now is stamped with the current format.
        let legacy: CalibrationStore = toml::from_str(
            "[gamepads.0123]\nup = { kind = \"axis\", code = 49, positive = false }\n",
        )
        .unwrap();
        let cal = legacy.get("0123").unwrap();
        assert_eq!(cal.format, 0);
        assert_eq!(cal.up, axis(49, false));

        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        // The pad arrives first: what it reports as it connects was not
        // pressed by anyone, so nothing is captured from it.
        session.advance(pad.clone(), &axes, &buttons);
        for step in 0..5 {
            // Down, then up: a control is captured by being pressed,
            // which is only known once it comes back up.
            buttons.insert(0x9000 + step as u32, true);
            session.advance(pad.clone(), &axes, &buttons);
            buttons.clear();
            session.advance(pad.clone(), &axes, &buttons);
        }
        while session.can_skip() {
            session.skip_current();
        }
        assert!(session.done());
        assert_eq!(session.to_calibration().format, CALIBRATION_FORMAT);
    }

    #[test]
    fn resolve_ignores_axes_below_threshold_and_unbound_actions() {
        let cal = GamepadCalibration {
            up: axis(1, false),
            ..Default::default()
        };
        let mut axes = BTreeMap::new();
        axes.insert(1, -0.4); // below AXIS_ACTIVE_THRESHOLD
        let s = cal.resolve(&axes, &BTreeMap::new());
        assert!(!s.up);
        // Unbound actions (down/left/right/fire/button2) are always inactive.
        assert_eq!(s, JoystickState::default());

        axes.insert(1, -0.9);
        assert!(cal.resolve(&axes, &BTreeMap::new()).up);
    }

    #[test]
    fn alternate_directions_or_with_their_primaries() {
        // A stick on the primary steps and a d-pad on the alternates:
        // either steers, and releasing one while the other is held
        // keeps the line asserted.
        let cal = GamepadCalibration {
            up: axis(1, false),
            down: axis(1, true),
            left: axis(0, false),
            right: axis(0, true),
            up_alt: button(0x9000),
            down_alt: button(0x9001),
            left_alt: button(0x9002),
            right_alt: button(0x9003),
            ..Default::default()
        };
        let mut axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        assert_eq!(cal.resolve(&axes, &buttons), JoystickState::default());

        buttons.insert(0x9003, true);
        let s = cal.resolve(&axes, &buttons);
        assert!(s.right && !s.left && !s.up && !s.down);

        axes.insert(1, -0.9);
        let s = cal.resolve(&axes, &buttons);
        assert!(s.up && s.right);

        buttons.clear();
        let s = cal.resolve(&axes, &buttons);
        assert!(s.up && !s.right);

        // A calibration without alternates (every file before them) is
        // unchanged: the primaries alone decide.
        let primaries_only = GamepadCalibration {
            up_alt: None,
            down_alt: None,
            left_alt: None,
            right_alt: None,
            ..cal
        };
        buttons.insert(0x9003, true);
        let s = primaries_only.resolve(&axes, &buttons);
        assert!(s.up && !s.right);
    }

    #[test]
    fn stick_deflection_shows_through_an_axis_pair() {
        // A direction pair bound to the two ends of one raw axis is a
        // stick, and its deflection is reported (up and right positive)
        // for Gamepad Mouse to pace the pointer by. Anything else -- a
        // d-pad, or two different axes -- reports none.
        let stick = GamepadCalibration {
            up: axis(1, false),
            down: axis(1, true),
            left: axis(0, false),
            right: axis(0, true),
            ..Default::default()
        };
        let mut axes = BTreeMap::new();
        assert_eq!(stick.resolve_pad(&axes, &BTreeMap::new()).stick, (0.0, 0.0));
        axes.insert(1, -0.25); // raw negative = the end bound to Up
        axes.insert(0, 0.75);
        assert_eq!(
            stick.resolve_pad(&axes, &BTreeMap::new()).stick,
            (0.75, 0.25)
        );
        // An inverted recording (up on the positive end) still reads up
        // as positive: the sign follows the binding, not the axis.
        let inverted = GamepadCalibration {
            up: axis(1, true),
            down: axis(1, false),
            ..stick.clone()
        };
        assert_eq!(
            inverted.resolve_pad(&axes, &BTreeMap::new()).stick,
            (0.75, -0.25)
        );

        let dpad = GamepadCalibration {
            up: button(0x9000),
            down: button(0x9001),
            left: axis(0, false),
            right: axis(2, true), // two different axes are not a pair
            ..Default::default()
        };
        axes.insert(2, 1.0);
        assert_eq!(dpad.resolve_pad(&axes, &BTreeMap::new()).stick, (0.0, 0.0));

        // With a stick on both the primaries and the alternates, the one
        // pushed further is the deflection reported.
        let two_sticks = GamepadCalibration {
            up_alt: axis(3, true),
            down_alt: axis(3, false),
            left_alt: axis(2, false),
            right_alt: axis(2, true),
            ..stick
        };
        axes.insert(3, 0.9);
        axes.insert(2, -0.5);
        assert_eq!(
            two_sticks.resolve_pad(&axes, &BTreeMap::new()).stick,
            (0.75, 0.9)
        );
    }

    #[test]
    fn calibration_session_walks_steps_with_neutral_gating() {
        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let mut axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();

        // No pad yet: nothing happens, pad arrival reports a change.
        assert!(!session.advance(None, &axes, &buttons));
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));

        // A deflected axis, pressed and released, captures step 0 (Up).
        // Nothing is captured while it is down: until it comes up it
        // could still turn out to be a hold.
        axes.insert(0x31, -0.9);
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));
        axes.clear();
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(1));
        assert_eq!(session.binding_text(0), "axis 31-");

        // The other way on the same axis is a different control, and it
        // binds step 1 the same way.
        axes.insert(0x31, 0.9);
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        axes.clear();
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.binding_text(1), "axis 31+");

        // Buttons capture too; required steps cannot be skipped.
        assert!(!session.can_skip());
        for expected_step in 2..5 {
            assert_eq!(session.current_step(), Some(expected_step));
            axes.clear();
            buttons.clear();
            buttons.insert(0x9000 + expected_step as u32, true);
            session.advance(pad.clone(), &axes, &buttons);
            buttons.clear();
            assert!(session.advance(pad.clone(), &axes, &buttons));
        }
        assert_eq!(session.binding_text(4), "button 9004");

        // The remaining CD32 extras are optional: skip them all.
        while !session.done() {
            assert!(session.can_skip());
            session.skip_current();
        }
        assert!(session.done());
        assert_eq!(session.binding_text(5), "skipped");

        let cal = session.to_calibration();
        assert_eq!(cal.up, axis(0x31, false));
        assert_eq!(cal.down, axis(0x31, true));
        assert_eq!(cal.fire, button(0x9004));
        assert_eq!(cal.green, None);
        assert_eq!(cal.quit, None);
    }

    #[test]
    fn quit_binding_resolves_host_side_only() {
        // A pad button bound to Quit must surface as the host-side quit
        // flag and never assert any emulated port line.
        let cal = GamepadCalibration {
            fire: button(0x90001),
            quit: button(0x9000A),
            ..Default::default()
        };
        let mut buttons = BTreeMap::new();
        buttons.insert(0x9000A, true);
        let pad = cal.resolve_pad(&BTreeMap::new(), &buttons);
        assert!(pad.quit);
        assert_eq!(pad.joystick, JoystickState::default());

        // Fire alone drives the port and leaves quit inactive; with
        // neither host control bound (older calibration files) nothing
        // ever arms a quit.
        buttons.clear();
        buttons.insert(0x90001, true);
        let pad = cal.resolve_pad(&BTreeMap::new(), &buttons);
        assert!(!pad.quit && pad.joystick.fire);
        let unbound = GamepadCalibration {
            fire: button(0x90001),
            ..Default::default()
        };
        assert!(!unbound.resolve_pad(&BTreeMap::new(), &buttons).quit);
    }

    #[test]
    fn menu_binding_stands_in_for_an_unbound_quit() {
        // A calibration that bound a Menu button but skipped Quit still
        // has a way out: the Menu button is the Quit hotkey too (the
        // window only acts on a hold), the same as Select on the
        // standard layout.
        let menu_only = GamepadCalibration {
            menu: button(0x9000B),
            ..Default::default()
        };
        let mut buttons = BTreeMap::new();
        buttons.insert(0x9000B, true);
        let pad = menu_only.resolve_pad(&BTreeMap::new(), &buttons);
        assert!(pad.menu && pad.quit);

        // Binding a separate Quit control takes the hold to itself: the
        // Menu button then only opens the menu.
        let both = GamepadCalibration {
            menu: button(0x9000B),
            quit: button(0x9000A),
            ..Default::default()
        };
        let pad = both.resolve_pad(&BTreeMap::new(), &buttons);
        assert!(pad.menu && !pad.quit);
        buttons.clear();
        buttons.insert(0x9000A, true);
        let pad = both.resolve_pad(&BTreeMap::new(), &buttons);
        assert!(!pad.menu && pad.quit);
    }

    #[test]
    fn calibration_session_captures_optional_quit_and_alternate_steps() {
        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        session.advance(pad.clone(), &axes, &buttons);
        let mut press = |session: &mut CalibrationSession, code: u32| {
            // Down, then up: a control is captured by being pressed,
            // which is only known once it comes back up.
            buttons.insert(code, true);
            session.advance(pad.clone(), &axes, &buttons);
            buttons.clear();
            session.advance(pad.clone(), &axes, &buttons)
        };

        // Capture the required steps, then skip the CD32 extras and the
        // Menu button to land on the optional Quit step.
        for step in 0..5 {
            press(&mut session, 0x9000 + step as u32);
        }
        const QUIT: usize = 12;
        assert_eq!(CAL_STEPS[QUIT].0, "Quit Copperline");
        while session.can_skip() && session.current_step() != Some(QUIT) {
            session.skip_current();
        }
        assert_eq!(session.current_step(), Some(QUIT));
        assert!(session.can_skip());
        assert!(press(&mut session, 0x900FF));
        assert_eq!(session.to_calibration().quit, button(0x900FF));

        // The alternate directions come last, each optional: a second
        // Up is bound, the other three are skipped.
        assert_eq!(session.current_step(), Some(QUIT + 1));
        assert_eq!(CAL_STEPS[QUIT + 1].0, "Up (alternate)");
        assert!(press(&mut session, 0x90010));
        while session.can_skip() {
            session.skip_current();
        }
        assert!(session.done());
        let cal = session.to_calibration();
        assert_eq!(cal.up_alt, button(0x90010));
        assert_eq!(cal.down_alt, None);
        assert_eq!(cal.left_alt, None);
        assert_eq!(cal.right_alt, None);

        // The post-capture live test reports the held bindings by name,
        // alternates included, and the alternate drives its line.
        let mut buttons = BTreeMap::new();
        buttons.insert(0x900FF, true);
        buttons.insert(0x90010, true);
        assert!(session.advance(pad, &axes, &buttons));
        assert_eq!(session.live_test(), "Quit Copperline, Up (alternate)");
        assert!(session.live_pad().joystick.up && session.live_pad().quit);
    }

    #[test]
    fn calibration_session_pauses_while_pad_disconnected() {
        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let mut axes = BTreeMap::new();
        let buttons = BTreeMap::new();
        session.advance(pad.clone(), &axes, &buttons);
        session.advance(pad.clone(), &axes, &buttons); // neutral seen

        // Pad gone: deflections are ignored until it returns, but the pad
        // identity is kept so a finished session could still be saved.
        assert!(session.advance(None, &axes, &buttons));
        assert!(!session.connected());
        assert_eq!(session.pad_name(), Some("Pad"));
        axes.insert(0x30, 1.0);
        assert!(!session.advance(None, &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));

        // On reconnect the stale deflection must not capture: it was
        // already there, so nobody pressed it. A fresh push does.
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));
        axes.clear();
        session.advance(pad.clone(), &axes, &buttons);
        axes.insert(0x30, 1.0);
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        axes.clear();
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(1));
    }

    #[test]
    fn calibration_store_round_trips_through_toml() {
        let mut store = CalibrationStore::default();
        store.gamepads.insert(
            "abc123".to_string(),
            GamepadCalibration {
                up: axis(5, false),
                fire: button(9),
                quit: button(11),
                up_alt: button(12),
                ..Default::default()
            },
        );
        let text = toml::to_string_pretty(&store).unwrap();
        let back: CalibrationStore = toml::from_str(&text).unwrap();
        assert_eq!(back.get("abc123").unwrap().up, axis(5, false));
        assert_eq!(back.get("abc123").unwrap().fire, button(9));
        assert_eq!(back.get("abc123").unwrap().quit, button(11));
        assert_eq!(back.get("abc123").unwrap().up_alt, button(12));
        assert!(back.get("abc123").unwrap().down.is_none());
        assert!(back.get("abc123").unwrap().down_alt.is_none());

        // A file written before the alternates existed carries none of
        // their keys and loads with every alternate unbound.
        let older: CalibrationStore = toml::from_str(
            "[gamepads.0123]\nup = { kind = \"axis\", code = 49, positive = false }\n\
             down = { kind = \"axis\", code = 49, positive = true }\n",
        )
        .unwrap();
        let cal = older.get("0123").unwrap();
        assert_eq!(cal.down, axis(49, true));
        assert!(cal.up_alt.is_none() && cal.right_alt.is_none());
    }
}

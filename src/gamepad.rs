// SPDX-License-Identifier: GPL-3.0-or-later

//! Generic USB gamepad support via a one-time guided calibration.
//!
//! gilrs is used with its SDL controller mappings DISABLED, so we read raw
//! axis/button event codes. That works for any pad regardless of how well it
//! is covered by SDL_GameControllerDB (many cheap "retro" pads have broken or
//! missing entries). Calibration records which raw input drives each Amiga
//! port-2 joystick direction and button, keyed by controller UUID and
//! persisted to a config file. Because calibration records the actual input
//! the user pushes for each direction, it needs no axis-sign or axis-order
//! assumptions -- inversion and layout fall out automatically.
//!
//! Besides the emulated controls, calibration can optionally bind one pad
//! input as a host-side Quit hotkey. That binding never reaches the emulated
//! port; the window turns a sustained hold of it into an application exit.

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadState {
    pub joystick: JoystickState,
    /// The calibrated Quit hotkey is currently held. Host-side only: the
    /// window turns a sustained hold into an application exit.
    pub quit: bool,
}

impl GamepadCalibration {
    fn resolve_pad(&self, axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> PadState {
        PadState {
            joystick: self.resolve(axes, buttons),
            quit: self.quit.is_some_and(|i| i.active(axes, buttons)),
        }
    }

    fn resolve(&self, axes: &BTreeMap<u32, f32>, buttons: &BTreeMap<u32, bool>) -> JoystickState {
        let active = |input: &Option<RawInput>| input.is_some_and(|i| i.active(axes, buttons));
        JoystickState {
            up: active(&self.up),
            down: active(&self.down),
            left: active(&self.left),
            right: active(&self.right),
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

/// A gilrs instance with SDL controller mappings turned off, plus the current
/// raw axis/button state accumulated from its events.
struct RawGamepads {
    gilrs: gilrs::Gilrs,
    axes: BTreeMap<u32, f32>,
    buttons: BTreeMap<u32, bool>,
}

impl RawGamepads {
    fn new() -> Result<Self> {
        let gilrs = gilrs::GilrsBuilder::new()
            .add_included_mappings(false)
            .add_env_mappings(false)
            .build()
            .map_err(|e| anyhow!("gamepad init: {e}"))?;
        Ok(Self {
            gilrs,
            axes: BTreeMap::new(),
            buttons: BTreeMap::new(),
        })
    }

    /// Drain pending events into the raw axis/button state.
    fn pump(&mut self) {
        while let Some(event) = self.gilrs.next_event() {
            match event.event {
                gilrs::EventType::AxisChanged(_, value, code) => {
                    self.axes.insert(code.into_u32(), value);
                }
                gilrs::EventType::ButtonChanged(_, value, code) => {
                    self.buttons
                        .insert(code.into_u32(), value >= AXIS_ACTIVE_THRESHOLD);
                }
                gilrs::EventType::ButtonPressed(_, code) => {
                    self.buttons.insert(code.into_u32(), true);
                }
                gilrs::EventType::ButtonReleased(_, code) => {
                    self.buttons.insert(code.into_u32(), false);
                }
                gilrs::EventType::Disconnected => {
                    self.axes.clear();
                    self.buttons.clear();
                }
                _ => {}
            }
        }
    }

    fn first_gamepad(&self) -> Option<gilrs::GamepadId> {
        self.gilrs.gamepads().next().map(|(id, _)| id)
    }
}

/// Runtime reader: maps the first connected, calibrated pad to the emulated
/// port-2 joystick. Held by the window and polled once per scheduler quantum.
pub struct GamepadReader {
    raw: Option<RawGamepads>,
    store: CalibrationStore,
    warned_uncalibrated: bool,
    /// UUID of the pad whose calibration we last announced as in use, so we
    /// log it once per pad (and again if a different pad is plugged in).
    logged_calibrated: Option<String>,
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
            logged_calibrated: None,
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
        self.logged_calibrated = None;
        Ok(())
    }

    /// Whether a connected, calibrated pad carries a Quit-hotkey binding.
    /// The window keeps the event loop polling while one is present: gilrs
    /// is polled, not evented, so a paused or powered-off machine would
    /// otherwise never observe the hold.
    pub fn quit_hotkey_present(&mut self) -> bool {
        let Some(raw) = self.raw.as_mut() else {
            return false;
        };
        raw.pump();
        let Some(id) = raw.first_gamepad() else {
            return false;
        };
        let uuid = uuid_hex(raw.gilrs.gamepad(id).uuid());
        self.store.get(&uuid).is_some_and(|cal| cal.quit.is_some())
    }

    /// Poll the gamepad and return its resolved state (emulated joystick
    /// lines plus the host-side Quit hotkey), or `None` when there is no
    /// pad or no calibration for the connected one.
    pub fn poll(&mut self) -> Option<PadState> {
        let raw = self.raw.as_mut()?;
        raw.pump();
        let id = raw.first_gamepad()?;
        let pad = raw.gilrs.gamepad(id);
        let uuid = uuid_hex(pad.uuid());
        match self.store.get(&uuid) {
            Some(cal) => {
                if self.logged_calibrated.as_deref() != Some(uuid.as_str()) {
                    self.logged_calibrated = Some(uuid.clone());
                    log::info!("using saved calibration for gamepad \"{}\"", pad.name());
                }
                Some(cal.resolve_pad(&raw.axes, &raw.buttons))
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
/// skipped (pads without CD32 extras skip the optional ones).
const CAL_STEPS: [(&str, bool); 12] = [
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
    ("Quit Copperline", false),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalPhase {
    /// Wait for every control to return to rest before sampling, so a
    /// held input from the previous step is not recaptured.
    WaitNeutral,
    Capture,
}

/// A guided calibration in progress: which step is being prompted, what
/// has been captured so far, and the connected pad's identity. Fed raw
/// gamepad state by [`GamepadReader::calibration_tick`].
pub struct CalibrationSession {
    bindings: [Option<RawInput>; CAL_STEPS.len()],
    step: usize,
    phase: CalPhase,
    /// The last pad seen, kept across disconnects so a finished session
    /// can still be saved if the pad is unplugged at the end.
    pad: Option<(String, String)>,
    connected: bool,
    backend_missing: bool,
    /// Once every step is captured, the names of the bindings currently
    /// being pressed, so the user can test the calibration before saving.
    live_test: String,
}

impl CalibrationSession {
    pub fn new() -> Self {
        Self {
            bindings: [None; CAL_STEPS.len()],
            step: 0,
            phase: CalPhase::WaitNeutral,
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

    /// Skip the current (optional) step.
    pub fn skip_current(&mut self) {
        if self.can_skip() {
            self.bindings[self.step] = None;
            self.step += 1;
            self.phase = CalPhase::WaitNeutral;
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
            // A (re)connected pad may still report a stale deflection;
            // require neutral before sampling for the current prompt.
            self.phase = CalPhase::WaitNeutral;
        }
        if !self.connected {
            return changed;
        }
        if self.done() {
            // Live test: show which captured bindings are active right now
            // so the calibration can be verified before saving.
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
            return changed;
        }
        match self.phase {
            CalPhase::WaitNeutral => {
                if raw_state_neutral(axes, buttons) {
                    self.phase = CalPhase::Capture;
                    // The phase flip itself is invisible; no redraw needed.
                }
            }
            CalPhase::Capture => {
                if let Some(input) = strongest_input_from(axes, buttons) {
                    self.bindings[self.step] = Some(input);
                    self.step += 1;
                    self.phase = CalPhase::WaitNeutral;
                    changed = true;
                }
            }
        }
        changed
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
            quit: b[11],
        }
    }
}

impl Default for CalibrationSession {
    fn default() -> Self {
        Self::new()
    }
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
        quit: capture(
            &mut raw,
            "the Quit Copperline hotkey (or wait to skip)",
            false,
        )?,
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
            green: None,
            yellow: None,
            play: None,
            rwd: None,
            ffw: None,
            quit: None,
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
    fn calibration_session_walks_steps_with_neutral_gating() {
        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let mut axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();

        // No pad yet: nothing happens, pad arrival reports a change.
        assert!(!session.advance(None, &axes, &buttons));
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));

        // Neutral first, then a deflected axis captures step 0 (Up).
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        axes.insert(0x31, -0.9);
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(1));
        assert_eq!(session.binding_text(0), "axis 31-");

        // Still held: the same input must not capture step 1 until the
        // controls return to neutral.
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(1));
        axes.clear();
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        axes.insert(0x31, 0.9);
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.binding_text(1), "axis 31+");

        // Buttons capture too; required steps cannot be skipped.
        assert!(!session.can_skip());
        for expected_step in 2..5 {
            assert_eq!(session.current_step(), Some(expected_step));
            axes.clear();
            buttons.clear();
            session.advance(pad.clone(), &axes, &buttons);
            buttons.insert(0x9000 + expected_step as u32, true);
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

        // Fire alone drives the port and leaves quit inactive; an unbound
        // quit (older calibration files) never activates.
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
    fn calibration_session_captures_optional_quit_step() {
        let mut session = CalibrationSession::new();
        let pad = Some(("Pad".to_string(), "abc123".to_string()));
        let axes = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        session.advance(pad.clone(), &axes, &buttons);

        // Capture the required steps, then skip the CD32 extras to land
        // on the final (optional) Quit step.
        for step in 0..5 {
            session.advance(pad.clone(), &axes, &buttons); // neutral
            buttons.insert(0x9000 + step as u32, true);
            session.advance(pad.clone(), &axes, &buttons);
            buttons.clear();
        }
        while session.can_skip() && session.current_step() != Some(CAL_STEPS.len() - 1) {
            session.skip_current();
        }
        assert_eq!(session.current_step(), Some(CAL_STEPS.len() - 1));
        assert!(session.can_skip());

        session.advance(pad.clone(), &axes, &buttons); // neutral
        buttons.insert(0x900FF, true);
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert!(session.done());
        assert_eq!(session.to_calibration().quit, button(0x900FF));

        // The post-capture live test reports the held quit binding by name.
        assert!(session.advance(pad, &axes, &buttons));
        assert_eq!(session.live_test(), "Quit Copperline");
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

        // On reconnect the stale deflection must not capture; the session
        // requires neutral first, then captures a fresh push.
        assert!(session.advance(pad.clone(), &axes, &buttons));
        assert!(!session.advance(pad.clone(), &axes, &buttons));
        assert_eq!(session.current_step(), Some(0));
        axes.clear();
        session.advance(pad.clone(), &axes, &buttons);
        axes.insert(0x30, 1.0);
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
                ..Default::default()
            },
        );
        let text = toml::to_string_pretty(&store).unwrap();
        let back: CalibrationStore = toml::from_str(&text).unwrap();
        assert_eq!(back.get("abc123").unwrap().up, axis(5, false));
        assert_eq!(back.get("abc123").unwrap().fire, button(9));
        assert_eq!(back.get("abc123").unwrap().quit, button(11));
        assert!(back.get("abc123").unwrap().down.is_none());
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-keyboard to controller mapping, and the autofire policy that rides on
//! top of it.
//!
//! Copperline drives an emulated game port from three host sources: a physical
//! gamepad (calibrated in `gamepad.rs`), scripted `--joy-after` events, and the
//! host keyboard. This module owns the last of those: which host keys stand in
//! for which controller control, for each of the two keyboard mappings, plus
//! the defaults and the persisted overrides.
//!
//! Two mappings exist so one keyboard can drive a two-controller setup. They
//! must not overlap: a key bound in both would drive both ports at once.
//!
//! Persistence follows the gamepad-calibration precedent: this is a host
//! preference, not part of the emulated machine, so it lives in the per-user
//! config directory (`keymap.toml`) rather than in a machine config TOML.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

/// One control on an emulated controller, in the order the remap UI lists
/// them. Directions and the two joystick buttons apply to every device; the
/// rest are CD32 pad buttons, inert on a plain joystick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JoyControl {
    Up,
    Down,
    Left,
    Right,
    /// Fire / red / button 1: /FIRx on the port, and the CD32 pad's red.
    Fire,
    /// Button 2 / blue: POTxY on the port, and the CD32 pad's blue.
    Button2,
    Green,
    Yellow,
    Play,
    Rewind,
    Forward,
}

/// Every control, in UI order.
pub const CONTROLS: [JoyControl; 11] = [
    JoyControl::Up,
    JoyControl::Down,
    JoyControl::Left,
    JoyControl::Right,
    JoyControl::Fire,
    JoyControl::Button2,
    JoyControl::Green,
    JoyControl::Yellow,
    JoyControl::Play,
    JoyControl::Rewind,
    JoyControl::Forward,
];

/// The two independent keyboard mappings (see the module docs).
pub const MAPPING_COUNT: usize = 2;

impl JoyControl {
    /// Stable index into a per-control array.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Label shown in the remap UI.
    pub const fn label(self) -> &'static str {
        match self {
            JoyControl::Up => "Up",
            JoyControl::Down => "Down",
            JoyControl::Left => "Left",
            JoyControl::Right => "Right",
            JoyControl::Fire => "Fire / Red",
            JoyControl::Button2 => "Button 2 / Blue",
            JoyControl::Green => "Green",
            JoyControl::Yellow => "Yellow",
            JoyControl::Play => "Play",
            JoyControl::Rewind => "Rewind",
            JoyControl::Forward => "Forward",
        }
    }

    /// Key used in the persisted TOML, and by the scripted-input vocabulary.
    pub const fn key(self) -> &'static str {
        match self {
            JoyControl::Up => "up",
            JoyControl::Down => "down",
            JoyControl::Left => "left",
            JoyControl::Right => "right",
            JoyControl::Fire => "fire",
            JoyControl::Button2 => "button2",
            JoyControl::Green => "green",
            JoyControl::Yellow => "yellow",
            JoyControl::Play => "play",
            JoyControl::Rewind => "rwd",
            JoyControl::Forward => "ffw",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        CONTROLS.into_iter().find(|c| c.key() == key)
    }
}

/// Host keys a mapping may bind. Deliberately not every `KeyCode`: this is the
/// set a controller binding makes sense on, and it doubles as the name table
/// (names are the `KeyCode` debug spellings, so they cannot drift from winit).
const BINDABLE_KEYS: &[KeyCode] = &[
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0,
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::Space,
    KeyCode::Enter,
    KeyCode::Tab,
    KeyCode::Backspace,
    KeyCode::ControlLeft,
    KeyCode::ControlRight,
    KeyCode::AltLeft,
    KeyCode::AltRight,
    KeyCode::ShiftLeft,
    KeyCode::ShiftRight,
    KeyCode::SuperLeft,
    KeyCode::SuperRight,
    KeyCode::Comma,
    KeyCode::Period,
    KeyCode::Slash,
    KeyCode::Semicolon,
    KeyCode::Quote,
    KeyCode::BracketLeft,
    KeyCode::BracketRight,
    KeyCode::Backslash,
    KeyCode::Minus,
    KeyCode::Equal,
    KeyCode::Backquote,
    KeyCode::Insert,
    KeyCode::Delete,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Numpad0,
    KeyCode::Numpad1,
    KeyCode::Numpad2,
    KeyCode::Numpad3,
    KeyCode::Numpad4,
    KeyCode::Numpad5,
    KeyCode::Numpad6,
    KeyCode::Numpad7,
    KeyCode::Numpad8,
    KeyCode::Numpad9,
    KeyCode::NumpadDecimal,
    KeyCode::NumpadEnter,
    KeyCode::NumpadAdd,
    KeyCode::NumpadSubtract,
    KeyCode::NumpadMultiply,
    KeyCode::NumpadDivide,
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
];

/// Whether a host key may be bound to a controller control.
pub fn is_bindable(code: KeyCode) -> bool {
    BINDABLE_KEYS.contains(&code)
}

/// Position of a bindable key in [`BINDABLE_KEYS`], which is its bit in
/// [`HeldKeys`].
fn key_index(code: KeyCode) -> Option<usize> {
    BINDABLE_KEYS.iter().position(|k| *k == code)
}

/// The bindable host keys currently held down, as a bitset.
///
/// Held state is tracked per *key* rather than per control on purpose: a
/// control may have several keys bound to it (fire has four by default), and
/// releasing one alias while another is still down must not release the
/// control. Counting presses per control would drift the first time a release
/// arrives without its press -- which happens whenever the window loses focus
/// mid-press.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HeldKeys {
    bits: [u64; BINDABLE_KEYS.len().div_ceil(64)],
}

impl HeldKeys {
    pub fn set(&mut self, code: KeyCode, held: bool) {
        let Some(index) = key_index(code) else { return };
        let (word, bit) = (index / 64, 1u64 << (index % 64));
        if held {
            self.bits[word] |= bit;
        } else {
            self.bits[word] &= !bit;
        }
    }

    pub fn is_set(&self, code: KeyCode) -> bool {
        match key_index(code) {
            Some(index) => self.bits[index / 64] & (1u64 << (index % 64)) != 0,
            None => false,
        }
    }

    pub fn any_held(&self) -> bool {
        self.bits.iter().any(|w| *w != 0)
    }
}

/// Persisted name of a host key: winit's own spelling, so the file stays
/// readable and the table cannot fall out of step with the enum.
pub fn key_name(code: KeyCode) -> String {
    format!("{code:?}")
}

/// The bindable key a persisted name refers to.
pub fn key_from_name(name: &str) -> Option<KeyCode> {
    BINDABLE_KEYS.iter().copied().find(|k| key_name(*k) == name)
}

/// Keyboard bindings for one of the two mappings: the host keys held for each
/// control. A control may have several keys (compact keyboards lack the
/// right-hand modifiers, so fire has left-hand aliases by default) or none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardMapping {
    keys: [Vec<KeyCode>; CONTROLS.len()],
}

impl KeyboardMapping {
    fn empty() -> Self {
        Self {
            keys: std::array::from_fn(|_| Vec::new()),
        }
    }

    fn from_pairs(pairs: &[(JoyControl, &[KeyCode])]) -> Self {
        let mut map = Self::empty();
        for (control, keys) in pairs {
            map.keys[control.index()] = keys.to_vec();
        }
        map
    }

    pub fn keys(&self, control: JoyControl) -> &[KeyCode] {
        &self.keys[control.index()]
    }

    /// Bind `code` to `control`, dropping it from whatever it was bound to
    /// before. A key can only mean one thing, and silently leaving a stale
    /// binding behind would make a port fire when the user pressed "up".
    pub fn bind(&mut self, control: JoyControl, code: KeyCode) {
        for keys in &mut self.keys {
            keys.retain(|k| *k != code);
        }
        self.keys[control.index()].push(code);
    }

    pub fn clear(&mut self, control: JoyControl) {
        self.keys[control.index()].clear();
    }

    /// The control `code` drives, if any.
    fn control_for(&self, code: KeyCode) -> Option<JoyControl> {
        CONTROLS
            .into_iter()
            .find(|c| self.keys[c.index()].contains(&code))
    }

    /// Whether any key bound to `control` is currently held.
    pub fn is_held(&self, control: JoyControl, held: &HeldKeys) -> bool {
        self.keys(control).iter().any(|k| held.is_set(*k))
    }

    /// The controller state a set of held host keys produces under this
    /// mapping. Several keys bound to one control simply OR together.
    pub fn joystick_state(&self, held: &HeldKeys) -> crate::gamepad::JoystickState {
        use JoyControl as C;
        crate::gamepad::JoystickState {
            up: self.is_held(C::Up, held),
            down: self.is_held(C::Down, held),
            left: self.is_held(C::Left, held),
            right: self.is_held(C::Right, held),
            fire: self.is_held(C::Fire, held),
            button2: self.is_held(C::Button2, held),
            green: self.is_held(C::Green, held),
            yellow: self.is_held(C::Yellow, held),
            play: self.is_held(C::Play, held),
            rwd: self.is_held(C::Rewind, held),
            ffw: self.is_held(C::Forward, held),
        }
    }

    /// Human-readable summary of a control's bindings for the remap UI.
    pub fn binding_text(&self, control: JoyControl) -> String {
        let keys = self.keys(control);
        if keys.is_empty() {
            return "-".to_string();
        }
        keys.iter()
            .map(|k| short_key_label(*k))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// Compact display name for a key, so a row of aliases fits the panel.
pub fn short_key_label(code: KeyCode) -> String {
    let name = key_name(code);
    if let Some(rest) = name.strip_prefix("Key") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("Digit") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("Arrow") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("Numpad") {
        return format!("KP{rest}");
    }
    match code {
        KeyCode::ControlLeft => "LCtrl".to_string(),
        KeyCode::ControlRight => "RCtrl".to_string(),
        KeyCode::AltLeft => "LAlt".to_string(),
        KeyCode::AltRight => "RAlt".to_string(),
        KeyCode::ShiftLeft => "LShift".to_string(),
        KeyCode::ShiftRight => "RShift".to_string(),
        KeyCode::SuperLeft => "LSuper".to_string(),
        KeyCode::SuperRight => "RSuper".to_string(),
        _ => name,
    }
}

/// Both keyboard mappings.
///
/// Mapping 0 (FS-UAE-compatible, plus left-hand fire keys): cursor keys for
/// directions; Right Ctrl / Right Alt / Left Ctrl / C for fire; Left Alt or X
/// for the second button; CD32 extras on D/S/Return/Z/A. On a mouse port the
/// same keys become pointer motion, with fire = left button, button 2 = right,
/// green = middle.
///
/// Mapping 1 (numpad): 8/2/4/6 for directions, 0 for fire, `.` for the second
/// button, numpad Enter for play. It stands in for the gamepad when a
/// two-controller setup has no physical pad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMap {
    mappings: [KeyboardMapping; MAPPING_COUNT],
}

impl Default for KeyMap {
    fn default() -> Self {
        Self {
            mappings: [
                KeyboardMapping::from_pairs(&[
                    (JoyControl::Up, &[KeyCode::ArrowUp]),
                    (JoyControl::Down, &[KeyCode::ArrowDown]),
                    (JoyControl::Left, &[KeyCode::ArrowLeft]),
                    (JoyControl::Right, &[KeyCode::ArrowRight]),
                    (
                        JoyControl::Fire,
                        &[
                            KeyCode::ControlRight,
                            KeyCode::AltRight,
                            KeyCode::ControlLeft,
                            KeyCode::KeyC,
                        ],
                    ),
                    (JoyControl::Button2, &[KeyCode::AltLeft, KeyCode::KeyX]),
                    (JoyControl::Green, &[KeyCode::KeyD]),
                    (JoyControl::Yellow, &[KeyCode::KeyS]),
                    (JoyControl::Play, &[KeyCode::Enter]),
                    (JoyControl::Rewind, &[KeyCode::KeyZ]),
                    (JoyControl::Forward, &[KeyCode::KeyA]),
                ]),
                KeyboardMapping::from_pairs(&[
                    (JoyControl::Up, &[KeyCode::Numpad8]),
                    (JoyControl::Down, &[KeyCode::Numpad2]),
                    (JoyControl::Left, &[KeyCode::Numpad4]),
                    (JoyControl::Right, &[KeyCode::Numpad6]),
                    (JoyControl::Fire, &[KeyCode::Numpad0]),
                    (JoyControl::Button2, &[KeyCode::NumpadDecimal]),
                    (JoyControl::Play, &[KeyCode::NumpadEnter]),
                ]),
            ],
        }
    }
}

impl KeyMap {
    pub fn mapping(&self, index: usize) -> &KeyboardMapping {
        &self.mappings[index.min(MAPPING_COUNT - 1)]
    }

    pub fn mapping_mut(&mut self, index: usize) -> &mut KeyboardMapping {
        &mut self.mappings[index.min(MAPPING_COUNT - 1)]
    }

    /// Bind `code` in mapping `index`, first clearing it from the *other*
    /// mapping. The two mappings drive different ports, so a key shared
    /// between them would move both controllers at once.
    pub fn bind(&mut self, index: usize, control: JoyControl, code: KeyCode) {
        let index = index.min(MAPPING_COUNT - 1);
        for (i, mapping) in self.mappings.iter_mut().enumerate() {
            if i == index {
                mapping.bind(control, code);
            } else {
                for keys in &mut mapping.keys {
                    keys.retain(|k| *k != code);
                }
            }
        }
    }

    /// Which mapping and control a host key drives, if any. This is the hot
    /// lookup on every key event.
    pub fn lookup(&self, code: KeyCode) -> Option<(usize, JoyControl)> {
        self.mappings
            .iter()
            .enumerate()
            .find_map(|(i, m)| m.control_for(code).map(|c| (i, c)))
    }

    /// Load the persisted map, falling back to the defaults when there is no
    /// file (the common case) or it cannot be read.
    pub fn load() -> Self {
        let Some(path) = keymap_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<KeyMapStore>(&text) {
            Ok(store) => store.into_keymap(),
            Err(e) => {
                log::warn!("ignoring unreadable keyboard map {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path =
            keymap_path().ok_or_else(|| anyhow!("no config directory for the keyboard map"))?;
        crate::paths::ensure_parent(&path)?;
        std::fs::write(&path, toml::to_string_pretty(&KeyMapStore::from(self))?)?;
        log::info!("saved keyboard controller map to {}", path.display());
        Ok(())
    }
}

fn keymap_path() -> Option<std::path::PathBuf> {
    crate::paths::config_file("keymap.toml")
}

/// The persisted map's path, for tests that exercise Save and must put the
/// developer's own file back afterwards.
#[cfg(test)]
pub fn keymap_path_for_test() -> Option<std::path::PathBuf> {
    keymap_path()
}

/// Serde shape of `keymap.toml`: one table per mapping, control name to the
/// list of host key names. Unknown control or key names are dropped with a
/// warning rather than failing the load -- a map written by a newer build
/// should still start an older one.
#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyMapStore {
    #[serde(default)]
    controller1: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    controller2: BTreeMap<String, Vec<String>>,
}

impl KeyMapStore {
    fn into_keymap(self) -> KeyMap {
        KeyMap {
            mappings: [
                decode_mapping(&self.controller1),
                decode_mapping(&self.controller2),
            ],
        }
    }
}

impl From<&KeyMap> for KeyMapStore {
    fn from(map: &KeyMap) -> Self {
        Self {
            controller1: encode_mapping(map.mapping(0)),
            controller2: encode_mapping(map.mapping(1)),
        }
    }
}

fn encode_mapping(mapping: &KeyboardMapping) -> BTreeMap<String, Vec<String>> {
    CONTROLS
        .into_iter()
        .map(|c| {
            (
                c.key().to_string(),
                mapping.keys(c).iter().copied().map(key_name).collect(),
            )
        })
        .collect()
}

fn decode_mapping(table: &BTreeMap<String, Vec<String>>) -> KeyboardMapping {
    let mut mapping = KeyboardMapping::empty();
    for (control_key, names) in table {
        let Some(control) = JoyControl::from_key(control_key) else {
            log::warn!("keymap.toml: ignoring unknown control {control_key:?}");
            continue;
        };
        for name in names {
            match key_from_name(name) {
                Some(code) => mapping.keys[control.index()].push(code),
                None => log::warn!("keymap.toml: ignoring unknown key {name:?}"),
            }
        }
    }
    mapping
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_layouts_and_do_not_overlap() {
        let map = KeyMap::default();
        assert_eq!(map.lookup(KeyCode::ArrowUp), Some((0, JoyControl::Up)));
        assert_eq!(map.lookup(KeyCode::Numpad8), Some((1, JoyControl::Up)));
        // Fire keeps all four default aliases on the one control.
        assert_eq!(
            map.lookup(KeyCode::ControlRight),
            Some((0, JoyControl::Fire))
        );
        assert_eq!(map.lookup(KeyCode::KeyC), Some((0, JoyControl::Fire)));
        assert_eq!(map.lookup(KeyCode::AltLeft), Some((0, JoyControl::Button2)));
        assert_eq!(map.lookup(KeyCode::F12), None);

        // No key drives both mappings: that would move two controllers at once.
        for &key in BINDABLE_KEYS {
            let in_0 = map.mapping(0).control_for(key).is_some();
            let in_1 = map.mapping(1).control_for(key).is_some();
            assert!(!(in_0 && in_1), "{key:?} is bound in both mappings");
        }
    }

    #[test]
    fn binding_a_key_moves_it_off_its_previous_control_and_mapping() {
        let mut map = KeyMap::default();
        map.bind(0, JoyControl::Up, KeyCode::KeyW);
        assert_eq!(map.lookup(KeyCode::KeyW), Some((0, JoyControl::Up)));

        // Rebinding the same key elsewhere moves it rather than duplicating.
        map.bind(0, JoyControl::Fire, KeyCode::KeyW);
        assert_eq!(map.lookup(KeyCode::KeyW), Some((0, JoyControl::Fire)));
        assert!(!map.mapping(0).keys(JoyControl::Up).contains(&KeyCode::KeyW));

        // And across mappings.
        map.bind(1, JoyControl::Down, KeyCode::KeyW);
        assert_eq!(map.lookup(KeyCode::KeyW), Some((1, JoyControl::Down)));
        assert!(map.mapping(0).control_for(KeyCode::KeyW).is_none());
    }

    #[test]
    fn clearing_a_control_unbinds_every_alias() {
        let mut map = KeyMap::default();
        assert_eq!(map.mapping(0).keys(JoyControl::Fire).len(), 4);
        map.mapping_mut(0).clear(JoyControl::Fire);
        assert_eq!(map.mapping(0).binding_text(JoyControl::Fire), "-");
        assert_eq!(map.lookup(KeyCode::ControlRight), None);
    }

    #[test]
    fn key_names_round_trip_through_the_persisted_form() {
        for &key in BINDABLE_KEYS {
            assert_eq!(key_from_name(&key_name(key)), Some(key), "{key:?}");
        }
        assert_eq!(key_from_name("NotAKey"), None);
    }

    #[test]
    fn keymap_round_trips_through_toml() {
        let mut map = KeyMap::default();
        map.bind(0, JoyControl::Up, KeyCode::KeyW);
        map.bind(1, JoyControl::Fire, KeyCode::NumpadAdd);
        map.mapping_mut(0).clear(JoyControl::Yellow);

        let text = toml::to_string_pretty(&KeyMapStore::from(&map)).unwrap();
        let back: KeyMapStore = toml::from_str(&text).unwrap();
        assert_eq!(back.into_keymap(), map);
    }

    #[test]
    fn unknown_control_and_key_names_are_dropped_not_fatal() {
        let text = r#"
            [controller1]
            up = ["ArrowUp", "NoSuchKey"]
            nosuchcontrol = ["KeyQ"]
        "#;
        let store: KeyMapStore = toml::from_str(text).unwrap();
        let map = store.into_keymap();
        assert_eq!(map.mapping(0).keys(JoyControl::Up), &[KeyCode::ArrowUp]);
        assert_eq!(map.lookup(KeyCode::KeyQ), None);
    }
}

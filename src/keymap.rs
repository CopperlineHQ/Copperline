// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-keyboard mapping: to the Amiga keyboard, and to the controllers.
//!
//! A host key reaches the Amiga keyboard by position: [`positional_rawkey`]
//! sends the rawkey of the Amiga key in the same place, whatever the host
//! layout prints on it, so the guest's own keymap decides what it types, as
//! on the real machine. The host key code is the scancode the host reports,
//! not the symbol its layout gives it, so a remap made in the host's keymap
//! (xmodmap, an XKB option) does not move it. [`KeyboardRemap`] is the remap
//! that does: per host key, a different Amiga key or none at all.
//!
//! Copperline drives an emulated game port from three host sources: a physical
//! gamepad (calibrated in `gamepad.rs`), scripted `--joy-after` events, and the
//! host keyboard. This module owns the last of those: which host keys stand in
//! for which controller control, for each of the two keyboard mappings, plus
//! the defaults and the persisted overrides. The autofire policy that gates
//! every source's fire button lives in [`crate::config`], since it is a
//! function of emulated time rather than of host keys. A key bound to a
//! controller control is taken by the controller while that mapping drives a
//! port, and types its Amiga key otherwise.
//!
//! A key means exactly one thing: binding it to a control unbinds it from
//! wherever it was, and the same rule is applied when loading a hand-written
//! file. Two mappings exist so one keyboard can drive a two-controller setup,
//! and they must not overlap either -- a key listed in both would bind to
//! whichever `lookup` reaches first, leaving the other silently dead.
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

/// The Amiga key a host key sits in the place of: the rawkey it sends when
/// nothing remaps it (see the module docs). Covers the alphanumeric block,
/// the modifiers as their nearest Amiga equivalents, function keys, cursor
/// keys and the keypad. Anything else returns None and types nothing.
pub fn positional_rawkey(code: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match code {
        // Letters (row-by-row, Amiga's funny layout)
        KeyA => 0x20,
        KeyB => 0x35,
        KeyC => 0x33,
        KeyD => 0x22,
        KeyE => 0x12,
        KeyF => 0x23,
        KeyG => 0x24,
        KeyH => 0x25,
        KeyI => 0x17,
        KeyJ => 0x26,
        KeyK => 0x27,
        KeyL => 0x28,
        KeyM => 0x37,
        KeyN => 0x36,
        KeyO => 0x18,
        KeyP => 0x19,
        KeyQ => 0x10,
        KeyR => 0x13,
        KeyS => 0x21,
        KeyT => 0x14,
        KeyU => 0x16,
        KeyV => 0x34,
        KeyW => 0x11,
        KeyX => 0x32,
        KeyY => 0x15,
        KeyZ => 0x31,
        // Top-row digits
        Digit1 => 0x01,
        Digit2 => 0x02,
        Digit3 => 0x03,
        Digit4 => 0x04,
        Digit5 => 0x05,
        Digit6 => 0x06,
        Digit7 => 0x07,
        Digit8 => 0x08,
        Digit9 => 0x09,
        Digit0 => 0x0A,
        // Punctuation
        Backquote => 0x00,
        Minus => 0x0B,
        Equal => 0x0C,
        Backslash => 0x0D,
        BracketLeft => 0x1A,
        BracketRight => 0x1B,
        Semicolon => 0x29,
        Quote => 0x2A,
        Comma => 0x38,
        Period => 0x39,
        Slash => 0x3A,
        // International keys: the ISO 102nd key between left Shift and
        // Z is Amiga rawkey $30; the Japanese Ro key sits in the same
        // matrix position on layouts that have it.
        IntlBackslash | IntlRo => 0x30,
        // Control
        Space => 0x40,
        Enter => 0x44,
        Backspace => 0x41,
        Tab => 0x42,
        Escape => 0x45,
        Delete => 0x46,
        // Amiga Help: F11 host-side (no dedicated host key exists).
        F11 => 0x5F,
        ShiftLeft => 0x60,
        ShiftRight => 0x61,
        CapsLock => 0x62,
        // The Amiga keyboard has a single Ctrl key (left side); there is no
        // right Ctrl. Map host ControlLeft to it. Host ControlRight has no
        // Amiga counterpart, so alias it to Right Amiga ($67) alongside
        // SuperRight -- many PC/laptop keyboards lack a right Super/Win key,
        // leaving Right Amiga otherwise unreachable.
        ControlLeft => 0x63,
        AltLeft => 0x64,
        AltRight => 0x65,
        SuperLeft => 0x66,
        SuperRight | ControlRight => 0x67,
        // Arrows
        ArrowUp => 0x4C,
        ArrowDown => 0x4D,
        ArrowRight => 0x4E,
        ArrowLeft => 0x4F,
        // Function keys
        F1 => 0x50,
        F2 => 0x51,
        F3 => 0x52,
        F4 => 0x53,
        F5 => 0x54,
        F6 => 0x55,
        F7 => 0x56,
        F8 => 0x57,
        F9 => 0x58,
        F10 => 0x59,
        // Numpad
        Numpad0 => 0x0F,
        Numpad1 => 0x1D,
        Numpad2 => 0x1E,
        Numpad3 => 0x1F,
        Numpad4 => 0x2D,
        Numpad5 => 0x2E,
        Numpad6 => 0x2F,
        Numpad7 => 0x3D,
        Numpad8 => 0x3E,
        Numpad9 => 0x3F,
        NumpadDecimal => 0x3C,
        NumpadEnter => 0x43,
        NumpadSubtract => 0x4A,
        NumpadAdd => 0x5E,
        NumpadMultiply => 0x5D,
        NumpadDivide => 0x5C,
        NumpadParenLeft => 0x5A,
        NumpadParenRight => 0x5B,
        _ => return None,
    })
}
/// Host keys a keyboard remap may move, beyond the controller-bindable set:
/// the ones that only make sense on the Amiga keyboard, and the spare keys a
/// PC or Mac keyboard has that an Amiga one does not. Every host key with a
/// [`positional_rawkey`] is in [`BINDABLE_KEYS`] or here, so any key that
/// types something can be moved, and the spares give the Amiga keys a host
/// keyboard lacks (Help, the keypad brackets) somewhere to live.
const REMAP_ONLY_KEYS: &[KeyCode] = &[
    KeyCode::CapsLock,
    KeyCode::Escape,
    KeyCode::F11,
    KeyCode::F12,
    KeyCode::F13,
    KeyCode::F14,
    KeyCode::F15,
    KeyCode::IntlBackslash,
    KeyCode::IntlRo,
    KeyCode::IntlYen,
    KeyCode::NumLock,
    KeyCode::NumpadParenLeft,
    KeyCode::NumpadParenRight,
    KeyCode::NumpadEqual,
    KeyCode::ScrollLock,
    KeyCode::Pause,
    KeyCode::PrintScreen,
    KeyCode::ContextMenu,
    KeyCode::Help,
];

/// Every host key a keyboard remap may move, in the order the Input Mapping
/// panel lists them.
fn remappable_keys() -> impl Iterator<Item = KeyCode> {
    BINDABLE_KEYS.iter().chain(REMAP_ONLY_KEYS.iter()).copied()
}

/// Whether a host key may be remapped to another Amiga key.
pub fn is_remappable(code: KeyCode) -> bool {
    BINDABLE_KEYS.contains(&code) || REMAP_ONLY_KEYS.contains(&code)
}

/// The remappable host key a persisted name refers to.
fn remappable_key_from_name(name: &str) -> Option<KeyCode> {
    remappable_keys().find(|k| key_name(*k) == name)
}

/// One key of the Amiga keyboard, for the remap table and the Input Mapping
/// panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmigaKey {
    /// The rawkey the keyboard MCU sends for it (RKM Libraries table 34-6).
    pub rawkey: u8,
    /// Persisted name in `keymap.toml`, one `parse_amiga_key` accepts.
    pub name: &'static str,
    /// What the Input Mapping panel calls it.
    pub label: &'static str,
}

const fn amiga_key(rawkey: u8, name: &'static str, label: &'static str) -> AmigaKey {
    AmigaKey {
        rawkey,
        name,
        label,
    }
}

/// The 96 keys of a full Amiga keyboard, in the order the Input Mapping panel
/// pages through them: the qualifiers and the keys a host keyboard places
/// differently first (they are what a remap is usually for), then the main
/// block row by row, then the keypad.
///
/// $2B and $30 are the two keys an ISO keyboard adds -- beside Return, and
/// between left Shift and Z -- and have no name a US cap gives them, so they
/// persist as their rawkeys.
pub const AMIGA_KEYS: &[AmigaKey] = &[
    amiga_key(0x63, "ctrl", "Ctrl"),
    amiga_key(0x62, "capslock", "Caps Lock"),
    amiga_key(0x60, "lshift", "Left Shift"),
    amiga_key(0x61, "rshift", "Right Shift"),
    amiga_key(0x64, "lalt", "Left Alt"),
    amiga_key(0x65, "ralt", "Right Alt"),
    amiga_key(0x66, "lamiga", "Left Amiga"),
    amiga_key(0x67, "ramiga", "Right Amiga"),
    amiga_key(0x5F, "help", "Help"),
    amiga_key(0x46, "del", "Del"),
    amiga_key(0x45, "esc", "Esc"),
    amiga_key(0x44, "return", "Return"),
    amiga_key(0x41, "backspace", "Backspace"),
    amiga_key(0x42, "tab", "Tab"),
    amiga_key(0x40, "space", "Space"),
    amiga_key(0x4C, "up", "Cursor Up"),
    amiga_key(0x4D, "down", "Cursor Down"),
    amiga_key(0x4F, "left", "Cursor Left"),
    amiga_key(0x4E, "right", "Cursor Right"),
    amiga_key(0x50, "f1", "F1"),
    amiga_key(0x51, "f2", "F2"),
    amiga_key(0x52, "f3", "F3"),
    amiga_key(0x53, "f4", "F4"),
    amiga_key(0x54, "f5", "F5"),
    amiga_key(0x55, "f6", "F6"),
    amiga_key(0x56, "f7", "F7"),
    amiga_key(0x57, "f8", "F8"),
    amiga_key(0x58, "f9", "F9"),
    amiga_key(0x59, "f10", "F10"),
    amiga_key(0x00, "grave", "` ~"),
    amiga_key(0x01, "1", "1 !"),
    amiga_key(0x02, "2", "2 @"),
    amiga_key(0x03, "3", "3 #"),
    amiga_key(0x04, "4", "4 $"),
    amiga_key(0x05, "5", "5 %"),
    amiga_key(0x06, "6", "6 ^"),
    amiga_key(0x07, "7", "7 &"),
    amiga_key(0x08, "8", "8 *"),
    amiga_key(0x09, "9", "9 ("),
    amiga_key(0x0A, "0", "0 )"),
    amiga_key(0x0B, "minus", "- _"),
    amiga_key(0x0C, "equal", "= +"),
    amiga_key(0x0D, "backslash", "\\ |"),
    amiga_key(0x10, "q", "Q"),
    amiga_key(0x11, "w", "W"),
    amiga_key(0x12, "e", "E"),
    amiga_key(0x13, "r", "R"),
    amiga_key(0x14, "t", "T"),
    amiga_key(0x15, "y", "Y"),
    amiga_key(0x16, "u", "U"),
    amiga_key(0x17, "i", "I"),
    amiga_key(0x18, "o", "O"),
    amiga_key(0x19, "p", "P"),
    amiga_key(0x1A, "lbracket", "[ {"),
    amiga_key(0x1B, "rbracket", "] }"),
    amiga_key(0x20, "a", "A"),
    amiga_key(0x21, "s", "S"),
    amiga_key(0x22, "d", "D"),
    amiga_key(0x23, "f", "F"),
    amiga_key(0x24, "g", "G"),
    amiga_key(0x25, "h", "H"),
    amiga_key(0x26, "j", "J"),
    amiga_key(0x27, "k", "K"),
    amiga_key(0x28, "l", "L"),
    amiga_key(0x29, "semicolon", "; :"),
    amiga_key(0x2A, "quote", "' \""),
    amiga_key(0x2B, "0x2B", "ISO $2B"),
    amiga_key(0x30, "0x30", "ISO $30"),
    amiga_key(0x31, "z", "Z"),
    amiga_key(0x32, "x", "X"),
    amiga_key(0x33, "c", "C"),
    amiga_key(0x34, "v", "V"),
    amiga_key(0x35, "b", "B"),
    amiga_key(0x36, "n", "N"),
    amiga_key(0x37, "m", "M"),
    amiga_key(0x38, "comma", ", <"),
    amiga_key(0x39, "period", ". >"),
    amiga_key(0x3A, "slash", "/ ?"),
    amiga_key(0x5A, "kplparen", "Keypad ("),
    amiga_key(0x5B, "kprparen", "Keypad )"),
    amiga_key(0x5C, "kpslash", "Keypad /"),
    amiga_key(0x5D, "kpstar", "Keypad *"),
    amiga_key(0x3D, "kp7", "Keypad 7"),
    amiga_key(0x3E, "kp8", "Keypad 8"),
    amiga_key(0x3F, "kp9", "Keypad 9"),
    amiga_key(0x4A, "kpminus", "Keypad -"),
    amiga_key(0x2D, "kp4", "Keypad 4"),
    amiga_key(0x2E, "kp5", "Keypad 5"),
    amiga_key(0x2F, "kp6", "Keypad 6"),
    amiga_key(0x5E, "kpplus", "Keypad +"),
    amiga_key(0x1D, "kp1", "Keypad 1"),
    amiga_key(0x1E, "kp2", "Keypad 2"),
    amiga_key(0x1F, "kp3", "Keypad 3"),
    amiga_key(0x0F, "kp0", "Keypad 0"),
    amiga_key(0x3C, "kpperiod", "Keypad ."),
    amiga_key(0x43, "kpenter", "Keypad Enter"),
];

/// The highest rawkey a key sends. Nothing above it is a key: $78 is the
/// keyboard's reset warning, and the codes between are unused.
const LAST_KEY_RAWKEY: u8 = 0x67;

/// The [`AMIGA_KEYS`] entry for a rawkey.
pub fn amiga_key_for(rawkey: u8) -> Option<&'static AmigaKey> {
    AMIGA_KEYS.iter().find(|k| k.rawkey == rawkey)
}

/// How a remapped rawkey is persisted: its name, or the rawkey itself for a
/// code no key of a full keyboard sends (a hand-written file may still name
/// one).
fn amiga_key_name(rawkey: u8) -> String {
    match amiga_key_for(rawkey) {
        Some(key) => key.name.to_string(),
        None => format!("0x{rawkey:02X}"),
    }
}

/// The persisted spelling of a host key that types nothing.
const NO_AMIGA_KEY: &str = "none";

/// Host keys moved off the Amiga key in their own position.
///
/// Only the moves are stored: a host key with no entry types its
/// [`positional_rawkey`], so the table stays empty until the user changes
/// something, and a new host key code winit learns needs no entry. A move
/// back to the positional key removes the entry rather than storing it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyboardRemap {
    /// Host key -> the rawkey it types instead, `None` for nothing at all.
    moves: BTreeMap<KeyCode, Option<u8>>,
}

impl KeyboardRemap {
    /// The rawkey a host key types: its move if it has one, its position
    /// otherwise. This is the lookup on every key event.
    pub fn rawkey_for(&self, code: KeyCode) -> Option<u8> {
        match self.moves.get(&code) {
            Some(target) => *target,
            None => positional_rawkey(code),
        }
    }

    /// Make `code` type `rawkey` (`None`: nothing).
    pub fn set(&mut self, code: KeyCode, rawkey: Option<u8>) {
        if rawkey == positional_rawkey(code) {
            self.moves.remove(&code);
        } else {
            self.moves.insert(code, rawkey);
        }
    }

    /// The host keys that type `rawkey`, in panel order. Several host keys
    /// may: Right Amiga is both Right Ctrl and Right Super by default.
    pub fn host_keys_for(&self, rawkey: u8) -> Vec<KeyCode> {
        remappable_keys()
            .filter(|code| self.rawkey_for(*code) == Some(rawkey))
            .collect()
    }

    /// Whether any move touches `rawkey`: a host key moved onto it, or one
    /// moved off it.
    pub fn touches(&self, rawkey: u8) -> bool {
        self.moves.iter().any(|(code, target)| {
            *target == Some(rawkey) || positional_rawkey(*code) == Some(rawkey)
        })
    }

    /// Put `rawkey` back the way it ships: every host key moved onto it goes
    /// back to its own position, and every host key moved off it types it
    /// again.
    pub fn reset(&mut self, rawkey: u8) {
        self.moves.retain(|code, target| {
            *target != Some(rawkey) && positional_rawkey(*code) != Some(rawkey)
        });
    }

    /// Drop every move.
    pub fn clear(&mut self) {
        self.moves.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Human-readable list of the host keys typing `rawkey`, for the panel.
    pub fn binding_text(&self, rawkey: u8) -> String {
        let keys = self.host_keys_for(rawkey);
        if keys.is_empty() {
            return "-".to_string();
        }
        keys.iter()
            .map(|k| short_key_label(*k))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// Both keyboard mappings, and the Amiga-keyboard remap.
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
    /// Host keys moved to another Amiga key; empty by default.
    keyboard: KeyboardRemap,
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
            keyboard: KeyboardRemap::default(),
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

    pub fn keyboard(&self) -> &KeyboardRemap {
        &self.keyboard
    }

    pub fn keyboard_mut(&mut self) -> &mut KeyboardRemap {
        &mut self.keyboard
    }

    /// Restore both controller mappings to the built-in layouts, leaving the
    /// keyboard remap alone.
    pub fn reset_controllers(&mut self) {
        self.mappings = Self::default().mappings;
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
        log::info!("saved keyboard map to {}", path.display());
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
/// list of host key names, and a `[keyboard]` table of host key name to the
/// Amiga key it types instead. Unknown control or key names are dropped with
/// a warning rather than failing the load -- a map written by a newer build
/// should still start an older one.
///
/// A mapping table that is absent keeps its built-in layout (Save always
/// writes both, empty or not), so a hand-written file holding only a
/// `[keyboard]` table does not unbind the controllers.
#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyMapStore {
    #[serde(default)]
    controller1: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    controller2: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    keyboard: BTreeMap<String, String>,
}

impl KeyMapStore {
    fn into_keymap(self) -> KeyMap {
        let defaults = KeyMap::default();
        let mut map = KeyMap {
            mappings: [KeyboardMapping::empty(), KeyboardMapping::empty()],
            keyboard: decode_keyboard(&self.keyboard),
        };
        // Both mappings are decoded into the one map so a key cannot be
        // claimed twice: the panel enforces "a key means one thing" as you
        // edit, and a hand-written file has to arrive at the same invariant.
        // Without this a key listed under two controls drives both at once,
        // and one listed under both mappings binds to whichever `lookup`
        // reaches first, leaving the other silently dead.
        //
        // The tables the file names go first, so what it says wins: an
        // absent table then fills in its built-in layout under the same
        // rule, quietly leaving out any default key a named table has
        // taken. Defaults first would let controller 1's Left Ctrl fire
        // shadow a file that binds Left Ctrl on controller 2 alone.
        let tables = [self.controller1, self.controller2];
        for (index, table) in tables.iter().enumerate() {
            if let Some(table) = table {
                decode_mapping_into(&mut map, index, table, true);
            }
        }
        for (index, table) in tables.iter().enumerate() {
            if table.is_none() {
                let table = encode_mapping(defaults.mapping(index));
                decode_mapping_into(&mut map, index, &table, false);
            }
        }
        map
    }
}

impl From<&KeyMap> for KeyMapStore {
    fn from(map: &KeyMap) -> Self {
        Self {
            controller1: Some(encode_mapping(map.mapping(0))),
            controller2: Some(encode_mapping(map.mapping(1))),
            keyboard: encode_keyboard(map.keyboard()),
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

/// Decode one persisted mapping table into `map` at `index`, skipping any key
/// another control or mapping has already claimed (with a warning when
/// `warn_duplicates`, which a built-in layout filling an absent table
/// turns off: there the skip is expected, not a mistake in the file).
///
/// Controls are walked in [`CONTROLS`] order rather than the table's
/// (alphabetical) key order, so which binding of a duplicated key survives is
/// the order the panel lists them in, not an artefact of the control names.
fn decode_mapping_into(
    map: &mut KeyMap,
    index: usize,
    table: &BTreeMap<String, Vec<String>>,
    warn_duplicates: bool,
) {
    for control_key in table.keys() {
        if JoyControl::from_key(control_key).is_none() {
            log::warn!("keymap.toml: ignoring unknown control {control_key:?}");
        }
    }
    for control in CONTROLS {
        let Some(names) = table.get(control.key()) else {
            continue;
        };
        for name in names {
            let Some(code) = key_from_name(name) else {
                log::warn!("keymap.toml: ignoring unknown key {name:?}");
                continue;
            };
            if let Some((claimed_index, claimed_control)) = map.lookup(code) {
                if !warn_duplicates {
                    continue;
                }
                log::warn!(
                    "keymap.toml: {name} is already bound to {} on controller {}; \
                     ignoring the duplicate on {}",
                    claimed_control.label(),
                    claimed_index + 1,
                    control.label()
                );
                continue;
            }
            map.bind(index, control, code);
        }
    }
}

fn encode_keyboard(remap: &KeyboardRemap) -> BTreeMap<String, String> {
    remap
        .moves
        .iter()
        .map(|(code, target)| {
            let target = match target {
                Some(rawkey) => amiga_key_name(*rawkey),
                None => NO_AMIGA_KEY.to_string(),
            };
            (key_name(*code), target)
        })
        .collect()
}

/// Decode the `[keyboard]` table: host key name to an Amiga key (any name or
/// code `--press-after` takes) or `none`. Entries that name no remappable
/// host key, or no Amiga key, are dropped with a warning.
fn decode_keyboard(table: &BTreeMap<String, String>) -> KeyboardRemap {
    let mut remap = KeyboardRemap::default();
    for (host, target) in table {
        let Some(code) = remappable_key_from_name(host) else {
            log::warn!("keymap.toml: ignoring unknown host key {host:?} in [keyboard]");
            continue;
        };
        let rawkey = if target.trim().eq_ignore_ascii_case(NO_AMIGA_KEY) {
            None
        } else {
            match crate::video::window::parse_amiga_key(target) {
                Some(rawkey) if rawkey <= LAST_KEY_RAWKEY => Some(rawkey),
                _ => {
                    log::warn!(
                        "keymap.toml: ignoring {host} = {target:?} in [keyboard]: \
                         not an Amiga key"
                    );
                    continue;
                }
            }
        };
        remap.set(code, rawkey);
    }
    remap
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

    /// A hand-written file must arrive at the same "a key means one thing"
    /// invariant the panel enforces as you edit. Left unchecked, a key listed
    /// under two controls drove both at once, and one listed under both
    /// mappings bound to whichever `lookup` reached first while the other
    /// binding silently did nothing.
    #[test]
    fn duplicate_bindings_in_a_hand_written_file_are_dropped() {
        let text = r#"
            [controller1]
            up = ["ArrowUp"]
            fire = ["ArrowUp", "KeyC"]
            [controller2]
            down = ["ArrowUp"]
            left = ["KeyC", "Numpad4"]
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();

        // First claim in UI order wins, and no other control keeps the key.
        assert_eq!(map.lookup(KeyCode::ArrowUp), Some((0, JoyControl::Up)));
        assert_eq!(map.mapping(0).keys(JoyControl::Up), &[KeyCode::ArrowUp]);
        assert_eq!(map.mapping(0).keys(JoyControl::Fire), &[KeyCode::KeyC]);
        assert!(map.mapping(1).keys(JoyControl::Down).is_empty());
        assert!(!map
            .mapping(1)
            .keys(JoyControl::Left)
            .contains(&KeyCode::KeyC));
        // Non-duplicate bindings alongside a dropped one still land.
        assert_eq!(map.mapping(1).keys(JoyControl::Left), &[KeyCode::Numpad4]);

        // With one key held, exactly one control reads as pressed.
        let mut held = HeldKeys::default();
        held.set(KeyCode::ArrowUp, true);
        let state = map.mapping(0).joystick_state(&held);
        assert!(state.up);
        assert!(!state.fire, "one key must not drive two controls");
        assert!(!map.mapping(1).joystick_state(&held).down);

        // Repeating a key under one control is deduplicated too, so the
        // binding text does not read "Up / Up".
        let text = r#"
            [controller1]
            up = ["ArrowUp", "ArrowUp"]
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        assert_eq!(map.mapping(0).keys(JoyControl::Up), &[KeyCode::ArrowUp]);
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

    #[test]
    fn host_keys_land_on_the_amiga_key_in_the_same_place() {
        // A positional map, not a symbolic one: the key left of A is Caps
        // Lock whatever the host layout prints on it, and Right Ctrl, which
        // the Amiga lacks, doubles as Right Amiga.
        assert_eq!(positional_rawkey(KeyCode::CapsLock), Some(0x62));
        assert_eq!(positional_rawkey(KeyCode::ControlLeft), Some(0x63));
        assert_eq!(positional_rawkey(KeyCode::ControlRight), Some(0x67));
        assert_eq!(positional_rawkey(KeyCode::KeyA), Some(0x20));
        assert_eq!(positional_rawkey(KeyCode::F11), Some(0x5F));
        assert_eq!(positional_rawkey(KeyCode::F12), None);
    }

    #[test]
    fn every_host_key_with_an_amiga_key_can_be_remapped() {
        // The keys the positional table maps that a controller cannot be
        // bound to: without them in the remap list, the panel could not
        // move Caps Lock -- the reason the remap exists.
        for code in [
            KeyCode::CapsLock,
            KeyCode::Escape,
            KeyCode::F11,
            KeyCode::IntlBackslash,
            KeyCode::IntlRo,
            KeyCode::NumpadParenLeft,
            KeyCode::NumpadParenRight,
        ] {
            assert!(positional_rawkey(code).is_some(), "{code:?}");
            assert!(is_remappable(code), "{code:?}");
            assert!(!is_bindable(code), "{code:?} is already bindable");
        }
        for code in remappable_keys() {
            assert_eq!(remappable_key_from_name(&key_name(code)), Some(code));
        }
        assert!(!is_remappable(KeyCode::Fn));
    }

    #[test]
    fn amiga_key_table_covers_the_keyboard_once_with_persistable_names() {
        // 96 keys: the A500/A2000/A4000 keyboard with its keypad and the
        // two ISO keys.
        assert_eq!(AMIGA_KEYS.len(), 96);
        let mut seen = [false; 128];
        for key in AMIGA_KEYS {
            assert!(key.rawkey <= LAST_KEY_RAWKEY, "{key:?}");
            assert!(!seen[key.rawkey as usize], "{key:?} listed twice");
            seen[key.rawkey as usize] = true;
            assert_eq!(
                crate::video::window::parse_amiga_key(key.name),
                Some(key.rawkey),
                "{key:?} does not read back"
            );
            assert_eq!(amiga_key_for(key.rawkey), Some(key));
            assert!(key.label.is_ascii() && !key.label.is_empty(), "{key:?}");
        }
        // Every key a host key types by default has a row to show it on.
        for code in remappable_keys() {
            if let Some(rawkey) = positional_rawkey(code) {
                assert!(amiga_key_for(rawkey).is_some(), "{code:?} -> {rawkey:#04x}");
            }
        }
        assert_eq!(amiga_key_name(0x63), "ctrl");
        assert_eq!(amiga_key_name(0x1C), "0x1C");
    }

    #[test]
    fn a_remap_moves_a_host_key_and_stores_only_the_move() {
        let mut remap = KeyboardRemap::default();
        assert_eq!(remap.rawkey_for(KeyCode::CapsLock), Some(0x62));
        assert_eq!(remap.host_keys_for(0x63), &[KeyCode::ControlLeft]);
        assert!(!remap.touches(0x63));

        // Caps Lock as a second Ctrl: Ctrl gains a key, Caps Lock loses its
        // only one, and both rows read as edited.
        remap.set(KeyCode::CapsLock, Some(0x63));
        assert_eq!(remap.rawkey_for(KeyCode::CapsLock), Some(0x63));
        assert_eq!(
            remap.host_keys_for(0x63),
            &[KeyCode::ControlLeft, KeyCode::CapsLock]
        );
        assert_eq!(remap.binding_text(0x63), "LCtrl / CapsLock");
        assert!(remap.host_keys_for(0x62).is_empty());
        assert_eq!(remap.binding_text(0x62), "-");
        assert!(remap.touches(0x63) && remap.touches(0x62));
        assert!(!remap.touches(0x20));

        // Moving a key back to its own place drops the entry altogether.
        remap.set(KeyCode::CapsLock, Some(0x62));
        assert!(remap.is_empty());

        // A key can be made to type nothing.
        remap.set(KeyCode::Pause, Some(0x5F));
        remap.set(KeyCode::F11, None);
        assert_eq!(remap.rawkey_for(KeyCode::F11), None);
        assert_eq!(remap.host_keys_for(0x5F), &[KeyCode::Pause]);
    }

    #[test]
    fn resetting_an_amiga_key_undoes_every_move_onto_and_off_it() {
        let mut remap = KeyboardRemap::default();
        // A swap: Caps Lock types Ctrl and Left Ctrl types Caps Lock, plus
        // an unrelated move that must survive.
        remap.set(KeyCode::CapsLock, Some(0x63));
        remap.set(KeyCode::ControlLeft, Some(0x62));
        remap.set(KeyCode::Pause, Some(0x5F));

        remap.reset(0x63);
        // Caps Lock (moved onto Ctrl) and Left Ctrl (moved off it) are back.
        assert_eq!(remap.rawkey_for(KeyCode::CapsLock), Some(0x62));
        assert_eq!(remap.rawkey_for(KeyCode::ControlLeft), Some(0x63));
        assert_eq!(remap.rawkey_for(KeyCode::Pause), Some(0x5F));
        assert!(!remap.touches(0x63) && !remap.touches(0x62));

        remap.clear();
        assert!(remap.is_empty());
    }

    #[test]
    fn keyboard_remap_round_trips_through_toml() {
        let mut map = KeyMap::default();
        map.keyboard_mut().set(KeyCode::CapsLock, Some(0x63));
        map.keyboard_mut().set(KeyCode::Pause, Some(0x5F));
        map.keyboard_mut().set(KeyCode::IntlBackslash, Some(0x2B));
        map.keyboard_mut().set(KeyCode::F11, None);

        let text = toml::to_string_pretty(&KeyMapStore::from(&map)).unwrap();
        assert!(text.contains("CapsLock = \"ctrl\""), "{text}");
        assert!(text.contains("Pause = \"help\""), "{text}");
        assert!(text.contains("IntlBackslash = \"0x2B\""), "{text}");
        assert!(text.contains("F11 = \"none\""), "{text}");
        let back: KeyMapStore = toml::from_str(&text).unwrap();
        assert_eq!(back.into_keymap(), map);
    }

    /// Save writes both controller tables even when one is empty, so the
    /// "absent table keeps its defaults" rule can never turn a mapping the
    /// user cleared back into the built-in one.
    #[test]
    fn a_fully_cleared_mapping_round_trips_as_empty() {
        let mut map = KeyMap::default();
        for control in CONTROLS {
            map.mapping_mut(1).clear(control);
        }
        let text = toml::to_string_pretty(&KeyMapStore::from(&map)).unwrap();
        let back = toml::from_str::<KeyMapStore>(&text).unwrap().into_keymap();
        assert_eq!(back, map);
        assert_eq!(back.lookup(KeyCode::Numpad8), None);
    }

    #[test]
    fn a_file_with_only_a_keyboard_table_keeps_the_controller_defaults() {
        let text = r#"
            [keyboard]
            CapsLock = "ctrl"
            ControlLeft = "Caps-Lock"
            Pause = "0x5f"
            F11 = "None"
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        assert_eq!(map.mapping(0), KeyMap::default().mapping(0));
        assert_eq!(map.mapping(1), KeyMap::default().mapping(1));
        let remap = map.keyboard();
        assert_eq!(remap.rawkey_for(KeyCode::CapsLock), Some(0x63));
        assert_eq!(remap.rawkey_for(KeyCode::ControlLeft), Some(0x62));
        assert_eq!(remap.rawkey_for(KeyCode::Pause), Some(0x5F));
        assert_eq!(remap.rawkey_for(KeyCode::F11), None);

        // A file that names one controller still gets the other's defaults,
        // less any key the named one has taken.
        let text = r#"
            [controller1]
            fire = ["Numpad0"]
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        assert_eq!(map.lookup(KeyCode::Numpad0), Some((0, JoyControl::Fire)));
        assert_eq!(map.lookup(KeyCode::Numpad8), Some((1, JoyControl::Up)));
        assert!(map.mapping(1).keys(JoyControl::Fire).is_empty());
    }

    /// The example in docs/guide/configuration.md ("Remapping Amiga keys").
    #[test]
    fn the_documented_keyboard_example_reads_as_described() {
        let text = r#"
            [keyboard]
            CapsLock = "ctrl"       # Caps Lock types Ctrl
            ControlLeft = "caps"    # and Left Ctrl types Caps Lock: a swap
            Insert = "help"         # a home for the Help key
            NumLock = "kplparen"    # and for the keypad brackets a PC keypad lacks
            ScrollLock = "kprparen"
            F11 = "none"            # F11 (Help by default) types nothing
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        let remap = map.keyboard();
        assert_eq!(remap.host_keys_for(0x63), &[KeyCode::CapsLock]);
        assert_eq!(remap.host_keys_for(0x62), &[KeyCode::ControlLeft]);
        assert_eq!(remap.host_keys_for(0x5F), &[KeyCode::Insert]);
        assert_eq!(
            remap.host_keys_for(0x5A),
            &[KeyCode::NumLock, KeyCode::NumpadParenLeft]
        );
        assert_eq!(
            remap.host_keys_for(0x5B),
            &[KeyCode::NumpadParenRight, KeyCode::ScrollLock]
        );
        assert_eq!(remap.rawkey_for(KeyCode::F11), None);
    }

    /// A file naming only controller 2 bound Left Ctrl there before absent
    /// tables kept their defaults; controller 1's default fire aliases
    /// Left Ctrl, and must not take it back.
    #[test]
    fn a_named_controller_table_wins_keys_over_the_other_tables_defaults() {
        let text = r#"
            [controller2]
            fire = ["ControlLeft"]
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        assert_eq!(
            map.lookup(KeyCode::ControlLeft),
            Some((1, JoyControl::Fire))
        );
        assert_eq!(
            map.mapping(0).keys(JoyControl::Fire),
            &[KeyCode::ControlRight, KeyCode::AltRight, KeyCode::KeyC]
        );
        assert_eq!(map.lookup(KeyCode::ArrowUp), Some((0, JoyControl::Up)));
    }

    #[test]
    fn unknown_keyboard_entries_are_dropped_not_fatal() {
        let text = r#"
            [keyboard]
            NoSuchKey = "ctrl"
            CapsLock = "nosuchamigakey"
            Pause = "0x78"
            ScrollLock = "help"
        "#;
        let map = toml::from_str::<KeyMapStore>(text).unwrap().into_keymap();
        let remap = map.keyboard();
        assert_eq!(remap.rawkey_for(KeyCode::CapsLock), Some(0x62));
        // $78 is the keyboard's reset warning, not a key.
        assert_eq!(remap.rawkey_for(KeyCode::Pause), None);
        assert_eq!(remap.rawkey_for(KeyCode::ScrollLock), Some(0x5F));
        assert_eq!(
            remap.host_keys_for(0x5F),
            &[KeyCode::F11, KeyCode::ScrollLock]
        );
    }

    #[test]
    fn resetting_the_controllers_keeps_the_keyboard_remap() {
        let mut map = KeyMap::default();
        map.bind(0, JoyControl::Up, KeyCode::KeyW);
        map.keyboard_mut().set(KeyCode::CapsLock, Some(0x63));
        map.reset_controllers();
        assert_eq!(map.mapping(0), KeyMap::default().mapping(0));
        assert_eq!(map.keyboard().rawkey_for(KeyCode::CapsLock), Some(0x63));
    }
}

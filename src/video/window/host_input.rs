// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-to-Amiga input mapping: entry-box characters, host keycode to
//! Amiga rawkey translation, `--press-after` key-name parsing, and the
//! gamepad-calibration view builder. Split out of `window.rs` for size;
//! same module family, full access to the parent's private items.

use super::*;

/// Hex digit for a key, for the debugger's address entry box.
/// Map a key to the character it types into the debugger entry box: digits, all
/// letters (for register names and the EQ/NE/.../AND/IGN condition mnemonics and
/// M<hex> memory operands), and Space. `push_entry_char` filters and uppercases.
pub(super) fn entry_char_for_key(code: KeyCode) -> Option<char> {
    use KeyCode::*;
    Some(match code {
        Digit0 | Numpad0 => '0',
        Digit1 | Numpad1 => '1',
        Digit2 | Numpad2 => '2',
        Digit3 | Numpad3 => '3',
        Digit4 | Numpad4 => '4',
        Digit5 | Numpad5 => '5',
        Digit6 | Numpad6 => '6',
        Digit7 | Numpad7 => '7',
        Digit8 | Numpad8 => '8',
        Digit9 | Numpad9 => '9',
        KeyA => 'A',
        KeyB => 'B',
        KeyC => 'C',
        KeyD => 'D',
        KeyE => 'E',
        KeyF => 'F',
        KeyG => 'G',
        KeyH => 'H',
        KeyI => 'I',
        KeyJ => 'J',
        KeyK => 'K',
        KeyL => 'L',
        KeyM => 'M',
        KeyN => 'N',
        KeyO => 'O',
        KeyP => 'P',
        KeyQ => 'Q',
        KeyR => 'R',
        KeyS => 'S',
        KeyT => 'T',
        KeyU => 'U',
        KeyV => 'V',
        KeyW => 'W',
        KeyX => 'X',
        KeyY => 'Y',
        KeyZ => 'Z',
        Space => ' ',
        _ => return None,
    })
}

/// Format a calibration session for the panel: pad identity, one row per
/// step with its captured binding, and a prompt for what to do next.
pub(super) fn build_calibration_view(
    session: &crate::gamepad::CalibrationSession,
) -> ui::CalibrationView {
    use crate::gamepad::CalibrationSession;
    let pad_line = if session.backend_missing() {
        "No gamepad backend available on this host".to_string()
    } else {
        match (session.pad_name(), session.connected()) {
            (Some(name), true) => format!("Controller: {name}"),
            (Some(name), false) => format!("Reconnect {name}..."),
            (None, _) => "Connect a controller...".to_string(),
        }
    };
    let rows = (0..CalibrationSession::step_count())
        .map(|index| ui::CalRow {
            label: CalibrationSession::step_label(index),
            binding: session.binding_text(index),
            current: session.current_step() == Some(index),
        })
        .collect();
    let status = if session.backend_missing() {
        "Calibration needs a gamepad backend (not available headless).".to_string()
    } else if session.done() {
        if session.live_test().is_empty() {
            "All steps captured. Push controls to test, then Save.".to_string()
        } else {
            format!("Testing: {}", session.live_test())
        }
    } else if !session.connected() {
        "Waiting for a controller to be connected.".to_string()
    } else if session.can_skip() {
        "Push and hold the control, or Skip if the pad lacks it.".to_string()
    } else {
        "Push and hold the control on the pad.".to_string()
    };
    ui::CalibrationView {
        pad_line,
        rows,
        status,
    }
}

/// Translate a winit `KeyCode` to an Amiga raw scan code.
/// Covers the alphanumeric block, common modifiers we treat as their
/// nearest Amiga equivalents, function keys, and arrows. Anything not
/// in the table returns None (the keypress is silently dropped).
pub(super) fn host_to_amiga_rawkey(code: KeyCode) -> Option<u8> {
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

/// Parse an Amiga raw key as either a decimal/hex code or a common
/// key name for scripted input.
pub fn parse_amiga_key(s: &str) -> Option<u8> {
    let trimmed = s.trim();
    if let Some(raw) = parse_u8(trimmed) {
        return Some(raw);
    }
    let name = trimmed.to_ascii_lowercase().replace(['-', '_', '+'], "");
    Some(match name.as_str() {
        "a" => 0x20,
        "b" => 0x35,
        "c" => 0x33,
        "d" => 0x22,
        "e" => 0x12,
        "f" => 0x23,
        "g" => 0x24,
        "h" => 0x25,
        "i" => 0x17,
        "j" => 0x26,
        "k" => 0x27,
        "l" => 0x28,
        "m" => 0x37,
        "n" => 0x36,
        "o" => 0x18,
        "p" => 0x19,
        "q" => 0x10,
        "r" => 0x13,
        "s" => 0x21,
        "t" => 0x14,
        "u" => 0x16,
        "v" => 0x34,
        "w" => 0x11,
        "x" => 0x32,
        "y" => 0x15,
        "z" => 0x31,
        "1" => 0x01,
        "2" => 0x02,
        "3" => 0x03,
        "4" => 0x04,
        "5" => 0x05,
        "6" => 0x06,
        "7" => 0x07,
        "8" => 0x08,
        "9" => 0x09,
        "0" => 0x0A,
        "space" => 0x40,
        "backspace" | "bs" => 0x41,
        "tab" => 0x42,
        "enter" | "return" => 0x44,
        "escape" | "esc" => 0x45,
        "delete" | "del" => 0x46,
        "up" | "arrowup" => 0x4C,
        "down" | "arrowdown" => 0x4D,
        "right" | "arrowright" => 0x4E,
        "left" | "arrowleft" => 0x4F,
        "f1" => 0x50,
        "f2" => 0x51,
        "f3" => 0x52,
        "f4" => 0x53,
        "f5" => 0x54,
        "f6" => 0x55,
        "f7" => 0x56,
        "f8" => 0x57,
        "f9" => 0x58,
        "f10" => 0x59,
        "lshift" | "leftshift" | "shift" => 0x60,
        "rshift" | "rightshift" => 0x61,
        "caps" | "capslock" => 0x62,
        "ctrl" | "control" | "lctrl" | "leftctrl" | "leftcontrol" | "rctrl" | "rightctrl"
        | "rightcontrol" => 0x63,
        "lalt" | "leftalt" | "alt" => 0x64,
        "ralt" | "rightalt" => 0x65,
        "lami" | "leftami" | "lamiga" | "leftamiga" | "ami" | "amiga" | "cmd" | "command"
        | "super" | "meta" => 0x66,
        "rami" | "rightami" | "ramiga" | "rightamiga" => 0x67,
        _ => return None,
    })
}

pub(super) fn parse_u8(s: &str) -> Option<u8> {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("$")) {
        u8::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

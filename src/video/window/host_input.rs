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
    pad_has_the_buttons: bool,
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
        if pad_has_the_buttons {
            "All steps captured. Choose Save or Cancel.".to_string()
        } else if session.live_test().is_empty() {
            "All steps captured. Push controls to test, hold any control for Save or Cancel."
                .to_string()
        } else {
            format!("Testing: {}", session.live_test())
        }
    } else if !session.connected() {
        "Waiting for a controller to be connected.".to_string()
    } else if session.can_skip() {
        "Push the control, or hold any control if the pad lacks it.".to_string()
    } else {
        "Push the control on the pad.".to_string()
    };
    ui::CalibrationView {
        pad_line,
        rows,
        status,
    }
}

/// Parse an Amiga raw key as either a decimal/hex code or a common
/// key name for scripted input.
pub fn parse_amiga_key(s: &str) -> Option<u8> {
    let trimmed = s.trim();
    // The digit keys are names. `1`-`9` share their raw codes, but the zero
    // key is $0A while raw code 0 is the backtick key, so a bare `0` has to
    // be taken as the name before the number (`0x00` still gives the code).
    if trimmed == "0" {
        return Some(0x0A);
    }
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
        "help" => 0x5F,
        "grave" | "backtick" => 0x00,
        "minus" => 0x0B,
        "equal" | "equals" => 0x0C,
        "backslash" => 0x0D,
        "lbracket" | "leftbracket" => 0x1A,
        "rbracket" | "rightbracket" => 0x1B,
        "semicolon" => 0x29,
        "quote" | "apostrophe" => 0x2A,
        "comma" => 0x38,
        "period" | "dot" => 0x39,
        "slash" => 0x3A,
        "kp0" => 0x0F,
        "kp1" => 0x1D,
        "kp2" => 0x1E,
        "kp3" => 0x1F,
        "kp4" => 0x2D,
        "kp5" => 0x2E,
        "kp6" => 0x2F,
        "kp7" => 0x3D,
        "kp8" => 0x3E,
        "kp9" => 0x3F,
        "kpperiod" | "kpdot" | "kpdecimal" => 0x3C,
        "kpenter" => 0x43,
        "kpminus" => 0x4A,
        "kplparen" | "kpleftparen" => 0x5A,
        "kprparen" | "kprightparen" => 0x5B,
        "kpslash" | "kpdivide" => 0x5C,
        "kpstar" | "kpmultiply" => 0x5D,
        "kpplus" => 0x5E,
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

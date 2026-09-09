// SPDX-License-Identifier: GPL-3.0-or-later

//! Host text to Amiga keystrokes.
//!
//! `--type-after SECS TEXT`, the `type` script directive, the control
//! protocol's `input.type`, and the window's Paste as Keystrokes action all
//! turn a host string into the raw-keycode press/release sequence a person
//! would produce on a US Amiga keyboard, spaced in emulated time so the
//! keyboard MCU (src/chipset/keyboard.rs) and the guest's keyboard driver
//! see one clean keystroke after another.
//!
//! The mapping is the US Amiga keymap: letters, digits, the punctuation on
//! the printed caps, with Shift held for the upper-case and shifted
//! symbols. A newline is Return, a tab is Tab, escape is Esc, backspace is
//! Backspace. Text from the command line may carry the C-style escapes
//! `\n`, `\t`, `\e`, `\b`, and `\\` (see [`unescape`]); text pasted from
//! the clipboard is taken as is.
//!
//! Timing: a keystroke is one press byte and one release byte on the
//! keyboard wire, each ~0.5 ms plus the guest's handshake; the MCU buffers
//! ten events of type-ahead and the guest's input.device processes them
//! from the SP interrupt. A key held for [`TYPE_KEY_HOLD_MS`] is released
//! long before any key-repeat delay, and the next key starts
//! [`TYPE_KEY_PITCH_MS`] after the previous one -- ten keys a second, which
//! keeps every press/release pair separated by at least a frame even after
//! the scheduler quantises the timestamps to frame boundaries, so no two
//! typed matrix keys are ever down together (the 6500/1 suppresses
//! ambiguous chords as ghosts). Shift goes down [`TYPE_SHIFT_LEAD_MS`]
//! before its key and comes up [`TYPE_SHIFT_HOLD_MS`] after it went down,
//! after the key it qualifies has been released: the order a typist's
//! fingers produce and the order input.device expects the qualifier
//! transitions in. The lead also keeps the order through an input
//! recording, which sorts its lines by press time.

/// Raw key code of the left Shift key, the qualifier used for shifted
/// characters.
pub const RAWKEY_LSHIFT: u8 = 0x60;

/// How long a typed key is held, in emulated milliseconds.
pub const TYPE_KEY_HOLD_MS: u32 = 50;
/// How long before its key Shift goes down.
pub const TYPE_SHIFT_LEAD_MS: u32 = 20;
/// How long Shift is held around a shifted key, from its own press: past
/// the key's release, and up again before the next key starts.
pub const TYPE_SHIFT_HOLD_MS: u32 = 90;
/// Emulated milliseconds from one typed key's press to the next key's press.
pub const TYPE_KEY_PITCH_MS: u32 = 100;

// The schedule's invariants, checked at compile time. A shifted key goes
// Shift down, key down, key up, Shift up, next key; and the scheduler
// fires at frame boundaries (20 ms PAL, ~16.7 ms NTSC), so a release and
// the next press of the same key must be more than a frame apart or the
// press is filtered as a duplicate of a key still held.
const _: () = assert!(
    TYPE_SHIFT_LEAD_MS >= 20,
    "Shift lands a frame ahead of its key"
);
const _: () = assert!(
    TYPE_SHIFT_HOLD_MS > TYPE_SHIFT_LEAD_MS + TYPE_KEY_HOLD_MS,
    "Shift outlives its key"
);
const _: () = assert!(
    TYPE_SHIFT_HOLD_MS <= TYPE_KEY_PITCH_MS,
    "Shift is up before the next key"
);
const _: () = assert!(
    TYPE_KEY_PITCH_MS - (TYPE_SHIFT_LEAD_MS + TYPE_KEY_HOLD_MS) > 20,
    "the same key re-pressed lands in a later frame"
);

/// One scheduled key of a typed string, relative to the string's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypedKey {
    /// Milliseconds after the string's start time at which the key goes down.
    pub offset_ms: u32,
    pub rawkey: u8,
    /// Milliseconds the key stays down.
    pub hold_ms: u32,
}

/// The US Amiga keymap entry for `c`: the raw key code and whether Shift
/// must be held to produce it. `None` for characters the keymap has no key
/// for (non-ASCII text, control characters other than the ones listed in
/// the module documentation).
pub fn keystroke(c: char) -> Option<(u8, bool)> {
    let unshifted = |raw: u8| Some((raw, false));
    let shifted = |raw: u8| Some((raw, true));
    match c {
        // Number row.
        '`' => unshifted(0x00),
        '~' => shifted(0x00),
        '1' => unshifted(0x01),
        '!' => shifted(0x01),
        '2' => unshifted(0x02),
        '@' => shifted(0x02),
        '3' => unshifted(0x03),
        '#' => shifted(0x03),
        '4' => unshifted(0x04),
        '$' => shifted(0x04),
        '5' => unshifted(0x05),
        '%' => shifted(0x05),
        '6' => unshifted(0x06),
        '^' => shifted(0x06),
        '7' => unshifted(0x07),
        '&' => shifted(0x07),
        '8' => unshifted(0x08),
        '*' => shifted(0x08),
        '9' => unshifted(0x09),
        '(' => shifted(0x09),
        '0' => unshifted(0x0A),
        ')' => shifted(0x0A),
        '-' => unshifted(0x0B),
        '_' => shifted(0x0B),
        '=' => unshifted(0x0C),
        '+' => shifted(0x0C),
        '\\' => unshifted(0x0D),
        '|' => shifted(0x0D),
        // Top letter row.
        'q' => unshifted(0x10),
        'w' => unshifted(0x11),
        'e' => unshifted(0x12),
        'r' => unshifted(0x13),
        't' => unshifted(0x14),
        'y' => unshifted(0x15),
        'u' => unshifted(0x16),
        'i' => unshifted(0x17),
        'o' => unshifted(0x18),
        'p' => unshifted(0x19),
        '[' => unshifted(0x1A),
        '{' => shifted(0x1A),
        ']' => unshifted(0x1B),
        '}' => shifted(0x1B),
        // Home row.
        'a' => unshifted(0x20),
        's' => unshifted(0x21),
        'd' => unshifted(0x22),
        'f' => unshifted(0x23),
        'g' => unshifted(0x24),
        'h' => unshifted(0x25),
        'j' => unshifted(0x26),
        'k' => unshifted(0x27),
        'l' => unshifted(0x28),
        ';' => unshifted(0x29),
        ':' => shifted(0x29),
        '\'' => unshifted(0x2A),
        '"' => shifted(0x2A),
        // Bottom row.
        'z' => unshifted(0x31),
        'x' => unshifted(0x32),
        'c' => unshifted(0x33),
        'v' => unshifted(0x34),
        'b' => unshifted(0x35),
        'n' => unshifted(0x36),
        'm' => unshifted(0x37),
        ',' => unshifted(0x38),
        '<' => shifted(0x38),
        '.' => unshifted(0x39),
        '>' => shifted(0x39),
        '/' => unshifted(0x3A),
        '?' => shifted(0x3A),
        // Upper-case letters: the lower-case key plus Shift.
        'A'..='Z' => keystroke(c.to_ascii_lowercase()).map(|(raw, _)| (raw, true)),
        // Whitespace and control keys.
        ' ' => unshifted(0x40),
        '\u{8}' => unshifted(0x41),
        '\t' => unshifted(0x42),
        '\n' => unshifted(0x44),
        '\u{1b}' => unshifted(0x45),
        '\u{7f}' => unshifted(0x46),
        _ => None,
    }
}

/// Resolve the C-style escapes a command line or script cannot otherwise
/// carry: `\n` (Return), `\t` (Tab), `\e` (Esc), `\b` (Backspace), `\r`
/// (also Return), and `\\` for a literal backslash. Any other backslash
/// sequence is kept as written.
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('r') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\u{1b}'),
            Some('b') => out.push('\u{8}'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The keystroke schedule for `text` (already unescaped), and the
/// characters the keymap could not type, in order of first appearance. A
/// carriage return is dropped so CRLF line ends type one Return.
pub fn keystrokes_for_text(text: &str) -> (Vec<TypedKey>, Vec<char>) {
    let mut keys = Vec::new();
    let mut untypable = Vec::new();
    let mut offset_ms = 0;
    for c in text.chars() {
        if c == '\r' {
            continue;
        }
        let Some((rawkey, shift)) = keystroke(c) else {
            if !untypable.contains(&c) {
                untypable.push(c);
            }
            continue;
        };
        if shift {
            // Pushed first and timed earlier: Shift reaches the keyboard
            // before the key it qualifies whichever frame each lands in.
            keys.push(TypedKey {
                offset_ms,
                rawkey: RAWKEY_LSHIFT,
                hold_ms: TYPE_SHIFT_HOLD_MS,
            });
        }
        keys.push(TypedKey {
            offset_ms: if shift {
                offset_ms + TYPE_SHIFT_LEAD_MS
            } else {
                offset_ms
            },
            rawkey,
            hold_ms: TYPE_KEY_HOLD_MS,
        });
        offset_ms += TYPE_KEY_PITCH_MS;
    }
    (keys, untypable)
}

/// Emulated milliseconds a typed string occupies, from its first press to
/// its last release. Zero for text with nothing typable.
pub fn typing_duration_ms(keys: &[TypedKey]) -> u32 {
    keys.iter()
        .map(|k| k.offset_ms + k.hold_ms)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_follows_the_us_amiga_layout() {
        assert_eq!(keystroke('a'), Some((0x20, false)));
        assert_eq!(keystroke('A'), Some((0x20, true)));
        assert_eq!(keystroke('1'), Some((0x01, false)));
        assert_eq!(keystroke('!'), Some((0x01, true)));
        assert_eq!(keystroke(' '), Some((0x40, false)));
        assert_eq!(keystroke('\n'), Some((0x44, false)));
        assert_eq!(keystroke('\t'), Some((0x42, false)));
        assert_eq!(keystroke('\u{1b}'), Some((0x45, false)));
        assert_eq!(keystroke(':'), Some((0x29, true)));
        assert_eq!(keystroke('"'), Some((0x2A, true)));
        assert_eq!(keystroke('|'), Some((0x0D, true)));
        assert_eq!(keystroke('\u{e9}'), None, "outside the US keymap");
        assert_eq!(keystroke('\u{1}'), None, "control character");
    }

    #[test]
    fn every_printable_ascii_character_is_typable() {
        for byte in 0x20u8..0x7F {
            let c = char::from(byte);
            assert!(keystroke(c).is_some(), "{c:?} has no key");
        }
        // Each unshifted key is a distinct raw code; the shifted symbol of
        // a key shares that code.
        let mut seen = std::collections::HashSet::new();
        for byte in 0x20u8..0x7F {
            let (raw, shift) = keystroke(char::from(byte)).unwrap();
            if !shift {
                assert!(seen.insert(raw), "raw {raw:#04X} mapped twice");
            }
        }
    }

    #[test]
    fn unescape_resolves_the_documented_sequences_only() {
        assert_eq!(unescape("dir\\n"), "dir\n");
        assert_eq!(unescape("a\\tb"), "a\tb");
        assert_eq!(unescape("\\e"), "\u{1b}");
        assert_eq!(unescape("\\b"), "\u{8}");
        assert_eq!(unescape("c:\\\\dir"), "c:\\dir");
        assert_eq!(unescape("\\r\\n"), "\n\n");
        assert_eq!(unescape("keep \\x here"), "keep \\x here");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn plain_text_types_one_key_per_character_at_the_pitch() {
        let (keys, bad) = keystrokes_for_text("ab\n");
        assert!(bad.is_empty());
        assert_eq!(
            keys,
            vec![
                TypedKey {
                    offset_ms: 0,
                    rawkey: 0x20,
                    hold_ms: TYPE_KEY_HOLD_MS
                },
                TypedKey {
                    offset_ms: TYPE_KEY_PITCH_MS,
                    rawkey: 0x35,
                    hold_ms: TYPE_KEY_HOLD_MS
                },
                TypedKey {
                    offset_ms: 2 * TYPE_KEY_PITCH_MS,
                    rawkey: 0x44,
                    hold_ms: TYPE_KEY_HOLD_MS
                },
            ]
        );
        assert_eq!(
            typing_duration_ms(&keys),
            2 * TYPE_KEY_PITCH_MS + TYPE_KEY_HOLD_MS
        );
    }

    #[test]
    fn shifted_characters_bracket_the_key_with_shift() {
        let (keys, bad) = keystrokes_for_text("A!");
        assert!(bad.is_empty());
        assert_eq!(keys.len(), 4);
        // Shift first, ahead of its key, held past the key's release.
        assert_eq!(keys[0].rawkey, RAWKEY_LSHIFT);
        assert_eq!(keys[0].offset_ms, 0);
        assert_eq!(keys[1].rawkey, 0x20);
        assert_eq!(keys[1].offset_ms, TYPE_SHIFT_LEAD_MS);
        assert!(keys[0].offset_ms + keys[0].hold_ms > keys[1].offset_ms + keys[1].hold_ms);
        // The next key's Shift starts after the previous Shift is released,
        // so the two Shift presses never overlap (a duplicate press would
        // be swallowed by the held-key filter).
        assert_eq!(keys[2].rawkey, RAWKEY_LSHIFT);
        assert!(keys[2].offset_ms >= keys[0].offset_ms + keys[0].hold_ms);
        assert_eq!(keys[3].rawkey, 0x01);
    }

    #[test]
    fn untypable_characters_are_reported_and_skipped() {
        let (keys, bad) = keystrokes_for_text("caf\u{e9}\u{e9} ok\r\n");
        assert_eq!(bad, vec!['\u{e9}']);
        let typed: Vec<u8> = keys.iter().map(|k| k.rawkey).collect();
        // c a f, space, o k, Return: the CR is dropped, not reported.
        assert_eq!(typed, vec![0x33, 0x20, 0x23, 0x40, 0x18, 0x27, 0x44]);
        assert_eq!(keystrokes_for_text(""), (Vec::new(), Vec::new()));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later

//! Parse what a terminal sends: Kitty keyboard protocol key reports (with
//! press, repeat and release), legacy key bytes for terminals without it, SGR
//! mouse reports, focus changes, bracketed pastes, and the replies to the
//! capability queries sent at start-up.
//!
//! Key reports are reduced to [`TermKey`], which names the *physical* key
//! where the terminal says which one it was: the Kitty protocol's base-layout
//! key (the key's place on a US PC-101 keyboard) wins over the character the
//! active layout produced, since the Amiga keyboard is a matrix of physical
//! keys too.

/// A key, by its place on the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermKey {
    /// A text key, by the unshifted character it carries on a US layout
    /// (lowercase letters, digits, punctuation, space). Characters that are
    /// not on a US layout pass through as they are.
    Char(char),
    Escape,
    Enter,
    Tab,
    Backspace,
    Insert,
    Delete,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    /// F1-F35.
    F(u8),
    Keypad(KeypadKey),
    Modifier(ModifierKey, Side),
    /// A Kitty key number with no meaning here (media keys and the like).
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
    Separator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModifierKey {
    Shift,
    Control,
    Alt,
    Super,
    Hyper,
    Meta,
    /// AltGr on layouts that report it apart from Alt.
    IsoLevel3Shift,
    IsoLevel5Shift,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

/// Modifier state as the Kitty protocol encodes it (its value minus one).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods(pub u8);

impl Mods {
    pub const SHIFT: u8 = 1;
    pub const ALT: u8 = 2;
    pub const CTRL: u8 = 4;
    pub const SUPER: u8 = 8;

    pub fn shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }
    pub fn alt(self) -> bool {
        self.0 & Self::ALT != 0
    }
    pub fn ctrl(self) -> bool {
        self.0 & Self::CTRL != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyReport {
    pub key: TermKey,
    pub mods: Mods,
    pub action: KeyAction,
    /// The key came as a legacy byte sequence (plain text, a control byte,
    /// SS3, ESC-prefixed Alt), which never has a release to follow: the
    /// receiver has to make it a tap. A CSI key report is a press unless it
    /// says otherwise (the Kitty protocol omits the event type of a plain
    /// press), so whether *its* release will come depends on the keyboard
    /// flags the session negotiated, not on the report.
    pub legacy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held.
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Buttons 8-11.
    Extra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub button: MouseButton,
    /// True for a press (or a motion report), false for a release.
    pub pressed: bool,
    pub motion: bool,
    /// Position as reported: pixels under SGR-pixel mode (1016), cells
    /// otherwise. Both 1-based.
    pub x: u32,
    pub y: u32,
    pub mods: Mods,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(KeyReport),
    Mouse(MouseReport),
    Focus(bool),
    Paste(String),
    /// `CSI ? flags u`: the Kitty keyboard enhancement flags in force.
    KeyboardFlags(u32),
    /// `CSI ? ... c`: the primary device attributes reply. Terminals answer
    /// queries in order, so this marks the end of the start-up probe.
    DeviceAttributes,
    /// An `APC G` reply to a graphics command: the image id it names and
    /// whether the terminal said `OK`.
    Graphics {
        id: u32,
        ok: bool,
    },
    /// `CSI 6 ; height ; width t`: one cell's size in pixels.
    CellPixels {
        width: u32,
        height: u32,
    },
    /// `CSI 4 ; height ; width t`: the text area's size in pixels.
    WindowPixels {
        width: u32,
        height: u32,
    },
}

const ESC: u8 = 0x1b;
const PASTE_END: &[u8] = b"\x1b[201~";

/// The most bytes of one unfinished sequence kept waiting for the rest.
/// Real reports are tens of bytes; past this it is noise, and dropped.
const MAX_PENDING: usize = 64 * 1024;

/// Incremental parser: bytes go in as they arrive, complete events come out,
/// and a sequence split across reads waits for the rest.
#[derive(Default)]
pub struct Parser {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse `bytes` and append the events completed so far to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Event>) {
        self.pending.extend_from_slice(bytes);
        let mut pos = 0;
        while pos < self.pending.len() {
            if let Some(paste) = self.paste.as_mut() {
                let rest = &self.pending[pos..];
                if let Some(end) = find(rest, PASTE_END) {
                    paste.extend_from_slice(&rest[..end]);
                    let text = String::from_utf8_lossy(paste).into_owned();
                    self.paste = None;
                    out.push(Event::Paste(text));
                    pos += end + PASTE_END.len();
                    continue;
                }
                // Keep a possible partial terminator for the next read.
                let keep = partial_suffix(rest, PASTE_END);
                paste.extend_from_slice(&rest[..rest.len() - keep]);
                pos = self.pending.len() - keep;
                break;
            }
            match parse_one(&self.pending[pos..]) {
                Parsed::Event(event, used) => {
                    pos += used;
                    match event {
                        Some(Inner::PasteStart) => self.paste = Some(Vec::new()),
                        Some(Inner::Event(event)) => out.push(event),
                        None => {}
                    }
                }
                Parsed::Incomplete => break,
            }
        }
        self.pending.drain(..pos);
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
        }
    }

    /// Whether all that is waiting is a lone ESC: the Escape key on a
    /// terminal not reporting it as a sequence, or the first byte of a
    /// sequence still on its way. Only time tells which.
    pub fn lone_escape_pending(&self) -> bool {
        self.pending == [ESC]
    }

    /// No more bytes followed a lone ESC: it was the Escape key. Any other
    /// unfinished sequence is left waiting for the rest of its bytes, however
    /// long the link takes to deliver them.
    pub fn flush_lone_escape(&mut self, out: &mut Vec<Event>) {
        if self.lone_escape_pending() {
            out.push(Event::Key(tap(TermKey::Escape, Mods::default())));
            self.pending.clear();
        }
    }
}

enum Inner {
    Event(Event),
    PasteStart,
}

enum Parsed {
    /// An event (or a sequence to ignore) and the bytes it used.
    Event(Option<Inner>, usize),
    Incomplete,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Length of the longest suffix of `data` that is a proper prefix of `needle`.
fn partial_suffix(data: &[u8], needle: &[u8]) -> usize {
    (1..needle.len().min(data.len() + 1))
        .rev()
        .find(|&n| data.ends_with(&needle[..n]))
        .unwrap_or(0)
}

fn tap(key: TermKey, mods: Mods) -> KeyReport {
    KeyReport {
        key,
        mods,
        action: KeyAction::Press,
        legacy: true,
    }
}

fn key_event(key: TermKey, mods: Mods) -> Parsed {
    Parsed::Event(Some(Inner::Event(Event::Key(tap(key, mods)))), 0)
}

fn with_len(parsed: Parsed, len: usize) -> Parsed {
    match parsed {
        Parsed::Event(event, _) => Parsed::Event(event, len),
        Parsed::Incomplete => Parsed::Incomplete,
    }
}

fn parse_one(buf: &[u8]) -> Parsed {
    let first = buf[0];
    if first != ESC {
        return parse_legacy_text(buf);
    }
    let Some(&second) = buf.get(1) else {
        return Parsed::Incomplete;
    };
    match second {
        b'[' => parse_csi(buf),
        b'_' => parse_apc(buf),
        b'O' => {
            // SS3: application-mode cursor keys and F1-F4.
            let Some(&code) = buf.get(2) else {
                return Parsed::Incomplete;
            };
            let key = match code {
                b'A' => Some(TermKey::Up),
                b'B' => Some(TermKey::Down),
                b'C' => Some(TermKey::Right),
                b'D' => Some(TermKey::Left),
                b'H' => Some(TermKey::Home),
                b'F' => Some(TermKey::End),
                b'P' => Some(TermKey::F(1)),
                b'Q' => Some(TermKey::F(2)),
                b'R' => Some(TermKey::F(3)),
                b'S' => Some(TermKey::F(4)),
                b'M' => Some(TermKey::Keypad(KeypadKey::Enter)),
                _ => None,
            };
            match key {
                Some(key) => with_len(key_event(key, Mods::default()), 3),
                None => Parsed::Event(None, 3),
            }
        }
        ESC => {
            // ESC ESC: an Escape key, then whatever follows.
            with_len(key_event(TermKey::Escape, Mods::default()), 1)
        }
        _ => {
            // Legacy Alt+key: ESC then the key's own bytes.
            match parse_legacy_text(&buf[1..]) {
                Parsed::Event(Some(Inner::Event(Event::Key(mut report))), used) => {
                    report.mods.0 |= Mods::ALT;
                    Parsed::Event(Some(Inner::Event(Event::Key(report))), used + 1)
                }
                Parsed::Event(other, used) => Parsed::Event(other, used + 1),
                Parsed::Incomplete => Parsed::Incomplete,
            }
        }
    }
}

/// A byte (or UTF-8 character) from a terminal not using the Kitty keyboard
/// protocol for it: a tap, with the Shift or Ctrl it implies.
fn parse_legacy_text(buf: &[u8]) -> Parsed {
    let first = buf[0];
    let (key, mods) = match first {
        b'\r' | b'\n' => (TermKey::Enter, 0),
        b'\t' => (TermKey::Tab, 0),
        0x7f | 0x08 => (TermKey::Backspace, 0),
        0x00 => (TermKey::Char(' '), Mods::CTRL),
        0x01..=0x1a => (TermKey::Char((b'a' + first - 1) as char), Mods::CTRL),
        0x1c..=0x1f => return Parsed::Event(None, 1),
        0x20..=0x7e => {
            let (base, shifted) = us_base_key(first as char);
            (TermKey::Char(base), if shifted { Mods::SHIFT } else { 0 })
        }
        _ => {
            // UTF-8: take the whole character.
            let len = match first {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return Parsed::Event(None, 1),
            };
            if buf.len() < len {
                return Parsed::Incomplete;
            }
            return match std::str::from_utf8(&buf[..len])
                .ok()
                .and_then(|s| s.chars().next())
            {
                Some(ch) => with_len(key_event(TermKey::Char(ch), Mods::default()), len),
                None => Parsed::Event(None, 1),
            };
        }
    };
    with_len(key_event(key, Mods(mods)), 1)
}

/// The US-layout key that types `ch`, and whether it takes Shift.
pub fn us_base_key(ch: char) -> (char, bool) {
    let shifted_of = |c| match c {
        '!' => Some('1'),
        '@' => Some('2'),
        '#' => Some('3'),
        '$' => Some('4'),
        '%' => Some('5'),
        '^' => Some('6'),
        '&' => Some('7'),
        '*' => Some('8'),
        '(' => Some('9'),
        ')' => Some('0'),
        '_' => Some('-'),
        '+' => Some('='),
        '{' => Some('['),
        '}' => Some(']'),
        '|' => Some('\\'),
        ':' => Some(';'),
        '"' => Some('\''),
        '<' => Some(','),
        '>' => Some('.'),
        '?' => Some('/'),
        '~' => Some('`'),
        _ => None,
    };
    if ch.is_ascii_uppercase() {
        (ch.to_ascii_lowercase(), true)
    } else if let Some(base) = shifted_of(ch) {
        (base, true)
    } else {
        (ch, false)
    }
}

/// `ESC _ ... ESC \`: only graphics replies (`G`) mean anything here.
fn parse_apc(buf: &[u8]) -> Parsed {
    let Some(end) = find(&buf[2..], b"\x1b\\") else {
        return Parsed::Incomplete;
    };
    let body = &buf[2..2 + end];
    let used = 2 + end + 2;
    let Some(body) = body.strip_prefix(b"G") else {
        return Parsed::Event(None, used);
    };
    let text = String::from_utf8_lossy(body);
    let (keys, message) = text.split_once(';').unwrap_or((&text, ""));
    let id = keys
        .split(',')
        .find_map(|kv| kv.strip_prefix("i="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Parsed::Event(
        Some(Inner::Event(Event::Graphics {
            id,
            ok: message == "OK",
        })),
        used,
    )
}

/// One `;`-separated CSI parameter, as its `:`-separated sub-parameters.
fn sub_params(field: &str) -> Vec<Option<u32>> {
    field
        .split(':')
        .map(|v| if v.is_empty() { None } else { v.parse().ok() })
        .collect()
}

fn parse_csi(buf: &[u8]) -> Parsed {
    // ESC [ params (0x30-0x3F)* intermediates (0x20-0x2F)* final (0x40-0x7E)
    let mut i = 2;
    while i < buf.len() && (0x20..=0x3f).contains(&buf[i]) {
        i += 1;
    }
    let Some(&final_byte) = buf.get(i) else {
        return Parsed::Incomplete;
    };
    let used = i + 1;
    if !(0x40..=0x7e).contains(&final_byte) {
        // Not a CSI sequence after all: drop the introducer.
        return Parsed::Event(None, 2);
    }
    let params = std::str::from_utf8(&buf[2..i]).unwrap_or("");
    with_len(interpret_csi(params, final_byte), used)
}

fn interpret_csi(params: &str, final_byte: u8) -> Parsed {
    let none = Parsed::Event(None, 0);
    if let Some(rest) = params.strip_prefix('<') {
        return if matches!(final_byte, b'M' | b'm') {
            parse_sgr_mouse(rest, final_byte == b'M').map_or(none, |m| {
                Parsed::Event(Some(Inner::Event(Event::Mouse(m))), 0)
            })
        } else {
            none
        };
    }
    if let Some(rest) = params.strip_prefix('?') {
        return match final_byte {
            b'u' => Parsed::Event(
                Some(Inner::Event(Event::KeyboardFlags(
                    rest.parse().unwrap_or(0),
                ))),
                0,
            ),
            b'c' => Parsed::Event(Some(Inner::Event(Event::DeviceAttributes)), 0),
            _ => none,
        };
    }
    if params.starts_with(['>', '=']) {
        return none;
    }
    let fields: Vec<&str> = params.split(';').collect();
    let field = |n: usize| fields.get(n).map(|f| sub_params(f)).unwrap_or_default();
    let number = field(0).first().copied().flatten();
    if final_byte == b't' {
        let f = |n| field(n).first().copied().flatten().unwrap_or(0);
        return match number {
            Some(6) => Parsed::Event(
                Some(Inner::Event(Event::CellPixels {
                    height: f(1),
                    width: f(2),
                })),
                0,
            ),
            Some(4) => Parsed::Event(
                Some(Inner::Event(Event::WindowPixels {
                    height: f(1),
                    width: f(2),
                })),
                0,
            ),
            _ => none,
        };
    }
    if params.is_empty() {
        match final_byte {
            b'I' => return Parsed::Event(Some(Inner::Event(Event::Focus(true))), 0),
            b'O' => return Parsed::Event(Some(Inner::Event(Event::Focus(false))), 0),
            b'Z' => return key_event(TermKey::Tab, Mods(Mods::SHIFT)),
            _ => {}
        }
    }
    if final_byte == b'~' && number == Some(200) {
        return Parsed::Event(Some(Inner::PasteStart), 0);
    }
    let key = match final_byte {
        b'u' => {
            let codes = field(0);
            let code = codes.first().copied().flatten().unwrap_or(0);
            // The base-layout key (third sub-parameter) names the physical
            // key; the first is what the active layout made of it.
            let base = codes.get(2).copied().flatten();
            kitty_key(base.unwrap_or(code)).or_else(|| kitty_key(code))
        }
        b'~' => number.and_then(tilde_key),
        b'A' => Some(TermKey::Up),
        b'B' => Some(TermKey::Down),
        b'C' => Some(TermKey::Right),
        b'D' => Some(TermKey::Left),
        b'E' => Some(TermKey::Keypad(KeypadKey::Digit(5))),
        b'F' => Some(TermKey::End),
        b'H' => Some(TermKey::Home),
        b'P' => Some(TermKey::F(1)),
        b'Q' => Some(TermKey::F(2)),
        b'S' => Some(TermKey::F(4)),
        _ => None,
    };
    let Some(key) = key else {
        return none;
    };
    let modifiers = field(1);
    let mods = Mods(
        modifiers
            .first()
            .copied()
            .flatten()
            .map_or(0, |m| m.saturating_sub(1) as u8),
    );
    let event_type = modifiers.get(1).copied().flatten();
    let action = match event_type {
        Some(2) => KeyAction::Repeat,
        Some(3) => KeyAction::Release,
        _ => KeyAction::Press,
    };
    Parsed::Event(
        Some(Inner::Event(Event::Key(KeyReport {
            key,
            mods,
            action,
            legacy: false,
        }))),
        0,
    )
}

/// The key a `CSI number ~` sequence names.
fn tilde_key(number: u32) -> Option<TermKey> {
    Some(match number {
        2 => TermKey::Insert,
        3 => TermKey::Delete,
        5 => TermKey::PageUp,
        6 => TermKey::PageDown,
        7 | 1 => TermKey::Home,
        8 | 4 => TermKey::End,
        11 => TermKey::F(1),
        12 => TermKey::F(2),
        13 => TermKey::F(3),
        14 => TermKey::F(4),
        15 => TermKey::F(5),
        17 => TermKey::F(6),
        18 => TermKey::F(7),
        19 => TermKey::F(8),
        20 => TermKey::F(9),
        21 => TermKey::F(10),
        23 => TermKey::F(11),
        24 => TermKey::F(12),
        29 => TermKey::Menu,
        57427 => TermKey::Keypad(KeypadKey::Digit(5)),
        _ => return None,
    })
}

/// The key a Kitty `CSI number u` key number names.
fn kitty_key(code: u32) -> Option<TermKey> {
    use ModifierKey::*;
    use Side::*;
    Some(match code {
        27 => TermKey::Escape,
        13 => TermKey::Enter,
        9 => TermKey::Tab,
        127 | 8 => TermKey::Backspace,
        57358 => TermKey::CapsLock,
        57359 => TermKey::ScrollLock,
        57360 => TermKey::NumLock,
        57361 => TermKey::PrintScreen,
        57362 => TermKey::Pause,
        57363 => TermKey::Menu,
        57376..=57398 => TermKey::F((code - 57376 + 13) as u8),
        57399..=57408 => TermKey::Keypad(KeypadKey::Digit((code - 57399) as u8)),
        57409 => TermKey::Keypad(KeypadKey::Decimal),
        57410 => TermKey::Keypad(KeypadKey::Divide),
        57411 => TermKey::Keypad(KeypadKey::Multiply),
        57412 => TermKey::Keypad(KeypadKey::Subtract),
        57413 => TermKey::Keypad(KeypadKey::Add),
        57414 => TermKey::Keypad(KeypadKey::Enter),
        57415 => TermKey::Keypad(KeypadKey::Equal),
        57416 => TermKey::Keypad(KeypadKey::Separator),
        // The keypad's navigation names (Num Lock off) are its digit keys.
        57417 => TermKey::Keypad(KeypadKey::Digit(4)),
        57418 => TermKey::Keypad(KeypadKey::Digit(6)),
        57419 => TermKey::Keypad(KeypadKey::Digit(8)),
        57420 => TermKey::Keypad(KeypadKey::Digit(2)),
        57421 => TermKey::Keypad(KeypadKey::Digit(9)),
        57422 => TermKey::Keypad(KeypadKey::Digit(3)),
        57423 => TermKey::Keypad(KeypadKey::Digit(7)),
        57424 => TermKey::Keypad(KeypadKey::Digit(1)),
        57425 => TermKey::Keypad(KeypadKey::Digit(0)),
        57426 => TermKey::Keypad(KeypadKey::Decimal),
        57427 => TermKey::Keypad(KeypadKey::Digit(5)),
        57441 => TermKey::Modifier(Shift, Left),
        57442 => TermKey::Modifier(Control, Left),
        57443 => TermKey::Modifier(Alt, Left),
        57444 => TermKey::Modifier(Super, Left),
        57445 => TermKey::Modifier(Hyper, Left),
        57446 => TermKey::Modifier(Meta, Left),
        57447 => TermKey::Modifier(Shift, Right),
        57448 => TermKey::Modifier(Control, Right),
        57449 => TermKey::Modifier(Alt, Right),
        57450 => TermKey::Modifier(Super, Right),
        57451 => TermKey::Modifier(Hyper, Right),
        57452 => TermKey::Modifier(Meta, Right),
        57453 => TermKey::Modifier(IsoLevel3Shift, Right),
        57454 => TermKey::Modifier(IsoLevel5Shift, Right),
        57344..=63743 => TermKey::Other(code),
        _ => {
            let ch = char::from_u32(code)?;
            if ch.is_control() {
                return None;
            }
            TermKey::Char(ch.to_lowercase().next().unwrap_or(ch))
        }
    })
}

fn parse_sgr_mouse(params: &str, pressed: bool) -> Option<MouseReport> {
    let mut fields = params.split(';').map(|f| f.parse::<u32>().ok());
    let code = fields.next()??;
    let x = fields.next()??;
    let y = fields.next()??;
    let low = code & 3;
    let button = if code & 128 != 0 {
        MouseButton::Extra
    } else if code & 64 != 0 {
        match low {
            0 => MouseButton::WheelUp,
            1 => MouseButton::WheelDown,
            2 => MouseButton::WheelLeft,
            _ => MouseButton::WheelRight,
        }
    } else {
        match low {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::None,
        }
    };
    let mut mods = 0;
    if code & 4 != 0 {
        mods |= Mods::SHIFT;
    }
    if code & 8 != 0 {
        mods |= Mods::ALT;
    }
    if code & 16 != 0 {
        mods |= Mods::CTRL;
    }
    Some(MouseReport {
        button,
        pressed,
        motion: code & 32 != 0,
        x,
        y,
        mods: Mods(mods),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<Event> {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        parser.feed(bytes, &mut out);
        out
    }

    fn key(key: TermKey, mods: u8, action: KeyAction, legacy: bool) -> Event {
        Event::Key(KeyReport {
            key,
            mods: Mods(mods),
            action,
            legacy,
        })
    }

    #[test]
    fn kitty_text_keys_report_press_and_release() {
        assert_eq!(
            parse(b"\x1b[97;1:1u\x1b[97;1:3u"),
            vec![
                key(TermKey::Char('a'), 0, KeyAction::Press, false),
                key(TermKey::Char('a'), 0, KeyAction::Release, false),
            ]
        );
        // Shift+a: the key is still 'a', with its shifted form alongside.
        assert_eq!(
            parse(b"\x1b[97:65;2:1u"),
            vec![key(TermKey::Char('a'), 1, KeyAction::Press, false)]
        );
        assert_eq!(
            parse(b"\x1b[97;1:2u"),
            vec![key(TermKey::Char('a'), 0, KeyAction::Repeat, false)]
        );
    }

    #[test]
    fn kitty_plain_press_omits_its_event_type() {
        // A press with no modifiers may come without the ";1:1": still a
        // CSI report (not legacy), so a release can follow it.
        assert_eq!(
            parse(b"\x1b[97u\x1b[97;1:3u\x1b[A\x1b[1;1:3A"),
            vec![
                key(TermKey::Char('a'), 0, KeyAction::Press, false),
                key(TermKey::Char('a'), 0, KeyAction::Release, false),
                key(TermKey::Up, 0, KeyAction::Press, false),
                key(TermKey::Up, 0, KeyAction::Release, false),
            ]
        );
    }

    #[test]
    fn kitty_base_layout_key_names_the_physical_key() {
        // A French AZERTY 'a' sits where US 'q' is: the base-layout key wins.
        assert_eq!(
            parse(b"\x1b[97::113;1:1u"),
            vec![key(TermKey::Char('q'), 0, KeyAction::Press, false)]
        );
    }

    #[test]
    fn kitty_functional_and_modifier_keys() {
        assert_eq!(
            parse(b"\x1b[57441;2:1u\x1b[57448;5:3u\x1b[27u\x1b[13;1:3u"),
            vec![
                key(
                    TermKey::Modifier(ModifierKey::Shift, Side::Left),
                    1,
                    KeyAction::Press,
                    false
                ),
                key(
                    TermKey::Modifier(ModifierKey::Control, Side::Right),
                    4,
                    KeyAction::Release,
                    false
                ),
                key(TermKey::Escape, 0, KeyAction::Press, false),
                key(TermKey::Enter, 0, KeyAction::Release, false),
            ]
        );
        assert_eq!(
            parse(b"\x1b[57399;1:1u\x1b[57414u"),
            vec![
                key(
                    TermKey::Keypad(KeypadKey::Digit(0)),
                    0,
                    KeyAction::Press,
                    false
                ),
                key(
                    TermKey::Keypad(KeypadKey::Enter),
                    0,
                    KeyAction::Press,
                    false
                ),
            ]
        );
    }

    #[test]
    fn legacy_form_keys_carry_kitty_event_types() {
        assert_eq!(
            parse(b"\x1b[1;1:3A\x1b[1;5D\x1b[24;1:1~\x1b[13~\x1b[3;1:3~\x1b[1;1:1P"),
            vec![
                key(TermKey::Up, 0, KeyAction::Release, false),
                key(TermKey::Left, 4, KeyAction::Press, false),
                key(TermKey::F(12), 0, KeyAction::Press, false),
                key(TermKey::F(3), 0, KeyAction::Press, false),
                key(TermKey::Delete, 0, KeyAction::Release, false),
                key(TermKey::F(1), 0, KeyAction::Press, false),
            ]
        );
    }

    #[test]
    fn legacy_bytes_are_taps_with_implied_modifiers() {
        assert_eq!(
            parse(b"aA!\r\x03\x7f"),
            vec![
                key(TermKey::Char('a'), 0, KeyAction::Press, true),
                key(TermKey::Char('a'), 1, KeyAction::Press, true),
                key(TermKey::Char('1'), 1, KeyAction::Press, true),
                key(TermKey::Enter, 0, KeyAction::Press, true),
                key(TermKey::Char('c'), 4, KeyAction::Press, true),
                key(TermKey::Backspace, 0, KeyAction::Press, true),
            ]
        );
        // Alt+x as ESC x, and SS3 cursor keys.
        assert_eq!(
            parse(b"\x1bx\x1bOA"),
            vec![
                key(TermKey::Char('x'), 2, KeyAction::Press, true),
                key(TermKey::Up, 0, KeyAction::Press, true),
            ]
        );
    }

    #[test]
    fn lone_escape_waits_then_flushes_as_the_escape_key() {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        parser.feed(b"\x1b", &mut out);
        assert!(out.is_empty());
        assert!(parser.lone_escape_pending());
        parser.flush_lone_escape(&mut out);
        assert_eq!(out, vec![key(TermKey::Escape, 0, KeyAction::Press, true)]);
        assert!(!parser.lone_escape_pending());
    }

    #[test]
    fn unfinished_sequences_survive_a_pause_in_the_link() {
        // A key release split across reads with a long gap: the Escape
        // timeout must not cut it short, or its tail would arrive as keys.
        let mut parser = Parser::new();
        let mut out = Vec::new();
        parser.feed(b"\x1b[97;", &mut out);
        assert!(!parser.lone_escape_pending());
        parser.flush_lone_escape(&mut out);
        assert!(out.is_empty());
        parser.feed(b"1:3u", &mut out);
        assert_eq!(
            out,
            vec![key(TermKey::Char('a'), 0, KeyAction::Release, false)]
        );
        // The same for a paste's terminator split across reads.
        out.clear();
        parser.feed(b"\x1b[200~hi\x1b[20", &mut out);
        parser.flush_lone_escape(&mut out);
        parser.feed(b"1~", &mut out);
        assert_eq!(out, vec![Event::Paste("hi".into())]);
    }

    #[test]
    fn sequences_split_across_reads_reassemble() {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        parser.feed(b"\x1b[9", &mut out);
        parser.feed(b"7;1:", &mut out);
        assert!(out.is_empty());
        parser.feed(b"3u", &mut out);
        assert_eq!(
            out,
            vec![key(TermKey::Char('a'), 0, KeyAction::Release, false)]
        );
    }

    #[test]
    fn sgr_mouse_reports() {
        assert_eq!(
            parse(b"\x1b[<0;120;45M\x1b[<35;121;46M\x1b[<2;5;6m\x1b[<65;1;1M"),
            vec![
                Event::Mouse(MouseReport {
                    button: MouseButton::Left,
                    pressed: true,
                    motion: false,
                    x: 120,
                    y: 45,
                    mods: Mods(0)
                }),
                Event::Mouse(MouseReport {
                    button: MouseButton::None,
                    pressed: true,
                    motion: true,
                    x: 121,
                    y: 46,
                    mods: Mods(0)
                }),
                Event::Mouse(MouseReport {
                    button: MouseButton::Right,
                    pressed: false,
                    motion: false,
                    x: 5,
                    y: 6,
                    mods: Mods(0)
                }),
                Event::Mouse(MouseReport {
                    button: MouseButton::WheelDown,
                    pressed: true,
                    motion: false,
                    x: 1,
                    y: 1,
                    mods: Mods(0)
                }),
            ]
        );
    }

    #[test]
    fn focus_paste_and_query_replies() {
        assert_eq!(
            parse(
                b"\x1b[I\x1b[O\x1b[?15u\x1b_Gi=31;OK\x1b\\\x1b_Gi=31;ENOTSUP:x\x1b\\\x1b[6;20;10t\x1b[?62;22c"
            ),
            vec![
                Event::Focus(true),
                Event::Focus(false),
                Event::KeyboardFlags(15),
                Event::Graphics { id: 31, ok: true },
                Event::Graphics { id: 31, ok: false },
                Event::CellPixels {
                    width: 10,
                    height: 20
                },
                Event::DeviceAttributes,
            ]
        );
    }

    #[test]
    fn bracketed_paste_collects_text_across_reads() {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        parser.feed(b"\x1b[200~echo hi\x1b[2", &mut out);
        assert!(out.is_empty());
        parser.feed(b"01~x", &mut out);
        assert_eq!(
            out,
            vec![
                Event::Paste("echo hi".into()),
                key(TermKey::Char('x'), 0, KeyAction::Press, true),
            ]
        );
    }

    #[test]
    fn us_base_key_unshifts_symbols() {
        assert_eq!(us_base_key('Q'), ('q', true));
        assert_eq!(us_base_key('?'), ('/', true));
        assert_eq!(us_base_key('/'), ('/', false));
        assert_eq!(us_base_key('~'), ('`', true));
    }
}

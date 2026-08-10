// SPDX-License-Identifier: GPL-3.0-or-later

//! The on-screen Amiga keyboard, drawn under the display.
//!
//! It is an A600: the one Amiga keyboard with no numeric keypad, so the
//! whole machine fits the 716-pixel canvas at a usable cap size instead of
//! spending a third of the width on a keypad. Geometry is the A600's own
//! 1u grid -- every row 16.5u wide, six rows plus the 0.5u float under the
//! function row -- and the rawkeys are RKM Libraries table 34-6.
//!
//! Clicks go to the keyboard MCU as rawkey transitions, not as host key
//! codes: $2B, the key beside Return, has no host key on every layout, and
//! a synthetic host event would also be offered to the keyboard-joystick
//! mapping first, which would steer a joystick with the cursor keys. An
//! on-screen Amiga keyboard types.
//!
//! The same strip serves the two things a host keyboard cannot do: keys
//! the host has no equivalent for (Help, both Amiga keys, $2B), and a
//! session driven entirely by the mouse.

use super::statusbar::{blend_pixel, draw_rect_bevel, fill_rect};
use super::Rect;
use super::{
    texture_height, texture_width, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE,
    BUTTON_FACE_HOVER, STATUS_BG, STATUS_BOTTOM, STATUS_TEXT, STATUS_TOP,
};
use crate::video::font;
use crate::video::FB_WIDTH;
use std::time::{Duration, Instant};

/// One grid unit, in canvas pixels. The A600 is 16.5u wide and the canvas
/// is 716, so 43.4 is the largest unit that fills it; 42 is the whole
/// number under that, and the 23 pixels it leaves become the side margin
/// that keeps the outermost caps off the window edge.
const KEY_UNIT: usize = 42;
/// The grid, in quarter-units. Everything on an A600 lands on a quarter:
/// the caps are 1, 1.25, 1.5, 1.75, 2 and 8 units, the gaps 0.5, and only
/// the ISO Return's stem offset is a quarter. Positions are accumulated in
/// these integers and converted once, so a row cannot drift.
const GRID_W_Q: usize = 66; // 16.5u
const GRID_H_Q: usize = 26; // 6.5u
/// A quarter-unit position in canvas pixels.
const fn u_px(q: usize) -> usize {
    q * KEY_UNIT / 4
}
const GRID_W: usize = u_px(GRID_W_Q);
const GRID_H: usize = u_px(GRID_H_Q);
/// The margin above and below the keys. The side margin is whatever the
/// grid leaves of the canvas, so the keyboard is centred on it.
const PAD_Y: usize = 5;
const PAD_X: usize = (FB_WIDTH - GRID_W) / 2;

/// How tall the strip is: the six rows plus the margin around them.
pub const KBD_PANEL_HEIGHT: usize = GRID_H + 2 * PAD_Y;

/// How far the visible cap is inset inside its grid cell. The whole cell
/// is live, so the gaps between caps still hit the key they belong to.
const CAP_INSET: usize = 2;

// The fascia is the status bar's, so the strip and the bar read as one
// piece of chrome rather than two greys that nearly match.
const PANEL_FACE: u32 = STATUS_BG;
const PANEL_EDGE_LIGHT: u32 = STATUS_TOP;
const PANEL_EDGE_DARK: u32 = STATUS_BOTTOM;

// Keycaps. Pale mouldings with near-black legends, the way an A600's caps
// are, rather than the dark chrome of the fascia they sit on -- a keyboard
// drawn in the fascia's own greys would read as a grille.
const CAP_IDLE: u32 = rgba(226, 223, 214);
const CAP_MOD: u32 = rgba(196, 193, 184);
const CAP_DOWN: u32 = rgba(168, 164, 152);
/// A locked qualifier, and the Caps Lock cap while its lamp is lit.
const CAP_LOCKED: u32 = rgba(232, 145, 84);
const CAP_INK: u32 = rgba(21, 23, 28);
/// The Caps Lock lamp in the corner of its own cap, driven by the MCU.
const CAPS_LED_ON: u32 = rgba(44, 200, 80);
/// How far a cap lifts under the pointer, matching the status bar's own
/// buttons so a hover reads the same wherever on the window it happens.
const HOVER_LIFT: f32 = 16.0;
/// How the moulding is shaded: the light from the top left, as every
/// bevel on the window has it.
const CAP_BEVEL_LIGHT: f32 = 26.0;
const CAP_BEVEL_DARK: f32 = -64.0;
/// How far a shifted legend falls back towards its cap, so the two lines
/// read as the printed pair they are rather than as two legends.
const SHIFT_DIM: f32 = 0.45;

/// A click on a qualifier shorter than this is a tap, which latches it for
/// the next keystroke; two taps inside the double window lock it down.
const TAP: Duration = Duration::from_millis(250);
const DOUBLE_TAP: Duration = Duration::from_millis(500);

const RAWKEY_CAPS_LOCK: u8 = 0x62;
const RAWKEY_CTRL: u8 = 0x63;
const RAWKEY_LEFT_AMIGA: u8 = 0x66;
const RAWKEY_RIGHT_AMIGA: u8 = 0x67;

/// The seven latching qualifiers, in the order their state is held. Caps
/// Lock is deliberately not among them: the MCU owns that latch (see
/// `chipset::keyboard::Keyboard::key_transition`), where the press toggles
/// the lamp and sends the down code on lock or the up code on unlock, and
/// the release sends nothing at all.
const MODIFIERS: [u8; 7] = [
    RAWKEY_CTRL,
    0x60, // left shift
    0x61, // right shift
    0x64, // left alt
    0x65, // right alt
    RAWKEY_LEFT_AMIGA,
    RAWKEY_RIGHT_AMIGA,
];

const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

/// The same colour lifted towards the light (positive) or turned away from
/// it (negative), which is all the moulding on a cap amounts to.
fn shade(colour: u32, by: f32) -> u32 {
    let [r, g, b, a] = colour.to_le_bytes();
    let step = |c: u8| (f32::from(c) + by).clamp(0.0, 255.0) as u8;
    u32::from_le_bytes([step(r), step(g), step(b), a])
}

/// `from` at 0, `to` at 1.
fn mix(from: u32, to: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (from.to_le_bytes(), to.to_le_bytes());
    let lerp = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t) as u8;
    u32::from_le_bytes([lerp(0), lerp(1), lerp(2), a[3]])
}

// --- the layout ----------------------------------------------------------

/// What is printed on a cap. Most legends are text in the 8x8 font; the
/// rest are drawn, because they are outside its ASCII range (the pound) or
/// are marks rather than characters (the cursor arrows, the Amiga key's
/// leaning A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Legend {
    None,
    Text(&'static str),
    Pound,
    Arrow(Arrow),
    /// The Amiga key's mark: outlined on the left key, filled on the
    /// right, as the case prints them.
    Amiga {
        hollow: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrow {
    Up,
    Down,
    Left,
    Right,
}

/// What a cap does beyond sending its rawkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    /// A latching qualifier (see [`KbdPanelState::press`]).
    Modifier,
    /// Caps Lock, whose latch and lamp belong to the MCU.
    Caps,
}

/// One keycap. A row lays out left to right and x accumulates, so a cap
/// carries only what differs from the default 1u key: `gap_q` is the gap
/// before it, `w_q` its width, both in quarter-units.
#[derive(Debug, Clone, Copy)]
struct KeySpec {
    gap_q: u8,
    w_q: u8,
    raw: u8,
    main: Legend,
    shift: Legend,
    kind: Kind,
    /// The ISO Return's stem: how far right of the arm's left edge it
    /// starts, and how wide it is, in quarter-units.
    stem: Option<(u8, u8)>,
}

impl KeySpec {
    const fn new(raw: u8, main: &'static str) -> Self {
        Self {
            gap_q: 0,
            w_q: 4,
            raw,
            main: Legend::Text(main),
            shift: Legend::None,
            kind: Kind::Plain,
            stem: None,
        }
    }

    /// A cap with nothing printed on it: the space bar.
    const fn blank(raw: u8) -> Self {
        Self::glyph(raw, Legend::None)
    }

    const fn glyph(raw: u8, main: Legend) -> Self {
        let mut spec = Self::new(raw, "");
        spec.main = main;
        spec
    }

    const fn shifted(mut self, shift: &'static str) -> Self {
        self.shift = Legend::Text(shift);
        self
    }

    const fn shift_glyph(mut self, shift: Legend) -> Self {
        self.shift = shift;
        self
    }

    const fn width(mut self, w_q: u8) -> Self {
        self.w_q = w_q;
        self
    }

    const fn gap(mut self, gap_q: u8) -> Self {
        self.gap_q = gap_q;
        self
    }

    const fn modifier(mut self) -> Self {
        self.kind = Kind::Modifier;
        self
    }

    const fn caps(mut self) -> Self {
        self.kind = Kind::Caps;
        self
    }

    const fn stem(mut self, dx_q: u8, w_q: u8) -> Self {
        self.stem = Some((dx_q, w_q));
        self
    }
}

struct KeyRow {
    /// The row's top edge, in quarter-units from the top of the grid.
    y_q: u8,
    keys: &'static [KeySpec],
}

/// The A600's 78 keys.
///
/// Rows 2, 3 and 4 stop short of 16.5u. That is not a missing key: it is
/// the notch the inverted-T cursor cluster sits in, and the two chips at
/// the end of this file live in it.
static ROWS: &[KeyRow] = &[
    KeyRow {
        y_q: 0,
        keys: &[
            KeySpec::new(0x45, "Esc").width(5),
            KeySpec::new(0x50, "F1").width(5).gap(2),
            KeySpec::new(0x51, "F2").width(5),
            KeySpec::new(0x52, "F3").width(5),
            KeySpec::new(0x53, "F4").width(5),
            KeySpec::new(0x54, "F5").width(5),
            KeySpec::new(0x55, "F6").width(5).gap(2),
            KeySpec::new(0x56, "F7").width(5),
            KeySpec::new(0x57, "F8").width(5),
            KeySpec::new(0x58, "F9").width(5),
            KeySpec::new(0x59, "F10").width(5),
            KeySpec::new(0x5F, "Help").width(5).gap(2),
        ],
    },
    KeyRow {
        y_q: 6,
        keys: &[
            KeySpec::new(0x00, "`").shifted("~").width(6),
            KeySpec::new(0x01, "1").shifted("!"),
            KeySpec::new(0x02, "2").shifted("\""),
            // The UK cap prints 3 and a pound; the US one 3 and a hash.
            KeySpec::new(0x03, "3").shift_glyph(Legend::Pound),
            KeySpec::new(0x04, "4").shifted("$"),
            KeySpec::new(0x05, "5").shifted("%"),
            KeySpec::new(0x06, "6").shifted("^"),
            KeySpec::new(0x07, "7").shifted("&"),
            KeySpec::new(0x08, "8").shifted("*"),
            KeySpec::new(0x09, "9").shifted("("),
            KeySpec::new(0x0A, "0").shifted(")"),
            KeySpec::new(0x0B, "-").shifted("_"),
            KeySpec::new(0x0C, "=").shifted("+"),
            KeySpec::new(0x0D, "\\").shifted("|"),
            KeySpec::new(0x41, "Bksp"),
            KeySpec::new(0x46, "Del"),
        ],
    },
    KeyRow {
        y_q: 10,
        keys: &[
            KeySpec::new(0x42, "Tab").width(8),
            KeySpec::new(0x10, "Q"),
            KeySpec::new(0x11, "W"),
            KeySpec::new(0x12, "E"),
            KeySpec::new(0x13, "R"),
            KeySpec::new(0x14, "T"),
            KeySpec::new(0x15, "Y"),
            KeySpec::new(0x16, "U"),
            KeySpec::new(0x17, "I"),
            KeySpec::new(0x18, "O"),
            KeySpec::new(0x19, "P"),
            KeySpec::new(0x1A, "[").shifted("{"),
            KeySpec::new(0x1B, "]").shifted("}"),
            // The ISO reverse-L Return. This is the wide top arm; the stem
            // hangs off its bottom edge, inset from the left so the two
            // right edges line up and the notch is bottom-left. Both end at
            // 15.5u, where the cursor cluster's notch begins -- the arm
            // reaches 0.25u further left than the stem, which is the shape
            // of the key, not a gap before it.
            KeySpec::new(0x44, "Ret").width(6).stem(1, 5),
        ],
    },
    KeyRow {
        y_q: 14,
        keys: &[
            KeySpec::new(RAWKEY_CTRL, "Ctrl").width(5).modifier(),
            KeySpec::new(RAWKEY_CAPS_LOCK, "Caps").caps(),
            KeySpec::new(0x20, "A"),
            KeySpec::new(0x21, "S"),
            KeySpec::new(0x22, "D"),
            KeySpec::new(0x23, "F"),
            KeySpec::new(0x24, "G"),
            KeySpec::new(0x25, "H"),
            KeySpec::new(0x26, "J"),
            KeySpec::new(0x27, "K"),
            KeySpec::new(0x28, "L"),
            KeySpec::new(0x29, ";").shifted(":"),
            KeySpec::new(0x2A, "'").shifted("@"),
            KeySpec::new(0x2B, "#").shifted("~"),
        ],
    },
    KeyRow {
        y_q: 18,
        keys: &[
            KeySpec::new(0x60, "Shift").width(7).modifier(),
            KeySpec::new(0x30, "\\").shifted("|"),
            KeySpec::new(0x31, "Z"),
            KeySpec::new(0x32, "X"),
            KeySpec::new(0x33, "C"),
            KeySpec::new(0x34, "V"),
            KeySpec::new(0x35, "B"),
            KeySpec::new(0x36, "N"),
            KeySpec::new(0x37, "M"),
            KeySpec::new(0x38, ",").shifted("<"),
            KeySpec::new(0x39, ".").shifted(">"),
            KeySpec::new(0x3A, "/").shifted("?"),
            KeySpec::new(0x61, "Shift").width(7).modifier(),
            KeySpec::glyph(0x4C, Legend::Arrow(Arrow::Up)),
        ],
    },
    KeyRow {
        y_q: 22,
        keys: &[
            KeySpec::new(0x64, "Alt").width(5).gap(2).modifier(),
            KeySpec::glyph(0x66, Legend::Amiga { hollow: true })
                .width(5)
                .modifier(),
            KeySpec::blank(0x40).width(32),
            KeySpec::glyph(0x67, Legend::Amiga { hollow: false })
                .width(5)
                .modifier(),
            KeySpec::new(0x65, "Alt").width(5).modifier(),
            KeySpec::glyph(0x4F, Legend::Arrow(Arrow::Left)),
            KeySpec::glyph(0x4D, Legend::Arrow(Arrow::Down)),
            KeySpec::glyph(0x4E, Legend::Arrow(Arrow::Right)),
        ],
    },
];

/// The only caps a US A600 prints differently. The shell is the same ISO
/// 78-key case either way, which is why a US machine ships blank keycaps
/// in the $2B and $30 positions rather than omitting the switches.
static US_LEGENDS: &[(u8, Legend, Legend)] = &[
    (0x02, Legend::Text("2"), Legend::Text("@")),
    (0x03, Legend::Text("3"), Legend::Text("#")),
    (0x2A, Legend::Text("'"), Legend::Text("\"")),
    (0x2B, Legend::None, Legend::None),
    (0x30, Legend::None, Legend::None),
];

/// What is printed on `spec`'s cap under the chosen legends.
fn legends_for(spec: &KeySpec, us: bool) -> (Legend, Legend) {
    if us {
        for (raw, main, shift) in US_LEGENDS {
            if *raw == spec.raw {
                return (*main, *shift);
            }
        }
    }
    (spec.main, spec.shift)
}

// --- geometry ------------------------------------------------------------

/// The strip's rect when it is actually up, and `None` when it is not.
///
/// Hit-testing must go through this: with the keyboard hidden its strip is
/// the status bar's, and testing it anyway swallows every click on the bar.
pub fn shown_panel_rect(top: usize) -> Option<Rect> {
    crate::video::keyboard_panel_shown().then(|| panel_rect(top))
}

/// The strip's rect within the presentation, `top` being the first row
/// below the display (and below the MT-32's panel, when that is up too).
pub fn panel_rect(top: usize) -> Rect {
    Rect {
        x: 0,
        y: top,
        w: FB_WIDTH,
        h: KBD_PANEL_HEIGHT,
    }
}

/// The grid's top-left corner within the strip.
fn grid_origin(panel: Rect) -> (usize, usize) {
    (panel.x + PAD_X, panel.y + PAD_Y)
}

/// The cell a key occupies, from its position and width in quarter-units.
///
/// Both edges are converted from the grid position rather than the width
/// being converted on its own, so neighbouring cells tile exactly: no
/// rounding can open a seam or overlap one cap onto the next.
fn cell_rect(panel: Rect, x_q: usize, y_q: usize, w_q: usize) -> Rect {
    let (ox, oy) = grid_origin(panel);
    let (x0, x1) = (ox + u_px(x_q), ox + u_px(x_q + w_q));
    let (y0, y1) = (oy + u_px(y_q), oy + u_px(y_q + 4));
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// The stem of an ISO Return, when the key has one: the cell hanging off
/// the bottom of its arm.
fn stem_cell_rect(panel: Rect, x_q: usize, y_q: usize, spec: &KeySpec) -> Option<Rect> {
    let (dx_q, w_q) = spec.stem?;
    Some(cell_rect(
        panel,
        x_q + usize::from(dx_q),
        y_q + 4,
        usize::from(w_q),
    ))
}

/// Walk every key, handing each its spec, its grid position in
/// quarter-units, and the cell it occupies.
fn each_key(panel: Rect, mut f: impl FnMut(&'static KeySpec, usize, usize, Rect)) {
    for row in ROWS {
        let mut x_q = 0usize;
        for spec in row.keys {
            x_q += usize::from(spec.gap_q);
            let y_q = usize::from(row.y_q);
            f(
                spec,
                x_q,
                y_q,
                cell_rect(panel, x_q, y_q, usize::from(spec.w_q)),
            );
            x_q += usize::from(spec.w_q);
        }
    }
}

/// A chip in the cursor notch. Both are one unit wide less a hair, and
/// 0.8u tall, sitting in the only part of an A600's outline with no keys
/// in it.
fn chip_rect(panel: Rect, tenths_y: usize) -> Rect {
    let (ox, oy) = grid_origin(panel);
    Rect {
        x: ox + u_px(62),
        y: oy + tenths_y * KEY_UNIT / 10,
        w: KEY_UNIT - 2 * CAP_INSET,
        h: 8 * KEY_UNIT / 10,
    }
}

/// The UK/US legend switch. It rides on the keyboard rather than in the
/// menu because it is a property of the caps, and because the notch is
/// there.
fn legend_chip_rect(panel: Rect) -> Rect {
    chip_rect(panel, 36)
}

/// The chip that puts the keyboard away, in the slot above the legend
/// switch -- deliberately the one farthest from the cursor keys, so a
/// missed arrow in a game cannot fold the keyboard mid-play.
fn close_chip_rect(panel: Rect) -> Rect {
    chip_rect(panel, 26)
}

/// Something on the strip the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdControl {
    /// A keycap, by its Amiga rawkey.
    Key(u8),
    /// The UK/US legend switch.
    Legends,
    /// The chip that hides the keyboard.
    Close,
}

/// Which control the pointer is over, if any.
pub fn control_at(panel: Rect, pos: (i32, i32)) -> Option<KbdControl> {
    if legend_chip_rect(panel).contains(pos) {
        return Some(KbdControl::Legends);
    }
    if close_chip_rect(panel).contains(pos) {
        return Some(KbdControl::Close);
    }
    let mut found = None;
    each_key(panel, |spec, x_q, y_q, cell| {
        if found.is_some() {
            return;
        }
        // The whole L of an ISO Return is one key, arm and stem alike.
        let hit = cell.contains(pos)
            || stem_cell_rect(panel, x_q, y_q, spec).is_some_and(|s| s.contains(pos));
        if hit {
            found = Some(KbdControl::Key(spec.raw));
        }
    });
    found
}

/// Whether moving from `previous` to `current` changed which cap is lit
/// under the pointer, and so needs the strip drawn again.
pub fn hover_changed(
    panel: Rect,
    previous: Option<(i32, i32)>,
    current: Option<(i32, i32)>,
) -> bool {
    previous.and_then(|pos| control_at(panel, pos))
        != current.and_then(|pos| control_at(panel, pos))
}

// --- state ---------------------------------------------------------------

/// How far a qualifier is latched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Latch {
    /// Not latched: it is down only while the mouse holds it.
    #[default]
    None,
    /// Armed for exactly one keystroke, and released with that key.
    OneShot,
    /// Held until it is clicked again.
    Locked,
}

#[derive(Debug, Clone, Copy, Default)]
struct ModState {
    /// The machine has been told this qualifier is down.
    down: bool,
    latch: Latch,
    /// The mouse button is on this cap right now, as opposed to the cap
    /// being latched down with nothing on it.
    held: bool,
    down_at: Option<Instant>,
    /// Another key was pressed while this qualifier was down, which is
    /// what tells a real chord apart from a bare tap that should latch.
    used_while_down: bool,
    last_tap_at: Option<Instant>,
}

/// What a click on the strip amounts to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KbdOutcome {
    /// Rawkey transitions for the keyboard MCU, in order.
    pub keys: Vec<(u8, bool)>,
    /// The close chip was clicked: put the strip away.
    pub close: bool,
}

impl KbdOutcome {
    fn send(&mut self, rawkey: u8, pressed: bool) {
        self.keys.push((rawkey, pressed));
    }
}

/// What the strip is holding down: one cap under the mouse (it is a mouse,
/// so one at a time) plus whatever qualifiers are latched, and which
/// legends the caps are wearing.
#[derive(Debug, Clone, Default)]
pub struct KbdPanelState {
    /// The cap the mouse button is holding, if any.
    pressed: Option<u8>,
    mods: [ModState; MODIFIERS.len()],
    us_legends: bool,
}

fn spec_for(rawkey: u8) -> Option<&'static KeySpec> {
    ROWS.iter()
        .flat_map(|row| row.keys)
        .find(|spec| spec.raw == rawkey)
}

fn modifier_index(rawkey: u8) -> Option<usize> {
    MODIFIERS.iter().position(|m| *m == rawkey)
}

impl KbdPanelState {
    /// Whether a cap is being held by the mouse.
    pub fn holding_key(&self) -> bool {
        self.pressed.is_some()
    }

    /// The mouse went down on `control`.
    pub fn press(&mut self, control: KbdControl, now: Instant) -> KbdOutcome {
        let mut out = KbdOutcome::default();
        match control {
            KbdControl::Legends => self.us_legends = !self.us_legends,
            KbdControl::Close => {
                // Acting on the press, like every key here: the strip is
                // gone before the button lifts, so anything it was holding
                // has to be handed back now.
                out = self.release_all();
                out.close = true;
            }
            KbdControl::Key(rawkey) => {
                let Some(spec) = spec_for(rawkey) else {
                    return out;
                };
                self.pressed = Some(rawkey);
                match spec.kind {
                    Kind::Modifier => {
                        let idx = modifier_index(rawkey).expect("qualifier is in MODIFIERS");
                        let m = &mut self.mods[idx];
                        m.held = true;
                        m.down_at = Some(now);
                        m.used_while_down = false;
                        if !m.down {
                            m.down = true;
                            out.send(rawkey, true);
                        }
                    }
                    // Caps Lock goes the ordinary way: the MCU owns its
                    // latch (a press flips the lamp and emits the down
                    // code on lock or the up code on unlock, and the
                    // release is discarded), so the strip sends the pair a
                    // real key sends and mirrors nothing.
                    Kind::Plain | Kind::Caps => {
                        out.send(rawkey, true);
                        // Any qualifier now held is being used, which is
                        // what tells its own release apart from a bare tap.
                        for m in &mut self.mods {
                            if m.down {
                                m.used_while_down = true;
                            }
                        }
                    }
                }
                // Checked on the qualifier path as well as the ordinary
                // one: Ctrl+Amiga+Amiga is made of nothing but qualifiers,
                // so the press that completes it is always a qualifier's.
                if self.reset_chord_held() {
                    out.keys.extend(self.release_all().keys);
                }
            }
        }
        out
    }

    /// The mouse button lifted, wherever the pointer had got to. One
    /// button holds one cap, so this ends whatever the last press began.
    pub fn release(&mut self, now: Instant) -> KbdOutcome {
        let mut out = KbdOutcome::default();
        let Some(rawkey) = self.pressed.take() else {
            return out;
        };
        let Some(spec) = spec_for(rawkey) else {
            return out;
        };
        match spec.kind {
            Kind::Modifier => {
                let idx = modifier_index(rawkey).expect("qualifier is in MODIFIERS");
                self.release_modifier(idx, now, &mut out);
            }
            Kind::Plain | Kind::Caps => {
                out.send(rawkey, false);
                // One-shots clear on the release, not the press, so the
                // guest sees the qualifier held across the whole keystroke.
                for (idx, m) in self.mods.iter_mut().enumerate() {
                    if m.latch == Latch::OneShot && !m.held {
                        m.down = false;
                        m.latch = Latch::None;
                        out.send(MODIFIERS[idx], false);
                    }
                }
            }
        }
        out
    }

    /// A qualifier clicked on its own stays down for the next keystroke;
    /// clicked twice it locks until clicked again; held down while another
    /// key is pressed it behaves like the real key, which is what a chord
    /// such as Ctrl+Amiga+Amiga wants from a one-button mouse.
    fn release_modifier(&mut self, idx: usize, now: Instant, out: &mut KbdOutcome) {
        let rawkey = MODIFIERS[idx];
        let m = &mut self.mods[idx];
        m.held = false;
        let tapped = m
            .down_at
            .is_some_and(|at| now.saturating_duration_since(at) < TAP)
            && !m.used_while_down;
        let double = m
            .last_tap_at
            .is_some_and(|at| now.saturating_duration_since(at) < DOUBLE_TAP);
        let mut clear = false;
        if !tapped {
            clear = true; // a real hold, released
        } else {
            match m.latch {
                Latch::Locked => clear = true, // clicking a locked one unlocks it
                Latch::OneShot if double => m.latch = Latch::Locked,
                Latch::OneShot => clear = true, // a second lone click disarms it
                Latch::None => m.latch = Latch::OneShot,
            }
        }
        if clear {
            m.down = false;
            m.latch = Latch::None;
            out.send(rawkey, false);
        }
        m.last_tap_at = Some(now);
    }

    /// The MCU has just latched Ctrl+Amiga+Amiga and is starting the reset
    /// flow; a human would now let go. Leaving the qualifiers latched
    /// would have `begin_power_up` report them still held (`set_held` runs
    /// before the `in_reset_flow` early return), and the next keystroke
    /// would reset the machine all over again.
    fn reset_chord_held(&self) -> bool {
        [RAWKEY_CTRL, RAWKEY_LEFT_AMIGA, RAWKEY_RIGHT_AMIGA]
            .into_iter()
            .all(|raw| modifier_index(raw).is_some_and(|idx| self.mods[idx].down))
    }

    /// Let go of everything the strip is holding: the cap under the mouse
    /// and every latched qualifier.
    ///
    /// Used by the reset chord and the close chip here, and by the window
    /// for every machine-lifecycle change that invalidates a latch --
    /// hiding the strip, running a new machine, loading a save state,
    /// powering off, and rebooting (`App::release_keyboard_panel_holds`).
    /// A latch is a host-side affordance, and one that outlived its
    /// machine would be drawn down over a keyboard MCU that has never
    /// heard of it.
    pub fn release_all(&mut self) -> KbdOutcome {
        let mut out = KbdOutcome::default();
        if let Some(rawkey) = self.pressed.take() {
            // A qualifier's release comes from the loop below instead,
            // where the latch is cleared with it.
            if spec_for(rawkey).is_some_and(|s| s.kind != Kind::Modifier) {
                out.send(rawkey, false);
            }
        }
        for (idx, m) in self.mods.iter_mut().enumerate() {
            if m.down {
                out.send(MODIFIERS[idx], false);
            }
            *m = ModState::default();
        }
        out
    }

    /// What the strip looks like right now. `caps_lit` is the MCU's own
    /// lamp, polled rather than mirrored from clicks: a save-state load
    /// changes it with no key pressed.
    pub fn view(&self, caps_lit: bool, hover: Option<KbdControl>) -> KbdPanelView {
        let mut view = KbdPanelView {
            caps_lit,
            us_legends: self.us_legends,
            hover,
            ..KbdPanelView::default()
        };
        if let Some(rawkey) = self.pressed {
            view.down[usize::from(rawkey & 0x7F)] = true;
        }
        for (idx, m) in self.mods.iter().enumerate() {
            let raw = usize::from(MODIFIERS[idx]);
            view.latch[raw] = m.latch;
            // Drawn down while it is genuinely down rather than latched:
            // a latch has its own mark.
            if m.down && m.latch == Latch::None {
                view.down[raw] = true;
            }
        }
        view
    }
}

/// What the strip needs to draw itself.
#[derive(Debug, Clone)]
pub struct KbdPanelView {
    /// Caps drawn held down, by rawkey.
    pub down: [bool; 0x80],
    /// How far each qualifier is latched, by rawkey.
    pub latch: [Latch; 0x80],
    /// The MCU's Caps Lock lamp.
    pub caps_lit: bool,
    /// Whether the caps wear US legends instead of UK.
    pub us_legends: bool,
    pub hover: Option<KbdControl>,
}

impl Default for KbdPanelView {
    fn default() -> Self {
        Self {
            down: [false; 0x80],
            latch: [Latch::None; 0x80],
            caps_lit: false,
            us_legends: false,
            hover: None,
        }
    }
}

// --- drawing -------------------------------------------------------------

/// Draw the keyboard.
pub fn draw(frame: &mut [u8], view: &KbdPanelView, top: usize, scale: usize) {
    let panel = panel_rect(top);
    fill_rect(frame, scaled(panel, scale), PANEL_FACE, scale);
    draw_rect_bevel(
        frame,
        scaled(panel, scale),
        PANEL_EDGE_LIGHT,
        PANEL_EDGE_DARK,
        scale,
    );
    each_key(panel, |spec, x_q, y_q, cell| {
        draw_key(
            frame,
            view,
            spec,
            cell,
            stem_cell_rect(panel, x_q, y_q, spec),
            scale,
        );
    });
    draw_legend_chip(frame, panel, view, scale);
    draw_close_chip(frame, panel, view, scale);
}

/// What colour a cap is, before the pointer lifts it.
fn cap_fill(view: &KbdPanelView, spec: &KeySpec) -> u32 {
    let raw = usize::from(spec.raw & 0x7F);
    if spec.kind == Kind::Caps && view.caps_lit {
        return CAP_LOCKED;
    }
    if view.down[raw] {
        return CAP_DOWN;
    }
    if view.latch[raw] == Latch::Locked {
        return CAP_LOCKED;
    }
    if spec.kind == Kind::Modifier {
        CAP_MOD
    } else {
        CAP_IDLE
    }
}

fn draw_key(
    frame: &mut [u8],
    view: &KbdPanelView,
    spec: &KeySpec,
    cell: Rect,
    stem_cell: Option<Rect>,
    scale: usize,
) {
    let raw = usize::from(spec.raw & 0x7F);
    let hovered = view.hover == Some(KbdControl::Key(spec.raw));
    let mut fill = cap_fill(view, spec);
    if hovered {
        fill = shade(fill, HOVER_LIFT);
    }
    let pressed = view.down[raw];
    let cap = inset(cell, CAP_INSET);
    draw_cap(frame, cap, fill, pressed, scale);
    if let Some(stem_cell) = stem_cell {
        // The stem starts one pixel inside the arm's bottom edge, so the
        // two boxes cannot leave a hairline between them, and the seam is
        // then painted out: what is left is one L-shaped moulding with the
        // arm's underside bevelled only where it actually overhangs.
        let stem_top = cap.y + cap.h - 1;
        let stem = Rect {
            x: stem_cell.x + CAP_INSET,
            y: stem_top,
            w: stem_cell.w - 2 * CAP_INSET,
            h: (stem_cell.y + stem_cell.h).saturating_sub(CAP_INSET + stem_top),
        };
        draw_cap(frame, stem, fill, pressed, scale);
        fill_rect(
            frame,
            scaled(
                Rect {
                    x: stem.x + 1,
                    y: cap.y + cap.h - 2,
                    w: stem.w.saturating_sub(2),
                    h: 3,
                },
                scale,
            ),
            fill,
            scale,
        );
    }
    // A one-shot qualifier gets a ring rather than a fill: armed for one
    // keystroke, not held down.
    if view.latch[raw] == Latch::OneShot {
        draw_rect_bevel(
            frame,
            scaled(inset(cap, 1), scale),
            CAP_LOCKED,
            CAP_LOCKED,
            scale,
        );
    }
    let (main, shift) = legends_for(spec, view.us_legends);
    draw_cap_legends(frame, cap, main, shift, scale);
    if spec.kind == Kind::Caps {
        draw_caps_led(frame, cap, fill, view.caps_lit, scale);
    }
}

/// A keycap: a flat moulding with the light on its top and left, turned
/// over while it is held down.
fn draw_cap(frame: &mut [u8], rect: Rect, fill: u32, pressed: bool, scale: usize) {
    let (near, far) = if pressed {
        (CAP_BEVEL_DARK, CAP_BEVEL_LIGHT)
    } else {
        (CAP_BEVEL_LIGHT, CAP_BEVEL_DARK)
    };
    let rect = scaled(rect, scale);
    fill_rect(frame, rect, fill, scale);
    draw_rect_bevel(frame, rect, shade(fill, near), shade(fill, far), scale);
}

/// The keycap lamp, in the corner of the cap the way an A600 sets it into
/// the moulding. Dark it is a hole in the cap rather than a colour of its
/// own, which is what an unlit LED under tinted plastic looks like.
fn draw_caps_led(frame: &mut [u8], cap: Rect, fill: u32, lit: bool, scale: usize) {
    let size = (KEY_UNIT / 7).max(3);
    let rect = Rect {
        x: cap.x + cap.w.saturating_sub(size + 4),
        y: cap.y + 4,
        w: size,
        h: size,
    };
    let colour = if lit {
        CAPS_LED_ON
    } else {
        mix(fill, rgba(0, 0, 0), 0.25)
    };
    fill_rect(frame, scaled(rect, scale), colour, scale);
}

/// What is printed on a cap: the main legend, with the shifted one above
/// it, smaller and dimmer, as the cap prints them.
fn draw_cap_legends(frame: &mut [u8], cap: Rect, main: Legend, shift: Legend, scale: usize) {
    let main_px = legend_px(main);
    let main_h = legend_height(main, main_px);
    if shift == Legend::None {
        let y = cap.y + cap.h.saturating_sub(main_h) / 2;
        draw_legend(frame, cap, y, main, main_px, CAP_INK, scale);
        return;
    }
    const SHIFT_PX: usize = 1;
    const LINE_GAP: usize = 2;
    let shift_h = font::GLYPH_H * SHIFT_PX;
    let block = shift_h + LINE_GAP + main_h;
    let top = cap.y + cap.h.saturating_sub(block) / 2;
    draw_legend(
        frame,
        cap,
        top,
        shift,
        SHIFT_PX,
        mix(CAP_INK, CAP_IDLE, SHIFT_DIM),
        scale,
    );
    draw_legend(
        frame,
        cap,
        top + shift_h + LINE_GAP,
        main,
        main_px,
        CAP_INK,
        scale,
    );
}

/// How large a legend is printed.
///
/// A cap prints a character legend large and a word legend -- Esc, Ctrl,
/// Shift, Alt, Bksp -- in a smaller face, and so does this: one character
/// at double size, anything longer at single. Sizing by what fits the cap
/// instead would put Alt and Ctrl, two qualifiers of the same width in the
/// same row, at different sizes, and F10 at a different size from F9.
fn legend_px(legend: Legend) -> usize {
    match legend {
        Legend::Text(s) if s.chars().count() > 1 => 1,
        _ => 2,
    }
}

fn legend_width(legend: Legend, px: usize) -> usize {
    match legend {
        Legend::None => 0,
        Legend::Text(s) => font::text_width(s, px),
        Legend::Pound | Legend::Arrow(_) => font::GLYPH_W * px,
        // Drawn at its own size, in canvas pixels, rather than as a glyph
        // scaled by whole font pixels.
        Legend::Amiga { .. } => AMIGA_W,
    }
}

fn legend_height(legend: Legend, px: usize) -> usize {
    match legend {
        Legend::Amiga { .. } => AMIGA_H,
        _ => font::GLYPH_H * px,
    }
}

/// One legend line, centred on the cap.
fn draw_legend(
    frame: &mut [u8],
    cap: Rect,
    y: usize,
    legend: Legend,
    px: usize,
    ink: u32,
    scale: usize,
) {
    let x = cap.x + cap.w.saturating_sub(legend_width(legend, px)) / 2;
    match legend {
        Legend::None => {}
        Legend::Text(s) => {
            font::draw_text(
                frame,
                texture_width(scale),
                texture_height(scale),
                x * scale,
                y * scale,
                s,
                ink,
                px * scale,
            );
        }
        Legend::Pound => blit_glyph(frame, x, y, &POUND, px, ink, scale),
        Legend::Arrow(dir) => blit_glyph(frame, x, y, arrow_glyph(dir), px, ink, scale),
        Legend::Amiga { hollow } => draw_amiga_mark(frame, x, y, hollow, ink, scale),
    }
}

/// The pound sign, which the 8x8 ASCII font has no cell for.
static POUND: [u8; 8] = [0x1C, 0x26, 0x02, 0x0F, 0x02, 0x02, 0x3F, 0x00];

/// The cursor marks. Bit 0 of a row is its leftmost pixel, as in the font.
static ARROW_UP: [u8; 8] = [0x18, 0x3C, 0x7E, 0xFF, 0x18, 0x18, 0x18, 0x00];
static ARROW_DOWN: [u8; 8] = [0x18, 0x18, 0x18, 0xFF, 0x7E, 0x3C, 0x18, 0x00];
static ARROW_LEFT: [u8; 8] = [0x08, 0x0C, 0xFE, 0xFF, 0xFE, 0x0C, 0x08, 0x00];
static ARROW_RIGHT: [u8; 8] = [0x10, 0x30, 0x7F, 0xFF, 0x7F, 0x30, 0x10, 0x00];

fn arrow_glyph(dir: Arrow) -> &'static [u8; 8] {
    match dir {
        Arrow::Up => &ARROW_UP,
        Arrow::Down => &ARROW_DOWN,
        Arrow::Left => &ARROW_LEFT,
        Arrow::Right => &ARROW_RIGHT,
    }
}

/// Blit an 8x8 mark at `px` canvas pixels per mark pixel, in the font's
/// own bit order.
fn blit_glyph(
    frame: &mut [u8],
    x: usize,
    y: usize,
    rows: &[u8; 8],
    px: usize,
    ink: u32,
    scale: usize,
) {
    for (row_idx, row) in rows.iter().enumerate() {
        for col in 0..8 {
            if row & (1 << col) == 0 {
                continue;
            }
            fill_rect(
                frame,
                scaled(
                    Rect {
                        x: x + col * px,
                        y: y + row_idx * px,
                        w: px,
                        h: px,
                    },
                    scale,
                ),
                ink,
                scale,
            );
        }
    }
}

// The Amiga key's mark: a capital A leaning to the right.
//
// The three strokes are capsules (line segments with a stroke radius) and
// each pixel is painted with its analytic coverage of them -- distance to
// the nearest stroke, resolved at the texture's own resolution and blended
// through an antialiased edge. Whole-pixel masks were tried twice and both
// read as staircases: the font's A sheared by whole font pixels, and a
// canvas-pixel mask whose 1 px steps became scale-sized blocks.

/// How tall the mark is, in canvas pixels: half a key, which leaves the
/// moulding around it on a 1u cap.
const AMIGA_H: usize = KEY_UNIT / 2;
/// Stroke weight, in canvas pixels. Three rather than two so the left
/// key's outline ring has an interior to be hollow around.
const AMIGA_STROKE: usize = 3;
/// The width of the cell the mark is centred in.
const AMIGA_W: usize = AMIGA_H * 9 / 10;
/// Where the crossbar's centreline sits, measured down the letter.
const AMIGA_BAR_Y: f32 = AMIGA_H as f32 * 0.69;

/// Distance from `p` to the segment `a`..`b`.
fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (p.0 - a.0, p.1 - a.1);
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let t = ((px * bx + py * by) / (bx * bx + by * by)).clamp(0.0, 1.0);
    let (dx, dy) = (px - t * bx, py - t * by);
    (dx * dx + dy * dy).sqrt()
}

/// The mark, filled on the right key and outlined on the left, as the
/// case prints the pair.
fn draw_amiga_mark(frame: &mut [u8], x: usize, y: usize, hollow: bool, ink: u32, scale: usize) {
    let h = AMIGA_H as f32;
    let r = AMIGA_STROKE as f32 / 2.0;
    // The outlined mark's ring is centred on the letter's edge, so it
    // reaches half a stroke beyond it; its geometry is inset by the same
    // amount so the ring's outer rim lands exactly where the solid mark's
    // silhouette does, and the two keys print the letter the same size.
    let ring = if hollow { r / 2.0 } else { 0.0 };
    let inset = r + ring;
    // The legs splay to fill the cell at the foot; the apex leans a
    // quarter of the height right of the letter's midline.
    let half = (AMIGA_W as f32 - 2.0 * inset) / 2.0;
    let apex = (half + inset + h / 4.0, inset);
    let foot_l = (inset, h - inset);
    let foot_r = (2.0 * half + inset, h - inset);
    // The crossbar runs between the legs' centrelines at its height.
    let t = (AMIGA_BAR_Y - apex.1) / (foot_l.1 - apex.1);
    let bar_l = (apex.0 + t * (foot_l.0 - apex.0), AMIGA_BAR_Y);
    let bar_r = (apex.0 + t * (foot_r.0 - apex.0), AMIGA_BAR_Y);
    let s = scale as f32;
    // One canvas pixel of slack around the cell, so the antialiased fade
    // at the letter's extremes is painted rather than cut off flat.
    let m = 1usize;
    for ty in 0..(AMIGA_H + 2 * m) * scale {
        for tx in 0..(AMIGA_W + 2 * m) * scale {
            // The pixel's centre, in the letter's own canvas units.
            let p = (
                (tx as f32 + 0.5) / s - m as f32,
                (ty as f32 + 0.5) / s - m as f32,
            );
            let d = segment_distance(p, apex, foot_l)
                .min(segment_distance(p, apex, foot_r))
                .min(segment_distance(p, bar_l, bar_r))
                - r;
            // Signed distance to the letter's edge, in texture pixels;
            // coverage fades over one pixel either side of it. The
            // outlined mark keeps a ring half a stroke wide around the
            // edge instead of the inside.
            let d = d * s;
            let alpha = if hollow {
                0.5 + ring * s - d.abs()
            } else {
                0.5 - d
            }
            .clamp(0.0, 1.0);
            if alpha > 0.0 {
                let (Some(px), Some(py)) = (
                    (x * scale + tx).checked_sub(m * scale),
                    (y * scale + ty).checked_sub(m * scale),
                ) else {
                    continue;
                };
                blend_pixel(frame, px, py, ink, alpha, scale);
            }
        }
    }
}

/// A notch chip: the same moulding as the status bar's own buttons, so the
/// two switches on the keyboard read as window furniture rather than as
/// keys with odd legends.
fn draw_chip(frame: &mut [u8], rect: Rect, hovered: bool, scale: usize) {
    let face = if hovered {
        BUTTON_FACE_HOVER
    } else {
        BUTTON_FACE
    };
    let scaled_rect = scaled(rect, scale);
    fill_rect(frame, scaled_rect, face, scale);
    draw_rect_bevel(
        frame,
        scaled_rect,
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
}

fn draw_legend_chip(frame: &mut [u8], panel: Rect, view: &KbdPanelView, scale: usize) {
    let rect = legend_chip_rect(panel);
    draw_chip(frame, rect, view.hover == Some(KbdControl::Legends), scale);
    let label = if view.us_legends { "US" } else { "UK" };
    let px = 2;
    let w = font::text_width(label, px);
    font::draw_text(
        frame,
        texture_width(scale),
        texture_height(scale),
        (rect.x + rect.w.saturating_sub(w) / 2) * scale,
        (rect.y + rect.h.saturating_sub(font::GLYPH_H * px) / 2) * scale,
        label,
        STATUS_TEXT,
        px * scale,
    );
}

/// The close chip's mark: two strokes crossing, drawn rather than typed so
/// it is the same weight as the arrows on the cursor caps.
fn draw_close_chip(frame: &mut [u8], panel: Rect, view: &KbdPanelView, scale: usize) {
    let rect = close_chip_rect(panel);
    draw_chip(frame, rect, view.hover == Some(KbdControl::Close), scale);
    let arm = (rect.h / 3).max(3);
    let x0 = rect.x + rect.w / 2 - arm / 2;
    let y0 = rect.y + rect.h / 2 - arm / 2;
    for step in 0..arm {
        for (dx, dy) in [(step, step), (step, arm - 1 - step)] {
            fill_rect(
                frame,
                scaled(
                    Rect {
                        x: x0 + dx,
                        y: y0 + dy,
                        w: 2,
                        h: 2,
                    },
                    scale,
                ),
                STATUS_TEXT,
                scale,
            );
        }
    }
}

fn inset(rect: Rect, by: usize) -> Rect {
    Rect {
        x: rect.x + by,
        y: rect.y + by,
        w: rect.w.saturating_sub(2 * by),
        h: rect.h.saturating_sub(2 * by),
    }
}

fn scaled(rect: Rect, scale: usize) -> Rect {
    Rect {
        x: rect.x * scale,
        y: rect.y * scale,
        w: rect.w * scale,
        h: rect.h * scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> Rect {
        panel_rect(500)
    }

    /// Puts the strip up for the length of a test and takes it down again
    /// however the test ends. The flag is this thread's own in a test
    /// build (see `crate::video::set_keyboard_panel_shown`), so it costs
    /// no other test anything -- but a test that left it set would still
    /// mislead the next one to run on the same thread.
    struct KeyboardUp;

    impl KeyboardUp {
        fn shown() -> Self {
            crate::video::set_keyboard_panel_shown(true);
            Self
        }
    }

    impl Drop for KeyboardUp {
        fn drop(&mut self) {
            crate::video::set_keyboard_panel_shown(false);
        }
    }

    fn centre(rect: Rect) -> (i32, i32) {
        ((rect.x + rect.w / 2) as i32, (rect.y + rect.h / 2) as i32)
    }

    /// The cell of the key carrying `rawkey`.
    fn cell_of(rawkey: u8) -> Rect {
        let mut found = None;
        each_key(panel(), |spec, _, _, cell| {
            if spec.raw == rawkey {
                found = Some(cell);
            }
        });
        found.unwrap_or_else(|| panic!("no key {rawkey:#04x}"))
    }

    /// Every row is laid out on the A600's own grid: three of them run the
    /// full 16.5u, and the other three stop at the cursor notch.
    #[test]
    fn the_rows_end_where_an_a600s_do() {
        // The home row stops at 14.25u because the ISO Return's stem fills
        // the rest of it; every other short row ends at the notch itself.
        let expected = [66usize, 66, 62, 57, 62, 66];
        for (row, want) in ROWS.iter().zip(expected) {
            let width: usize = row
                .keys
                .iter()
                .map(|k| usize::from(k.gap_q) + usize::from(k.w_q))
                .sum();
            assert_eq!(width, want, "row at y={}q", row.y_q);
        }
        // The Return's stem takes the home row out to the notch.
        let stem = ROWS[2]
            .keys
            .iter()
            .find_map(|k| k.stem)
            .expect("the home row's Return has a stem");
        assert_eq!(57 + usize::from(stem.1), 62, "the stem ends at the notch");
    }

    /// No cap escapes the strip, and none overlaps another.
    #[test]
    fn every_cap_fits_the_strip_and_nothing_overlaps() {
        let panel = panel();
        let mut cells: Vec<(u8, Rect)> = Vec::new();
        each_key(panel, |spec, x_q, y_q, cell| {
            cells.push((spec.raw, cell));
            if let Some(stem) = stem_cell_rect(panel, x_q, y_q, spec) {
                cells.push((spec.raw, stem));
            }
        });
        assert_eq!(cells.len(), 79, "78 keys plus the Return's stem");
        for (raw, cell) in &cells {
            assert!(cell.x >= panel.x, "key {raw:#04x} runs off the left");
            assert!(
                cell.x + cell.w <= panel.x + panel.w,
                "key {raw:#04x} runs off the right"
            );
            assert!(cell.y >= panel.y, "key {raw:#04x} runs off the top");
            assert!(
                cell.y + cell.h <= panel.y + panel.h,
                "key {raw:#04x} runs off the bottom"
            );
            assert!(cell.w > 0 && cell.h > 0, "key {raw:#04x} has no size");
        }
        for (i, (raw_a, a)) in cells.iter().enumerate() {
            for (raw_b, b) in &cells[i + 1..] {
                if raw_a == raw_b {
                    continue; // the arm and stem of one Return
                }
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{raw_a:#04x} and {raw_b:#04x} overlap");
            }
        }
        // The chips live in the notch, clear of every cap.
        for chip in [legend_chip_rect(panel), close_chip_rect(panel)] {
            assert!(chip.y >= panel.y && chip.y + chip.h <= panel.y + panel.h);
            for (raw, cell) in &cells {
                let apart = cell.x + cell.w <= chip.x
                    || chip.x + chip.w <= cell.x
                    || cell.y + cell.h <= chip.y
                    || chip.y + chip.h <= cell.y;
                assert!(apart, "the notch chip overlaps key {raw:#04x}");
            }
        }
    }

    /// The whole L of an ISO Return is one key: a wide arm with a narrower
    /// stem hanging off it, both ending at the notch.
    #[test]
    fn the_return_is_an_iso_reverse_l() {
        let panel = panel();
        let mut arm = None;
        let mut stem = None;
        each_key(panel, |spec, x_q, y_q, cell| {
            if spec.raw == 0x44 {
                arm = Some(cell);
                stem = stem_cell_rect(panel, x_q, y_q, spec);
            }
        });
        let (arm, stem) = (arm.unwrap(), stem.unwrap());
        assert_eq!(arm.x + arm.w, stem.x + stem.w, "right edges line up");
        assert!(arm.x < stem.x, "the arm reaches further left than the stem");
        assert_eq!(arm.y + arm.h, stem.y, "the stem hangs off the arm");
        assert!(stem.w < arm.w, "the stem is the narrower box");
        // Both halves answer as the same key.
        assert_eq!(control_at(panel, centre(arm)), Some(KbdControl::Key(0x44)));
        assert_eq!(control_at(panel, centre(stem)), Some(KbdControl::Key(0x44)));
    }

    /// Clicking a cap finds the key printed on it, the notch chips answer
    /// for themselves, and the empty parts of the strip answer for nothing.
    #[test]
    fn the_pointer_finds_the_key_it_is_over() {
        let panel = panel();
        for raw in [0x45, 0x59, 0x40, 0x62, 0x66, 0x4C, 0x2B, 0x00] {
            assert_eq!(
                control_at(panel, centre(cell_of(raw))),
                Some(KbdControl::Key(raw)),
                "key {raw:#04x}"
            );
        }
        assert_eq!(
            control_at(panel, centre(legend_chip_rect(panel))),
            Some(KbdControl::Legends)
        );
        assert_eq!(
            control_at(panel, centre(close_chip_rect(panel))),
            Some(KbdControl::Close)
        );
        // The notch itself, between the two chips and clear of both.
        let (ox, oy) = grid_origin(panel);
        let between = ((ox + u_px(63)) as i32, (oy + u_px(14) + 2) as i32);
        assert_eq!(control_at(panel, between), None, "the notch is empty");
        // The margin around the grid, and the strip's own corners.
        assert_eq!(control_at(panel, (1, panel.y as i32 + 1)), None);
        assert_eq!(
            control_at(panel, ((FB_WIDTH - 1) as i32, (panel.y + 1) as i32)),
            None
        );
    }

    /// The gap between two caps still belongs to a key, because the whole
    /// grid cell is live and only the moulding inside it is inset.
    #[test]
    fn the_gaps_between_caps_are_still_live() {
        let panel = panel();
        let q = cell_of(0x10); // Q, in the middle of a row of 1u keys
        let seam = ((q.x + q.w) as i32 - 1, centre(q).1);
        assert_eq!(control_at(panel, seam), Some(KbdControl::Key(0x10)));
    }

    /// A US machine prints five caps differently and is otherwise the same
    /// keyboard.
    #[test]
    fn the_legend_switch_only_moves_the_caps_a_us_machine_prints() {
        let hash = spec_for(0x03).unwrap();
        assert_eq!(legends_for(hash, false).1, Legend::Pound);
        assert_eq!(legends_for(hash, true).1, Legend::Text("#"));
        let blank = spec_for(0x30).unwrap();
        assert_eq!(legends_for(blank, false).0, Legend::Text("\\"));
        assert_eq!(legends_for(blank, true).0, Legend::None);
        // Everything else is the same cap either way.
        let a = spec_for(0x20).unwrap();
        assert_eq!(legends_for(a, false), legends_for(a, true));
    }

    /// A qualifier clicked once arms itself for the next keystroke and
    /// comes back up with it; clicked twice it locks; clicked while locked
    /// it lets go.
    #[test]
    fn a_qualifier_latches_for_one_keystroke_and_locks_on_a_double_click() {
        let mut kbd = KbdPanelState::default();
        let t0 = Instant::now();
        let shift = KbdControl::Key(0x60);

        // Click: down on the press, still down on the release.
        assert_eq!(kbd.press(shift, t0).keys, vec![(0x60, true)]);
        let out = kbd.release(t0 + Duration::from_millis(50));
        assert!(out.keys.is_empty(), "a tap leaves it down: {out:?}");
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::OneShot);

        // The next ordinary key takes it with them, on the release.
        let a = KbdControl::Key(0x20);
        assert_eq!(
            kbd.press(a, t0 + Duration::from_secs(1)).keys,
            vec![(0x20, true)]
        );
        assert_eq!(
            kbd.release(t0 + Duration::from_secs(1)).keys,
            vec![(0x20, false), (0x60, false)],
            "the qualifier is released after the key it qualified"
        );
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::None);

        // Two clicks inside the double window lock it down.
        let mut kbd = KbdPanelState::default();
        kbd.press(shift, t0);
        kbd.release(t0 + Duration::from_millis(20));
        kbd.press(shift, t0 + Duration::from_millis(120));
        let out = kbd.release(t0 + Duration::from_millis(140));
        assert!(out.keys.is_empty(), "still down: {out:?}");
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::Locked);
        // A key now leaves it locked.
        kbd.press(a, t0 + Duration::from_secs(2));
        assert_eq!(
            kbd.release(t0 + Duration::from_secs(2)).keys,
            vec![(0x20, false)]
        );
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::Locked);
        // Clicking it again lets it go.
        kbd.press(shift, t0 + Duration::from_secs(3));
        assert_eq!(
            kbd.release(t0 + Duration::from_secs(3)).keys,
            vec![(0x60, false)]
        );
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::None);
    }

    /// Pressed and held rather than clicked, a qualifier behaves like the
    /// real key: it comes up when the mouse does, with nothing latched.
    #[test]
    fn a_qualifier_held_down_comes_up_with_the_mouse() {
        let mut kbd = KbdPanelState::default();
        let t0 = Instant::now();
        assert_eq!(
            kbd.press(KbdControl::Key(0x60), t0).keys,
            vec![(0x60, true)]
        );
        let out = kbd.release(t0 + TAP + Duration::from_millis(50));
        assert_eq!(out.keys, vec![(0x60, false)], "a hold is a hold");
        assert_eq!(kbd.view(false, None).latch[0x60], Latch::None);
    }

    /// Caps Lock is an ordinary key with a lamp on it: the strip sends the
    /// pair a real key sends and never mirrors the latch, which is the
    /// MCU's (the press flips it, the release does nothing at all).
    #[test]
    fn caps_lock_is_a_plain_key_whose_lamp_the_mcu_owns() {
        let mut kbd = KbdPanelState::default();
        let t0 = Instant::now();
        let caps = KbdControl::Key(RAWKEY_CAPS_LOCK);
        assert_eq!(kbd.press(caps, t0).keys, vec![(RAWKEY_CAPS_LOCK, true)]);
        assert_eq!(kbd.release(t0).keys, vec![(RAWKEY_CAPS_LOCK, false)]);
        // The lamp comes from the machine, not from the clicks: neither
        // press moved it here.
        assert!(!kbd.view(false, None).caps_lit);
        assert!(kbd.view(true, None).caps_lit);
        // And a cap left down when the strip goes away is handed back.
        kbd.press(caps, t0);
        assert_eq!(kbd.release_all().keys, vec![(RAWKEY_CAPS_LOCK, false)]);
    }

    /// Ctrl+Amiga+Amiga starts the MCU's reset flow, and the strip lets go
    /// of all three: latched qualifiers would be reported held through the
    /// power-up stream and reset the machine again on the next keystroke.
    #[test]
    fn the_reset_chord_drops_every_latch() {
        let mut kbd = KbdPanelState::default();
        let t0 = Instant::now();
        // Latched one at a time, as a one-button mouse must.
        for (n, raw) in [RAWKEY_CTRL, RAWKEY_LEFT_AMIGA].into_iter().enumerate() {
            let at = t0 + Duration::from_millis(100 * n as u64);
            kbd.press(KbdControl::Key(raw), at);
            kbd.release(at + Duration::from_millis(10));
        }
        let at = t0 + Duration::from_millis(400);
        let out = kbd.press(KbdControl::Key(RAWKEY_RIGHT_AMIGA), at);
        assert_eq!(out.keys.first(), Some(&(RAWKEY_RIGHT_AMIGA, true)));
        for raw in [RAWKEY_CTRL, RAWKEY_LEFT_AMIGA, RAWKEY_RIGHT_AMIGA] {
            assert!(
                out.keys.contains(&(raw, false)),
                "{raw:#04x} was let go: {out:?}"
            );
        }
        let view = kbd.view(false, None);
        for raw in MODIFIERS {
            assert_eq!(view.latch[usize::from(raw)], Latch::None);
            assert!(!view.down[usize::from(raw)]);
        }
        assert!(!kbd.holding_key(), "nothing is left under the mouse");
    }

    /// The close chip puts the keyboard away and hands back whatever it
    /// was holding, since the strip is gone before the button lifts.
    #[test]
    fn the_close_chip_releases_what_it_was_holding() {
        let mut kbd = KbdPanelState::default();
        let t0 = Instant::now();
        kbd.press(KbdControl::Key(0x60), t0);
        kbd.release(t0 + Duration::from_millis(10)); // latched
        let out = kbd.press(KbdControl::Close, t0 + Duration::from_secs(1));
        assert!(out.close);
        assert_eq!(out.keys, vec![(0x60, false)]);
    }

    /// The legend switch changes the caps and nothing else.
    #[test]
    fn the_legend_chip_only_swaps_the_legends() {
        let mut kbd = KbdPanelState::default();
        assert!(!kbd.view(false, None).us_legends);
        let out = kbd.press(KbdControl::Legends, Instant::now());
        assert!(out.keys.is_empty() && !out.close);
        assert!(kbd.view(false, None).us_legends);
    }

    /// Renders the keyboard to `target/ui-preview-keyboard-panel.png` under
    /// COPPERLINE_UI_PREVIEW, for looking at while the design settles.
    #[test]
    fn the_keyboard_panel_draws_itself() {
        use crate::video::window::present_height;

        // The drawing helpers clamp against the presentation texture, so
        // the strip is drawn where it really sits and cropped out after.
        let _guard = KeyboardUp::shown();
        let scale = 2;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let top = present_height();
        let panel = panel_rect(top);
        let (px, py) = (panel.x * scale, panel.y * scale);
        let (pw, ph) = (panel.w * scale, panel.h * scale);

        let mut frame = vec![0u8; w * h * 4];
        let mut state = KbdPanelState::default();
        let t0 = Instant::now();
        // One qualifier latched for the next keystroke, one locked, and a
        // cap held down, so the preview shows every state a cap has.
        state.press(KbdControl::Key(0x60), t0);
        state.release(t0 + Duration::from_millis(10));
        state.press(KbdControl::Key(0x63), t0 + Duration::from_millis(20));
        state.release(t0 + Duration::from_millis(30));
        state.press(KbdControl::Key(0x63), t0 + Duration::from_millis(40));
        state.release(t0 + Duration::from_millis(50));
        state.press(KbdControl::Key(0x24), t0 + Duration::from_secs(1));
        // Caps lit, and the pointer resting on a cap.
        let view = state.view(true, Some(KbdControl::Key(0x35)));
        draw(&mut frame, &view, top, scale);

        let mut strip = Vec::with_capacity(pw * ph * 4);
        for row in 0..ph {
            let start = ((py + row) * w + px) * 4;
            strip.extend_from_slice(&frame[start..start + pw * 4]);
        }
        assert_ne!(&strip[0..4], &[0, 0, 0, 0], "the fascia was painted");
        // A cap in the middle of the strip really got its moulding, read
        // clear of both its bevel and its centred legend.
        let a = cell_of(0x20);
        let cap = pixel_at(&strip, pw, (a.x + 6, a.y + 6 - panel.y), scale);
        assert_eq!(cap, CAP_IDLE.to_le_bytes(), "the A cap is drawn");

        if crate::envcfg::flag("COPPERLINE_UI_PREVIEW") {
            let path = "target/ui-preview-keyboard-panel.png";
            let file = std::fs::File::create(path).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), pw as u32, ph as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&strip)
                .unwrap();
            eprintln!("saved {path}");
        }
    }

    /// A canvas pixel, in the strip's own coordinates, read out of the
    /// cropped strip.
    fn pixel_at(strip: &[u8], strip_w: usize, pos: (usize, usize), scale: usize) -> [u8; 4] {
        let (x, y) = (pos.0 * scale, pos.1 * scale);
        strip[(y * strip_w + x) * 4..(y * strip_w + x) * 4 + 4]
            .try_into()
            .unwrap()
    }

    /// It draws at both texture scales without running off the end of a
    /// correctly sized buffer.
    #[test]
    fn it_draws_at_every_texture_scale() {
        let _guard = KeyboardUp::shown();
        let top = crate::video::window::present_height();
        for scale in [1, 2] {
            let (w, h) = (texture_width(scale), texture_height(scale));
            let mut frame = vec![0u8; w * h * 4];
            let view = KbdPanelState::default().view(false, None);
            draw(&mut frame, &view, top, scale);
            // The strip was painted rather than left transparent.
            let row = (top + 1) * scale;
            assert_ne!(&frame[row * w * 4..row * w * 4 + 4], &[0, 0, 0, 0]);
        }
    }
}

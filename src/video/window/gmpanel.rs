// SPDX-License-Identifier: GPL-3.0-or-later

//! The General MIDI synthesizer's front panel, drawn under the display.
//!
//! Laid out the way a Sound Canvas is: power and the VOLUME knob at the
//! left, the wide amber LCD -- four rows of values and the sixteen-column
//! bar matrix -- then ALL and MUTE, and the eight left/right pairs in two
//! columns. It is a panel for driving the emulated synth, not a picture
//! of the hardware: the proportions follow the unit because that is what
//! makes it legible, and nothing is copied from it.
//!
//! Every character on the glass comes composed from Coppersynth's own
//! panel; this module draws fascia, forwards presses, and never invents
//! text of its own. The pointer mechanics are the MT-32 panel's:
//! left-click is a momentary press, right-click latches a button down,
//! and the unit's two-button gestures are made by latching one and
//! clicking the other -- through a power-on, for the start-up screens.

use coppersynth::panel::{Button, Dir, Pair, Screen};

use super::statusbar::draw_power_glyph_sized;
use super::statusbar::{draw_rect_bevel, fill_rect};
use super::Rect;
use super::{
    texture_height, texture_width, LED_BEZEL_DARK, LED_BEZEL_LIGHT, POWER_GLYPH_OFF,
    POWER_GLYPH_ON, STATUS_BOTTOM,
};
use crate::video::font;

/// How tall the panel is: exactly double the status bar, which is what
/// four rows of buttons and the taller glass want.
pub const GM_PANEL_HEIGHT: usize = 88;

// The fascia and its buttons are the status bar's, so the strips read as
// one piece of chrome.
use super::{BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE, STATUS_BG, STATUS_TEXT, STATUS_TOP};
const PANEL_FACE: u32 = STATUS_BG;
const PANEL_EDGE_LIGHT: u32 = STATUS_TOP;
const PANEL_EDGE_DARK: u32 = STATUS_BOTTOM;
const CAPTION: u32 = STATUS_TEXT;
const BUTTON_FACE_PRESSED: u32 = rgba(30, 30, 28);
const HOVER_LIFT: f32 = 16.0;

/// The glass. A Sound Canvas is the MT-32's negative: a bright amber
/// backlight with dark characters printed over it, and nothing at all
/// when the light goes out.
const GLASS_LIT: u32 = rgba(233, 158, 48);
const GLASS_DARK: u32 = rgba(58, 40, 22);
const INK: u32 = rgba(43, 22, 8);
/// Captions printed on the glass sit lighter than the values written
/// under them, as printed things do against driven ones.
const GLASS_PRINT: f32 = 0.42;
/// The matrix's unlit dots, a shade off the backlight.
const CELL_GRAIN: f32 = 0.12;
/// The gloss surround the glass is set into.
const LCD_SURROUND: u32 = rgba(24, 24, 24);
const LCD_SURROUND_SHEEN: u32 = rgba(52, 52, 52);
/// An LED's face, dark and lit. Green for power, amber for ALL and MUTE,
/// as the unit wears them.
const LED_DARK: u32 = rgba(20, 16, 9);
const LED_POWER: u32 = rgba(96, 220, 120);
const LED_AMBER: u32 = rgba(240, 170, 60);

// --- geometry ------------------------------------------------------------
//
// Left to right: power with its standby LED, the VOLUME knob, the LCD,
// ALL and MUTE, then the pairs in two columns of four. Everything is
// measured from the panel's own top-left.

const PAD: usize = 8;
const LCD_BEZEL: usize = 2;
const LCD_W: usize = 400;
const LCD_H: usize = 80;
/// The knob, drawn with the same dome and rim as the MT-32's dial.
const DIAL_D: usize = 34;
const DIAL_CAPTION: &str = "VOLUME";
const POWER_W: usize = 16;
const POWER_H: usize = 12;
const LED_W: usize = 10;
const LED_H: usize = 6;
/// A pair: one moulding split into its two halves.
const PAIR_W: usize = 64;
const PAIR_H: usize = 11;
const PAIR_SPLIT: usize = 2;
/// The round buttons: ALL, MUTE, and the PART halves.
const ROUND_D: usize = 12;
const CAPTION_H: usize = 9;
const ROW_PITCH: usize = 21;
const COL_GAP: usize = 14;
const GROUP_GAP: usize = 12;

/// The eight pairs in fascia order: left column top to bottom, then the
/// right column, as the unit prints them.
const PAIR_GRID: [(Pair, &str, usize, usize); 8] = [
    (Pair::Part, "PART", 0, 0),
    (Pair::Instrument, "INSTRUMENT", 1, 0),
    (Pair::Level, "LEVEL", 0, 1),
    (Pair::Pan, "PAN", 1, 1),
    (Pair::Reverb, "REVERB", 0, 2),
    (Pair::Chorus, "CHORUS", 1, 2),
    (Pair::KeyShift, "KEY SHIFT", 0, 3),
    (Pair::MidiCh, "MIDI CH", 1, 3),
];

/// Something on the panel the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmControl {
    All,
    Mute,
    Arrow(Pair, Dir),
    /// The VOLUME knob.
    Dial,
    /// Copperline's power switch, as on the MT-32 panel.
    Power,
}

/// What the panel needs to draw itself.
#[derive(Debug, Clone)]
pub struct GmPanelView {
    /// The glass, exactly as the engine's panel composed it.
    pub screen: Screen,
    pub powered: bool,
    /// Whether the MUTE lamp should be blinking rather than steady --
    /// the monitor is on.
    pub mute_blinks: bool,
    /// This half of the blink, when it does.
    pub blink_on: bool,
    /// Where the VOLUME knob stands, 0..=1.
    pub volume: f32,
    /// Buttons standing in: latched down, or lit under a click.
    pub down: Vec<GmControl>,
    pub hover: Option<GmControl>,
}

/// The glass with its light out: what the view carries while the unit
/// is switched off. Nothing in it is drawn.
pub fn dark_screen() -> Screen {
    Screen {
        part: String::new(),
        instrument: String::new(),
        name: String::new(),
        level: String::new(),
        pan: String::new(),
        reverb: String::new(),
        chorus: String::new(),
        key_shift: String::new(),
        midi_ch: String::new(),
        bars: [0; 16],
        all_led: false,
        mute_led: false,
        translating: false,
    }
}

/// A press resolved against what is latched: what the window should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmPress {
    None,
    /// Hand this to the engine's panel.
    Button(Button),
    /// Switch on, with these buttons held through the power-on.
    PowerOn(Vec<Button>),
    PowerOff,
}

/// The panel's rect when it is actually up, and `None` when it is not.
pub fn shown_panel_rect(top: usize) -> Option<Rect> {
    crate::video::gm_panel_shown().then(|| panel_rect(top))
}

/// The panel's rect, its top edge at `top`.
pub fn panel_rect(top: usize) -> Rect {
    Rect {
        x: 0,
        y: top,
        w: crate::video::FB_WIDTH,
        h: GM_PANEL_HEIGHT,
    }
}

fn power_rect(panel: Rect) -> Rect {
    Rect {
        x: panel.x + PAD,
        y: panel.y + 10,
        w: POWER_W,
        h: POWER_H,
    }
}

fn power_led_rect(panel: Rect) -> Rect {
    let power = power_rect(panel);
    Rect {
        x: power.x + power.w + 5,
        y: power.y + (power.h - LED_H) / 2,
        w: LED_W,
        h: LED_H,
    }
}

fn dial_rect(panel: Rect) -> Rect {
    let power = power_rect(panel);
    Rect {
        x: panel.x + PAD + 2,
        y: power.y + power.h + 16,
        w: DIAL_D,
        h: DIAL_D,
    }
}

fn lcd_rect(panel: Rect) -> Rect {
    let dial = dial_rect(panel);
    Rect {
        x: dial.x + DIAL_D + PAD + GROUP_GAP,
        y: panel.y + (panel.h - LCD_H) / 2,
        w: LCD_W,
        h: LCD_H,
    }
}

/// The origin of the pair grid, anchored against the right edge so the
/// glass takes whatever width the buttons leave.
fn pairs_origin(panel: Rect) -> (usize, usize) {
    let grid_w = 2 * PAIR_W + COL_GAP;
    (panel.x + panel.w - PAD - grid_w, panel.y + 2)
}

/// ALL and MUTE, on their own strip between the glass and the pairs.
fn round_rect(panel: Rect, second: bool) -> Rect {
    let x = pairs_origin(panel).0 - GROUP_GAP - ROUND_D - 10;
    let y = panel.y + if second { 52 } else { 22 };
    Rect {
        x,
        y,
        w: ROUND_D,
        h: ROUND_D,
    }
}

/// One half of a pair. The PART pair wears round halves, the rest the
/// wide split moulding, as on the unit.
fn arrow_rect(panel: Rect, pair: Pair, dir: Dir) -> Rect {
    let (x0, y0) = pairs_origin(panel);
    let (_, _, col, row) = PAIR_GRID
        .iter()
        .find(|(p, ..)| *p == pair)
        .copied()
        .unwrap_or(PAIR_GRID[0]);
    let x = x0 + col * (PAIR_W + COL_GAP);
    let y = y0 + row * ROW_PITCH + CAPTION_H;
    if pair == Pair::Part {
        // Two round buttons centred where the moulding would be.
        let cx = x + PAIR_W / 2;
        let off = match dir {
            Dir::Left => cx - ROUND_D - 3,
            Dir::Right => cx + 3,
        };
        return Rect {
            x: off,
            y,
            w: ROUND_D,
            h: PAIR_H,
        };
    }
    let half = (PAIR_W - PAIR_SPLIT) / 2;
    Rect {
        x: match dir {
            Dir::Left => x,
            Dir::Right => x + half + PAIR_SPLIT,
        },
        y,
        w: half,
        h: PAIR_H,
    }
}

/// Which control the pointer is over, if any.
pub fn control_at(panel: Rect, pos: (i32, i32)) -> Option<GmControl> {
    if power_rect(panel).contains(pos) {
        return Some(GmControl::Power);
    }
    if dial_rect(panel).contains(pos) {
        return Some(GmControl::Dial);
    }
    if round_rect(panel, false).contains(pos) {
        return Some(GmControl::All);
    }
    if round_rect(panel, true).contains(pos) {
        return Some(GmControl::Mute);
    }
    for (pair, ..) in PAIR_GRID {
        for dir in [Dir::Left, Dir::Right] {
            if arrow_rect(panel, pair, dir).contains(pos) {
                return Some(GmControl::Arrow(pair, dir));
            }
        }
    }
    None
}

/// What answers to being hovered: buttons, not the knob or the switch.
pub fn hover_at(panel: Rect, pos: (i32, i32)) -> Option<GmControl> {
    control_at(panel, pos).filter(|c| !matches!(c, GmControl::Dial | GmControl::Power))
}

/// Whether a pointer move changed which button lights under it.
pub fn hover_changed(
    panel: Rect,
    previous: Option<(i32, i32)>,
    current: Option<(i32, i32)>,
) -> bool {
    previous.and_then(|pos| hover_at(panel, pos)) != current.and_then(|pos| hover_at(panel, pos))
}

// --- drawing -------------------------------------------------------------

/// Draw the panel.
pub fn draw(frame: &mut [u8], view: &GmPanelView, top: usize, scale: usize) {
    let panel = panel_rect(top);
    fill_rect(frame, scaled(panel, scale), PANEL_FACE, scale);
    draw_rect_bevel(
        frame,
        scaled(panel, scale),
        PANEL_EDGE_LIGHT,
        PANEL_EDGE_DARK,
        scale,
    );
    draw_power(frame, panel, view, scale);
    draw_dial(frame, panel, view.volume, scale);
    draw_lcd(frame, panel, view, scale);
    draw_rounds(frame, panel, view, scale);
    draw_pairs(frame, panel, view, scale);
}

fn draw_power(frame: &mut [u8], panel: Rect, view: &GmPanelView, scale: usize) {
    let rect = power_rect(panel);
    text(
        frame,
        rect.x,
        rect.y.saturating_sub(9),
        "POWER",
        CAPTION,
        1,
        scale,
    );
    draw_button(frame, rect, view.powered, false, scale);
    draw_power_glyph_sized(
        frame,
        (rect.x + rect.w / 2) * scale,
        (rect.y + rect.h / 2) * scale - scale,
        3.5,
        if view.powered {
            POWER_GLYPH_ON
        } else {
            POWER_GLYPH_OFF
        },
        scale,
    );
    raised_display(
        frame,
        power_led_rect(panel),
        if view.powered { LED_POWER } else { LED_DARK },
        scale,
    );
}

/// The VOLUME knob: the MT-32 dial's face and rim, full travel meaning
/// full output.
fn draw_dial(frame: &mut [u8], panel: Rect, value: f32, scale: usize) {
    let dial = dial_rect(panel);
    let (cx, cy) = (
        dial.x as f32 + dial.w as f32 / 2.0,
        dial.y as f32 + dial.h as f32 / 2.0,
    );
    let radius = dial.w as f32 / 2.0;
    for y in 0..dial.h {
        let dy = y as f32 + 0.5 - dial.h as f32 / 2.0;
        let half = (radius * radius - dy * dy).max(0.0).sqrt();
        if half < 0.5 {
            continue;
        }
        let row = Rect {
            x: (cx - half) as usize,
            y: dial.y + y,
            w: (half * 2.0) as usize,
            h: 1,
        };
        let dome = shade(BUTTON_FACE, -dy / radius * 22.0);
        fill_rect(frame, scaled(row, scale), dome, scale);
    }
    const RIM_STEPS: usize = 256;
    for i in 0..RIM_STEPS {
        let angle = i as f32 / RIM_STEPS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let facing = 0.5 - (cos + sin) * std::f32::consts::FRAC_1_SQRT_2 * 0.5;
        let dot = Rect {
            x: (cx + cos * (radius - 0.5)) as usize,
            y: (cy + sin * (radius - 0.5)) as usize,
            w: 1,
            h: 1,
        };
        fill_rect(
            frame,
            scaled(dot, scale),
            mix(BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, facing),
            scale,
        );
    }
    // The pointer, from seven o'clock round to five.
    let (sin, cos) = dial_angle(value).sin_cos();
    let (reach, from) = (radius - 4.0, radius / 2.0);
    let mut r = from;
    while r <= reach {
        let dot = Rect {
            x: (cx + cos * r) as usize,
            y: (cy + sin * r) as usize,
            w: 2,
            h: 2,
        };
        fill_rect(frame, scaled(dot, scale), rgba(230, 230, 226), scale);
        r += 0.5;
    }
    text(
        frame,
        (dial.x + dial.w / 2).saturating_sub(text_w(DIAL_CAPTION, 1) / 2),
        dial.y + dial.h + 4,
        DIAL_CAPTION,
        CAPTION,
        1,
        scale,
    );
}

/// Where the dial's travel begins and how far it turns.
const DIAL_START: f32 = 0.75 * std::f32::consts::PI;
const DIAL_SWEEP: f32 = 1.5 * std::f32::consts::PI;

fn dial_angle(value: f32) -> f32 {
    DIAL_START + value.clamp(0.0, 1.0) * DIAL_SWEEP
}

/// Where `pos` stands along the travel, or `None` in the dead arc.
fn dial_value_at(panel: Rect, pos: (i32, i32)) -> Option<f32> {
    let dial = dial_rect(panel);
    let cx = dial.x as f32 + dial.w as f32 / 2.0;
    let cy = dial.y as f32 + dial.h as f32 / 2.0;
    let angle = (pos.1 as f32 - cy).atan2(pos.0 as f32 - cx);
    let from_start = (angle - DIAL_START).rem_euclid(std::f32::consts::TAU);
    (from_start <= DIAL_SWEEP).then(|| from_start / DIAL_SWEEP)
}

/// The glass and everything on it.
fn draw_lcd(frame: &mut [u8], panel: Rect, view: &GmPanelView, scale: usize) {
    let lcd = lcd_rect(panel);
    // The gloss surround, then the recessed glass.
    let surround = Rect {
        x: lcd.x.saturating_sub(6),
        y: panel.y + 2,
        w: lcd.w + 12,
        h: panel.h - 4,
    };
    fill_rect(frame, scaled(surround, scale), LCD_SURROUND, scale);
    fill_rect(
        frame,
        scaled(
            Rect {
                x: surround.x,
                y: surround.y,
                w: surround.w,
                h: 1,
            },
            scale,
        ),
        LCD_SURROUND_SHEEN,
        scale,
    );
    let glass = if view.powered { GLASS_LIT } else { GLASS_DARK };
    raised_display(frame, lcd, glass, scale);
    if !view.powered {
        return;
    }
    let screen = &view.screen;
    let inner_x = lcd.x + LCD_BEZEL + 4;
    let inner_y = lcd.y + LCD_BEZEL + 3;
    let print = mix(INK, glass, GLASS_PRINT);

    // The text block: four rows of caption-over-value, the top row
    // carrying part, instrument and name.
    let row_y = |row: usize| inner_y + row * 18;
    text(frame, inner_x, row_y(0), "PART", print, 1, scale);
    text(frame, inner_x + 40, row_y(0), "INSTRUMENT", print, 1, scale);
    let value_y = |row: usize| row_y(row) + 8;
    text(frame, inner_x, value_y(0), &screen.part, INK, 1, scale);
    text(
        frame,
        inner_x + 40,
        value_y(0),
        &screen.instrument,
        INK,
        1,
        scale,
    );
    text(frame, inner_x + 76, value_y(0), &screen.name, INK, 1, scale);

    let labels = [
        ("LEVEL", "PAN"),
        ("REVERB", "CHORUS"),
        ("K SHIFT", "MIDI CH"),
    ];
    let values = [
        (&screen.level, &screen.pan),
        (&screen.reverb, &screen.chorus),
        (&screen.key_shift, &screen.midi_ch),
    ];
    for row in 0..3 {
        let (left_label, right_label) = labels[row];
        let (left_value, right_value) = values[row];
        text(frame, inner_x, row_y(row + 1), left_label, print, 1, scale);
        text(
            frame,
            inner_x + 76,
            row_y(row + 1),
            right_label,
            print,
            1,
            scale,
        );
        text(frame, inner_x, value_y(row + 1), left_value, INK, 1, scale);
        text(
            frame,
            inner_x + 76,
            value_y(row + 1),
            right_value,
            INK,
            1,
            scale,
        );
    }

    // The bar matrix, right-anchored in the glass: sixteen columns of
    // sixteen dots. Numbers go under every fourth column and the last
    // -- all sixteen have no room at this size, and these are enough
    // to count by.
    const DOT: usize = 3;
    const PITCH_X: usize = 9;
    const PITCH_Y: usize = 4;
    let matrix_x = lcd.x + lcd.w - LCD_BEZEL - 4 - (15 * PITCH_X + DOT + 3);
    let matrix_y = inner_y + 1;
    let cell = mix(INK, glass, 1.0 - CELL_GRAIN);
    for column in 0..16 {
        let bar = screen.bars[column];
        for row in 0..16 {
            let lit = bar & (1 << row) != 0;
            let dot = Rect {
                x: matrix_x + column * PITCH_X,
                y: matrix_y + (15 - row) * PITCH_Y,
                w: DOT + 3,
                h: DOT,
            };
            fill_rect(
                frame,
                scaled(dot, scale),
                if lit { INK } else { cell },
                scale,
            );
        }
        if column % 4 == 0 || column == 15 {
            let label = (column + 1).to_string();
            text(
                frame,
                matrix_x + column * PITCH_X + 1,
                matrix_y + 16 * PITCH_Y + 2,
                &label,
                print,
                1,
                scale,
            );
        }
    }
}

/// ALL and MUTE: round buttons with their lamps beside them.
fn draw_rounds(frame: &mut [u8], panel: Rect, view: &GmPanelView, scale: usize) {
    for (control, label, second, lamp_lit) in [
        (
            GmControl::All,
            "ALL",
            false,
            view.powered && view.screen.all_led,
        ),
        (
            GmControl::Mute,
            "MUTE",
            true,
            view.powered
                && if view.mute_blinks {
                    view.blink_on
                } else {
                    view.screen.mute_led
                },
        ),
    ] {
        let rect = round_rect(panel, second);
        text(
            frame,
            rect.x.saturating_sub(text_w(label, 1) + 6),
            rect.y + 2,
            label,
            CAPTION,
            1,
            scale,
        );
        let down = view.down.contains(&control);
        let hovered = view.hover == Some(control);
        draw_round_button(frame, rect, down, hovered, scale);
        raised_display(
            frame,
            Rect {
                x: rect.x + 1,
                y: rect.y + rect.h + 4,
                w: LED_W,
                h: LED_H,
            },
            if lamp_lit { LED_AMBER } else { LED_DARK },
            scale,
        );
    }
}

/// The pair grid, each with its caption printed above.
fn draw_pairs(frame: &mut [u8], panel: Rect, view: &GmPanelView, scale: usize) {
    let (x0, y0) = pairs_origin(panel);
    for (pair, label, col, row) in PAIR_GRID {
        let x = x0 + col * (PAIR_W + COL_GAP);
        let y = y0 + row * ROW_PITCH;
        text(
            frame,
            x + PAIR_W / 2 - text_w(label, 1) / 2,
            y,
            label,
            CAPTION,
            1,
            scale,
        );
        for dir in [Dir::Left, Dir::Right] {
            let rect = arrow_rect(panel, pair, dir);
            let control = GmControl::Arrow(pair, dir);
            let down = view.down.contains(&control);
            let hovered = view.hover == Some(control);
            if pair == Pair::Part {
                draw_round_button(frame, rect, down, hovered, scale);
            } else {
                draw_button(frame, rect, down, hovered, scale);
            }
            // The moulding wears its arrow.
            let glyph = match dir {
                Dir::Left => "<",
                Dir::Right => ">",
            };
            let tx = rect.x + rect.w / 2 - text_w(glyph, 1) / 2;
            let ty = rect.y + rect.h.saturating_sub(font::GLYPH_H) / 2 + 1;
            text(frame, tx, ty, glyph, CAPTION, 1, scale);
        }
    }
}

fn draw_button(frame: &mut [u8], rect: Rect, pressed: bool, hovered: bool, scale: usize) {
    let rect = scaled(rect, scale);
    let (face, near, far) = if pressed {
        (BUTTON_FACE_PRESSED, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT)
    } else {
        (BUTTON_FACE, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK)
    };
    let face = if hovered {
        shade(face, HOVER_LIFT)
    } else {
        face
    };
    fill_rect(frame, rect, face, scale);
    draw_rect_bevel(frame, rect, near, far, scale);
}

/// A round button: the moulding turned on a lathe rather than cut
/// square, drawn as a disc with the same light.
fn draw_round_button(frame: &mut [u8], rect: Rect, pressed: bool, hovered: bool, scale: usize) {
    let (cx, cy) = (
        rect.x as f32 + rect.w as f32 / 2.0,
        rect.y as f32 + rect.h as f32 / 2.0,
    );
    let radius = rect.w.min(rect.h) as f32 / 2.0;
    let face = if pressed {
        BUTTON_FACE_PRESSED
    } else if hovered {
        shade(BUTTON_FACE, HOVER_LIFT)
    } else {
        BUTTON_FACE
    };
    for y in 0..rect.h {
        let dy = y as f32 + 0.5 - rect.h as f32 / 2.0;
        let half = (radius * radius - dy * dy).max(0.0).sqrt();
        if half < 0.5 {
            continue;
        }
        let row = Rect {
            x: (cx - half) as usize,
            y: rect.y + y,
            w: (half * 2.0) as usize,
            h: 1,
        };
        fill_rect(frame, scaled(row, scale), face, scale);
    }
    const RIM_STEPS: usize = 64;
    for i in 0..RIM_STEPS {
        let angle = i as f32 / RIM_STEPS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let facing = 0.5 - (cos + sin) * std::f32::consts::FRAC_1_SQRT_2 * 0.5;
        let facing = if pressed { 1.0 - facing } else { facing };
        let dot = Rect {
            x: (cx + cos * (radius - 0.5)) as usize,
            y: (cy + sin * (radius - 0.5)) as usize,
            w: 1,
            h: 1,
        };
        fill_rect(
            frame,
            scaled(dot, scale),
            mix(BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, facing),
            scale,
        );
    }
}

/// A recessed-bezel display with `face` behind the glass, as the status
/// bar's counters use.
fn raised_display(frame: &mut [u8], rect: Rect, face: u32, scale: usize) {
    let outer = scaled(rect, scale);
    fill_rect(frame, outer, LED_BEZEL_DARK, scale);
    draw_rect_bevel(frame, outer, LED_BEZEL_LIGHT, STATUS_BOTTOM, scale);
    let inset = LCD_BEZEL * scale;
    fill_rect(
        frame,
        Rect {
            x: outer.x + inset,
            y: outer.y + inset,
            w: outer.w.saturating_sub(inset * 2),
            h: outer.h.saturating_sub(inset * 2),
        },
        face,
        scale,
    );
}

fn text(frame: &mut [u8], x: usize, y: usize, s: &str, color: u32, px: usize, scale: usize) {
    font::draw_text(
        frame,
        texture_width(scale),
        texture_height(scale),
        x * scale,
        y * scale,
        s,
        color,
        px * scale,
    );
}

fn text_w(s: &str, px: usize) -> usize {
    s.chars().count() * font::GLYPH_W * px
}

fn scaled(rect: Rect, scale: usize) -> Rect {
    Rect {
        x: rect.x * scale,
        y: rect.y * scale,
        w: rect.w * scale,
        h: rect.h * scale,
    }
}

const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

fn shade(colour: u32, by: f32) -> u32 {
    let [r, g, b, a] = colour.to_le_bytes();
    let channel = |c: u8| (c as f32 + by).clamp(0.0, 255.0) as u8;
    u32::from_le_bytes([channel(r), channel(g), channel(b), a])
}

fn mix(from: u32, to: u32, t: f32) -> u32 {
    let [fr, fg, fb, fa] = from.to_le_bytes();
    let [tr, tg, tb, _] = to.to_le_bytes();
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    u32::from_le_bytes([channel(fr, tr), channel(fg, tg), channel(fb, tb), fa])
}

// --- pointer mechanics ---------------------------------------------------

/// A button held on the VOLUME knob.
#[derive(Debug)]
struct DialGrab {
    clockwise: bool,
    pressed_at: std::time::Instant,
    last_step: std::time::Instant,
    /// Where along the travel the hand took hold, and the volume there:
    /// a drag moves from the grip rather than jumping to the finger.
    from_travel: Option<f32>,
    from_value: f32,
    dragging: bool,
}

const DIAL_REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(350);
const DIAL_REPEAT_EVERY: std::time::Duration = std::time::Duration::from_millis(60);
/// One click of the knob.
const DIAL_STEP: f32 = 1.0 / 32.0;
/// The most buttons a combination wants latched at once.
const HOLD_LIMIT: usize = 3;

/// The pointer side of the panel: latching, the momentary flash, and
/// the knob's grab. The semantic state lives in the engine's own panel.
#[derive(Debug, Default)]
pub struct GmPanel {
    /// Buttons latched down by right-clicking them.
    holding: Vec<GmControl>,
    /// The button a plain click is lighting until the mouse comes up.
    flash: Option<GmControl>,
    dial: Option<DialGrab>,
}

impl GmPanel {
    /// Everything standing in, for the view.
    pub fn down(&self) -> Vec<GmControl> {
        self.holding
            .iter()
            .chain(self.flash.iter())
            .copied()
            .collect()
    }

    /// A press on `control`. The window carries out what comes back.
    pub fn press(&mut self, control: GmControl, left: bool, powered: bool) -> GmPress {
        if control == GmControl::Dial {
            // The knob is not a button; the window steps or drags it.
            return GmPress::None;
        }
        if control == GmControl::Power {
            // The switch takes whatever is latched with it.
            let held = self.held_buttons();
            self.holding.clear();
            return if powered {
                GmPress::PowerOff
            } else {
                GmPress::PowerOn(held)
            };
        }
        // Right-clicking latches a button down; that is how two-button
        // gestures are made with one pointer.
        if !left {
            self.latch(control);
            // A pair latched whole resolves at once when running.
            if powered {
                if let Some(button) = self.latched_gesture() {
                    self.holding.clear();
                    return GmPress::Button(button);
                }
            }
            return GmPress::None;
        }
        self.flash = Some(control);
        let latched = std::mem::take(&mut self.holding);
        let button = resolve(control, &latched);
        if powered {
            GmPress::Button(button)
        } else {
            GmPress::None
        }
    }

    /// Let a plain click's button back out.
    pub fn release_press(&mut self) {
        self.flash = None;
    }

    fn latch(&mut self, control: GmControl) {
        if let Some(i) = self.holding.iter().position(|&h| h == control) {
            self.holding.remove(i);
        } else if self.holding.len() >= HOLD_LIMIT {
            self.holding.clear();
        } else {
            self.holding.push(control);
        }
    }

    /// A gesture the latched set already names whole: both halves of a
    /// pair, or ALL with MUTE.
    fn latched_gesture(&self) -> Option<Button> {
        let [a, b] = self.holding[..] else {
            return None;
        };
        match (a, b) {
            (GmControl::Arrow(p1, d1), GmControl::Arrow(p2, d2)) if p1 == p2 && d1 != d2 => {
                Some(Button::Both(p1))
            }
            (GmControl::All, GmControl::Mute) | (GmControl::Mute, GmControl::All) => {
                Some(Button::Monitor)
            }
            _ => None,
        }
    }

    /// What the latched set means held through a power-on.
    fn held_buttons(&self) -> Vec<Button> {
        // Both halves of one pair collapse to the pair held whole.
        for (i, &a) in self.holding.iter().enumerate() {
            for &b in &self.holding[i + 1..] {
                if let (GmControl::Arrow(p1, d1), GmControl::Arrow(p2, d2)) = (a, b) {
                    if p1 == p2 && d1 != d2 && self.holding.len() == 2 {
                        return vec![Button::Both(p1)];
                    }
                }
            }
        }
        self.holding
            .iter()
            .filter_map(|&control| match control {
                GmControl::All => Some(Button::All),
                GmControl::Mute => Some(Button::Mute),
                GmControl::Arrow(pair, dir) => Some(Button::Arrow(pair, dir)),
                GmControl::Dial | GmControl::Power => None,
            })
            .collect()
    }

    // --- the knob --------------------------------------------------------

    /// Take hold of the knob: a click steps it, holding on repeats, and
    /// moving drags it. Returns the new volume to apply, if it moved.
    pub fn grab_dial(
        &mut self,
        clockwise: bool,
        pos: (i32, i32),
        panel: Rect,
        volume: f32,
    ) -> Option<f32> {
        let stepped = (volume + if clockwise { DIAL_STEP } else { -DIAL_STEP }).clamp(0.0, 1.0);
        let now = std::time::Instant::now();
        self.dial = Some(DialGrab {
            clockwise,
            pressed_at: now,
            last_step: now,
            from_travel: dial_value_at(panel, pos),
            from_value: stepped,
            dragging: false,
        });
        Some(stepped)
    }

    /// Follow the hand while a button is held: the turn is measured
    /// from where the knob was taken, so it moves under the hand.
    pub fn drag_dial(&mut self, pos: (i32, i32), panel: Rect) -> Option<f32> {
        let grab = self.dial.as_mut()?;
        let from = grab.from_travel?;
        let now_at = dial_value_at(panel, pos)?;
        let delta = now_at - from;
        if !grab.dragging && delta.abs() < 0.02 {
            return None;
        }
        grab.dragging = true;
        Some((grab.from_value + delta).clamp(0.0, 1.0))
    }

    /// Step on while the button rests on the knob.
    pub fn repeat_dial(&mut self, volume: f32) -> Option<f32> {
        let grab = self.dial.as_mut()?;
        if grab.dragging || grab.pressed_at.elapsed() < DIAL_REPEAT_DELAY {
            return None;
        }
        if grab.last_step.elapsed() < DIAL_REPEAT_EVERY {
            return None;
        }
        grab.last_step = std::time::Instant::now();
        let step = if grab.clockwise {
            DIAL_STEP
        } else {
            -DIAL_STEP
        };
        Some((volume + step).clamp(0.0, 1.0))
    }

    pub fn release_dial(&mut self) {
        self.dial = None;
    }

    pub fn dial_held(&self) -> bool {
        self.dial.is_some()
    }
}

/// A plain click against what was latched: the semantic press.
fn resolve(control: GmControl, latched: &[GmControl]) -> Button {
    if let [one] = latched[..] {
        match (one, control) {
            (GmControl::Arrow(p1, d1), GmControl::Arrow(p2, d2)) if p1 == p2 && d1 != d2 => {
                return Button::Both(p1);
            }
            (GmControl::All, GmControl::Mute) | (GmControl::Mute, GmControl::All) => {
                return Button::Monitor;
            }
            _ => {}
        }
    }
    match control {
        GmControl::All => Button::All,
        GmControl::Mute => Button::Mute,
        GmControl::Arrow(pair, dir) => Button::Arrow(pair, dir),
        // Unreachable by construction; a harmless answer regardless.
        GmControl::Dial | GmControl::Power => Button::All,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a pixel assertion: draws the fascia into a buffer and keeps
    /// PNGs beside the build for a human to look at -- powered with a
    /// lively screen, and switched off. Asserts only that the drawing
    /// stayed inside the panel's strip.
    #[test]
    fn preview_renders_for_the_eye() {
        crate::video::set_gm_panel_shown(true);
        let scale = 2;
        let top = super::super::present_height();
        let (w, h) = (
            super::super::texture_width(scale),
            super::super::texture_height(scale),
        );
        let mut screen = dark_screen();
        screen.part = "01".to_string();
        screen.instrument = "001".to_string();
        screen.name = "Grand Piano".to_string();
        screen.level = "100".to_string();
        screen.pan = "0".to_string();
        screen.reverb = "40".to_string();
        screen.chorus = "0".to_string();
        screen.key_shift = "0".to_string();
        screen.midi_ch = "01".to_string();
        for (i, bar) in screen.bars.iter_mut().enumerate() {
            // A staircase with a peak dot floating over it.
            let height = (i as u32) % 12 + 1;
            *bar = ((1u32 << height) - 1) as u16 | 1 << (height + 2).min(15);
        }
        screen.mute_led = false;
        let shots: [(&str, GmPanelView); 2] = [
            (
                "gmpanel-preview.png",
                GmPanelView {
                    screen,
                    powered: true,
                    mute_blinks: false,
                    blink_on: false,
                    volume: 0.8,
                    down: vec![GmControl::Arrow(Pair::Level, Dir::Right)],
                    hover: Some(GmControl::Arrow(Pair::Pan, Dir::Left)),
                },
            ),
            (
                "gmpanel-dark.png",
                GmPanelView {
                    screen: dark_screen(),
                    powered: false,
                    mute_blinks: false,
                    blink_on: false,
                    volume: 0.8,
                    down: Vec::new(),
                    hover: None,
                },
            ),
        ];
        for (name, view) in shots {
            let mut frame = vec![0u8; w * h * 4];
            draw(&mut frame, &view, top, scale);
            // Everything above the strip must still be untouched zeros.
            let strip_start = top * scale * w * 4;
            assert!(
                frame[..strip_start].iter().all(|&b| b == 0),
                "the panel must not paint above its strip"
            );
            let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(name);
            let file = std::fs::File::create(&out).expect("png file");
            let panel_h = GM_PANEL_HEIGHT * scale;
            let mut enc = png::Encoder::new(file, w as u32, panel_h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("png header");
            let mut data = Vec::with_capacity(w * panel_h * 4);
            for row in 0..panel_h {
                let from = ((top * scale + row) * w) * 4;
                for px in frame[from..from + w * 4].chunks(4) {
                    // The texture is plain RGBA bytes already.
                    data.extend([px[0], px[1], px[2], 255]);
                }
            }
            writer.write_image_data(&data).expect("png rows");
        }
    }

    /// The geometry answers where it says it does: every control's rect
    /// reports that control, and nothing overlaps another.
    #[test]
    fn controls_answer_at_their_rects() {
        let panel = panel_rect(400);
        let mut all = vec![
            (GmControl::Power, power_rect(panel)),
            (GmControl::Dial, dial_rect(panel)),
            (GmControl::All, round_rect(panel, false)),
            (GmControl::Mute, round_rect(panel, true)),
        ];
        for (pair, ..) in PAIR_GRID {
            for dir in [Dir::Left, Dir::Right] {
                all.push((GmControl::Arrow(pair, dir), arrow_rect(panel, pair, dir)));
            }
        }
        for (control, rect) in &all {
            let centre = ((rect.x + rect.w / 2) as i32, (rect.y + rect.h / 2) as i32);
            assert_eq!(
                control_at(panel, centre),
                Some(*control),
                "{control:?} must answer at its own centre"
            );
        }
        for (i, (_, a)) in all.iter().enumerate() {
            for (_, b) in &all[i + 1..] {
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "controls must not overlap");
            }
        }
        // And the whole grid stays inside the panel.
        for (_, rect) in &all {
            assert!(rect.x + rect.w <= panel.x + panel.w);
            assert!(rect.y + rect.h <= panel.y + panel.h);
        }
    }

    /// The pointer gestures resolve to the unit's: both halves of a
    /// pair make Both, ALL with MUTE makes Monitor, and the latched set
    /// rides through a power-on.
    #[test]
    fn latching_makes_the_gestures() {
        let mut panel = GmPanel::default();
        // Latch LEVEL <, click LEVEL >.
        assert_eq!(
            panel.press(GmControl::Arrow(Pair::Level, Dir::Left), false, true),
            GmPress::None
        );
        assert_eq!(
            panel.press(GmControl::Arrow(Pair::Level, Dir::Right), true, true),
            GmPress::Button(Button::Both(Pair::Level))
        );
        // Latch ALL, then latch MUTE: the gesture resolves at once.
        assert_eq!(panel.press(GmControl::All, false, true), GmPress::None);
        assert_eq!(
            panel.press(GmControl::Mute, false, true),
            GmPress::Button(Button::Monitor)
        );
        // Both INSTRUMENT halves latched through a power-on: Init All's
        // combination arrives as the pair held whole.
        panel.press(GmControl::Arrow(Pair::Instrument, Dir::Left), false, false);
        panel.press(GmControl::Arrow(Pair::Instrument, Dir::Right), false, false);
        assert_eq!(
            panel.press(GmControl::Power, true, false),
            GmPress::PowerOn(vec![Button::Both(Pair::Instrument)])
        );
        // ALL and MUTE latched through a power-on: the version screen's.
        panel.press(GmControl::All, false, false);
        panel.press(GmControl::Mute, false, false);
        assert_eq!(
            panel.press(GmControl::Power, true, false),
            GmPress::PowerOn(vec![Button::All, Button::Mute])
        );
        // Switched on with nothing latched, the switch turns it off.
        assert_eq!(panel.press(GmControl::Power, true, true), GmPress::PowerOff);
    }

    /// A powered-off unit takes no button presses, but still reads what
    /// is held on it.
    #[test]
    fn dark_buttons_do_nothing() {
        let mut panel = GmPanel::default();
        assert_eq!(
            panel.press(GmControl::Arrow(Pair::Part, Dir::Right), true, false),
            GmPress::None
        );
    }
}

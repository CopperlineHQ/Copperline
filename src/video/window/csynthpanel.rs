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

use coppersynth::panel::{Button, Dir, Pair, Screen, NAME_COLS};

use super::statusbar::{draw_rect_bevel, fill_rect};
use super::Rect;
use super::{texture_height, texture_width, LED_BEZEL_DARK, LED_BEZEL_LIGHT, STATUS_BOTTOM};
use crate::video::font;

/// How tall the panel is: exactly double the status bar, which is what
/// four rows of buttons and the taller glass want.
pub const CSYNTH_PANEL_HEIGHT: usize = 88;

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
const GLASS_DARK: u32 = rgba(150, 160, 159);
const INK: u32 = rgba(43, 22, 8);
/// Captions printed on the glass sit lighter than the values written
/// under them, as printed things do against driven ones.
const GLASS_PRINT: f32 = 0.42;
/// The matrix's unlit dots, a shade off the backlight.
const CELL_GRAIN: f32 = 0.12;
/// The gloss surround the glass is set into.
const LCD_SURROUND: u32 = rgba(24, 24, 24);
const LCD_SURROUND_SHEEN: u32 = rgba(52, 52, 52);
/// The LED buttons (ALL, MUTE, LOAD) and the power lamp: a misted
/// clear lens when off, and the backlight's own orange when lit.
const LED_OFF: u32 = rgba(104, 98, 88);
const LED_LIT: u32 = GLASS_LIT;

// --- geometry ------------------------------------------------------------
//
// Left to right: power with its standby LED, the VOLUME knob, the LCD,
// ALL and MUTE, then the pairs in two columns of four. Everything is
// measured from the panel's own top-left.

const PAD: usize = 8;
const LCD_BEZEL: usize = 2;
const LCD_W: usize = 380;
const LCD_H: usize = 80;
/// The knob, drawn with the same dome and rim as the MT-32's dial.
const DIAL_D: usize = 36;
const DIAL_CAPTION: &str = "VOLUME";
/// The power switch wears a pair-half's moulding, with its lamp beside.
const POWER_W: usize = 31;
const POWER_H: usize = 11;
const POWER_LED_D: usize = 7;
/// A pair: one moulding split into its two halves.
const PAIR_W: usize = 64;
const PAIR_H: usize = 11;
const PAIR_SPLIT: usize = 2;
/// The round buttons: ALL, MUTE, and the PART halves.
const ROUND_D: usize = 12;
const CAPTION_H: usize = 9;
const ROW_PITCH: usize = 21;

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
pub enum CsynthControl {
    All,
    Mute,
    /// The soundfont picker: the one button with no hardware ancestor.
    Load,
    Arrow(Pair, Dir),
    /// The VOLUME knob.
    Dial,
    /// Copperline's power switch, as on the MT-32 panel.
    Power,
}

/// What the panel needs to draw itself.
#[derive(Debug, Clone)]
pub struct CsynthPanelView {
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
    pub down: Vec<CsynthControl>,
    pub hover: Option<CsynthControl>,
}

/// The glass with its light out: what the view carries while the unit
/// is switched off. Nothing in it is drawn.
pub fn dark_screen() -> Screen {
    Screen {
        part: String::new(),
        instrument: String::new(),
        name: String::new(),
        subtitle: String::new(),
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
pub enum CsynthPress {
    None,
    /// Hand this to the engine's panel.
    Button(Button),
    /// Switch on, with these buttons held through the power-on.
    PowerOn(Vec<Button>),
    PowerOff,
    /// Open the soundfont picker.
    Load,
}

/// The panel's rect when it is actually up, and `None` when it is not.
pub fn shown_panel_rect(top: usize) -> Option<Rect> {
    crate::video::csynth_panel_shown().then(|| panel_rect(top))
}

/// The panel's rect, its top edge at `top`.
pub fn panel_rect(top: usize) -> Rect {
    Rect {
        x: 0,
        y: top,
        w: crate::video::FB_WIDTH,
        h: CSYNTH_PANEL_HEIGHT,
    }
}

fn power_rect(panel: Rect) -> Rect {
    Rect {
        x: panel.x + PAD,
        y: panel.y + 18,
        w: POWER_W,
        h: POWER_H,
    }
}

fn power_led_rect(panel: Rect) -> Rect {
    let power = power_rect(panel);
    Rect {
        x: power.x + power.w + 8,
        y: power.y + 2,
        w: POWER_LED_D,
        h: POWER_LED_D,
    }
}

fn dial_rect(panel: Rect) -> Rect {
    // Its top on the power switch's top, as the two sit on the shelf.
    let power = power_rect(panel);
    Rect {
        x: panel.x + PAD + 50,
        y: power.y,
        w: DIAL_D,
        h: DIAL_D,
    }
}

fn lcd_rect(panel: Rect) -> Rect {
    let dial = dial_rect(panel);
    Rect {
        x: dial.x + DIAL_D + 10,
        y: panel.y + (panel.h - LCD_H) / 2,
        w: LCD_W,
        h: LCD_H,
    }
}

/// The three button columns right of the glass -- the LED lenses and
/// the two pair columns -- spaced evenly across what the glass leaves.
fn right_columns(panel: Rect) -> (usize, usize, usize) {
    let lcd = lcd_rect(panel);
    let lo = lcd.x + lcd.w;
    let hi = panel.x + panel.w - PAD;
    // The lens column's caption hangs to the left of its lenses.
    let rounds_w = 21 + ROUND_D;
    let fixed = rounds_w + 2 * PAIR_W;
    let gap = hi.saturating_sub(lo).saturating_sub(fixed) / 3;
    let rounds_x = lo + gap + 21;
    let pairs1 = lo + gap + rounds_w + gap;
    let pairs2 = pairs1 + PAIR_W + gap;
    (rounds_x, pairs1, pairs2)
}

/// The pair grid's top edge.
fn pairs_origin(panel: Rect) -> (usize, usize) {
    (right_columns(panel).1, panel.y + 2)
}

/// The LED-button column between the glass and the pairs: ALL, MUTE
/// and LOAD, top to bottom.
fn round_rect(panel: Rect, slot: usize) -> Rect {
    Rect {
        x: right_columns(panel).0,
        y: panel.y + 13 + slot * 26,
        w: ROUND_D,
        h: ROUND_D,
    }
}

/// One half of a pair. The PART pair wears round halves, the rest the
/// wide split moulding, as on the unit.
fn arrow_rect(panel: Rect, pair: Pair, dir: Dir) -> Rect {
    let (_, pairs1, pairs2) = right_columns(panel);
    let y0 = panel.y + 2;
    let (_, _, col, row) = PAIR_GRID
        .iter()
        .find(|(p, ..)| *p == pair)
        .copied()
        .unwrap_or(PAIR_GRID[0]);
    let x = if col == 0 { pairs1 } else { pairs2 };
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
pub fn control_at(panel: Rect, pos: (i32, i32)) -> Option<CsynthControl> {
    if power_rect(panel).contains(pos) {
        return Some(CsynthControl::Power);
    }
    if dial_rect(panel).contains(pos) {
        return Some(CsynthControl::Dial);
    }
    if round_rect(panel, 0).contains(pos) {
        return Some(CsynthControl::All);
    }
    if round_rect(panel, 1).contains(pos) {
        return Some(CsynthControl::Mute);
    }
    if round_rect(panel, 2).contains(pos) {
        return Some(CsynthControl::Load);
    }
    for (pair, ..) in PAIR_GRID {
        for dir in [Dir::Left, Dir::Right] {
            if arrow_rect(panel, pair, dir).contains(pos) {
                return Some(CsynthControl::Arrow(pair, dir));
            }
        }
    }
    None
}

/// What answers to being hovered: buttons -- the switch included now
/// that it presses like one -- but not the knob, which follows the
/// hand already.
pub fn hover_at(panel: Rect, pos: (i32, i32)) -> Option<CsynthControl> {
    control_at(panel, pos).filter(|c| !matches!(c, CsynthControl::Dial))
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
pub fn draw(frame: &mut [u8], view: &CsynthPanelView, top: usize, scale: usize) {
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
    // The marque, printed on the fascia where the unit signs itself --
    // set by each letter's ink rather than the font's cells, so the
    // word spaces evenly.
    let lcd = lcd_rect(panel);
    text_kerned(
        frame,
        panel.x + PAD,
        (lcd.y + lcd.h).saturating_sub(LCD_BEZEL + font::GLYPH_H),
        "Coppersynth",
        CAPTION,
        scale,
    );
}

/// Text advanced by each glyph's own ink width plus a fixed gap: the
/// cell font reads unevenly for a word set large on the fascia, and
/// this is the marque's answer.
fn text_kerned(frame: &mut [u8], x: usize, y: usize, s: &str, color: u32, scale: usize) {
    let mut pen = x;
    for ch in s.chars() {
        let glyph = font::glyph(ch);
        let mut lo = font::GLYPH_W;
        let mut hi = 0;
        for bits in glyph.iter() {
            for col in 0..font::GLYPH_W {
                if bits & (1 << col) != 0 {
                    lo = lo.min(col);
                    hi = hi.max(col + 1);
                }
            }
        }
        if lo >= hi {
            pen += 3;
            continue;
        }
        for (row, bits) in glyph.iter().enumerate() {
            for col in lo..hi {
                if bits & (1 << col) == 0 {
                    continue;
                }
                let dot = Rect {
                    x: pen + col - lo,
                    y: y + row,
                    w: 1,
                    h: 1,
                };
                fill_rect(frame, scaled(dot, scale), color, scale);
            }
        }
        pen += hi - lo + 1;
    }
}

fn draw_power(frame: &mut [u8], panel: Rect, view: &CsynthPanelView, scale: usize) {
    let rect = power_rect(panel);
    text_small(
        frame,
        rect.x + POWER_W.saturating_sub(text_small_w("POWER")) / 2,
        panel.y + 7,
        "POWER",
        CAPTION,
        scale,
    );
    // Momentary, like every button on the fascia: it pops in under the
    // pointer and back out. The lamp is what says the unit is on.
    let pressed = view.down.contains(&CsynthControl::Power);
    let hovered = view.hover == Some(CsynthControl::Power);
    draw_button(frame, rect, pressed, hovered, scale, true, true);
    draw_led_dot(frame, power_led_rect(panel), view.powered, scale);
}

/// A plain round LED: a flat disc of its colour, drawn as mirrored
/// rows about an integer centre so neither side sheds pixels.
fn draw_led_dot(frame: &mut [u8], rect: Rect, lit: bool, scale: usize) {
    let colour = if lit { LED_LIT } else { LED_OFF };
    let d = rect.w.min(rect.h);
    let r = (d as f32 - 1.0) / 2.0;
    for y in 0..d {
        let dy = y as f32 - r;
        let half = (r * r - dy * dy).max(0.0).sqrt().round() as usize;
        let row = Rect {
            x: rect.x + (d - 1) / 2 - half,
            y: rect.y + y,
            w: 2 * half + 1,
            h: 1,
        };
        fill_rect(frame, scaled(row, scale), colour, scale);
    }
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
    text_small(
        frame,
        (dial.x + dial.w / 2).saturating_sub(text_small_w(DIAL_CAPTION) / 2),
        panel.y + 7,
        DIAL_CAPTION,
        CAPTION,
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
fn draw_lcd(frame: &mut [u8], panel: Rect, view: &CsynthPanelView, scale: usize) {
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
    sunken_display(frame, lcd, glass, scale);
    let screen = &view.screen;
    // A shade closer to the edge than it was, which is what buys the
    // name band its twenty characters.
    let inner_x = lcd.x + LCD_BEZEL + 1;
    let inner_y = lcd.y + LCD_BEZEL + 3;
    // The legends, the meter numbers and the scale are ink printed on
    // the glass overlay, not driven segments: they keep their weight
    // with the light out, exactly as the unit's do. Only the driven
    // things -- values, name, bars -- need the power.
    let print = mix(INK, glass, GLASS_PRINT);

    // The text block: four rows of caption-over-value, the top row
    // carrying part, instrument and name.
    // Two label columns, their four rows spread evenly down the glass:
    // PART, LEVEL, REVERB and K SHIFT, then INST, PAN, CHORUS and
    // MIDI CH beside them.
    let inner_h = lcd.h - 2 * LCD_BEZEL - 6;
    let cell_h = 16;
    let pitch = (inner_h - cell_h) / 3;
    let col2_x = inner_x + 33;
    let ghost = if view.powered {
        mix(INK, glass, 1.0 - CELL_GRAIN)
    } else {
        shade(glass, -7.0)
    };
    let left = [
        ("PART", &screen.part),
        ("LEVEL", &screen.level),
        ("REVERB", &screen.reverb),
        ("K SHIFT", &screen.key_shift),
    ];
    let right = [
        ("INST", &screen.instrument),
        ("PAN", &screen.pan),
        ("CHORUS", &screen.chorus),
        ("MIDI CH", &screen.midi_ch),
    ];
    for (row, ((l_label, l_value), (r_label, r_value))) in left.iter().zip(right.iter()).enumerate()
    {
        let y = inner_y + row * pitch;
        for (x, label, value) in [(inner_x, l_label, l_value), (col2_x, r_label, r_value)] {
            // The three-cell ghost lines up down its column, the label
            // centred over it and the value filling it from the right,
            // as a display's digits do.
            let field_x = x + 6;
            let field_w = 3 * font::GLYPH_W - 1;
            let label_x = field_x + field_w.saturating_sub(text_small_w(label)) / 2;
            text_small(frame, label_x, y, label, print, scale);
            for cell in 0..3 {
                let block = Rect {
                    x: field_x + cell * font::GLYPH_W,
                    y: y + 8,
                    w: font::GLYPH_W - 1,
                    h: 8,
                };
                fill_rect(frame, scaled(block, scale), ghost, scale);
            }
            if view.powered {
                let shown: String = value.chars().take(3).collect();
                let text_x = field_x + 3 * font::GLYPH_W - text_w(&shown, 1);
                text(frame, text_x, y + 8, &shown, INK, 1, scale);
            }
        }
    }

    // The bar matrix, right-anchored, its pitch loose enough for all
    // sixteen numbers to sit evenly under their columns.
    const DOT: usize = 3;
    const PITCH_X: usize = 10;
    const PITCH_Y: usize = 4;
    let matrix_x = lcd.x + lcd.w - LCD_BEZEL - 4 - (15 * PITCH_X + DOT + 3);
    let matrix_y = inner_y + 1;
    let cell = ghost;
    // The scale up the left edge: ten dots, the first, fifth and
    // tenth a size larger, printed on the glass whether the light is
    // on or not.
    for i in 0..10u32 {
        let big = i == 0 || i == 4 || i == 9;
        let span = 15 * PITCH_Y + DOT;
        let y = matrix_y + span - 1 - (i as usize * (span - 2)) / 9;
        let size = if big { 2 } else { 1 };
        let dot = Rect {
            x: matrix_x - 4 - size + 1,
            y: y.saturating_sub(size / 2),
            w: size,
            h: size,
        };
        fill_rect(frame, scaled(dot, scale), print, scale);
    }
    for column in 0..16 {
        let bar = screen.bars[column];
        for row in 0..16 {
            let lit = view.powered && bar & (1 << row) != 0;
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
        let label = (column + 1).to_string();
        let w = text_small_w(&label);
        text_small(
            frame,
            matrix_x + column * PITCH_X + (PITCH_X - w) / 2 - 1,
            matrix_y + 16 * PITCH_Y + 2,
            &label,
            print,
            scale,
        );
    }

    // The name, taller than everything, centred in the band between
    // the label columns and the bars: twenty characters' worth, a full
    // SF2 preset name.
    let band_lo = col2_x + 34;
    let band_hi = matrix_x.saturating_sub(5);
    let name_w = screen.name.chars().count().min(NAME_COLS) * TALL_PITCH;
    let name_x = band_lo + (band_hi.saturating_sub(band_lo).saturating_sub(name_w)) / 2;
    if view.powered {
        if screen.subtitle.is_empty() {
            let name_y = inner_y + (inner_h.saturating_sub(16)) / 2;
            text_tall(frame, name_x, name_y, &screen.name, INK, scale);
        } else {
            // Two lines share the band: the name above, the smaller
            // subtitle centred under it.
            let block_h = 16 + 4 + font::GLYPH_H;
            let name_y = inner_y + (inner_h.saturating_sub(block_h)) / 2;
            text_tall(frame, name_x, name_y, &screen.name, INK, scale);
            let sub_w = screen.subtitle.chars().count() * font::GLYPH_W;
            let sub_x = band_lo + (band_hi.saturating_sub(band_lo).saturating_sub(sub_w)) / 2;
            // A long line eases left rather than running into the bars.
            let sub_x = sub_x.min((matrix_x - 2).saturating_sub(sub_w));
            text(
                frame,
                sub_x,
                name_y + 16 + 4,
                &screen.subtitle,
                INK,
                1,
                scale,
            );
        }
    }
}

/// The name row's pitch: a column tighter than the font's own cell,
/// which is what lets sixteen characters sit in the band.
const TALL_PITCH: usize = 7;

/// Text at double height on the tightened pitch: taller than the
/// labels, and sixteen of it fits beside the bars.
fn text_tall(frame: &mut [u8], x: usize, y: usize, s: &str, color: u32, scale: usize) {
    // The shared font's asterisk fills all eight columns, which the
    // tightened pitch would run straight into the next cell -- the
    // drum-kit marker gets a five-column star that keeps the gap.
    const STAR: [u8; 8] = [0, 0x15, 0x0E, 0x1F, 0x0E, 0x15, 0, 0];
    for (cell, ch) in s.chars().enumerate() {
        let glyph = if ch == '*' { &STAR } else { font::glyph(ch) };
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..font::GLYPH_W {
                if bits & (1 << col) == 0 {
                    continue;
                }
                let dot = Rect {
                    x: x + cell * TALL_PITCH + col,
                    y: y + row * 2,
                    w: 1,
                    h: 2,
                };
                fill_rect(frame, scaled(dot, scale), color, scale);
            }
        }
    }
}

/// The label font: three by five, for the glass's helper text and the
/// bar numbers, clearly smaller than the values.
const SMALL_W: usize = 4;

fn text_small(frame: &mut [u8], x: usize, y: usize, s: &str, color: u32, scale: usize) {
    for (cell, ch) in s.chars().enumerate() {
        let glyph = small_glyph(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3 {
                if bits & (1 << (2 - col)) == 0 {
                    continue;
                }
                let dot = Rect {
                    x: x + cell * SMALL_W + col,
                    y: y + row,
                    w: 1,
                    h: 1,
                };
                fill_rect(frame, scaled(dot, scale), color, scale);
            }
        }
    }
}

fn text_small_w(s: &str) -> usize {
    s.chars().count() * SMALL_W - 1
}

#[rustfmt::skip]
fn small_glyph(ch: char) -> [u8; 5] {
    match ch.to_ascii_uppercase() {
        'A' => [0b111, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b111, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b111, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b111, 0b101, 0b101, 0b101, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
        'R' => [0b111, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        _ => [0b000; 5],
    }
}

/// ALL, MUTE and LOAD: the buttons are LEDs themselves -- a misted
/// clear lens when off, the backlight's orange when their state is on.
fn draw_rounds(frame: &mut [u8], panel: Rect, view: &CsynthPanelView, scale: usize) {
    let mute_lit = if view.mute_blinks {
        view.blink_on
    } else {
        view.screen.mute_led
    };
    for (control, label, slot, lit) in [
        (
            CsynthControl::All,
            "ALL",
            0,
            view.powered && view.screen.all_led,
        ),
        (CsynthControl::Mute, "MUTE", 1, view.powered && mute_lit),
        (CsynthControl::Load, "LOAD", 2, false),
    ] {
        let rect = round_rect(panel, slot);
        text_small(
            frame,
            rect.x.saturating_sub(text_small_w(label) + 6),
            rect.y + 4,
            label,
            CAPTION,
            scale,
        );
        let down = view.down.contains(&control);
        let hovered = view.hover == Some(control);
        draw_led_disc(frame, rect, lit, down || hovered, scale);
    }
}

/// A round LED lens: frosted and unlit, or driven the backlight's
/// orange. `pressed` darkens it the way a finger on a lens does.
fn draw_led_disc(frame: &mut [u8], rect: Rect, lit: bool, pressed: bool, scale: usize) {
    let (cx, cy) = (
        rect.x as f32 + rect.w as f32 / 2.0,
        rect.y as f32 + rect.h as f32 / 2.0,
    );
    let radius = rect.w.min(rect.h) as f32 / 2.0;
    let face = match (lit, pressed) {
        (true, false) => shade(LED_LIT, 10.0),
        (true, true) => shade(LED_LIT, -18.0),
        (false, false) => LED_OFF,
        (false, true) => shade(LED_OFF, -18.0),
    };
    for y in 0..rect.h {
        let dy = y as f32 + 0.5 - rect.h as f32 / 2.0;
        let half = (radius * radius - dy * dy).max(0.0).sqrt();
        if half < 0.5 {
            continue;
        }
        // The lens is domed: brighter above centre, falling below.
        let dome = shade(face, -dy / radius * 14.0);
        let row = Rect {
            x: (cx - half) as usize,
            y: rect.y + y,
            w: (half * 2.0) as usize,
            h: 1,
        };
        fill_rect(frame, scaled(row, scale), dome, scale);
    }
    // A dark seat ring holds the lens in the fascia.
    const RIM_STEPS: usize = 48;
    for i in 0..RIM_STEPS {
        let angle = i as f32 / RIM_STEPS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let dot = Rect {
            x: (cx + cos * (radius - 0.5)) as usize,
            y: (cy + sin * (radius - 0.5)) as usize,
            w: 1,
            h: 1,
        };
        fill_rect(frame, scaled(dot, scale), PANEL_EDGE_DARK, scale);
    }
}

/// The pair grid, each with its caption printed above.
fn draw_pairs(frame: &mut [u8], panel: Rect, view: &CsynthPanelView, scale: usize) {
    let (_, pairs1, pairs2) = right_columns(panel);
    let y0 = pairs_origin(panel).1;
    for (pair, label, col, row) in PAIR_GRID {
        let x = if col == 0 { pairs1 } else { pairs2 };
        let y = y0 + row * ROW_PITCH;
        text_small(
            frame,
            x + PAIR_W / 2 - text_small_w(label) / 2,
            y + 2,
            label,
            CAPTION,
            scale,
        );
        for dir in [Dir::Left, Dir::Right] {
            let rect = arrow_rect(panel, pair, dir);
            let control = CsynthControl::Arrow(pair, dir);
            let down = view.down.contains(&control);
            let hovered = view.hover == Some(control);
            if pair == Pair::Part {
                draw_round_button(frame, rect, down, hovered, scale);
            } else {
                // Rounded where the moulding meets the fascia, square
                // where the two halves meet each other.
                let (round_l, round_r) = match dir {
                    Dir::Left => (true, false),
                    Dir::Right => (false, true),
                };
                draw_button(frame, rect, down, hovered, scale, round_l, round_r);
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

/// A button moulding with softened corners: the corner pixels are cut
/// where `round_l`/`round_r` say, and left square where two halves of
/// a pair join.
fn draw_button(
    frame: &mut [u8],
    rect: Rect,
    pressed: bool,
    hovered: bool,
    scale: usize,
    round_l: bool,
    round_r: bool,
) {
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
    let cut_l = if round_l { 2 } else { 0 };
    let cut_r = if round_r { 2 } else { 0 };
    // The face, its cut corners left to the fascia.
    fill_rect(
        frame,
        scaled(
            Rect {
                x: rect.x + cut_l,
                y: rect.y,
                w: rect.w - cut_l - cut_r,
                h: rect.h,
            },
            scale,
        ),
        face,
        scale,
    );
    for (x, cut) in [(rect.x, cut_l), (rect.x + rect.w - 1, cut_r)] {
        fill_rect(
            frame,
            scaled(
                Rect {
                    x,
                    y: rect.y + cut,
                    w: 1,
                    h: rect.h - 2 * cut,
                },
                scale,
            ),
            face,
            scale,
        );
    }
    // The bevel: light along top and left, dark along bottom and right,
    // each stopping short of a cut corner.
    let lines = [
        (rect.x + cut_l, rect.y, rect.w - cut_l - cut_r, 1, near),
        (
            rect.x + cut_l,
            rect.y + rect.h - 1,
            rect.w - cut_l - cut_r,
            1,
            far,
        ),
        (rect.x, rect.y + cut_l, 1, rect.h - 2 * cut_l, near),
        (
            rect.x + rect.w - 1,
            rect.y + cut_r,
            1,
            rect.h - 2 * cut_r,
            far,
        ),
    ];
    for (x, y, w, h, colour) in lines {
        fill_rect(frame, scaled(Rect { x, y, w, h }, scale), colour, scale);
    }
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

/// The glass set into the fascia rather than standing off it: the
/// bevel's light falls on the bottom and right, the way a recess
/// catches it, with the same depth the raised treatment had.
fn sunken_display(frame: &mut [u8], rect: Rect, face: u32, scale: usize) {
    let outer = scaled(rect, scale);
    fill_rect(frame, outer, LED_BEZEL_DARK, scale);
    draw_rect_bevel(frame, outer, STATUS_BOTTOM, LED_BEZEL_LIGHT, scale);
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
/// The most buttons a combination wants latched at once: two pairs
/// held whole take all four.
const HOLD_LIMIT: usize = 4;

/// An arrow button held down, repeating and gathering speed.
#[derive(Debug)]
struct ArrowHold {
    button: Button,
    pressed_at: std::time::Instant,
    last: std::time::Instant,
    steps: u32,
}

/// The pointer side of the panel: latching, the momentary flash, and
/// the knob's grab. The semantic state lives in the engine's own panel.
#[derive(Debug, Default)]
pub struct CsynthPanel {
    /// Buttons latched down by right-clicking them.
    holding: Vec<CsynthControl>,
    /// The button a plain click is lighting until the mouse comes up.
    flash: Option<CsynthControl>,
    dial: Option<DialGrab>,
    hold: Option<ArrowHold>,
}

impl CsynthPanel {
    /// Everything standing in, for the view.
    pub fn down(&self) -> Vec<CsynthControl> {
        self.holding
            .iter()
            .chain(self.flash.iter())
            .copied()
            .collect()
    }

    /// A press on `control`. The window carries out what comes back.
    pub fn press(&mut self, control: CsynthControl, left: bool, powered: bool) -> CsynthPress {
        if control == CsynthControl::Dial {
            // The knob is not a button; the window steps or drags it.
            return CsynthPress::None;
        }
        if control == CsynthControl::Load {
            // Momentary and outside the unit's combinations: it asks
            // the host for its file picker.
            self.flash = left.then_some(control);
            return if left {
                CsynthPress::Load
            } else {
                CsynthPress::None
            };
        }
        if control == CsynthControl::Power {
            // The switch takes whatever is latched with it, and pops
            // in under the click like every other button -- the lamp,
            // not the moulding, says whether the unit is on.
            self.flash = left.then_some(control);
            let held = self.held_buttons();
            self.holding.clear();
            return if powered {
                CsynthPress::PowerOff
            } else {
                CsynthPress::PowerOn(held)
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
                    return CsynthPress::Button(button);
                }
            } else {
                // The splash combination switches the unit on itself:
                // completing the fourth latch is the power press.
                let held = self.held_buttons();
                if held.len() == 2
                    && held.contains(&Button::Both(Pair::MidiCh))
                    && held.contains(&Button::Both(Pair::Instrument))
                {
                    self.holding.clear();
                    return CsynthPress::PowerOn(held);
                }
            }
            return CsynthPress::None;
        }
        self.flash = Some(control);
        let latched = std::mem::take(&mut self.holding);
        let button = resolve(control, &latched);
        if powered {
            // A plain arrow held down repeats, gathering speed.
            if let (Button::Arrow(..), true) = (button, latched.is_empty()) {
                let now = std::time::Instant::now();
                self.hold = Some(ArrowHold {
                    button,
                    pressed_at: now,
                    last: now,
                    steps: 0,
                });
            }
            CsynthPress::Button(button)
        } else {
            CsynthPress::None
        }
    }

    /// Let a plain click's button back out.
    pub fn release_press(&mut self) {
        self.flash = None;
        self.hold = None;
    }

    /// The next repeat of a held arrow, once it has been held a moment
    /// -- faster the longer it is held.
    pub fn repeat_button(&mut self) -> Option<Button> {
        let hold = self.hold.as_mut()?;
        if hold.pressed_at.elapsed() < std::time::Duration::from_millis(400) {
            return None;
        }
        let interval = match hold.steps {
            0..=11 => 140,
            12..=39 => 55,
            _ => 25,
        };
        if hold.last.elapsed() < std::time::Duration::from_millis(interval) {
            return None;
        }
        hold.last = std::time::Instant::now();
        hold.steps += 1;
        Some(hold.button)
    }

    fn latch(&mut self, control: CsynthControl) {
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
            (CsynthControl::Arrow(p1, d1), CsynthControl::Arrow(p2, d2))
                if p1 == p2 && d1 != d2 =>
            {
                Some(Button::Both(p1))
            }
            (CsynthControl::All, CsynthControl::Mute)
            | (CsynthControl::Mute, CsynthControl::All) => Some(Button::Monitor),
            _ => None,
        }
    }

    /// What the latched set means held through a power-on: halves of a
    /// pair latched together collapse to the pair held whole, and the
    /// rest map one for one.
    fn held_buttons(&self) -> Vec<Button> {
        let mut arrows: Vec<(Pair, Dir)> = Vec::new();
        let mut out = Vec::new();
        for &control in &self.holding {
            match control {
                CsynthControl::All => out.push(Button::All),
                CsynthControl::Mute => out.push(Button::Mute),
                CsynthControl::Arrow(pair, dir) => arrows.push((pair, dir)),
                CsynthControl::Load | CsynthControl::Dial | CsynthControl::Power => {}
            }
        }
        while let Some((pair, dir)) = arrows.pop() {
            if let Some(i) = arrows.iter().position(|&(p, d)| p == pair && d != dir) {
                arrows.remove(i);
                out.push(Button::Both(pair));
            } else {
                out.push(Button::Arrow(pair, dir));
            }
        }
        out
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
fn resolve(control: CsynthControl, latched: &[CsynthControl]) -> Button {
    if let [one] = latched[..] {
        match (one, control) {
            (CsynthControl::Arrow(p1, d1), CsynthControl::Arrow(p2, d2))
                if p1 == p2 && d1 != d2 =>
            {
                return Button::Both(p1);
            }
            (CsynthControl::All, CsynthControl::Mute)
            | (CsynthControl::Mute, CsynthControl::All) => {
                return Button::Monitor;
            }
            _ => {}
        }
    }
    match control {
        CsynthControl::All => Button::All,
        CsynthControl::Mute => Button::Mute,
        CsynthControl::Arrow(pair, dir) => Button::Arrow(pair, dir),
        // Unreachable by construction; a harmless answer regardless.
        CsynthControl::Load | CsynthControl::Dial | CsynthControl::Power => Button::All,
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
        crate::video::set_csynth_panel_shown(true);
        let scale = 2;
        let top = super::super::present_height();
        let (w, h) = (
            super::super::texture_width(scale),
            super::super::texture_height(scale),
        );
        let mut screen = dark_screen();
        screen.part = "01".to_string();
        screen.instrument = "001".to_string();
        screen.name = "Bright Grand Piano X".to_string();
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
        let mut splash = dark_screen();
        splash.name = "COPPERSYNTH".to_string();
        splash.subtitle = "v0.9.0 2026-08-16".to_string();
        let shots: [(&str, CsynthPanelView); 3] = [
            (
                "csynthpanel-splash.png",
                CsynthPanelView {
                    screen: splash,
                    powered: true,
                    mute_blinks: false,
                    blink_on: false,
                    volume: 0.8,
                    down: Vec::new(),
                    hover: None,
                },
            ),
            (
                "csynthpanel-preview.png",
                CsynthPanelView {
                    screen,
                    powered: true,
                    mute_blinks: false,
                    blink_on: false,
                    volume: 0.8,
                    down: vec![CsynthControl::Arrow(Pair::Level, Dir::Right)],
                    hover: Some(CsynthControl::Arrow(Pair::Pan, Dir::Left)),
                },
            ),
            (
                "csynthpanel-dark.png",
                CsynthPanelView {
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
            let panel_h = CSYNTH_PANEL_HEIGHT * scale;
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
            (CsynthControl::Power, power_rect(panel)),
            (CsynthControl::Dial, dial_rect(panel)),
            (CsynthControl::All, round_rect(panel, 0)),
            (CsynthControl::Mute, round_rect(panel, 1)),
            (CsynthControl::Load, round_rect(panel, 2)),
        ];
        for (pair, ..) in PAIR_GRID {
            for dir in [Dir::Left, Dir::Right] {
                all.push((
                    CsynthControl::Arrow(pair, dir),
                    arrow_rect(panel, pair, dir),
                ));
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
        let mut panel = CsynthPanel::default();
        // Latch LEVEL <, click LEVEL >.
        assert_eq!(
            panel.press(CsynthControl::Arrow(Pair::Level, Dir::Left), false, true),
            CsynthPress::None
        );
        assert_eq!(
            panel.press(CsynthControl::Arrow(Pair::Level, Dir::Right), true, true),
            CsynthPress::Button(Button::Both(Pair::Level))
        );
        // Latch ALL, then latch MUTE: the gesture resolves at once.
        assert_eq!(
            panel.press(CsynthControl::All, false, true),
            CsynthPress::None
        );
        assert_eq!(
            panel.press(CsynthControl::Mute, false, true),
            CsynthPress::Button(Button::Monitor)
        );
        // Both INSTRUMENT halves latched through a power-on: the
        // default-font combination arrives as the pair held whole.
        panel.press(
            CsynthControl::Arrow(Pair::Instrument, Dir::Left),
            false,
            false,
        );
        panel.press(
            CsynthControl::Arrow(Pair::Instrument, Dir::Right),
            false,
            false,
        );
        assert_eq!(
            panel.press(CsynthControl::Power, true, false),
            CsynthPress::PowerOn(vec![Button::Both(Pair::Instrument)])
        );
        // The splash combination needs no power press at all: the
        // fourth latch switches the unit on itself.
        panel.press(CsynthControl::Arrow(Pair::MidiCh, Dir::Left), false, false);
        panel.press(CsynthControl::Arrow(Pair::MidiCh, Dir::Right), false, false);
        panel.press(
            CsynthControl::Arrow(Pair::Instrument, Dir::Left),
            false,
            false,
        );
        let fired = panel.press(
            CsynthControl::Arrow(Pair::Instrument, Dir::Right),
            false,
            false,
        );
        let CsynthPress::PowerOn(held) = fired else {
            panic!("the fourth latch must power the unit: {fired:?}");
        };
        assert!(held.contains(&Button::Both(Pair::MidiCh)));
        assert!(held.contains(&Button::Both(Pair::Instrument)));
        // ALL and MUTE latched through a power-on: the version screen's.
        panel.press(CsynthControl::All, false, false);
        panel.press(CsynthControl::Mute, false, false);
        assert_eq!(
            panel.press(CsynthControl::Power, true, false),
            CsynthPress::PowerOn(vec![Button::All, Button::Mute])
        );
        // Switched on with nothing latched, the switch turns it off.
        assert_eq!(
            panel.press(CsynthControl::Power, true, true),
            CsynthPress::PowerOff
        );
    }

    /// A powered-off unit takes no button presses, but still reads what
    /// is held on it.
    #[test]
    fn dark_buttons_do_nothing() {
        let mut panel = CsynthPanel::default();
        assert_eq!(
            panel.press(CsynthControl::Arrow(Pair::Part, Dir::Right), true, false),
            CsynthPress::None
        );
    }
}

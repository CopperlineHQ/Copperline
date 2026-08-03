// SPDX-License-Identifier: GPL-3.0-or-later

//! The Munt MT-32's front panel, drawn under the display.
//!
//! Laid out the way the unit is: the LCD at the left with the MIDI MESSAGE
//! lamp beside it, then the six part buttons under an underlined PART, then
//! the four function buttons, and the Select/Volume dial at the right with
//! its travel marked out around it. It is a panel for driving the emulated
//! synth, not a picture of the hardware -- the proportions follow the unit
//! because that is what makes it legible, and nothing is copied from it.

use super::statusbar::draw_power_glyph_sized;
use super::statusbar::{draw_rect_bevel, fill_rect};
use super::Rect;
use super::{
    texture_height, texture_width, LED_BEZEL_DARK, LED_BEZEL_LIGHT, POWER_GLYPH_OFF,
    POWER_GLYPH_ON, STATUS_BOTTOM,
};
use crate::video::font;

/// How tall the panel is. Two rows of buttons and their captions need more
/// than the status bar's 44, and the dial wants to be round with room for
/// the marks around it.
pub const MT32_PANEL_HEIGHT: usize = 60;

// The fascia and its buttons are the status bar's, so the two strips read
// as one piece of chrome rather than two greys that nearly match.
use super::{BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, BUTTON_FACE, STATUS_BG, STATUS_TEXT, STATUS_TOP};
const PANEL_FACE: u32 = STATUS_BG;
const PANEL_EDGE_LIGHT: u32 = STATUS_TOP;
const PANEL_EDGE_DARK: u32 = STATUS_BOTTOM;
const CAPTION: u32 = STATUS_TEXT;
/// The gloss panel the display and its lamp are set into: the fascia is a
/// matt near-black, and this is the shinier black around the glass, as the
/// unit has.
const LCD_SURROUND: u32 = rgba(9, 9, 11);
const LCD_SURROUND_SHEEN: u32 = rgba(26, 26, 30);

/// What the glass and its characters look like, per style.
///
/// Unlit is its own colour: the MT-32's backlight simply goes out, while a
/// JV-1080 keeps its blue a shade darker.
struct LcdColours {
    glass: u32,
    dark_glass: u32,
    text: u32,
    /// The unlit dot matrix behind each character, when the display shows
    /// one. A shade off the field around it, which is what makes the cells
    /// countable without competing with the writing over them.
    cell: Option<u32>,
}

fn lcd_colours() -> LcdColours {
    use crate::config::Mt32Lcd;
    match crate::video::mt32_lcd() {
        // Lit in the same green as the status bar's track counter, so the
        // two readouts match. An OLED drives its dots rather than masking
        // them, so there is no matrix to see behind the characters.
        Mt32Lcd::Oled => LcdColours {
            glass: super::TRACK_DISPLAY_BG,
            dark_glass: rgba(4, 5, 4),
            text: super::TRACK_SEGMENT_ON,
            cell: None,
        },
        // Lighter green characters over a lit green backlight; black once
        // that backlight is out.
        Mt32Lcd::Mt32 => LcdColours {
            glass: rgba(20, 45, 12),
            // Unlit it is bare glass rather than a hole: the same green with
            // the backlight out of it, barely above the surround, so the
            // panel still reads as having a display in it.
            dark_glass: rgba(8, 17, 7),
            text: rgba(198, 222, 107),
            // The cells sit a shade above the field they are masked out of,
            // which is what makes them countable at a glance.
            cell: Some(rgba(38, 68, 22)),
        },
        // Deep blue, pale green characters, and the blue stays when it is
        // off.
        Mt32Lcd::Jv1080 => LcdColours {
            glass: rgba(10, 26, 74),
            dark_glass: rgba(6, 15, 44),
            // A little brighter than it would otherwise want to be: the
            // gaps between its dots show deep blue rather than dark green,
            // which takes more out of the characters than the MT-32's do.
            text: rgba(184, 240, 110),
            // There on the unit too, but only just: a shade off the glass,
            // where the MT-32's own can be counted at a glance.
            cell: Some(rgba(16, 34, 86)),
        },
    }
}
/// The MIDI MESSAGE lamp, dark and lit. It is a lamp behind tinted plastic
/// rather than part of the display, so it keeps its own green whichever
/// glass the panel is wearing; unlit it is barely green at all.
const LED_DARK: u32 = rgba(9, 20, 11);
const LED_LIT: u32 = rgba(52, 206, 74);
/// A button being held down: darker than its own face, near enough the
/// fascia that it reads as having gone into the panel rather than changed
/// colour. Unlit buttons are the status bar's, and so is the dial -- the
/// same moulding, turned instead of pressed.
const BUTTON_FACE_PRESSED: u32 = rgba(30, 30, 28);
/// How far a button lifts under the pointer: within a shade of the step the
/// status bar's own buttons take, so a hover reads the same wherever on the
/// window it happens.
const HOVER_LIFT: f32 = 16.0;
const DIAL_MARK: u32 = rgba(200, 202, 208);
/// How far the dome lifts the top of the dial's face above the bottom.
/// Small on purpose: enough to round it, not enough to read as a sphere.
const DIAL_DOME: f32 = 14.0;
/// How much the moulding just inside the rim catches the same light.
const DIAL_SHOULDER: f32 = 34.0;
/// Positions printed on the fascia around the dial, the first at seven
/// o'clock and the last at five, as the unit marks its travel.
const DIAL_MARKS: usize = 24;
/// How far outside the rim they sit, and what they are printed in: dimmer
/// than the captions, so they read as marks on the moulding rather than as
/// another row of labels.
const DIAL_MARK_GAP: f32 = 3.0;
const DIAL_TICK: u32 = rgba(96, 94, 86);

const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

/// The same colour lifted towards the light (positive) or turned away from
/// it (negative), which is all the shading on the dial's face amounts to.
fn shade(colour: u32, by: f32) -> u32 {
    let [r, g, b, a] = colour.to_le_bytes();
    let step = |c: u8| (f32::from(c) + by).clamp(0.0, 255.0) as u8;
    u32::from_le_bytes([step(r), step(g), step(b), a])
}

/// `from` at 0, `to` at 1. The dial's rim runs between the two edge colours
/// this way rather than switching between them, so the turn reads as round
/// instead of as two arcs meeting at a corner.
fn mix(from: u32, to: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (from.to_le_bytes(), to.to_le_bytes());
    let lerp = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t) as u8;
    u32::from_le_bytes([lerp(0), lerp(1), lerp(2), a[3]])
}

/// Where the dial's travel begins and how far it turns: from seven o'clock,
/// clockwise round the top, to five o'clock. A value at its maximum -- full
/// volume, say -- stands at the end of that travel.
const DIAL_START: f32 = std::f32::consts::PI * 2.0 / 3.0;
const DIAL_SWEEP: f32 = std::f32::consts::PI * 5.0 / 3.0;

/// The angle the pointer stands at for `value`, a fraction of the travel.
fn dial_angle(value: f32) -> f32 {
    DIAL_START + value.clamp(0.0, 1.0) * DIAL_SWEEP
}

/// What the panel needs to draw itself: the display as the engine reports it,
/// and which controls are lit.
#[derive(Debug, Clone, Default)]
pub struct Mt32PanelView {
    /// The 20 characters on the LCD.
    pub lcd: String,
    /// Whether the MIDI MESSAGE lamp is lit.
    pub led: bool,
    /// Which part buttons are lit, and which function buttons. The unit has
    /// no lamps in its buttons; these say which ones the panel is standing
    /// on, so a two- or three-button function shows all of them at once.
    pub parts_lit: [bool; 6],
    pub functions_lit: [bool; 4],
    /// Where the dial stands, as a fraction of its travel: 0 at seven
    /// o'clock, 1 at five.
    pub dial: f32,
    /// Whether the synth is switched on. Off leaves the fascia drawn and
    /// dark, as an unpowered unit looks.
    pub powered: bool,
    /// What the pointer is over, so that control can answer to it. The
    /// power switch and the dial are left out: one is a switch rather than
    /// a button, and the other already follows the pointer.
    pub hover: Option<Mt32Control>,
}

/// What holding MASTER VOLUME and pressing another button reaches.
///
/// Every one of the MT-32's two-button functions is MASTER VOLUME plus one
/// other, so the panel offers them by right-clicking MASTER VOLUME to hold
/// it and then pressing the other button.
///
/// From the owner's manual, pages 15-17. One correction: the manual's text
/// for Reverb Mode says SOUND GROUP, but its own diagram arrows at VOLUME,
/// and SOUND GROUP already carries Master Tuning on the page above. The
/// diagram is followed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chord {
    /// + SOUND GROUP: 427.5 to 452.6 Hz.
    MasterTune,
    /// + VOLUME: reverb mode.
    ReverbMode,
    /// + SOUND: the unit number a SysEx message addresses.
    UnitNumber,
    /// + PART 1/2/3: parts 6, 7 and 8, which have no buttons of their own.
    ShowPart(usize),
    /// + PART 4, then PART 1: spill notes past the unit's capacity out of
    ///   MIDI OUT.
    OverflowAssign,
    /// + PART 5, then PART 1: move parts 1-8 down onto channels 1-8.
    MidiChannels,
    /// + PART RHYTHM, then PART 1: back to the power-on settings.
    AllReset,
}

impl Chord {
    /// Whether this one waits for a further press before it takes effect,
    /// as the manual's three-button procedures do.
    fn needs_confirming(self) -> bool {
        matches!(
            self,
            Chord::OverflowAssign | Chord::MidiChannels | Chord::AllReset
        )
    }

    /// The button held with MASTER VOLUME to reach it.
    fn partner(self) -> Mt32Control {
        match self {
            Chord::MasterTune => Mt32Control::Function(Function::SoundGroup),
            Chord::ReverbMode => Mt32Control::Function(Function::Volume),
            Chord::UnitNumber => Mt32Control::Function(Function::Sound),
            Chord::ShowPart(part) => Mt32Control::Part(part - 5),
            Chord::OverflowAssign => Mt32Control::Part(3),
            Chord::MidiChannels => Mt32Control::Part(4),
            Chord::AllReset => Mt32Control::Part(5),
        }
    }
}

/// The four buttons to the right of the part block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    SoundGroup,
    Volume,
    Sound,
    MasterVolume,
}

impl Function {
    /// Its slot in the panel's lit table.
    pub fn index(self) -> usize {
        match self {
            Function::SoundGroup => 0,
            Function::Volume => 1,
            Function::Sound => 2,
            Function::MasterVolume => 3,
        }
    }
}

/// Something on the panel the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mt32Control {
    /// A part button, 0-5 (1-5 then Rhythm).
    Part(usize),
    Function(Function),
    /// The dial, turned by dragging across it or clicking either button.
    Dial,
    /// The power switch, which is Copperline's, not the unit's: the hardware
    /// has its at the back, and a panel on screen needs one to hand.
    Power,
}

// --- geometry ------------------------------------------------------------
//
// Left to right, as on the unit: LCD, lamp, six part buttons, four function
// buttons, dial. Everything is measured from the panel's own top-left so the
// whole block can be moved by moving `panel`.

const PAD: usize = 8;
/// The LCD's characters are drawn double so a 20-character line reads as a
/// display rather than a caption.
const LCD_PX: usize = 2;
/// How far the bezel around the glass is inset, so what sits on the glass
/// is placed against the glass rather than against the bezel.
const LCD_BEZEL: usize = 2;
/// The unlit matrix behind each character, in the character's own cell: the
/// columns and rows a character can light -- the last column of the cell is
/// the gap to the next one -- and the shorter block below it, as wide as
/// the matrix itself, standing in for the row the controller keeps for a
/// cursor.
const CELL_COLS: usize = font::GLYPH_W - 1;
const CELL_ROWS: usize = 7;
const CELL_FOOT_GAP: usize = 1;
const CELL_FOOT_H: usize = 2;
/// How far the rule between one dot and the next falls back towards the
/// field behind the matrix. The dots are only two pixels across, so this
/// has to stay small or the grid reads louder than the characters over it.
const CELL_GRAIN: f32 = 0.35;
/// The same, for the dots a character is made of. Smaller than the matrix's
/// own: the grid should be plain on the glass and only felt in the writing.
const TEXT_GRAIN: f32 = 0.16;
const LCD_W: usize = crate::mt32::LCD_WIDTH * font::GLYPH_W * LCD_PX + 12;
const LCD_H: usize = 27;
const LED_W: usize = 22;
const LED_H: usize = 8;
/// What is printed over the lamp, and the column it needs: as wide as the
/// caption, so the text clears the glass on its left and the buttons on its
/// right.
const LED_CAPTION: &str = "MIDI";
const LED_GROUP_W: usize = 4 * font::GLYPH_W;
const SQUARE_W: usize = 24;
const SQUARE_H: usize = 18;
const RECT_W: usize = 62;
const RECT_H: usize = 18;
const ROW_GAP: usize = 4;
const COL_GAP: usize = 4;
const GROUP_GAP: usize = 14;
const DIAL_D: usize = 36;
/// What is printed over the dial.
const DIAL_CAPTION: &str = "SEL/VOL";
/// The power switch: about half a part button, in the bottom corner.
const POWER_W: usize = 14;
const POWER_H: usize = 12;
/// Rows of buttons sit under a caption line, so the block starts below it.
const CAPTION_H: usize = 8;
/// How far the rule under PART stops short of the word at either end.
const RULE_GAP: usize = 3;

/// The panel's rect when it is actually up, and `None` when it is not.
///
/// Hit-testing must go through this: with the panel hidden its strip is the
/// status bar's, and testing it anyway swallows every click on the bar.
pub fn shown_panel_rect(present_h: usize) -> Option<Rect> {
    crate::video::mt32_panel_shown().then(|| panel_rect(present_h))
}

/// The panel's rect within the presentation, sitting between the display and
/// the status bar.
pub fn panel_rect(present_h: usize) -> Rect {
    Rect {
        x: 0,
        y: present_h,
        w: crate::video::FB_WIDTH,
        h: MT32_PANEL_HEIGHT,
    }
}

fn lcd_rect(panel: Rect) -> Rect {
    Rect {
        x: panel.x + PAD,
        y: panel.y + (panel.h - LCD_H) / 2,
        w: LCD_W,
        h: LCD_H,
    }
}

/// The left edge of the lamp's column.
fn led_group_x(panel: Rect) -> usize {
    let lcd = lcd_rect(panel);
    lcd.x + lcd.w + GROUP_GAP
}

fn led_rect(panel: Rect) -> Rect {
    let lcd = lcd_rect(panel);
    Rect {
        // Centred on the caption's ink rather than its cell box, which is
        // wider than the letters in it.
        x: (led_group_x(panel) + text_ink_centre(LED_CAPTION, 1)).saturating_sub(LED_W / 2),
        y: lcd.y + lcd.h / 2 - LED_H / 2 + 5,
        w: LED_W,
        h: LED_H,
    }
}

/// Where the six part buttons start: past the lamp and its caption.
fn parts_origin(panel: Rect) -> (usize, usize) {
    let x = led_group_x(panel) + LED_GROUP_W + GROUP_GAP;
    let block_h = CAPTION_H + 2 * SQUARE_H + ROW_GAP;
    // A hair below centre, so the captions above the block clear the edge.
    (x, panel.y + (panel.h - block_h) / 2 + 3)
}

/// The rect of part button `n` (0-5): three across, two down, 1-3 over 4-R.
fn part_rect(panel: Rect, n: usize) -> Rect {
    let (x0, y0) = parts_origin(panel);
    let (col, row) = (n % 3, n / 3);
    Rect {
        x: x0 + col * (SQUARE_W + COL_GAP),
        y: y0 + CAPTION_H + row * (SQUARE_H + ROW_GAP),
        w: SQUARE_W,
        h: SQUARE_H,
    }
}

/// The rect of function button `f`: two across, two down.
fn function_rect(panel: Rect, f: Function) -> Rect {
    let (x0, y0) = parts_origin(panel);
    let x = x0 + 3 * (SQUARE_W + COL_GAP) + GROUP_GAP;
    let (col, row) = match f {
        Function::SoundGroup => (0, 0),
        Function::Volume => (1, 0),
        Function::Sound => (0, 1),
        Function::MasterVolume => (1, 1),
    };
    Rect {
        x: x + col * (RECT_W + COL_GAP),
        y: y0 + CAPTION_H + row * (RECT_H + ROW_GAP),
        w: RECT_W,
        h: RECT_H,
    }
}

/// The dial, at the right of the block.
fn dial_rect(panel: Rect) -> Rect {
    // Midway between the buttons on its left and the power switch on its
    // right, so it sits in the gap rather than against one side of it.
    let volume = function_rect(panel, Function::Volume);
    let block_right = volume.x + volume.w;
    let gap = power_rect(panel).x.saturating_sub(block_right);
    // Sitting on the block of buttons rather than on the panel, so the
    // caption above it has the same room to breathe as PART does over
    // theirs, and the dial still finishes clear of the bottom edge.
    let top = part_rect(panel, 0).y;
    let bottom = part_rect(panel, 3).y + SQUARE_H;
    Rect {
        x: block_right + gap.saturating_sub(DIAL_D) / 2,
        y: (top + bottom) / 2 - DIAL_D / 2,
        w: DIAL_D,
        h: DIAL_D,
    }
}

/// Where `pos` stands along the dial's travel, 0 at seven o'clock and 1 at
/// five, or `None` when it is in the gap between the two ends.
fn dial_value_at(panel: Rect, pos: (i32, i32)) -> Option<f32> {
    let dial = dial_rect(panel);
    let cx = dial.x as f32 + dial.w as f32 / 2.0;
    let cy = dial.y as f32 + dial.h as f32 / 2.0;
    let angle = (pos.1 as f32 - cy).atan2(pos.0 as f32 - cx);
    // Measured from the start of the travel, going the way it turns.
    let from_start = (angle - DIAL_START).rem_euclid(std::f32::consts::TAU);
    (from_start <= DIAL_SWEEP).then(|| from_start / DIAL_SWEEP)
}

/// What holding MASTER VOLUME and pressing `control` reaches, if anything.
fn chord_for(control: Mt32Control) -> Option<Chord> {
    Some(match control {
        Mt32Control::Function(Function::SoundGroup) => Chord::MasterTune,
        Mt32Control::Function(Function::Volume) => Chord::ReverbMode,
        Mt32Control::Function(Function::Sound) => Chord::UnitNumber,
        // Buttons 1, 2 and 3 stand for parts 6, 7 and 8; the unit has nine
        // parts and six buttons.
        Mt32Control::Part(n @ 0..=2) => Chord::ShowPart(n + 5),
        Mt32Control::Part(3) => Chord::OverflowAssign,
        Mt32Control::Part(4) => Chord::MidiChannels,
        Mt32Control::Part(5) => Chord::AllReset,
        _ => return None,
    })
}

/// The power switch, in the bottom right corner past the dial.
fn power_rect(panel: Rect) -> Rect {
    Rect {
        x: panel.x + panel.w - POWER_W - PAD,
        y: panel.y + panel.h - POWER_H - 6,
        w: POWER_W,
        h: POWER_H,
    }
}

/// Which control the pointer is over, if any.
pub fn control_at(panel: Rect, pos: (i32, i32)) -> Option<Mt32Control> {
    for n in 0..6 {
        if part_rect(panel, n).contains(pos) {
            return Some(Mt32Control::Part(n));
        }
    }
    for f in [
        Function::SoundGroup,
        Function::Volume,
        Function::Sound,
        Function::MasterVolume,
    ] {
        if function_rect(panel, f).contains(pos) {
            return Some(Mt32Control::Function(f));
        }
    }
    if power_rect(panel).contains(pos) {
        return Some(Mt32Control::Power);
    }
    dial_rect(panel).contains(pos).then_some(Mt32Control::Dial)
}

/// What the pointer is over, of the controls that answer to being hovered.
///
/// The switch latches with the power and the dial already follows the hand,
/// so neither takes a highlight -- which also means neither is worth a
/// redraw when the pointer crosses it.
pub fn hover_at(panel: Rect, pos: (i32, i32)) -> Option<Mt32Control> {
    control_at(panel, pos).filter(|c| matches!(c, Mt32Control::Part(_) | Mt32Control::Function(_)))
}

/// Whether moving from `previous` to `current` changed which button is lit
/// under the pointer, and so needs the panel drawn again.
pub fn hover_changed(
    panel: Rect,
    previous: Option<(i32, i32)>,
    current: Option<(i32, i32)>,
) -> bool {
    previous.and_then(|pos| hover_at(panel, pos)) != current.and_then(|pos| hover_at(panel, pos))
}

// --- drawing -------------------------------------------------------------

/// Draw the panel.
pub fn draw(frame: &mut [u8], view: &Mt32PanelView, present_h: usize, scale: usize) {
    let panel = panel_rect(present_h);
    fill_rect(frame, scaled(panel, scale), PANEL_FACE, scale);
    draw_rect_bevel(
        frame,
        scaled(panel, scale),
        PANEL_EDGE_LIGHT,
        PANEL_EDGE_DARK,
        scale,
    );

    draw_lcd_surround(frame, panel, scale);
    draw_lcd(frame, panel, view, scale);
    draw_led(frame, panel, view.led, scale);
    draw_parts(frame, panel, view, scale);
    draw_functions(frame, panel, view, scale);
    draw_dial(frame, panel, view.dial, scale);
    draw_power(frame, panel, view.powered, scale);
}

/// The power switch: the same mark the status bar uses, green lit and red
/// dark, on a button small enough not to take room from the instrument.
fn draw_power(frame: &mut [u8], panel: Rect, powered: bool, scale: usize) {
    let rect = power_rect(panel);
    // Latching, as the unit's is: in while it is on, out while it is off.
    draw_button(frame, rect, powered, false, scale);
    draw_power_glyph_sized(
        frame,
        (rect.x + rect.w / 2) * scale,
        (rect.y + rect.h / 2) * scale - scale,
        3.5,
        if powered {
            POWER_GLYPH_ON
        } else {
            POWER_GLYPH_OFF
        },
        scale,
    );
}

/// The gloss the display and its lamp are set into, squared off against the
/// block of buttons beside it: the bottom on the underside of their lower
/// row, the left edge set in by what the caption line leaves at the top,
/// and the top far enough up that the glass sits in the same depth of gloss
/// either way.
fn lcd_surround_rect(panel: Rect) -> Rect {
    let (right, caption) = (parts_origin(panel).0 - GROUP_GAP / 2, caption_y(panel));
    let bottom = part_rect(panel, 5).y + SQUARE_H;
    let left = panel.x + caption.saturating_sub(panel.y);
    // The bottom is squared off against the buttons, which leaves it deeper
    // below the glass than the caption line leaves above it. Take the top up
    // by the difference so the glass sits in the same depth of gloss either
    // way, without moving the glass or anything around it.
    let lcd = lcd_rect(panel);
    let below = bottom.saturating_sub(lcd.y + lcd.h);
    let top = lcd.y.saturating_sub(below).min(caption);
    Rect {
        x: left,
        y: top,
        w: right.saturating_sub(left),
        h: bottom.saturating_sub(top),
    }
}

/// Paint it, with a sheen along the top to read as polished rather than
/// matt, as the unit's is.
fn draw_lcd_surround(frame: &mut [u8], panel: Rect, scale: usize) {
    let rect = lcd_surround_rect(panel);
    fill_rect(frame, scaled(rect, scale), LCD_SURROUND, scale);
    fill_rect(
        frame,
        scaled(
            Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: 1,
            },
            scale,
        ),
        LCD_SURROUND_SHEEN,
        scale,
    );
}

fn draw_lcd(frame: &mut [u8], panel: Rect, view: &Mt32PanelView, scale: usize) {
    // The same raised display the status bar's track counter uses: a lit
    // top-left edge over a dark surround, with the glass inset inside it.
    let colours = lcd_colours();
    let lcd = lcd_rect(panel);
    let glass = if view.powered {
        colours.glass
    } else {
        colours.dark_glass
    };
    raised_display(frame, lcd, glass, scale);
    // With the backlight out there is nothing behind the glass to see, not
    // even the matrix.
    if !view.powered {
        return;
    }
    // Centred on the glass rather than on the bezel around it, and on the
    // whole matrix rather than on the characters alone: the cursor row
    // below them belongs to the same object, and measuring without it
    // leaves the line sitting low.
    let glass_w = lcd.w - 2 * LCD_BEZEL;
    let glass_h = lcd.h - 2 * LCD_BEZEL;
    let matrix_w = (crate::mt32::LCD_WIDTH - 1) * font::GLYPH_W * LCD_PX + CELL_COLS * LCD_PX;
    let matrix_h = CELL_ROWS * LCD_PX + CELL_FOOT_GAP + CELL_FOOT_H;
    let tx = lcd.x + LCD_BEZEL + glass_w.saturating_sub(matrix_w) / 2;
    let ty = lcd.y + LCD_BEZEL + glass_h.saturating_sub(matrix_h) / 2;
    // The engine gives 20 characters; anything longer is its own business.
    for (i, ch) in view.lcd.chars().take(crate::mt32::LCD_WIDTH).enumerate() {
        let x = tx + i * font::GLYPH_W * LCD_PX;
        let cell = Rect {
            x,
            y: ty,
            w: CELL_COLS * LCD_PX,
            h: CELL_ROWS * LCD_PX,
        };
        // Every position shows its own matrix whether anything is lit in it
        // or not, with the row the controller keeps for a cursor sitting
        // under it -- which is what gives an LCD its grid of faint blocks.
        if let Some(unlit) = colours.cell {
            let foot = Rect {
                y: cell.y + cell.h + CELL_FOOT_GAP,
                h: CELL_FOOT_H,
                ..cell
            };
            fill_rect(frame, scaled(cell, scale), unlit, scale);
            fill_rect(frame, scaled(foot, scale), unlit, scale);
            // Ruled into its dots, a shade under the block itself. Barely
            // a difference at all: enough that the matrix has a grain, not
            // enough to read as lines drawn on it.
            let grain = mix(unlit, glass, CELL_GRAIN);
            for block in [cell, foot] {
                for row in 0..block.h / LCD_PX {
                    fill_rect(
                        frame,
                        scaled(
                            Rect {
                                y: block.y + row * LCD_PX + LCD_PX - 1,
                                h: 1,
                                ..block
                            },
                            scale,
                        ),
                        grain,
                        scale,
                    );
                }
                for col in 0..block.w / LCD_PX {
                    fill_rect(
                        frame,
                        scaled(
                            Rect {
                                x: block.x + col * LCD_PX + LCD_PX - 1,
                                w: 1,
                                ..block
                            },
                            scale,
                        ),
                        grain,
                        scale,
                    );
                }
            }
        }
        let solid = ch == crate::mt32::ACTIVE_PART;
        if solid {
            // A part with something sounding on it: every dot in the cell
            // driven at once, which is the shape the controller is given.
            fill_rect(frame, scaled(cell, scale), colours.text, scale);
        } else {
            text(
                frame,
                x,
                ty,
                ch.encode_utf8(&mut [0; 4]),
                colours.text,
                LCD_PX,
                scale,
            );
        }
        // The characters are made of the same dots as the matrix behind
        // them, so rule them the same way -- but more faintly. The grid
        // belongs to the glass; in the writing it should only be felt.
        if colours.cell.is_some() {
            let ink_grain = mix(colours.text, glass, TEXT_GRAIN);
            let glyph = font::glyph(ch);
            for row in 0..CELL_ROWS {
                for col in 0..CELL_COLS {
                    if !solid && glyph[row] & (1 << col) == 0 {
                        continue;
                    }
                    let (dx, dy) = (cell.x + col * LCD_PX, cell.y + row * LCD_PX);
                    // The dot's own far edges, which is where its neighbour
                    // begins.
                    for edge in [
                        Rect {
                            x: dx,
                            y: dy + LCD_PX - 1,
                            w: LCD_PX,
                            h: 1,
                        },
                        Rect {
                            x: dx + LCD_PX - 1,
                            y: dy,
                            w: 1,
                            h: LCD_PX,
                        },
                    ] {
                        fill_rect(frame, scaled(edge, scale), ink_grain, scale);
                    }
                }
            }
        }
    }
}

fn draw_led(frame: &mut [u8], panel: Rect, lit: bool, scale: usize) {
    let led = led_rect(panel);
    raised_display(frame, led, if lit { LED_LIT } else { LED_DARK }, scale);
    // Printed above the lamp as it is on the fascia, at the head of the
    // column; the lamp is centred under its ink (see led_rect).
    text(
        frame,
        led_group_x(panel),
        led.y.saturating_sub(13),
        LED_CAPTION,
        CAPTION,
        1,
        scale,
    );
}

fn draw_parts(frame: &mut [u8], panel: Rect, view: &Mt32PanelView, scale: usize) {
    let x0 = parts_origin(panel).0;
    // The bracket the six buttons sit under, as it is printed on the fascia:
    // the word underlined out to the edges of the block. The rule sits on
    // the row the letters' own feet stand on, and stops short of their ink
    // by the same margin at each end -- measured from the ink, so the gap
    // beside the T's crossbar matches the one beside the P's stem rather
    // than opening up under the narrower part of the letter.
    let block_w = 3 * SQUARE_W + 2 * COL_GAP;
    let y = caption_y(panel);
    let label_w = text_w("PART", 1);
    let label_x = x0 + block_w / 2 - label_w / 2;
    text(frame, label_x, y, "PART", CAPTION, 1, scale);
    let (ink_lo, ink_hi) = text_ink_span("PART", 1);
    let (word_lo, word_hi) = (label_x + ink_lo, label_x + ink_hi);
    let rule_y = y + text_ink_feet("PART", 1);
    let (rule_lo, rule_hi) = (word_lo.saturating_sub(RULE_GAP), word_hi + RULE_GAP);
    for (rx, rw) in [
        (x0, rule_lo.saturating_sub(x0)),
        (rule_hi, (x0 + block_w).saturating_sub(rule_hi)),
    ] {
        fill_rect(
            frame,
            scaled(
                Rect {
                    x: rx,
                    y: rule_y,
                    w: rw,
                    h: 1,
                },
                scale,
            ),
            CAPTION,
            scale,
        );
    }
    for (n, label) in ["1", "2", "3", "4", "5", "R"].into_iter().enumerate() {
        let rect = part_rect(panel, n);
        let hovered = view.hover == Some(Mt32Control::Part(n));
        draw_button(frame, rect, view.parts_lit[n], hovered, scale);
        let tx = rect.x + rect.w / 2 - text_w(label, 1) / 2;
        let ty = rect.y + (rect.h - font::GLYPH_H) / 2;
        text(frame, tx, ty, label, CAPTION, 1, scale);
    }
}

fn draw_functions(frame: &mut [u8], panel: Rect, view: &Mt32PanelView, scale: usize) {
    for (f, label) in [
        (Function::SoundGroup, "GROUP"),
        (Function::Volume, "VOLUME"),
        (Function::Sound, "SOUND"),
        (Function::MasterVolume, "MASTER"),
    ] {
        let rect = function_rect(panel, f);
        let hovered = view.hover == Some(Mt32Control::Function(f));
        draw_button(frame, rect, view.functions_lit[f.index()], hovered, scale);
        let tx = rect.x + rect.w / 2 - text_w(label, 1) / 2;
        let ty = rect.y + (rect.h - font::GLYPH_H) / 2;
        text(frame, tx, ty, label, CAPTION, 1, scale);
    }
}

/// A recessed-bezel display with `face` behind the glass: the treatment the
/// status bar's track counter and volume track already use, so the panel's
/// LCD and lamp read as the same kind of object.
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

fn draw_button(frame: &mut [u8], rect: Rect, pressed: bool, hovered: bool, scale: usize) {
    let rect = scaled(rect, scale);
    // Pressed, the moulding turns over: the light that caught its top and
    // left now falls past it onto the bottom and right, and the face sits
    // in its own shadow. Standing proud, the bevel is the way round every
    // other button on the panel wears it.
    let (face, near, far) = if pressed {
        (BUTTON_FACE_PRESSED, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT)
    } else {
        (BUTTON_FACE, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK)
    };
    // Under the pointer it lifts, by what the status bar's own buttons lift
    // by, so the two answer the same way.
    let face = if hovered {
        shade(face, HOVER_LIFT)
    } else {
        face
    };
    fill_rect(frame, rect, face, scale);
    draw_rect_bevel(frame, rect, near, far, scale);
}

/// The Select/Volume dial: a round face, its detents marked along the
/// travel, and a pointer showing where it stands.
fn draw_dial(frame: &mut [u8], panel: Rect, value: f32, scale: usize) {
    let dial = dial_rect(panel);
    let (cx, cy) = (
        dial.x as f32 + dial.w as f32 / 2.0,
        dial.y as f32 + dial.h as f32 / 2.0,
    );
    let radius = dial.w as f32 / 2.0;

    // The face, as a disc of rows, domed by lifting the top towards the
    // light and letting the bottom fall away from it. The light comes from
    // the top left, as it does on the bevel around every button beside it.
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
        let dome = shade(BUTTON_FACE, -dy / radius * DIAL_DOME);
        fill_rect(frame, scaled(row, scale), dome, scale);
    }

    // The rim, and the shoulder just inside it. Both run between the two
    // edge colours the buttons are bevelled with, by how squarely that part
    // of the turn faces the light, so the ring shades round rather than
    // meeting itself at a corner. Stepped finely enough that it closes.
    const RIM_STEPS: usize = 256;
    for i in 0..RIM_STEPS {
        let angle = i as f32 / RIM_STEPS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        // 1 where the rim turns fully into the light, 0 fully away.
        let facing = 0.5 - (cos + sin) * std::f32::consts::FRAC_1_SQRT_2 * 0.5;
        let plot = |frame: &mut [u8], at: f32, colour: u32| {
            let dot = Rect {
                x: (cx + cos * at) as usize,
                y: (cy + sin * at) as usize,
                w: 1,
                h: 1,
            };
            fill_rect(frame, scaled(dot, scale), colour, scale);
        };
        plot(
            frame,
            radius - 0.5,
            mix(BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT, facing),
        );
        // Inside it the moulding turns back towards the face, so the same
        // light reaches it faintly: enough to round the edge off.
        plot(
            frame,
            radius - 1.5,
            shade(BUTTON_FACE, (facing - 0.5) * DIAL_SHOULDER),
        );
    }
    // The travel marked out around it, the way the unit prints it on the
    // fascia: the first mark at seven o'clock, the last at five, and the
    // pointer standing against them.
    for i in 0..DIAL_MARKS {
        let (sin, cos) = dial_angle(i as f32 / (DIAL_MARKS - 1) as f32).sin_cos();
        let dot = Rect {
            x: (cx + cos * (radius + DIAL_MARK_GAP)) as usize,
            y: (cy + sin * (radius + DIAL_MARK_GAP)) as usize,
            w: 1,
            h: 1,
        };
        fill_rect(frame, scaled(dot, scale), DIAL_TICK, scale);
    }

    // The pointer: a mark from half way out to just inside the rim,
    // stepped in half pixels so it reads as a line rather than a dotted one.
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
        fill_rect(frame, scaled(dot, scale), DIAL_MARK, scale);
        r += 0.5;
    }

    text(
        frame,
        (dial.x + dial.w / 2).saturating_sub(text_ink_centre(DIAL_CAPTION, 1)),
        caption_y(panel),
        DIAL_CAPTION,
        CAPTION,
        1,
        scale,
    );
}

/// The line the block captions sit on, so PART and SEL/VOL read as one row
/// of labels across the fascia rather than two.
fn caption_y(panel: Rect) -> usize {
    let top = parts_origin(panel).1 + CAPTION_H;
    panel.y + (top - panel.y).saturating_sub(font::GLYPH_H) / 2
}

/// Text in the panel font, at `px` whole pixels per font pixel. Coordinates
/// are the panel's own, so callers work in one space.
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

/// How wide `s` is at `px`, counting the cells it occupies.
fn text_w(s: &str, px: usize) -> usize {
    s.chars().count() * font::GLYPH_W * px
}

/// Where the ink of `s` begins and ends, measured from its origin.
///
/// A glyph cell is wider than the mark inside it, so anything butted or
/// centred on the cell box sits visibly off. This looks at which columns
/// are actually set and works from those instead.
fn text_ink_span(s: &str, px: usize) -> (usize, usize) {
    let (mut lo, mut hi) = (usize::MAX, 0);
    for (cell, ch) in s.chars().enumerate() {
        let glyph = font::glyph(ch);
        for col in 0..font::GLYPH_W {
            if glyph.iter().any(|row| row & (1 << col) != 0) {
                let x = cell * font::GLYPH_W + col;
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    if lo == usize::MAX {
        return (0, text_w(s, px));
    }
    (lo * px, (hi + 1) * px)
}

/// Where the ink of `s` is centred, measured from its origin.
fn text_ink_centre(s: &str, px: usize) -> usize {
    let (lo, hi) = text_ink_span(s, px);
    (lo + hi) / 2
}

/// The row the letters of `s` stand on, measured from their origin: where a
/// rule has to sit to run on from their feet rather than through them or
/// below them.
fn text_ink_feet(s: &str, px: usize) -> usize {
    s.chars()
        .filter_map(|ch| {
            let glyph = font::glyph(ch);
            (0..font::GLYPH_H).rev().find(|&row| glyph[row] != 0)
        })
        .max()
        .unwrap_or(font::GLYPH_H - 1)
        * px
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

    /// Everything fits inside the panel, in the order the unit has it:
    /// LCD, lamp, parts, functions, dial.
    #[test]
    fn the_panel_lays_out_left_to_right_and_fits() {
        let panel = panel_rect(500);
        let lcd = lcd_rect(panel);
        let led = led_rect(panel);
        let first_part = part_rect(panel, 0);
        let first_fn = function_rect(panel, Function::SoundGroup);
        let dial = dial_rect(panel);

        assert!(lcd.x + lcd.w <= led.x, "the lamp sits right of the LCD");
        assert!(
            led.x + led.w <= first_part.x,
            "the parts sit right of the lamp"
        );
        assert!(
            part_rect(panel, 2).x + SQUARE_W <= first_fn.x,
            "the functions sit right of the parts"
        );
        assert!(
            function_rect(panel, Function::Volume).x + RECT_W <= dial.x,
            "the dial sits right of the functions"
        );
        assert!(
            dial.x + dial.w <= panel.x + panel.w,
            "the whole block fits the panel width"
        );

        for n in 0..6 {
            let r = part_rect(panel, n);
            assert!(r.y >= panel.y && r.y + r.h <= panel.y + panel.h, "part {n}");
        }
        assert!(dial.y >= panel.y && dial.y + dial.h <= panel.y + panel.h);
    }

    /// Renders the panel to `target/ui-preview-mt32-panel.png` under
    /// COPPERLINE_UI_PREVIEW, for looking at while the design settles.
    #[test]
    fn the_panel_draws_itself() {
        use crate::video::window::{present_height, texture_height, texture_width};

        // The drawing helpers clamp against the presentation texture, so the
        // panel is drawn where it really sits and the strip cropped out.
        crate::video::set_mt32_panel_shown(true);
        let scale = 2;
        let (w, h) = (texture_width(scale), texture_height(scale));
        let present_h = present_height();
        let panel = panel_rect(present_h);
        let (px, py) = (panel.x * scale, panel.y * scale);
        let (pw, ph) = (panel.w * scale, panel.h * scale);

        for (style, name) in [
            (crate::config::Mt32Lcd::Oled, "oled"),
            (crate::config::Mt32Lcd::Mt32, "mt32"),
            (crate::config::Mt32Lcd::Jv1080, "jv1080"),
        ] {
            crate::video::set_mt32_lcd(style);
            // Once lit, once not: the styles differ in how they go dark.
            for powered in [true, false] {
                let mut frame = vec![0u8; w * h * 4];
                let view = Mt32PanelView {
                    // Parts 1 and 4 sounding, so the preview shows the
                    // filled cell the engine asks for as well as the
                    // characters.
                    lcd: format!("{a} 2 3 {a} 5 R |vol:100", a = crate::mt32::ACTIVE_PART),
                    led: powered,
                    parts_lit: [true, false, false, false, false, false],
                    functions_lit: [false, false, false, true],
                    dial: 1.0,
                    powered,
                    // Part 2 under the pointer, so the preview shows a
                    // hover beside a press and a button at rest.
                    hover: Some(Mt32Control::Part(1)),
                };
                draw(&mut frame, &view, present_h, scale);

                let mut strip = Vec::with_capacity(pw * ph * 4);
                for row in 0..ph {
                    let start = ((py + row) * w + px) * 4;
                    strip.extend_from_slice(&frame[start..start + pw * 4]);
                }
                // The fascia was painted, rather than the strip staying
                // transparent.
                assert_ne!(&strip[0..4], &[0, 0, 0, 0], "{name} should be drawn");

                if !crate::envcfg::flag("COPPERLINE_UI_PREVIEW") {
                    continue;
                }
                let suffix = if powered { "" } else { "-off" };
                let path = format!("target/ui-preview-mt32-panel-{name}{suffix}.png");
                let file = std::fs::File::create(&path).unwrap();
                let mut enc =
                    png::Encoder::new(std::io::BufWriter::new(file), pw as u32, ph as u32);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header()
                    .unwrap()
                    .write_image_data(&strip)
                    .unwrap();
                eprintln!("saved {path}");
            }
        }
        crate::video::set_mt32_lcd(crate::config::Mt32Lcd::Oled);
        crate::video::set_mt32_panel_shown(false);
    }

    /// The two-button functions are the ones the manual documents, each on
    /// the button MASTER VOLUME is held with.
    #[test]
    fn the_chords_are_the_ones_the_manual_documents() {
        assert_eq!(
            chord_for(Mt32Control::Function(Function::SoundGroup)),
            Some(Chord::MasterTune)
        );
        assert_eq!(
            chord_for(Mt32Control::Function(Function::Volume)),
            Some(Chord::ReverbMode)
        );
        assert_eq!(
            chord_for(Mt32Control::Function(Function::Sound)),
            Some(Chord::UnitNumber)
        );
        assert_eq!(chord_for(Mt32Control::Part(5)), Some(Chord::AllReset));

        // Buttons 1-3 reach parts 6-8, which have no buttons of their own.
        for (button, part) in [(0, 5), (1, 6), (2, 7)] {
            assert_eq!(
                chord_for(Mt32Control::Part(button)),
                Some(Chord::ShowPart(part))
            );
        }
        // The three that want a further press before they do anything.
        assert_eq!(chord_for(Mt32Control::Part(3)), Some(Chord::OverflowAssign));
        assert_eq!(chord_for(Mt32Control::Part(4)), Some(Chord::MidiChannels));
        for chord in [Chord::OverflowAssign, Chord::MidiChannels, Chord::AllReset] {
            assert!(chord.needs_confirming(), "{chord:?}");
        }
        for chord in [Chord::MasterTune, Chord::ReverbMode, Chord::UnitNumber] {
            assert!(!chord.needs_confirming(), "{chord:?}");
        }

        // MASTER VOLUME is the button being held, not one to hold it with.
        assert_eq!(
            chord_for(Mt32Control::Function(Function::MasterVolume)),
            None
        );
        assert_eq!(chord_for(Mt32Control::Power), None);

        // Every chord names the button it is reached through, and that
        // button leads back to it.
        for chord in [
            Chord::MasterTune,
            Chord::ReverbMode,
            Chord::UnitNumber,
            Chord::ShowPart(5),
            Chord::OverflowAssign,
            Chord::MidiChannels,
            Chord::AllReset,
        ] {
            assert_eq!(chord_for(chord.partner()), Some(chord), "{chord:?}");
        }
    }

    /// A pair is two buttons held together, so it takes two right clicks
    /// and starts with MASTER VOLUME. Nothing else can stumble into one.
    #[test]
    fn a_pair_takes_two_right_clicks_from_master_volume() {
        let rect = panel_rect(500);
        let master = Mt32Control::Function(Function::MasterVolume);
        let group = Mt32Control::Function(Function::SoundGroup);
        let press = |p: &mut Mt32Panel, c, left| p.press(c, left, (0, 0), rect, None);

        // Right, then right: the pair, with both buttons lit and nothing
        // else.
        let mut panel = Mt32Panel::default();
        press(&mut panel, master, false);
        press(&mut panel, group, false);
        assert_eq!(panel.mode, Mode::Chord(Chord::MasterTune));
        assert_eq!(
            panel.functions_lit(),
            [true, false, false, true],
            "only the two that made the pair"
        );

        // Left first: a plain press of MASTER VOLUME, and the next right
        // click cannot turn it into a pair.
        let mut panel = Mt32Panel::default();
        press(&mut panel, master, true);
        press(&mut panel, group, false);
        assert_eq!(panel.mode, Mode::Function(Function::MasterVolume));

        // Right first, left second: the second is a press, not a hold.
        let mut panel = Mt32Panel::default();
        press(&mut panel, master, false);
        press(&mut panel, group, true);
        assert_eq!(panel.mode, Mode::Function(Function::SoundGroup));

        // Holding anything but MASTER VOLUME lights nothing: no pair
        // begins with it.
        let mut panel = Mt32Panel::default();
        press(&mut panel, group, false);
        assert_eq!(panel.mode, Mode::Home);
        assert_eq!(panel.functions_lit(), [false; 4]);
        assert_eq!(panel.parts_lit(), [false; 6]);
    }

    /// Nothing is standing on anything until someone presses something,
    /// and switching the unit off puts it back there.
    #[test]
    fn a_panel_at_rest_has_nothing_held_down() {
        let mut panel = Mt32Panel::default();
        let at_rest = |p: &Mt32Panel| {
            let view = p.view(String::new(), false, true, None);
            (view.parts_lit, view.functions_lit)
        };
        assert_eq!(
            at_rest(&panel),
            ([false; 6], [false; 4]),
            "just switched on"
        );

        // Stand on something, and it shows.
        let rect = panel_rect(500);
        panel.press(
            Mt32Control::Function(Function::Volume),
            true,
            (0, 0),
            rect,
            None,
        );
        assert_ne!(
            at_rest(&panel),
            ([false; 6], [false; 4]),
            "a button is down"
        );

        // Off and on again is a fresh unit, whatever was held before.
        panel.reset();
        assert_eq!(
            at_rest(&panel),
            ([false; 6], [false; 4]),
            "after a power cycle"
        );
    }

    /// The pointer finds each control where it is drawn.
    #[test]
    fn every_control_is_hit_where_it_is_drawn() {
        let panel = panel_rect(500);
        let middle = |r: Rect| ((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
        for n in 0..6 {
            assert_eq!(
                control_at(panel, middle(part_rect(panel, n))),
                Some(Mt32Control::Part(n))
            );
        }
        for f in [
            Function::SoundGroup,
            Function::Volume,
            Function::Sound,
            Function::MasterVolume,
        ] {
            assert_eq!(
                control_at(panel, middle(function_rect(panel, f))),
                Some(Mt32Control::Function(f))
            );
        }
        assert_eq!(
            control_at(panel, middle(dial_rect(panel))),
            Some(Mt32Control::Dial)
        );
        assert_eq!(
            control_at(panel, middle(power_rect(panel))),
            Some(Mt32Control::Power)
        );
        assert_eq!(
            control_at(panel, (2, 2)),
            None,
            "the fascia is not a control"
        );
    }
}

// --- the panel as a thing that remembers what it is doing ----------------

/// What a press needs the window to do, which the panel cannot reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    /// Nothing but a redraw.
    None,
    /// Put this on the on-screen display.
    Say(String),
    /// Switch the unit off, or on.
    Power(bool),
    /// Off and on again, which is what a reset amounts to here.
    Recycle,
}

/// What the panel is standing on.
///
/// The unit's buttons carry no lamps, so nothing is lit at rest; a mode
/// lights exactly the buttons that reached it, which is one, two or three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The main screen. The engine owns the display and nothing is lit.
    Home,
    /// One button: it edits the value it names, for the shown part.
    Function(Function),
    /// MASTER VOLUME held with one other, editing what that pair names.
    Chord(Chord),
    /// A pair that wants a further press before it does anything, as the
    /// manual's three-button procedures do.
    Confirm(Chord),
}

/// A button held on the Select/Volume dial.
#[derive(Debug)]
struct DialGrab {
    /// Which way a repeat steps: left clockwise, right the other way.
    clockwise: bool,
    pressed_at: std::time::Instant,
    last_step: std::time::Instant,
    /// Where along the travel the hand took hold, and what the value was
    /// there. A drag moves from the grip rather than jumping to the finger.
    from_travel: Option<f32>,
    from_value: u8,
    /// Set once the hand has moved, which stops the repeat.
    dragging: bool,
}

/// How long a button sits on the dial before it starts repeating.
const DIAL_REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(350);

/// The MT-32's front panel: which part it is showing, which buttons are
/// down, and what it believes each editable value to be.
///
/// The synth has no way to read a patch parameter back, so the panel tracks
/// what it has written -- which is what the hardware's own firmware does.
#[derive(Debug)]
pub struct Mt32Panel {
    mode: Mode,
    /// The button being held down, put there by right-clicking it. Only
    /// MASTER VOLUME leads anywhere, which is the unit's own arrangement.
    held: Option<Mt32Control>,
    /// The part being shown, 0-7 for parts 1-8 and 8 for rhythm.
    part: usize,
    master_volume: u8,
    part_level: [u8; 9],
    part_timbre: [(u8, u8); 9],
    master_tune: u8,
    reverb_mode: u8,
    /// Whether parts 1-8 have been moved down onto channels 1-8, which is
    /// the unit's other channel arrangement.
    channels_shifted: bool,
    dial: Option<DialGrab>,
}

impl Default for Mt32Panel {
    fn default() -> Self {
        Self {
            mode: Mode::Home,
            held: None,
            part: 0,
            // The engine's own power-on values: full volume, parts at 80,
            // the first timbre, A = 442 Hz, room reverb.
            master_volume: 100,
            part_level: [80; 9],
            part_timbre: [(0, 0); 9],
            master_tune: 74,
            reverb_mode: 0,
            channels_shifted: false,
            dial: None,
        }
    }
}

impl Mt32Panel {
    /// Start over, as a unit just switched on is.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Let the pointer go, ending any turn of the dial.
    pub fn release_dial(&mut self) {
        self.dial = None;
    }

    /// Whether a button is being held on the dial.
    pub fn dial_held(&self) -> bool {
        self.dial.is_some()
    }

    /// A press on the panel.
    ///
    /// Right-clicking a button holds it down, as a finger would; left
    /// clicking presses it. Holding MASTER VOLUME and pressing another is
    /// how every one of the unit's two-button functions is reached, and a
    /// few of those then wait for a third press.
    pub fn press(
        &mut self,
        control: Mt32Control,
        left: bool,
        pos: (i32, i32),
        rect: Rect,
        synth: Option<&mut crate::mt32::Mt32Synth>,
    ) -> PanelAction {
        // The dial is not a button: its two clicks step it either way.
        if control == Mt32Control::Dial {
            self.grab_dial(left, pos, rect, synth);
            return PanelAction::None;
        }
        if control == Mt32Control::Power {
            return PanelAction::Power(true);
        }

        // A pair is two buttons held down together, so both presses are
        // right-clicks: MASTER VOLUME to take hold, then the button it is
        // held with. A left click is a plain press and lets go of it.
        let master = Mt32Control::Function(Function::MasterVolume);
        if !left && self.held == Some(master) && control != master {
            self.held = None;
            return match chord_for(control) {
                Some(chord) => self.enter_chord(chord),
                None => PanelAction::None,
            };
        }

        // A pair waiting to be confirmed takes the next part press.
        if let (Mode::Confirm(chord), Mt32Control::Part(n)) = (self.mode, control) {
            return self.confirm_chord(chord, n, synth);
        }

        if !left {
            // Only MASTER VOLUME leads anywhere held down -- every one of
            // the unit's pairs starts with it -- so holding anything else
            // would light a button that could not do anything.
            if control == master {
                self.held = if self.held == Some(master) {
                    None
                } else {
                    Some(master)
                };
            }
            return PanelAction::None;
        }

        self.held = None;
        match control {
            // Pressing what is already showing goes back to the main
            // screen, which is where the unit rests with nothing lit.
            Mt32Control::Part(n) if self.part == n && self.mode != Mode::Home => {
                self.release(synth);
            }
            Mt32Control::Function(f) if self.mode == Mode::Function(f) => self.release(synth),
            Mt32Control::Part(n) => {
                self.part = n;
                self.mode = Mode::Function(Function::Volume);
            }
            Mt32Control::Function(f) => self.mode = Mode::Function(f),
            Mt32Control::Dial | Mt32Control::Power => {}
        }
        PanelAction::None
    }

    /// Let the display go back to the engine, as the unit does when it
    /// returns to its main screen.
    fn release(&mut self, synth: Option<&mut crate::mt32::Mt32Synth>) {
        self.mode = Mode::Home;
        self.held = None;
        if let Some(synth) = synth {
            synth.show_main_display();
        }
    }

    /// Enter a two-button function. The ones that write settings do so
    /// through the same system memory a program would; the ones that want a
    /// further press wait for it.
    fn enter_chord(&mut self, chord: Chord) -> PanelAction {
        match chord {
            Chord::ShowPart(part) => {
                // Parts 6, 7 and 8 have no buttons of their own.
                self.part = part;
                self.mode = Mode::Function(Function::Volume);
                PanelAction::Say(format!("Munt MT-32: part {}", part + 1))
            }
            Chord::UnitNumber => {
                // The unit number picks which MT-32 a SysEx message is
                // addressed to. With one synth on a private cable there is
                // nothing to disambiguate, and the engine offers no way to
                // change it.
                self.mode = Mode::Chord(chord);
                PanelAction::Say("Munt MT-32: unit number is fixed at 1".to_string())
            }
            _ if chord.needs_confirming() => {
                self.mode = Mode::Confirm(chord);
                PanelAction::None
            }
            _ => {
                self.mode = Mode::Chord(chord);
                PanelAction::None
            }
        }
    }

    /// The further press a three-button procedure waits for.
    ///
    /// The manual's procedures all confirm with PART 1; All Reset takes any
    /// of 2 to 5 as the lighter reset that spares the patch memory.
    fn confirm_chord(
        &mut self,
        chord: Chord,
        part: usize,
        synth: Option<&mut crate::mt32::Mt32Synth>,
    ) -> PanelAction {
        use crate::mt32::addr;
        let confirmed = part == 0;
        match chord {
            Chord::AllReset if part <= 4 => PanelAction::Recycle,
            Chord::MidiChannels if confirmed => {
                // Parts 1-8 move down onto channels 1-8; rhythm stays on 10.
                self.channels_shifted = !self.channels_shifted;
                let base = u8::from(!self.channels_shifted);
                let channels: Vec<u8> = (0..8).map(|p| base + p).collect();
                if let Some(synth) = synth {
                    synth.write_memory(addr::CHAN_ASSIGN, &channels);
                    synth.show_main_display();
                }
                self.mode = Mode::Home;
                self.held = None;
                PanelAction::Say(format!(
                    "Munt MT-32: parts 1-8 on channels {}-{}",
                    base + 1,
                    base + 8
                ))
            }
            Chord::OverflowAssign if confirmed => {
                self.release(synth);
                // Spilling notes past the unit's capacity needs a second
                // synth on MIDI OUT, which there is nowhere to put here.
                PanelAction::Say("Munt MT-32: overflow assign needs a second unit".to_string())
            }
            _ => {
                self.release(synth);
                PanelAction::None
            }
        }
    }

    // --- the dial --------------------------------------------------------

    /// Take hold of the dial: a click steps it, and holding on repeats.
    fn grab_dial(
        &mut self,
        clockwise: bool,
        pos: (i32, i32),
        rect: Rect,
        synth: Option<&mut crate::mt32::Mt32Synth>,
    ) {
        self.step_dial(if clockwise { 1 } else { -1 }, synth);
        let now = std::time::Instant::now();
        self.dial = Some(DialGrab {
            clockwise,
            pressed_at: now,
            last_step: now,
            from_travel: dial_value_at(rect, pos),
            from_value: self.dial_target().0,
            dragging: false,
        });
    }

    /// Follow the hand round the dial while a button is held.
    ///
    /// The turn is measured from where the dial was taken hold of, so it
    /// moves under the hand instead of snapping to wherever the pointer
    /// happens to be -- a knob does not jump when you touch it.
    pub fn drag_dial(
        &mut self,
        pos: (i32, i32),
        rect: Rect,
        synth: Option<&mut crate::mt32::Mt32Synth>,
    ) {
        let Some(grab) = &self.dial else { return };
        let (Some(from_travel), from_value) = (grab.from_travel, grab.from_value) else {
            return;
        };
        let Some(travel) = dial_value_at(rect, pos) else {
            return;
        };
        let max = f32::from(self.dial_target().1);
        let moved = ((travel - from_travel) * max).round();
        if moved == 0.0 {
            return;
        }
        if let Some(grab) = &mut self.dial {
            grab.dragging = true;
        }
        self.set_dial_value((f32::from(from_value) + moved).clamp(0.0, max) as u8, synth);
    }

    /// Step the dial on while a button is held still on it, accelerating the
    /// longer it is down, the way a held key repeats. Returns whether it
    /// moved, so the caller knows to redraw.
    pub fn repeat_dial(&mut self, synth: Option<&mut crate::mt32::Mt32Synth>) -> bool {
        let Some(grab) = &self.dial else { return false };
        if grab.dragging {
            return false;
        }
        let held = grab.pressed_at.elapsed();
        if held < DIAL_REPEAT_DELAY {
            return false;
        }
        // From four a second up to twenty, reached after two seconds down.
        let ramp = ((held - DIAL_REPEAT_DELAY).as_secs_f32() / 2.0).clamp(0.0, 1.0);
        if grab.last_step.elapsed() < std::time::Duration::from_secs_f32(0.25 - 0.2 * ramp) {
            return false;
        }
        let clockwise = grab.clockwise;
        if let Some(grab) = &mut self.dial {
            grab.last_step = std::time::Instant::now();
        }
        self.step_dial(if clockwise { 1 } else { -1 }, synth);
        true
    }

    fn step_dial(&mut self, by: i32, synth: Option<&mut crate::mt32::Mt32Synth>) {
        let (value, max) = self.dial_target();
        self.set_dial_value(
            (i32::from(value) + by).clamp(0, i32::from(max)) as u8,
            synth,
        );
    }

    /// The value the dial is standing on and how high it goes. With nothing
    /// engaged it shows the master volume, which is what the unit rests on.
    fn dial_target(&self) -> (u8, u8) {
        let part = self.part;
        match self.mode {
            // A pair takes the dial over from either button's own meaning.
            Mode::Chord(Chord::MasterTune) => (self.master_tune, 127),
            Mode::Chord(Chord::ReverbMode) => (self.reverb_mode, 3),
            Mode::Function(Function::Volume) => (self.part_level[part], 100),
            Mode::Function(Function::SoundGroup) => (self.part_timbre[part].0, 3),
            Mode::Function(Function::Sound) => (self.part_timbre[part].1, 63),
            _ => (self.master_volume, 100),
        }
    }

    /// Set the value the dial is on, and write it to the synth.
    ///
    /// Each lands as a DT1 message in the same memory a program would
    /// write, so the engine reacts -- and puts the result on its display.
    fn set_dial_value(&mut self, value: u8, synth: Option<&mut crate::mt32::Mt32Synth>) {
        use crate::mt32::addr;
        let part = self.part;
        let (address, value) = match self.mode {
            Mode::Chord(Chord::MasterTune) => {
                self.master_tune = value.min(127);
                (addr::MASTER_TUNE, self.master_tune)
            }
            // The engine models the four reverb rooms; the unit's 0-10 is a
            // combination of mode, time and level, of which this is the mode.
            Mode::Chord(Chord::ReverbMode) => {
                self.reverb_mode = value.min(3);
                (addr::REVERB_MODE, self.reverb_mode)
            }
            Mode::Function(Function::Volume) => {
                self.part_level[part] = value.min(100);
                (
                    addr::patch(part, addr::PATCH_OUTPUT_LEVEL),
                    self.part_level[part],
                )
            }
            // Four banks: the two internal groups, then memory and rhythm.
            Mode::Function(Function::SoundGroup) => {
                self.part_timbre[part].0 = value.min(3);
                (
                    addr::patch(part, addr::PATCH_TIMBRE_GROUP),
                    self.part_timbre[part].0,
                )
            }
            Mode::Function(Function::Sound) => {
                self.part_timbre[part].1 = value.min(63);
                (
                    addr::patch(part, addr::PATCH_TIMBRE_NUMBER),
                    self.part_timbre[part].1,
                )
            }
            _ => {
                self.master_volume = value.min(100);
                (addr::MASTER_VOLUME, self.master_volume)
            }
        };
        if let Some(synth) = synth {
            synth.write_memory(address, &[value]);
        }
    }

    // --- what it looks like ----------------------------------------------

    /// The panel as it should be drawn, over an engine reporting `lcd` and
    /// `led`, powered or not.
    pub fn view(
        &self,
        lcd: String,
        led: bool,
        powered: bool,
        hover: Option<Mt32Control>,
    ) -> Mt32PanelView {
        let (value, max) = self.dial_target();
        Mt32PanelView {
            // With a button engaged the panel owns the display, as the
            // hardware's firmware does while you are editing; let go of it
            // and the engine's own display comes back.
            lcd: self.lcd().unwrap_or(lcd),
            led,
            parts_lit: self.parts_lit(),
            functions_lit: self.functions_lit(),
            dial: if max == 0 {
                0.0
            } else {
                f32::from(value) / f32::from(max)
            },
            powered,
            hover,
        }
    }

    /// Which part buttons are lit: the one the panel is standing on, with 1,
    /// 2 and 3 standing in for parts 6, 7 and 8, plus whichever a pair was
    /// reached through. Nothing at rest -- the unit has no lamps.
    fn parts_lit(&self) -> [bool; 6] {
        let mut lit = [false; 6];
        if let Some(Mt32Control::Part(n)) = self.held {
            lit[n] = true;
        }
        match self.mode {
            Mode::Home => {}
            // A pair or a prompt lights the button it was reached through.
            Mode::Chord(chord) | Mode::Confirm(chord) => {
                if let Mt32Control::Part(n) = chord.partner() {
                    lit[n] = true;
                }
            }
            Mode::Function(f) if f != Function::MasterVolume => {
                // Parts 1-5 and Rhythm are their own buttons; 6, 7 and 8
                // show on the buttons that reach them, 1, 2 and 3.
                let button = match self.part {
                    p @ 0..=5 => p,
                    p => p - 5,
                };
                lit[button.min(5)] = true;
            }
            Mode::Function(_) => {}
        }
        lit
    }

    /// Which function buttons are lit. A pair lights MASTER VOLUME as well
    /// as its partner, so all of the buttons that reached it are shown.
    fn functions_lit(&self) -> [bool; 4] {
        let mut lit = [false; 4];
        if let Some(Mt32Control::Function(f)) = self.held {
            lit[f.index()] = true;
        }
        match self.mode {
            Mode::Home => {}
            Mode::Function(f) => lit[f.index()] = true,
            Mode::Chord(chord) | Mode::Confirm(chord) => {
                lit[Function::MasterVolume.index()] = true;
                if let Mt32Control::Function(f) = chord.partner() {
                    lit[f.index()] = true;
                }
            }
        }
        lit
    }

    /// The line the panel puts on the display while a button is engaged,
    /// naming the part and the value being turned. `None` at rest, where the
    /// engine's own display shows instead.
    fn lcd(&self) -> Option<String> {
        let function = match self.mode {
            Mode::Home => return None,
            // The unit's own wording, and its own units: the tune reads in
            // hertz across 427.5 to 452.6.
            Mode::Chord(Chord::MasterTune) => {
                let hz = 427.5 + f32::from(self.master_tune) * (452.6 - 427.5) / 127.0;
                return Some(format!("Master Tune :{hz:5.1}Hz"));
            }
            Mode::Chord(Chord::ReverbMode) => {
                return Some(format!("** Reverb mode  : {}", self.reverb_mode))
            }
            Mode::Chord(Chord::UnitNumber) => return Some("Unit Number :    1".to_string()),
            Mode::Chord(_) => return None,
            // The prompts the manual shows for the three-button procedures.
            Mode::Confirm(Chord::AllReset) => return Some("** All Reset OK? [1]".to_string()),
            Mode::Confirm(Chord::MidiChannels) => return Some("MIDI Channel?    [1]".to_string()),
            Mode::Confirm(_) => return Some("Overflow Assign? [1]".to_string()),
            Mode::Function(f) => f,
        };
        // Parts read 1-5 and R, as they are labelled.
        let name = if self.part == 5 {
            "R".to_string()
        } else {
            (self.part + 1).to_string()
        };
        Some(match function {
            Function::MasterVolume => format!("Master vol      {:>3}", self.master_volume),
            Function::Volume => format!("Part {name}  volume  {:>3}", self.part_level[self.part]),
            Function::SoundGroup => {
                format!(
                    "Part {name}  group    {:>2}",
                    self.part_timbre[self.part].0 + 1
                )
            }
            Function::Sound => {
                format!(
                    "Part {name}  sound    {:>2}",
                    self.part_timbre[self.part].1 + 1
                )
            }
        })
    }
}

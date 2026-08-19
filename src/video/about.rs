//! The About panel: what it says, and how it makes its entrance.
//!
//! Everything plays off one clock -- milliseconds since the panel
//! opened -- so the entrance is a pure function of time and draws
//! nothing, allocates nothing and runs nothing while the panel is
//! closed.
//!
//! Everything editable sits at the top of the file: the names in
//! [`CONTRIBUTORS`] and [`PATREON_SPONSORS`] (add a name, the page
//! re-times and re-wraps itself), the pacing constants, and the wave
//! sheets in [`LAYERS`]. Nothing below them needs touching for a
//! content change.

use super::font;
use super::ui::{
    draw_panel_text, wrap_text, PANEL_TEXT, PANEL_TEXT_DIM, PANEL_TEXT_HILIGHT, TITLE_H,
};
use super::window::{fill_rect, rgba, scale_rect, texture_height, texture_width, Rect};

/// Contributors and Patreon sponsors credited on the page. Keep both
/// in step with CREDITS.md and the website's Community section.
const CONTRIBUTORS: &[&str] = &[
    "Bernie Innocenti",
    "Lee Hobson",
    "jbl007",
    "Simon Dick",
    "Nicolas Ramz",
    "Ben Letchford",
    "Volker Schwaberow",
    "Matt Harlum",
];
const PATREON_SPONSORS: &[&str] = &["Lee Hobson"];

/// One title letter every so often: ten letters make the
/// near-three-second assembly.
const TITLE_LETTER_MS: u64 = 280;
/// The breath between the lines that follow.
const LINE_MS: u64 = 90;
/// The breath between credited names.
const NAME_MS: u64 = 150;
/// The wave strip: its height in pixels -- shared by both pages, and
/// sized so the running page's fuller text always leaves it room --
/// and the width of one column.
const WAVE_H: usize = 64;
const WAVE_COL: usize = 8;

/// One sheet of water: its motion, its share of the stage, and the
/// colours it climbs between.
struct Layer {
    /// Phase advance per second; higher rolls faster.
    speed: f32,
    /// Phase step per column; higher packs the crests tighter.
    spacing: f32,
    /// Fraction of the strip this sheet's crests may reach.
    height: f32,
    /// Brightness multiplier; below 1.0 pushes the sheet back.
    depth: f32,
    /// Trough and crest colours the gradient climbs between.
    dark: (f32, f32, f32),
    bright: (f32, f32, f32),
}

/// The sheets, back to front: silver mist, deep copper, molten copper.
const LAYERS: [Layer; 3] = [
    Layer {
        speed: 2.6,
        spacing: 0.22,
        height: 0.85,
        depth: 0.45,
        dark: (40.0, 44.0, 52.0),
        bright: (205.0, 210.0, 220.0),
    },
    Layer {
        speed: 3.8,
        spacing: 0.3,
        height: 0.65,
        depth: 0.6,
        dark: (60.0, 28.0, 14.0),
        bright: (230.0, 140.0, 70.0),
    },
    Layer {
        speed: 5.4,
        spacing: 0.35,
        height: 1.0,
        depth: 1.0,
        dark: (70.0, 32.0, 16.0),
        bright: (255.0, 186.0, 100.0),
    },
];

pub struct AboutView {
    /// Emulated-machine summary lines (built once at startup, refreshed
    /// on a ROM swap -- never per frame).
    pub machine_lines: Vec<String>,
    /// Milliseconds since the panel opened, driving the entrance.
    pub elapsed_ms: u64,
    /// Whether the machine lines are real facts (bulleted) rather
    /// than the configuration screen's centred invitation. Power state
    /// does not matter: a stopped machine keeps its reference card.
    pub machine_fitted: bool,
}

/// The entrance timetable: each element's turn comes in drawing
/// order, one call per element, so the layout is the schedule and
/// inserting a line never needs it rebalanced.
struct Timetable {
    elapsed: u64,
    gate: u64,
}

impl Timetable {
    /// This element's turn: past due means drawn.
    fn due(&mut self, step: u64) -> bool {
        self.gate += step;
        self.elapsed >= self.gate
    }

    /// How many of a list have arrived, one every `each_ms` once the
    /// list's own turn (after `step`) has come.
    fn arrivals(&mut self, step: u64, each_ms: u64, count: usize) -> usize {
        let start = self.gate + step;
        self.gate = start + each_ms * count as u64;
        if self.elapsed < start {
            return 0;
        }
        (((self.elapsed - start) / each_ms) as usize + 1).min(count)
    }
}

pub(in crate::video) fn draw(frame: &mut [u8], rect: Rect, view: &AboutView, scale: usize) {
    let elapsed = view.elapsed_ms;
    let cx = |text: &str, px: usize| rect.x + rect.w.saturating_sub(text.len() * 8 * px) / 2;

    // The title assembles itself like luggage on a belt: each letter
    // slides in from the right edge and rides to its slot, the C
    // settling first and the far 'e' arriving last, while the lines
    // below print alongside on their own beat. (ASCII, so the byte
    // slice per glyph is sound.)
    let title = "Copperline";
    let glyph_w = 8 * 3;
    let x0 = cx(title, 3);
    let mut y = rect.y + TITLE_H + 14;
    let settled = (elapsed / TITLE_LETTER_MS) as usize;
    for i in 0..title.len() {
        let slot = x0 + i * glyph_w;
        let glyph = &title[i..i + 1];
        if i < settled {
            draw_panel_text(frame, slot, y, glyph, PANEL_TEXT_HILIGHT, 3, scale);
        } else if i == settled {
            // In flight: the slot is always left of the entry point, so
            // the ride is a plain lerp toward it.
            let progress = (elapsed % TITLE_LETTER_MS) as f32 / TITLE_LETTER_MS as f32;
            let from = (rect.x + rect.w - glyph_w - 8) as f32;
            let x = from + (slot as f32 - from) * progress;
            draw_panel_text(frame, x as usize, y, glyph, PANEL_TEXT_HILIGHT, 3, scale);
        }
    }
    y += 30;

    let mut clock = Timetable { elapsed, gate: 0 };

    let version = concat!("version ", env!("COPPERLINE_DISPLAY_VERSION"));
    if clock.due(LINE_MS) {
        draw_panel_text(frame, cx(version, 1), y, version, PANEL_TEXT_DIM, 1, scale);
    }
    y += 14;
    let tagline = "A cycle-stepped Amiga emulator";
    if clock.due(LINE_MS) {
        draw_panel_text(frame, cx(tagline, 2), y, tagline, PANEL_TEXT, 2, scale);
    }
    y += 22;
    let author = "by Andrew \"LinuxJedi\" Hutchings";
    if clock.due(LINE_MS) {
        draw_panel_text(frame, cx(author, 1), y, author, PANEL_TEXT_DIM, 1, scale);
    }
    y += 24;

    // Machine info sits between the small print and the headline: one
    // and a half glyphs. That size only exists in raw pixels, so this
    // block draws through the font directly; rounding up keeps a 1x
    // window at full headline size.
    let px = (3 * scale).div_ceil(2);
    let char_w = (8 * px).div_ceil(scale);
    let (tw, th) = (texture_width(scale), texture_height(scale));
    if view.machine_fitted {
        // The reference card: each fact behind a hanging bullet, wrapped
        // continuations sitting square under the text.
        let width = (rect.w.saturating_sub(48) / char_w).saturating_sub(2);
        for line in &view.machine_lines {
            for (i, part) in wrap_text(line, width, width).into_iter().enumerate() {
                let (x, text) = if i == 0 {
                    (rect.x + 24, format!("> {part}"))
                } else {
                    (rect.x + 24 + 2 * char_w, part)
                };
                if clock.due(LINE_MS) {
                    font::draw_text(frame, tw, th, x * scale, y * scale, &text, PANEL_TEXT, px);
                }
                y += char_w + 3;
            }
        }
    } else {
        // The idle page's invitation: centred, no bullet.
        for line in &view.machine_lines {
            if clock.due(LINE_MS) {
                let x = rect.x + rect.w.saturating_sub(line.len() * char_w) / 2;
                font::draw_text(frame, tw, th, x * scale, y * scale, line, PANEL_TEXT, px);
            }
            y += char_w + 3;
        }
    }
    y += 10;

    for line in [
        "m68k CPU core (MIT)",
        "font8x8 by Daniel Hepper / Marcel Sondaar",
        "winit + pixels + cpal + gilrs",
    ] {
        if clock.due(LINE_MS) {
            let line = format!("* {line}");
            draw_panel_text(frame, rect.x + 24 + 16, y, &line, PANEL_TEXT_DIM, 1, scale);
        }
        y += 12;
    }
    y += 10;

    // Each label takes a line of its own; the names arrive one by one
    // beneath it, indented. Wrapping re-flows as each name lands, which
    // is the point -- the line grows in front of you.
    let max_small = rect.w.saturating_sub(48 + 16) / 8;
    for (label, names) in [
        ("Contributors", CONTRIBUTORS),
        ("Patreon sponsors", PATREON_SPONSORS),
    ] {
        if clock.due(LINE_MS) {
            let line = format!("{label}:");
            draw_panel_text(frame, rect.x + 24, y, &line, PANEL_TEXT, 1, scale);
        }
        y += 12;
        let arrived = clock.arrivals(NAME_MS, NAME_MS, names.len());
        for part in wrap_text(&names[..arrived].join(", "), max_small, max_small) {
            draw_panel_text(frame, rect.x + 24 + 16, y, &part, PANEL_TEXT, 1, scale);
            y += 12;
        }
        // A breath of space between the groups.
        y += 6;
    }

    // The floor show, playing from the moment the panel opens across
    // the panel's whole width, and kept below the text -- whatever room
    // is left under the last line is its stage, so growing the credits
    // can shrink the water but never flood the words.
    let base = rect.y + rect.h - 8;
    let room = base.saturating_sub(y + 6).min(WAVE_H);
    if room < 16 {
        return;
    }
    // One column in from the left so the water clears the panel's
    // trim; being a whole column, the inset keeps the grid landing
    // exactly on the unchanged right edge.
    let strip = Rect {
        x: rect.x + WAVE_COL,
        y: base - room,
        w: rect.w - WAVE_COL,
        h: room,
    };
    draw_waves(frame, strip, elapsed as f32 / 1000.0, scale);
}

/// The floor show: the wave sheets of [`LAYERS`] in parallax, drawing
/// themselves in from the left, each column joining as its sheet's
/// front reaches it and swelling up from nothing over its first
/// wavelength.
fn draw_waves(frame: &mut [u8], strip: Rect, t: f32, scale: usize) {
    let base = strip.y + strip.h;
    let cols = strip.w / WAVE_COL;
    for layer in &LAYERS {
        for c in 0..cols {
            let phase = t * layer.speed - c as f32 * layer.spacing;
            if phase <= 0.0 {
                continue;
            }
            let swell = (phase / std::f32::consts::TAU).min(1.0);
            let lift = swell * (1.0 + phase.sin()) * 0.5;
            // lift is 0..=1 and height <= 1, so h stays within the strip
            // and the subtractions below cannot underflow.
            let h = (((strip.h as f32 - 6.0) * layer.height * lift) as usize + 6).min(strip.h);
            draw_bar(frame, strip.x + c * WAVE_COL, base, h, lift, layer, scale);
        }
    }
}

/// One bar of one sheet: a vertical gradient from the layer's dark
/// waterline up to its bright crest, scaled by how high the bar
/// stands (`lift`) so the swell glows as it rises, with a fleck of
/// white light on the tallest.
fn draw_bar(
    frame: &mut [u8],
    x: usize,
    base: usize,
    h: usize,
    lift: f32,
    layer: &Layer,
    scale: usize,
) {
    let bands = (h / 5).max(1);
    for b in 0..bands {
        let top = base - h + b * h / bands;
        let bottom = base - h + (b + 1) * h / bands;
        let glow = lift * (1.0 - (b as f32 + 0.5) / bands as f32);
        let chan = |dark: f32, bright: f32| (layer.depth * (dark + (bright - dark) * glow)) as u32;
        let color = rgba(
            chan(layer.dark.0, layer.bright.0),
            chan(layer.dark.1, layer.bright.1),
            chan(layer.dark.2, layer.bright.2),
        );
        let band = Rect {
            x,
            y: top,
            w: WAVE_COL - 1,
            h: bottom - top,
        };
        fill_rect(frame, scale_rect(band, scale), color, scale);
    }
    if lift > 0.85 && h > 8 {
        let fleck = Rect {
            x,
            y: base - h,
            w: WAVE_COL - 1,
            h: 2,
        };
        let l = (layer.depth * 235.0) as u32;
        fill_rect(frame, scale_rect(fleck, scale), rgba(l, l, l + 8), scale);
    }
}

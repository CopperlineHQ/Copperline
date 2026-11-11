// SPDX-License-Identifier: GPL-3.0-or-later

//! The Toaster's digital video effects (DVE): moves, squeezes, flips,
//! push-ons and the rest of the 4000FX "Digital", "Trajectory" and
//! "Push-Pull" families.
//!
//! The card has no geometry engine. With EH0's `LINP` (and `LUT`) clear,
//! framestore playback stops counting lines and pixels by itself and
//! follows an address the Amiga sends with every line:
//!
//! - the line comes from the line number `L` in the line's header (sprite
//!   1's B and I words, whatever kind of header the line carries): the
//!   output line shows framestore line `2L + field`, so picture row `y`
//!   (framestore line `y + 1`) shows picture row `2L + f - 1`, `f` the
//!   row's field parity;
//! - across the line, every quad (four card pixels, one subcarrier cycle)
//!   carries an 8-bit source-quad number on the B and I lines, drawn the
//!   same way as the read-back labels (I the high nibble, B the low, first
//!   pixel most significant): output card quad `c` shows framestore
//!   samples `4·Q(c) .. 4·Q(c)+3`.
//!
//! Quad codes 0-191 address the line; `$E8`-`$EF` are the card's own
//! colour quads (borders, the luma-key backgrounds); `$FF` is "no
//! picture here" (the background shows through under QUAF), `$F8`-`$FE`
//! drop-shadow quads under TRANSQUAD. The rest show black.
//!
//! Recovered from the switcher (`ScaleCalc`, `ScaleXOut`, `ScaleYOut`,
//! `InitDVESprite1Buff`, `_BCPalette`) and checked against the 4000FX
//! effect files, whose final frames decode to the identity map. See
//! `docs/internals/video.md`.

use super::composite;
use super::video::{black, AMIGA_LINE0, PICTURE_H, PICTURE_LINE0, PICTURE_W, PICTURE_X0};
use super::Card;
use crate::videoslot::FieldPixels;

/// Card quads per line.
const QUADS: usize = 192;

/// Quad code: transparent, the background shows.
pub const QUAF_BYTE: u8 = 0xFF;

/// The card's colour quads `$E8`-`$EF` (`_BCPalette`: Y on the 70-200
/// scale, I and Q signed in the same units): black, white, then the bar
/// colours yellow, cyan, green, magenta, red, blue.
const COLOUR_QUADS: [(f32, f32, f32); 8] = [
    (71.0, 0.0, 0.0),
    (200.0, 0.0, 0.0),
    (157.0, 31.0, -30.0),
    (138.0, -58.0, -20.0),
    (128.0, -27.0, -51.0),
    (110.0, 27.0, 51.0),
    (100.0, 58.0, 20.0),
    (81.0, -31.0, 30.0),
];

fn colour_quad(code: u8) -> u32 {
    let (y, i, q) = COLOUR_QUADS[usize::from(code - 0xE8)];
    let scale = composite::WHITE - composite::BLACK;
    composite::rgba_of_yiq((y - composite::BLACK) / scale, i / scale, q / scale)
}

/// A quad's source number in the Warp palette (`PACO_Warp0-3`): red bit 0
/// of each pixel's low nibble is the B line (the low nibble of Q), green
/// bit 0 the I line (the high nibble), first pixel most significant. Only
/// those two bits: the switcher's interface stays on screen during an
/// effect, and its colours use the other low bits for the overlay and the
/// alpha codes.
fn warp_label(row: &[u32], quad_col: i64, scale: i64) -> Option<u8> {
    let mut hi = 0u8;
    let mut lo = 0u8;
    for j in 0..4i64 {
        let col = usize::try_from(quad_col + j * scale).ok()?;
        let [r, g, _, _] = row.get(col)?.to_le_bytes();
        hi |= (g & 1) << (3 - j);
        lo |= (r & 1) << (3 - j);
    }
    Some((hi << 4) | lo)
}

/// Source rows a line number can reach: the picture and the framestore
/// lines below it (the border lines, `L` = 252).
pub const SOURCE_ROWS: usize = super::LINES - PICTURE_LINE0;

/// One frame's addressing.
pub struct DveMap {
    /// Per picture row, the source row: a picture row, or past the picture
    /// a framestore line below it (`None`: off the framestore).
    rows: Vec<Option<usize>>,
    /// Per picture row, the quad code of each of the line's `QUADS`
    /// quads (the card pixel's quad is `(x + PICTURE_X0) >> 2`).
    quads: Vec<u8>,
}

/// Quads of one picture row, from the picture's first: card pixel 12 is
/// in quad 3, pixel 763 in quad 190.
const ROW_QUADS: usize = PICTURE_X0 / 4;
const ROW_QUADS_END: usize = (PICTURE_X0 + PICTURE_W).div_ceil(4);

impl DveMap {
    /// A map with nothing in it, to be filled by [`Self::read_field`].
    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            quads: Vec::new(),
        }
    }

    /// Read the addressing off a field: the header and quad codes of the
    /// Amiga line under each picture row.
    pub fn from_field(field: &FieldPixels<'_>, x0: i64) -> Self {
        let mut map = Self::empty();
        map.read_field(field, x0);
        map
    }

    /// [`Self::from_field`] into this map, reusing its memory.
    pub fn read_field(&mut self, field: &FieldPixels<'_>, x0: i64) {
        let scale = field.canvas_scale as i64;
        self.rows.clear();
        self.rows.resize(PICTURE_H, None);
        self.quads.clear();
        self.quads.resize(PICTURE_H * QUADS, QUAF_BYTE);
        let mut last_vpos = None;
        for y in 0..PICTURE_H {
            let fl = y + PICTURE_LINE0;
            let f = fl & 1;
            let vpos = AMIGA_LINE0 + (fl >> 1) as u32;
            let Some(header) = Card::header(field, vpos) else {
                continue;
            };
            let (before, from_here) = self.quads.split_at_mut(y * QUADS);
            let quads = &mut from_here[..QUADS];
            if last_vpos == Some(vpos) {
                // The same Amiga line as the row above: its quads.
                quads.copy_from_slice(&before[(y - 1) * QUADS..y * QUADS]);
            } else {
                last_vpos = Some(vpos);
                let row = field.row(vpos).unwrap_or(&[]);
                for (c, q) in quads.iter_mut().enumerate() {
                    *q = warp_label(row, x0 + (c as i64) * 4 * scale, scale).unwrap_or(QUAF_BYTE);
                }
            }
            let l = usize::from(((header.i >> 12) << 4) | (header.b >> 12));
            // Picture row = framestore line - 1.
            self.rows[y] = (2 * l + f)
                .checked_sub(PICTURE_LINE0)
                .filter(|&r| r < SOURCE_ROWS);
        }
    }

    /// Whether any line shows a framestore line below the picture.
    pub fn reaches_below(&self) -> bool {
        self.rows.iter().flatten().any(|&r| r >= PICTURE_H)
    }

    /// The quad code at picture pixel `(x, y)`.
    fn code_at(&self, x: usize, y: usize) -> u8 {
        self.quads[y * QUADS + ((x + PICTURE_X0) >> 2)]
    }

    /// The effect's picture from a linear one: the framestore playback as
    /// addressed. `src` holds the picture rows and may go on with the
    /// lines below it; rows it lacks show black.
    pub fn apply(&self, src: &[u32], out: &mut [u32]) {
        let black = black();
        for (y, line) in out.chunks_exact_mut(PICTURE_W).enumerate() {
            let Some(&row) = self.rows.get(y) else {
                line.fill(black);
                continue;
            };
            let quads = &self.quads[y * QUADS..(y + 1) * QUADS];
            let src_row = row.and_then(|r| src.get(r * PICTURE_W..(r + 1) * PICTURE_W));
            // Quad by quad: card pixel p = x + 12 shows sample 4q + (p & 3),
            // and the picture's own columns are samples 12 on, so the
            // quad's four pixels are source pixels 4q - 12 + (0..4) when
            // those exist.
            for c in ROW_QUADS..ROW_QUADS_END {
                let x0 = (c * 4).saturating_sub(PICTURE_X0);
                let x1 = (c * 4 + 4 - PICTURE_X0).min(PICTURE_W);
                let pixels = &mut line[x0..x1];
                let fill = match quads[c] {
                    q @ 0..=191 => {
                        let first = 4 * usize::from(q) + (x0 + PICTURE_X0) % 4;
                        match src_row.and_then(|row| row.get(first.wrapping_sub(PICTURE_X0)..)) {
                            Some(from) if first >= PICTURE_X0 && from.len() >= pixels.len() => {
                                pixels.copy_from_slice(&from[..pixels.len()]);
                                continue;
                            }
                            Some(from) if first >= PICTURE_X0 => {
                                pixels[..from.len()].copy_from_slice(from);
                                pixels[from.len()..].fill(black);
                                continue;
                            }
                            // Samples left of the picture: the ones the
                            // picture has, black for the rest.
                            _ => {
                                for (j, o) in pixels.iter_mut().enumerate() {
                                    let sample = first + j;
                                    *o = src_row
                                        .and_then(|row| {
                                            sample
                                                .checked_sub(PICTURE_X0)
                                                .and_then(|sx| row.get(sx))
                                        })
                                        .copied()
                                        .unwrap_or(black);
                                }
                                continue;
                            }
                        }
                    }
                    q @ 0xE8..=0xEF => colour_quad(q),
                    _ => black,
                };
                pixels.fill(fill);
            }
        }
    }

    /// The fader's B weight per pixel for the DVE key modes: under QUAF
    /// every quad but `$FF` shows the effect; under TRANSQUAD only the
    /// drop-shadow quads do, a pixel at a time (bit `3 - j` of the code).
    pub fn key(&self, transquad: bool, cd: u8) -> Vec<u8> {
        let mut key = Vec::new();
        self.key_into(transquad, cd, &mut key);
        key
    }

    /// [`Self::key`] into a buffer kept between frames.
    pub fn key_into(&self, transquad: bool, cd: u8, out: &mut Vec<u8>) {
        out.clear();
        out.resize(PICTURE_W * PICTURE_H, 0);
        for (y, line) in out.chunks_exact_mut(PICTURE_W).enumerate() {
            for (x, k) in line.iter_mut().enumerate() {
                let q = self.code_at(x, y);
                let j = (x + PICTURE_X0) & 3;
                let on = if transquad {
                    q >= 0xF8 && (q >> (3 - j)) & 1 != 0
                } else {
                    q != QUAF_BYTE
                };
                *k = if on { cd } else { 0 };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(rows: Vec<Option<usize>>, quad_of: impl Fn(usize, usize) -> u8) -> DveMap {
        let mut quads = vec![QUAF_BYTE; QUADS * PICTURE_H];
        for y in 0..PICTURE_H {
            for c in 0..QUADS {
                quads[y * QUADS + c] = quad_of(y, c);
            }
        }
        DveMap { rows, quads }
    }

    /// The addressing applied one pixel at a time, as the card does it.
    fn apply_reference(map: &DveMap, src: &[u32], out: &mut [u32]) {
        for (y, line) in out.chunks_exact_mut(PICTURE_W).enumerate() {
            let src_row = map.rows[y].and_then(|r| src.get(r * PICTURE_W..(r + 1) * PICTURE_W));
            for (x, o) in line.iter_mut().enumerate() {
                *o = match map.code_at(x, y) {
                    q @ 0..=191 => {
                        let p = x + PICTURE_X0;
                        let sample = 4 * usize::from(q) + (p & 3);
                        src_row
                            .and_then(|row| {
                                sample.checked_sub(PICTURE_X0).and_then(|sx| row.get(sx))
                            })
                            .copied()
                            .unwrap_or_else(black)
                    }
                    q @ 0xE8..=0xEF => colour_quad(q),
                    _ => black(),
                };
            }
        }
    }

    #[test]
    fn the_quad_by_quad_apply_matches_the_pixel_by_pixel_one() {
        // Quads from everywhere: the line's start (samples left of the
        // picture), its end (past it), colours, shadows, transparency,
        // and rows from off the framestore.
        let mut seed = 0x2545_F491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let rows: Vec<Option<usize>> = (0..PICTURE_H)
            .map(|_| match next() % 8 {
                0 => None,
                1 => Some(PICTURE_H + (next() as usize % 40)),
                _ => Some(next() as usize % PICTURE_H),
            })
            .collect();
        let codes: Vec<u8> = (0..PICTURE_H * QUADS)
            .map(|_| match next() % 10 {
                0 => QUAF_BYTE,
                1 => 0xE8 + (next() % 8) as u8,
                2 => 0xF8 + (next() % 7) as u8,
                3 => 0xC0 + (next() % 40) as u8,
                4 => (next() % 4) as u8,
                5 => 188 + (next() % 4) as u8,
                _ => (next() % 192) as u8,
            })
            .collect();
        let map = map_with(rows, |y, c| codes[y * QUADS + c]);
        let mut src = ramp();
        let border = u32::from_le_bytes([200, 10, 10, 0xFF]);
        src.extend(std::iter::repeat_n(border, PICTURE_W * 20));
        let mut fast = vec![0; PICTURE_W * PICTURE_H];
        let mut slow = vec![0; PICTURE_W * PICTURE_H];
        map.apply(&src, &mut fast);
        apply_reference(&map, &src, &mut slow);
        assert_eq!(fast, slow);
        // And with only the picture rows to read from.
        map.apply(&src[..PICTURE_W * PICTURE_H], &mut fast);
        apply_reference(&map, &src[..PICTURE_W * PICTURE_H], &mut slow);
        assert_eq!(fast, slow);
    }

    fn ramp() -> Vec<u32> {
        (0..PICTURE_W * PICTURE_H)
            .map(|i| u32::from_le_bytes([(i % 251) as u8, (i / PICTURE_W % 251) as u8, 7, 0xFF]))
            .collect()
    }

    #[test]
    fn the_identity_map_shows_the_framestore_unchanged() {
        // Quad c addresses itself, every row its own line.
        let map = map_with((0..PICTURE_H).map(Some).collect(), |_, c| c as u8);
        let src = ramp();
        let mut out = vec![0; src.len()];
        map.apply(&src, &mut out);
        assert_eq!(out, src);
    }

    #[test]
    fn the_quad_number_rides_on_red_and_green_bit_zero_only() {
        // Q = $A5: I line (green) 1010, B line (red) 0101, under interface
        // colours whose blue low bits are set throughout.
        let px = |r: u8, g: u8| u32::from_le_bytes([0x90 | r, 0x92 | g, 0x9B, 0xFF]);
        let row = [px(0, 1), px(1, 0), px(0, 1), px(1, 0)];
        assert_eq!(warp_label(&row, 0, 1), Some(0xA5));
    }

    #[test]
    fn border_lines_come_from_below_the_picture() {
        // L = 252: framestore line 504/505, past the picture's 480 rows,
        // where the switcher keeps the border colour.
        let mut rows: Vec<Option<usize>> = (0..PICTURE_H).map(Some).collect();
        rows[0] = Some(503);
        let map = map_with(rows, |_, c| c as u8);
        assert!(map.reaches_below());
        let mut src = ramp();
        let border = u32::from_le_bytes([200, 10, 10, 0xFF]);
        src.extend(std::iter::repeat_n(
            border,
            PICTURE_W * (SOURCE_ROWS - PICTURE_H),
        ));
        let mut out = vec![0; PICTURE_W * PICTURE_H];
        map.apply(&src, &mut out);
        assert!(out[..PICTURE_W].iter().all(|&p| p == border));
        // Without those lines the border shows black, not a panic.
        map.apply(&src[..PICTURE_W * PICTURE_H], &mut out);
        assert!(out[..PICTURE_W].iter().all(|&p| p == black()));
    }

    #[test]
    fn a_push_moves_the_picture_by_whole_quads() {
        // Everything one quad to the right, rows two lines down.
        let map = map_with(
            (0..PICTURE_H).map(|y| y.checked_sub(2)).collect(),
            |_, c| c.saturating_sub(1) as u8,
        );
        let src = ramp();
        let mut out = vec![0; src.len()];
        map.apply(&src, &mut out);
        assert_eq!(out[10 * PICTURE_W + 100], src[8 * PICTURE_W + 96]);
        assert_eq!(out[0], black());
    }

    #[test]
    fn transparent_and_colour_quads() {
        let map = map_with((0..PICTURE_H).map(Some).collect(), |_, c| match c {
            0..=50 => QUAF_BYTE,
            51..=60 => 0xE9,
            61..=70 => 0xF9,
            _ => c as u8,
        });
        let src = ramp();
        let mut out = vec![0; src.len()];
        map.apply(&src, &mut out);
        let at = |c: usize| out[4 * c - PICTURE_X0];
        assert_eq!(at(10), black());
        let white = at(55).to_le_bytes();
        assert!(
            white[0] > 240 && white[1] > 240 && white[2] > 240,
            "{white:?}"
        );
        let quaf = map.key(false, 200);
        assert_eq!(quaf[4 * 10 - PICTURE_X0], 0);
        assert_eq!(quaf[4 * 100 - PICTURE_X0], 200);
        // TRANSQUAD: $F9 = %1111_1001: pixels 0 and 3 of the quad.
        let trans = map.key(true, 255);
        let q = 4 * 65 - PICTURE_X0;
        assert_eq!(&trans[q..q + 4], &[255, 0, 0, 255]);
        assert_eq!(trans[4 * 100 - PICTURE_X0], 0);
    }
}

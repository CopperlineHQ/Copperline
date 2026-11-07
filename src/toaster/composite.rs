// SPDX-License-Identifier: GPL-3.0-or-later

//! The framestore's sample format: NTSC composite video sampled at four
//! times the colour subcarrier (4fsc, 14.318 MHz), one byte per sample, on
//! the I and Q axes, with a second, subcarrier-complemented copy of every
//! line.
//!
//! Recovered from the Toaster's own framestore files (`Framestore/ColorBars`
//! is SMPTE bars: an ILBM of 752 x 480 pixels in 16 planes, the normal
//! sample in one byte and the complemented one in the other, planes MSB
//! first) and the switcher's read self-test, whose 71-200 ramp spans the
//! video range:
//!
//! - Luma: blanking 60, black (7.5 IRE setup) 70, 100 % white 200: one
//!   unit is 1/130 of the black-to-white swing.
//! - Chroma rides on luma at the same scale. The four samples of a quad sit
//!   on the `-I`, `-Q`, `+I`, `+Q` axes of a line whose subcarrier phase is
//!   positive, and on the opposite ones when it is negative: NTSC's
//!   subcarrier inverts from one line of a field to the next and between
//!   the two fields' matching lines.
//! - The complemented copy is the same picture with the chroma inverted
//!   (the field III/IV version of the line). Normal + complemented is
//!   twice the luma; their difference is twice the chroma, so the card can
//!   separate the two without a comb filter.

/// Blanking level.
pub const BLANK: f32 = 60.0;
/// Black: 7.5 IRE setup.
pub const BLACK: f32 = 70.0;
/// 100 % white.
pub const WHITE: f32 = 200.0;
/// Code units per unit of luma or chroma (gamma-corrected 0..1 scale).
const SCALE: f32 = WHITE - BLACK;

/// Subcarrier phase of framestore line `line`: `true` where a quad's first
/// sample lies on `-I` in the normal copy.
pub fn line_phase_positive(line: usize) -> bool {
    (line / 2 + line % 2).is_multiple_of(2)
}

#[inline]
fn rgb_of(pixel: u32) -> (f32, f32, f32) {
    let [r, g, b, _] = pixel.to_le_bytes();
    (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

#[inline]
fn yiq(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        0.299 * r + 0.587 * g + 0.114 * b,
        0.596 * r - 0.274 * g - 0.322 * b,
        0.211 * r - 0.523 * g + 0.312 * b,
    )
}

#[inline]
pub fn rgba_of_yiq(y: f32, i: f32, q: f32) -> u32 {
    let r = y + 0.956 * i + 0.621 * q;
    let g = y - 0.272 * i - 0.647 * q;
    let b = y - 1.106 * i + 1.703 * q;
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    u32::from_le_bytes([c(r), c(g), c(b), 0xFF])
}

#[inline]
fn code(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Encode one line of RGBA pixels as the card digitises it: `normal` and
/// `complemented` copies, one sample per pixel.
pub fn encode_line(rgba: &[u32], line: usize, normal: &mut [u8], complemented: &mut [u8]) {
    let s = if line_phase_positive(line) { 1.0 } else { -1.0 };
    for (p, &px) in rgba.iter().enumerate() {
        let (r, g, b) = rgb_of(px);
        let (y, i, q) = yiq(r, g, b);
        let luma = BLACK + SCALE * y;
        // Quad axes on a positive line: -I, -Q, +I, +Q.
        let c = SCALE
            * match p % 4 {
                0 => -i,
                1 => -q,
                2 => i,
                _ => q,
            }
            * s;
        if let Some(n) = normal.get_mut(p) {
            *n = code(luma + c);
        }
        if let Some(m) = complemented.get_mut(p) {
            *m = code(luma - c);
        }
    }
}

/// Decode one stored line back to RGBA. Luma is the mean of the two
/// copies; I and Q come from the quad the pixel sits in.
pub fn decode_line(normal: &[u8], complemented: &[u8], line: usize, out: &mut [u32]) {
    let s = if line_phase_positive(line) { 1.0 } else { -1.0 };
    let n = normal.len().min(complemented.len()).min(out.len());
    // Chroma per sample, signed onto its axis: +I, +Q, +I, +Q.
    let chroma = |p: usize| {
        let c = (f32::from(normal[p]) - f32::from(complemented[p])) / 2.0 * s;
        if p % 4 < 2 {
            -c
        } else {
            c
        }
    };
    for p in 0..n {
        let y = ((f32::from(normal[p]) + f32::from(complemented[p])) / 2.0 - BLACK) / SCALE;
        // The other axis is the neighbouring sample; average the pair on
        // each side so a colour edge does not fringe one way.
        let (i_at, q_at) = if p % 2 == 0 { (p, p + 1) } else { (p - 1, p) };
        let i = chroma(i_at.min(n - 1));
        let q = chroma(q_at.min(n - 1));
        out[p] = rgba_of_yiq(y, i / SCALE, q / SCALE);
    }
}

/// Decode one line from a single copy, with no complemented twin to pair
/// it with (a Flyer field, or any one field of moving video): a notch at
/// the subcarrier (`[1, 2, 1] / 4` over samples two apart) keeps luma, and
/// chroma is what it takes out. `positive` is the line's subcarrier phase
/// as stored (a complemented copy is the opposite of its picture line's).
pub fn decode_single(samples: &[u8], positive: bool, out: &mut [u32]) {
    let n = samples.len().min(out.len());
    if n == 0 {
        return;
    }
    let s = if positive { 1.0 } else { -1.0 };
    let at = |p: isize| f32::from(samples[p.clamp(0, n as isize - 1) as usize]);
    let luma: Vec<f32> = (0..n as isize)
        .map(|p| (at(p - 2) + 2.0 * at(p) + at(p + 2)) / 4.0)
        .collect();
    // Chroma per sample, signed onto its axis: +I, +Q, +I, +Q.
    let chroma = |p: usize| {
        let p = p.min(n - 1);
        let c = (f32::from(samples[p]) - luma[p]) * s;
        if p % 4 < 2 {
            -c
        } else {
            c
        }
    };
    for p in 0..n {
        let y = (luma[p] - BLACK) / SCALE;
        let (i_at, q_at) = if p % 2 == 0 { (p, p + 1) } else { (p - 1, p) };
        // Each axis twice in a quad: average the pair.
        let i = (chroma(i_at) + chroma(i_at ^ 2)) / 2.0;
        let q = (chroma(q_at) + chroma(q_at ^ 2)) / 2.0;
        out[p] = rgba_of_yiq(y, i / SCALE, q / SCALE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8) -> u32 {
        u32::from_le_bytes([r, g, b, 0xFF])
    }

    #[test]
    fn a_single_copy_decodes_flat_colour() {
        for colour in [
            rgba(191, 191, 0),
            rgba(0, 191, 191),
            rgba(191, 0, 0),
            rgba(128, 128, 128),
        ] {
            let line = vec![colour; 64];
            let (mut n, mut c) = (vec![0u8; 64], vec![0u8; 64]);
            encode_line(&line, 7, &mut n, &mut c);
            let mut out = vec![0u32; 64];
            decode_single(&n, line_phase_positive(7), &mut out);
            let want = colour.to_le_bytes();
            for got in &out[8..56] {
                let got = got.to_le_bytes();
                for k in 0..3 {
                    assert!(
                        (i32::from(got[k]) - i32::from(want[k])).abs() <= 3,
                        "{got:?} vs {want:?}"
                    );
                }
            }
            decode_single(&c, !line_phase_positive(7), &mut out);
            let got = out[20].to_le_bytes();
            assert!((i32::from(got[0]) - i32::from(want[0])).abs() <= 3);
        }
    }

    #[test]
    fn encodes_like_the_colorbars_framestore() {
        // 75 % yellow on line 10 of ColorBars: samples 188, 126, 125, 187
        // normal, the complement shifted by half a cycle.
        let line = vec![rgba(191, 191, 0); 4];
        let (mut n, mut c) = ([0u8; 4], [0u8; 4]);
        encode_line(&line, 10, &mut n, &mut c);
        for (got, want) in n.iter().zip([188u8, 126, 125, 187]) {
            assert!((i32::from(*got) - i32::from(want)).abs() <= 2, "{n:?}");
        }
        assert_eq!([c[0], c[1], c[2], c[3]], [n[2], n[3], n[0], n[1]]);
        // The next line's subcarrier is inverted.
        let (mut n11, mut c11) = ([0u8; 4], [0u8; 4]);
        encode_line(&line, 11, &mut n11, &mut c11);
        assert_eq!(n11, c);
        // Black and 100 % white.
        let (mut b, mut bc) = ([0u8; 4], [0u8; 4]);
        encode_line(&[rgba(0, 0, 0); 4], 0, &mut b, &mut bc);
        assert_eq!(b, [70; 4]);
        encode_line(&[rgba(255, 255, 255); 4], 0, &mut b, &mut bc);
        assert_eq!(b, [200; 4]);
    }

    #[test]
    fn decode_inverts_encode() {
        for (line, colour) in [
            (10, rgba(191, 191, 0)),
            (11, rgba(0, 191, 191)),
            (400, rgba(191, 0, 191)),
            (3, rgba(30, 60, 200)),
        ] {
            let px = vec![colour; 16];
            let (mut n, mut c) = ([0u8; 16], [0u8; 16]);
            encode_line(&px, line, &mut n, &mut c);
            let mut out = [0u32; 16];
            decode_line(&n, &c, line, &mut out);
            for &o in &out[4..12] {
                for (a, b) in o.to_le_bytes().iter().zip(colour.to_le_bytes()) {
                    assert!(
                        (i32::from(*a) - i32::from(b)).abs() <= 6,
                        "{o:08X} vs {colour:08X}"
                    );
                }
            }
        }
    }
}

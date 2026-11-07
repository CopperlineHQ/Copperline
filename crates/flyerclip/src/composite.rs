// SPDX-License-Identifier: GPL-3.0-or-later

//! The Flyer's sample format: NTSC composite at 4fsc, one byte per sample,
//! blanking 60, black 70-71, white 200, the same as the Toaster framestore.
//! A quad of samples sits on -I, -Q, +I, +Q on lines of positive subcarrier
//! phase and on +I, +Q, -I, -Q otherwise.
//!
//! Field `f` (0-3 = I-IV) line `y` (0 = NTSC line 21 of field I) is
//! positive when `y + f + f/2` is even (FillLineActive, flyerlibvtasc.asm:
//! 1645-1671); fields I and IV share a phase, as do II and III
//! (FlyerProc.c:429). In a Toaster framestore it is picture line
//! `2y + (f & 1)`, in the normal copy for f < 2, the complemented one else.

pub use crate::vtasc::phase_positive;
use crate::vtasc::SAMPLES_PER_LINE;

/// Composite line from per-sample (Y, I, Q) in sample units (Y absolute,
/// e.g. 70 + 130*luma; I/Q signed amplitudes on the same scale).
pub fn line_from_yiq(yiq: impl Fn(usize) -> (f32, f32, f32), field: u32, line: usize) -> Vec<u8> {
    let s = if phase_positive(field, line) {
        1.0
    } else {
        -1.0
    };
    (0..SAMPLES_PER_LINE)
        .map(|x| {
            let (y, i, q) = yiq(x);
            let c = match x & 3 {
                0 => -i,
                1 => -q,
                2 => i,
                _ => q,
            } * s;
            (y + c).round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

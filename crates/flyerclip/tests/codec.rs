// SPDX-License-Identifier: GPL-3.0-or-later

//! VTASC codec tests: hardware-generated test vectors from the Flyer
//! firmware, then encode/decode round trips on synthetic composite fields.

use flyerclip::composite::line_from_yiq;
use flyerclip::vtasc::*;

/// The matte-black field the firmware builds in DRAM (CreateMatteField,
/// flyer/src/vid.c:4227-4267): 48 blocks of zero bits except two 8-byte
/// sequences, at 0x0000 and 0x00BF (vid.c:144-151). This is VTASC data as
/// the chips expect it, so it pins down the bitstream exactly.
fn matte(seq: &[u8; 16]) -> Vec<u8> {
    let mut m = vec![0u8; 48 * 512];
    m[..8].copy_from_slice(&seq[..8]);
    m[0xBF..0xBF + 8].copy_from_slice(&seq[8..]);
    m
}
const OLDBLACKSEQ: [u8; 16] = [
    0xC5, 0x80, 0x12, 0x21, 0x58, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const NEWBLACKSEQ: [u8; 16] = [
    0xC5, 0, 0, 0, 0, 0, 0, 0, 0xA0, 0x44, 0x08, 0xEE, 0, 0, 0, 0,
];

fn params(field: u32, algo: Algo, tolerance: u8, fir: Option<FirSet>) -> FieldParams {
    FieldParams {
        field,
        algo,
        tolerance,
        rnd_freq: 122,
        rnd_seed: (field % 255) as u8,
        fir,
    }
}

#[test]
fn firmware_matte_decodes_to_black() {
    for (seq, algo) in [(&NEWBLACKSEQ, Algo::D2), (&OLDBLACKSEQ, Algo::OldD2)] {
        let m = matte(seq);
        for field in 0..4 {
            let lines = decode_field_wide(&m, &params(field, algo, 0, None));
            assert_eq!(lines.len(), TOASTER_LINES);
            for (y, l) in lines.iter().enumerate() {
                assert!(
                    l.iter().all(|&s| s == NTSC_BLACK),
                    "{algo:?} field {field} line {y}: {:?}",
                    &l[..16]
                );
            }
            // With the playback FIR the interior stays black too.
            let lines = decode_field(&m, &params(field, algo, 0, fir_preset(2, true)));
            for l in &lines {
                assert!(l[8..SAMPLES_PER_LINE - 8].iter().all(|&s| s == NTSC_BLACK));
            }
        }
    }
}

#[test]
fn encoder_reproduces_firmware_matte_bitstream() {
    let black = vec![vec![NTSC_BLACK; SAMPLES_PER_LINE]; TOASTER_LINES];
    for (seq, algo) in [(&NEWBLACKSEQ, Algo::D2), (&OLDBLACKSEQ, Algo::OldD2)] {
        let want = matte(seq);
        for field in 0..4 {
            let got = encode_field(&black, &params(field, algo, 0, None));
            assert_eq!(got.len() % BITBUF_SIZE, 0);
            let n = got.len().min(want.len());
            assert_eq!(&got[..n], &want[..n], "{algo:?} field {field}");
            assert!(got[n..].iter().all(|&b| b == 0));
        }
    }
}

// ---------------------------------------------------------------- content

fn ramp_field(field: u32) -> Vec<Vec<u8>> {
    (0..TOASTER_LINES)
        .map(|y| {
            line_from_yiq(
                |x| (70.0 + 130.0 * x as f32 / 751.0 + (y % 7) as f32, 0.0, 0.0),
                field,
                y,
            )
        })
        .collect()
}

/// 75 % SMPTE colour bars, I/Q from YIQ of the RGB bars, on the 4fsc rule.
fn bars_field(field: u32) -> Vec<Vec<u8>> {
    let bars: [(f32, f32, f32); 8] = [
        (0.75, 0.75, 0.75),
        (0.75, 0.75, 0.0),
        (0.0, 0.75, 0.75),
        (0.0, 0.75, 0.0),
        (0.75, 0.0, 0.75),
        (0.75, 0.0, 0.0),
        (0.0, 0.0, 0.75),
        (0.0, 0.0, 0.0),
    ];
    let yiq: Vec<(f32, f32, f32)> = bars
        .iter()
        .map(|&(r, g, b)| {
            let y = 0.299 * r + 0.587 * g + 0.114 * b;
            let i = 0.596 * r - 0.274 * g - 0.322 * b;
            let q = 0.211 * r - 0.523 * g + 0.312 * b;
            (70.0 + 130.0 * y, 130.0 * i, 130.0 * q)
        })
        .collect();
    (0..TOASTER_LINES)
        .map(|y| line_from_yiq(|x| yiq[x * 8 / SAMPLES_PER_LINE], field, y))
        .collect()
}

/// Deterministic noise in 40..220 (stresses large errors and escapes).
fn noise_field() -> Vec<Vec<u8>> {
    let mut s = 0x1234_5678u32;
    (0..TOASTER_LINES)
        .map(|_| {
            (0..SAMPLES_PER_LINE)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    40 + (s % 181) as u8
                })
                .collect()
        })
        .collect()
}

struct Err {
    coded_max: u8,
    regen_max: u8,
    regen_mean: f64,
}

fn compare(a: &[Vec<u8>], b: &[Vec<u8>], p: &FieldParams, skip_edge: usize) -> Err {
    let (mut cm, mut rm, mut rs, mut rn) = (0u8, 0u8, 0f64, 0usize);
    for y in 0..TOASTER_LINES {
        for x in skip_edge..SAMPLES_PER_LINE - skip_edge {
            let d = a[y][x].abs_diff(b[y][x]);
            if sample_was_coded(p, y, x) {
                cm = cm.max(d);
            } else {
                rm = rm.max(d);
                rs += d as f64;
                rn += 1;
            }
        }
    }
    Err {
        coded_max: cm,
        regen_max: rm,
        regen_mean: if rn > 0 { rs / rn as f64 } else { 0.0 },
    }
}

#[test]
fn d2_round_trip_error_bounded_by_tolerance() {
    for field in 0..4 {
        for (name, src) in [
            ("ramp", ramp_field(field)),
            ("bars", bars_field(field)),
            ("noise", noise_field()),
        ] {
            for tol in 0..=3u8 {
                for algo in [Algo::D2, Algo::OldD2] {
                    let p = params(field, algo, tol, None);
                    let coded = encode_field(&src, &p);
                    let out = decode_field(&coded, &p);
                    let e = compare(&src, &out, &p, 0);
                    // Tolerance t drops t bits: error in [0, 2^t - 1]. The
                    // one exception: an error of -128 at t = 0 (the escape
                    // code) is coded as +-127.
                    let bound =
                        ((1u16 << tol) - 1).max(if tol == 0 && name == "noise" { 1 } else { 0 })
                            as u8;
                    assert!(
                        e.coded_max <= bound,
                        "{name} {algo:?} f{field} tol{tol}: max err {} > {bound}",
                        e.coded_max
                    );
                }
            }
        }
    }
}

#[test]
fn d2_tolerance_zero_is_lossless_on_video() {
    for field in 0..4 {
        for src in [ramp_field(field), bars_field(field)] {
            let p = params(field, Algo::D2, 0, None);
            assert_eq!(decode_field(&encode_field(&src, &p), &p), src);
        }
    }
}

#[test]
fn sub_nyquist_round_trip() {
    for field in 0..4 {
        for (name, src, mean_bound) in [
            ("ramp", ramp_field(field), 1.0),
            ("bars", bars_field(field), 2.5),
        ] {
            for tol in 0..=3u8 {
                let p = params(field, Algo::SubNyquist, tol, None);
                let coded = encode_field(&src, &p);
                let full = encode_field(&src, &params(field, Algo::D2, tol, None));
                assert!(coded.len() <= full.len(), "SN should not be bigger than D2");
                let out = decode_field(&coded, &p);
                let e = compare(&src, &out, &p, 0);
                let bound = (1u8 << tol) - 1;
                assert!(
                    e.coded_max <= bound,
                    "{name} SN f{field} tol{tol}: coded err {} > {bound}",
                    e.coded_max
                );
                // Regenerated samples: horizontal luma average + chroma
                // from the lines above and below; exact for flat colour.
                assert!(
                    e.regen_mean <= mean_bound + bound as f64,
                    "{name} SN f{field} tol{tol}: regen mean err {:.2} (max {})",
                    e.regen_mean,
                    e.regen_max
                );
            }
        }
    }
}

/// Mean absolute error over the interior (12 samples in from each end, where
/// the filters see NTSC_BLANK past the line).
fn interior_mean(a: &[Vec<u8>], b: &[Vec<u8>]) -> f64 {
    let n = (a.len() * (SAMPLES_PER_LINE - 24)) as f64;
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            x[12..SAMPLES_PER_LINE - 12]
                .iter()
                .zip(&y[12..SAMPLES_PER_LINE - 12])
                .map(|(p, q)| p.abs_diff(*q) as f64)
                .sum::<f64>()
        })
        .sum::<f64>()
        / n
}

/// Zero-phase amplitude of a FIR set at angular frequency w (rad/sample):
/// sum over taps of coef * 2cos(w(d+1/2)) / 256, centre tap = coefs[7].
fn fir_gain(f: &FirSet, w: f64) -> f64 {
    (0..8)
        .map(|d| {
            let c = (f.coefs[7 - d] & 0x3FF) as i32;
            let c = if c & 0x200 != 0 { c - 1024 } else { c } as f64;
            c * 2.0 * (w * (d as f64 + 0.5)).cos()
        })
        .sum::<f64>()
        / 256.0
}

#[test]
fn fir_preset_responses() {
    use std::f64::consts::PI;
    for set in 1..=4u8 {
        let (pre, post) = (
            fir_preset(set, false).unwrap(),
            fir_preset(set, true).unwrap(),
        );
        let dc = fir_gain(&pre, 0.0) * fir_gain(&post, 0.0);
        eprintln!(
            "set {set}: pre gain {:.3}@0 {:.3}@fsc/2 {:.3}@fsc; pre*post {:.3}@0 {:.3}@fsc/4 {:.3}@fsc/2 {:.3}@fsc",
            fir_gain(&pre, 0.0),
            fir_gain(&pre, PI / 4.0),
            fir_gain(&pre, PI / 2.0),
            dc,
            fir_gain(&pre, PI / 8.0) * fir_gain(&post, PI / 8.0),
            fir_gain(&pre, PI / 4.0) * fir_gain(&post, PI / 4.0),
            fir_gain(&pre, PI / 2.0) * fir_gain(&post, PI / 2.0),
        );
    }
    // Set 2 (the Flyer's fixed playback set): unity at DC, the pre filter
    // keeps ~27 % of fsc, the pair returns 1.16 x chroma.
    let (pre, post) = (fir_preset(2, false).unwrap(), fir_preset(2, true).unwrap());
    assert!((fir_gain(&pre, 0.0) - 1.0).abs() < 1e-9 && (fir_gain(&post, 0.0) - 1.0).abs() < 1e-9);
    let g =
        fir_gain(&pre, std::f64::consts::FRAC_PI_2) * fir_gain(&post, std::f64::consts::FRAC_PI_2);
    assert!((g - 1.157).abs() < 0.01, "{g}");
}

#[test]
fn fir_pre_post_round_trip() {
    for field in 0..4 {
        // (set, content, max mean abs error)
        for (set, name, src, mean_bound) in [
            (2u8, "ramp", ramp_field(field), 1.5),
            (2, "bars", bars_field(field), 6.0),
            (4, "ramp", ramp_field(field), 1.5),
            (4, "bars", bars_field(field), 1.5),
        ] {
            let enc = FieldParams {
                fir: fir_preset(set, false),
                ..params(field, Algo::D2, 0, None)
            };
            let dec = FieldParams {
                fir: fir_preset(set, true),
                ..enc
            };
            let out = decode_field(&encode_field(&src, &enc), &dec);
            let mean = interior_mean(&src, &out);
            eprintln!("FIR set {set} {name} f{field}: mean err {mean:.3}");
            assert!(
                mean <= mean_bound,
                "set {set} {name} f{field}: FIR round trip mean err {mean:.3}"
            );
        }
    }
}

#[test]
fn dither_is_subtractive() {
    // Same content, different seeds/frequencies: tolerance bound still holds.
    let src = bars_field(0);
    for (freq, seed) in [(122u8, 0u8), (13, 7), (0, 254), (255, 1)] {
        for tol in 1..=3u8 {
            let p = FieldParams {
                field: 0,
                algo: Algo::D2,
                tolerance: tol,
                rnd_freq: freq,
                rnd_seed: seed,
                fir: None,
            };
            let out = decode_field(&encode_field(&src, &p), &p);
            let e = compare(&src, &out, &p, 0);
            assert!(
                e.coded_max < (1 << tol),
                "freq {freq} seed {seed} tol {tol}: {}",
                e.coded_max
            );
            // Wrong seed on decode breaks the bound (the dither is really applied).
            let q = FieldParams {
                rnd_seed: seed.wrapping_add(1),
                ..p
            };
            let bad = decode_field(&encode_field(&src, &p), &q);
            assert_ne!(bad, out);
        }
    }
}

#[test]
fn compression_sizes_are_plausible() {
    // Report sizes in blocks; a Std field budget is 0x70 blocks.
    for (name, src) in [
        ("ramp", ramp_field(0)),
        ("bars", bars_field(0)),
        ("noise", noise_field()),
    ] {
        let mut row = vec![];
        for (algo, tol) in [
            (Algo::D2, 0),
            (Algo::D2, 1),
            (Algo::SubNyquist, 0),
            (Algo::SubNyquist, 1),
            (Algo::SubNyquist, 2),
        ] {
            let p = params(0, algo, tol, fir_preset(2, false));
            row.push(encode_field(&src, &p).len() / 512);
        }
        eprintln!("{name}: blocks D2/0 D2/1 SN/0 SN/1 SN/2 = {row:?}");
    }
}

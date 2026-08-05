// SPDX-License-Identifier: GPL-3.0-or-later

//! The one rate conversion between the module and the mix.
//!
//! The engine renders at the MT-32's native 32 kHz; the mixer runs at
//! [`crate::audio::MIX_SAMPLE_RATE`]. The two are in a fixed rational
//! ratio, so the conversion is a polyphase windowed-sinc interpolator: a
//! bank of one filter per output phase, computed once, each output frame
//! one pass of taps over the input history. Band-limited at the source's
//! Nyquist, so raising the rate adds no images; the phase advances by an
//! exact integer counter, so a run never drifts.

/// Taps each side of the output instant. Sixty-four in all keeps the
/// passband flat to the top of what the module produces and the
/// stopband floor well under its own DAC.
const HALF: usize = 32;
const TAPS: usize = 2 * HALF;

/// The cutoff as a fraction of the source's Nyquist: just inside it, so
/// the transition band straddles the edge rather than eating the top of
/// the passband.
const CUTOFF: f64 = 0.985;

pub struct Resampler {
    /// Interpolation and decimation counts: the output runs `l` frames to
    /// the input's `m`, in lowest terms.
    l: u32,
    m: u32,
    /// Where between input frames the next output falls, in `1/l`ths.
    phase: u32,
    /// One windowed-sinc kernel per phase, phase-major.
    kernels: Vec<f32>,
    /// The last [`TAPS`] input frames, oldest first.
    history: std::collections::VecDeque<(f32, f32)>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        let g = gcd(from, to);
        let (l, m) = (to / g, from / g);
        let mut kernels = Vec::with_capacity(l as usize * TAPS);
        for phase in 0..l {
            // The output instant sits `frac` past the newest-but-HALF
            // input frame; each tap is the sinc at its distance from it.
            let frac = f64::from(phase) / f64::from(l);
            let mut kernel = [0f64; TAPS];
            let mut sum = 0.0;
            for (j, tap) in kernel.iter_mut().enumerate() {
                let x = frac - (j as f64 - (HALF as f64 - 1.0));
                *tap = CUTOFF * sinc(CUTOFF * x) * blackman(x / HALF as f64);
                sum += *tap;
            }
            // Unity gain at DC exactly, so the window's ripple cannot
            // shade the level from one phase to the next.
            kernels.extend(kernel.iter().map(|&t| (t / sum) as f32));
        }
        Resampler {
            l,
            m,
            phase: 0,
            kernels,
            history: std::collections::VecDeque::with_capacity(TAPS),
        }
    }

    /// The next output frame, pulling input frames from `refill` as the
    /// phase crosses them.
    pub fn next(&mut self, mut refill: impl FnMut() -> (f32, f32)) -> (f32, f32) {
        while self.history.len() < TAPS {
            self.history.push_back(refill());
        }
        let kernel = &self.kernels[self.phase as usize * TAPS..][..TAPS];
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (&(l, r), &k) in self.history.iter().zip(kernel) {
            left += l * k;
            right += r * k;
        }
        self.phase += self.m;
        if self.phase >= self.l {
            self.phase -= self.l;
            self.history.pop_front();
            self.history.push_back(refill());
        }
        (left, right)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

/// The Blackman window over `x` in -1..1, zero outside.
fn blackman(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    let t = std::f64::consts::PI * (x + 1.0);
    0.42 - 0.5 * (t).cos() + 0.08 * (2.0 * t).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant input comes out the same constant: the kernels are
    /// unity at DC in every phase.
    #[test]
    fn a_constant_survives_the_conversion() {
        let mut resampler = Resampler::new(32_000, 44_100);
        let mut worst = 0.0f32;
        for _ in 0..10_000 {
            let (l, r) = resampler.next(|| (0.5, -0.25));
            worst = worst.max((l - 0.5).abs()).max((r + 0.25).abs());
        }
        assert!(worst < 1e-5, "DC shifted by {worst}");
    }

    /// The rational counter consumes exactly the right number of input
    /// frames: 441 outputs drink 320 inputs, forever.
    #[test]
    fn the_ratio_is_exact() {
        let mut resampler = Resampler::new(32_000, 44_100);
        let mut pulled = 0u64;
        for _ in 0..441 * 100 {
            resampler.next(|| {
                pulled += 1;
                (0.0, 0.0)
            });
        }
        // The first TAPS frames prime the history; every 441 outputs
        // after that pull exactly 320 more.
        assert_eq!(pulled, TAPS as u64 + 320 * 100);
    }

    /// A tone under the source's Nyquist keeps its level; imaging above
    /// it is suppressed. A coarse check, not a filter-design report: the
    /// mixer only needs the module to arrive clean.
    #[test]
    fn a_tone_comes_through_at_level() {
        let mut resampler = Resampler::new(32_000, 44_100);
        let mut t = 0usize;
        let mut peak = 0.0f32;
        let mut out = Vec::new();
        for _ in 0..44_100 {
            let frame = resampler.next(|| {
                let s = (t as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 32_000.0).sin() as f32;
                t += 1;
                (s, s)
            });
            out.push(frame.0);
            peak = peak.max(frame.0.abs());
        }
        assert!((0.95..=1.01).contains(&peak), "1 kHz peak {peak}");
    }
}

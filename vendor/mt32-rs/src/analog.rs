// SPDX-License-Identifier: LGPL-2.1-or-later

//! The analogue stage between the DAC and the jacks: the sample-and-hold
//! circuit and the low-pass filter, as the reference approximates them.
//!
//! Four models. Digital-only bypasses the circuit and leaves the native
//! rate untouched. Coarse runs a nine-tap integer FIR at the native rate:
//! the audible shape of the filter without its imaging. Accurate carries
//! the full character -- a forty-nine tap polyphase FIR that interpolates
//! by three and keeps every second sample, so the output leaves at
//! 48 kHz with the mirror spectra the real converter let through.
//! Oversampled is the same filter keeping every sample, 96 kHz.
//!
//! Two circuits: the first-generation MT-32's, and the revision in the
//! later units and the CM-32L line. Both sets of taps are the reference
//! project's, fitted to measurements of the real hardware.
//!
//! The output gains live here, as they do in the hardware's output
//! stage: eight fraction bits, synth and reverb channels separate.

/// Which analogue model the module runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AnalogMode {
    /// No circuit at all: the LA32's stream as it stands, 32 kHz.
    #[default]
    DigitalOnly,
    /// The filter's audible shape at the native rate.
    Coarse,
    /// The whole circuit, imaging included, at 48 kHz.
    Accurate,
    /// The whole circuit with nothing decimated, at 96 kHz.
    Oversampled,
}

/// The first generation's sample-and-hold plus filter, nine taps in
/// fourteen fraction bits.
static COARSE_TAPS_MT32: [i32; 9] = [20848, -3609, -2589, 2943, -1827, 887, -385, 180, -114];

/// The later units' circuit: unity DC gain once the amplifier joined.
static COARSE_TAPS_CM32L: [i32; 9] = [21965, -6608, 590, 1084, -1142, 812, -510, 314, -204];

const COARSE_FRACTION_BITS: u32 = 14;
const COARSE_DELAY: usize = 8;

/// The full-circuit FIR for the first generation: forty-nine taps over
/// sixteen delay slots in three phases.
#[allow(clippy::excessive_precision)] // the reference's own digits
static ACCURATE_TAPS_MT32: [f32; 49] = [
    0.003429281,
    0.025929869,
    0.096587777,
    0.228884848,
    0.372413431,
    0.412386503,
    0.263980018,
    -0.014504962,
    -0.237394528,
    -0.257043496,
    -0.103436603,
    0.063996095,
    0.124562333,
    0.083703206,
    0.013921662,
    -0.033475018,
    -0.046239712,
    -0.029310921,
    0.00126585,
    0.021060961,
    0.017925605,
    0.003559874,
    -0.005105248,
    -0.005647917,
    -0.004157918,
    -0.002065664,
    0.00158747,
    0.003762585,
    0.001867137,
    -0.001090028,
    -0.001433979,
    -0.00022367,
    4.34308e-5,
    -0.000247827,
    0.000157087,
    0.000605823,
    0.000197317,
    -0.000370511,
    -0.000261202,
    9.96069e-5,
    9.85073e-5,
    -5.28754e-5,
    -1.00912e-5,
    7.69943e-5,
    2.03162e-5,
    -5.67967e-5,
    -3.30637e-5,
    1.61958e-5,
    1.73041e-5,
];

/// And for the later circuit.
#[allow(clippy::excessive_precision)] // the reference's own digits
static ACCURATE_TAPS_CM32L: [f32; 49] = [
    0.003917452,
    0.030693861,
    0.116424199,
    0.275101674,
    0.43217361,
    0.431247894,
    0.183255659,
    -0.174955671,
    -0.354240244,
    -0.212401714,
    0.072259178,
    0.204655344,
    0.108336211,
    -0.039099027,
    -0.075138174,
    -0.026261906,
    0.00582663,
    0.003052193,
    0.00613657,
    0.017017951,
    0.008732535,
    -0.011027427,
    -0.012933664,
    0.001158097,
    0.006765958,
    0.00046778,
    -0.002191106,
    0.001561017,
    0.001842871,
    -0.001996876,
    -0.002315836,
    0.000980965,
    0.001817454,
    -0.000243272,
    -0.000972848,
    0.000149941,
    0.000498886,
    -0.000204436,
    -0.000347415,
    0.000142386,
    0.000249137,
    -4.32946e-5,
    -0.000131231,
    3.88575e-7,
    4.48813e-5,
    -1.31906e-6,
    -1.03499e-5,
    7.71971e-6,
    2.86721e-6,
];

const ACCURATE_DELAY: usize = 16;
const ACCURATE_PHASES: u32 = 3;

/// How many more input frames each leftover output frame needs, by the
/// filter's current phase: the regular two-of-three decimation, and the
/// oversampled run that keeps everything.
static DELTAS_REGULAR: [[u32; 3]; 3] = [[0, 0, 0], [1, 1, 0], [1, 2, 1]];
static DELTAS_OVERSAMPLED: [[u32; 3]; 3] = [[0, 0, 0], [1, 0, 0], [1, 0, 1]];

/// Eight fraction bits on the output gains, as the reference keeps them.
const GAIN_FRACTION_BITS: u32 = 8;

/// The streams the digital half hands over: what bypasses the reverb,
/// what fed it, and what came back wet. All at the native rate.
#[derive(Default)]
pub struct Streams {
    pub non_l: Vec<i16>,
    pub non_r: Vec<i16>,
    pub dry_l: Vec<i16>,
    pub dry_r: Vec<i16>,
    pub wet_l: Vec<i16>,
    pub wet_r: Vec<i16>,
}

impl Streams {
    /// Silence through the first `len` frames, growing to fit.
    pub fn clear(&mut self, len: usize) {
        for stream in [
            &mut self.non_l,
            &mut self.non_r,
            &mut self.dry_l,
            &mut self.dry_r,
            &mut self.wet_l,
            &mut self.wet_r,
        ] {
            stream.clear();
            stream.resize(len, 0);
        }
    }
}

/// One channel's filter.
enum Lpf {
    Null,
    Coarse {
        taps: &'static [i32; 9],
        ring: [i32; COARSE_DELAY],
        pos: usize,
    },
    Accurate {
        taps: &'static [f32; 49],
        deltas: &'static [[u32; 3]; 3],
        increment: u32,
        ring: [f32; ACCURATE_DELAY],
        pos: usize,
        phase: u32,
    },
}

impl Lpf {
    fn new(mode: AnalogMode, old_mt32_lpf: bool) -> Lpf {
        match mode {
            AnalogMode::DigitalOnly => Lpf::Null,
            AnalogMode::Coarse => Lpf::Coarse {
                taps: if old_mt32_lpf {
                    &COARSE_TAPS_MT32
                } else {
                    &COARSE_TAPS_CM32L
                },
                ring: [0; COARSE_DELAY],
                pos: 0,
            },
            AnalogMode::Accurate | AnalogMode::Oversampled => Lpf::Accurate {
                taps: if old_mt32_lpf {
                    &ACCURATE_TAPS_MT32
                } else {
                    &ACCURATE_TAPS_CM32L
                },
                deltas: if mode == AnalogMode::Oversampled {
                    &DELTAS_OVERSAMPLED
                } else {
                    &DELTAS_REGULAR
                },
                increment: if mode == AnalogMode::Oversampled {
                    1
                } else {
                    2
                },
                ring: [0.0; ACCURATE_DELAY],
                pos: 0,
                phase: 0,
            },
        }
    }

    /// Whether the filter has an output ready before taking more input:
    /// the interpolated frames between the real ones.
    fn has_next_sample(&self) -> bool {
        match self {
            Lpf::Accurate {
                phase, increment, ..
            } => *increment <= *phase,
            _ => false,
        }
    }

    fn process(&mut self, sample: i32) -> i32 {
        match self {
            Lpf::Null => sample,
            Lpf::Coarse { taps, ring, pos } => {
                // The ninth tap catches the sample about to leave the
                // delay line; new input is clipped in, as the reference
                // does it.
                let mut sum = i64::from(taps[COARSE_DELAY]) * i64::from(ring[*pos]);
                ring[*pos] = i32::from(clip(sample));
                for (i, &tap) in taps.iter().take(COARSE_DELAY).enumerate() {
                    sum += i64::from(tap) * i64::from(ring[(i + *pos) & (COARSE_DELAY - 1)]);
                }
                *pos = pos.wrapping_sub(1) & (COARSE_DELAY - 1);
                (sum >> COARSE_FRACTION_BITS) as i32
            }
            Lpf::Accurate {
                taps,
                increment,
                ring,
                pos,
                phase,
                ..
            } => {
                let mask = ACCURATE_DELAY - 1;
                // Phase zero reaches one tap beyond the delay line, at
                // the slot the new sample is about to reclaim.
                let mut sum = if *phase == 0 {
                    taps[ACCURATE_DELAY * ACCURATE_PHASES as usize] * ring[*pos]
                } else {
                    0.0
                };
                if *phase < *increment {
                    ring[*pos] = sample as f32;
                }
                let mut tap_ix = *phase as usize;
                for slot in 0..ACCURATE_DELAY {
                    sum += taps[tap_ix] * ring[(slot + *pos) & mask];
                    tap_ix += ACCURATE_PHASES as usize;
                }
                *phase += *increment;
                if ACCURATE_PHASES <= *phase {
                    *phase -= ACCURATE_PHASES;
                    *pos = pos.wrapping_sub(1) & mask;
                }
                (ACCURATE_PHASES as f32 * sum) as i32
            }
        }
    }

    fn output_sample_rate(&self) -> u32 {
        match self {
            Lpf::Accurate { increment, .. } => crate::SAMPLE_RATE * ACCURATE_PHASES / *increment,
            _ => crate::SAMPLE_RATE,
        }
    }

    /// How many input frames `out_len` output frames will drink, from
    /// the phase the filter stands at.
    fn in_sample_count(&self, out_len: usize) -> usize {
        match self {
            Lpf::Accurate {
                deltas,
                increment,
                phase,
                ..
            } => {
                let cycles = out_len / ACCURATE_PHASES as usize;
                let remainder = out_len - cycles * ACCURATE_PHASES as usize;
                cycles * *increment as usize + deltas[remainder][*phase as usize] as usize
            }
            _ => out_len,
        }
    }

    /// Advance past `out_len` output frames without producing them, for
    /// the stretches the module renders shut down.
    fn add_position_increment(&mut self, out_len: usize) {
        if let Lpf::Accurate {
            increment, phase, ..
        } = self
        {
            *phase = (*phase + out_len as u32 * *increment) % ACCURATE_PHASES;
        }
    }
}

/// The stage itself: one filter per channel, and the output gains.
pub struct Analog {
    left: Lpf,
    right: Lpf,
    /// Eight-bit fixed point. The wet gain carries the CM-32L low-pass
    /// compensation factor on every model, the MT-32 flavours included:
    /// the reference asks its reverb-compatibility question before it
    /// counts itself as opened, so the answer is always no. Mirrored
    /// for bit-identity.
    synth_gain: i32,
    reverb_gain: i32,
}

impl Analog {
    pub fn new(mode: AnalogMode, old_mt32_lpf: bool) -> Analog {
        Analog {
            left: Lpf::new(mode, old_mt32_lpf),
            right: Lpf::new(mode, old_mt32_lpf),
            synth_gain: 1 << GAIN_FRACTION_BITS,
            reverb_gain: (0.68 * (1 << GAIN_FRACTION_BITS) as f32) as i32,
        }
    }

    /// The rate frames leave the jacks at.
    pub fn output_sample_rate(&self) -> u32 {
        self.left.output_sample_rate()
    }

    /// How many native frames the digital half must render for `out_len`
    /// output frames.
    pub fn dac_streams_length(&self, out_len: usize) -> usize {
        self.left.in_sample_count(out_len)
    }

    /// The stretches rendered shut down still move the filters' clock.
    pub fn skip(&mut self, out_len: usize) {
        self.left.add_position_increment(out_len);
        self.right.add_position_increment(out_len);
    }

    /// One block through the stage: combine the streams under the gains,
    /// filter, clip to the jacks.
    pub fn process(&mut self, out: &mut [(i16, i16)], streams: &Streams) {
        let mut at = 0;
        for frame in out.iter_mut() {
            let (left, right) = if self.left.has_next_sample() {
                (self.left.process(0), self.right.process(0))
            } else {
                let in_l = (i32::from(streams.non_l[at]) + i32::from(streams.dry_l[at]))
                    * self.synth_gain
                    + i32::from(streams.wet_l[at]) * self.reverb_gain;
                let in_r = (i32::from(streams.non_r[at]) + i32::from(streams.dry_r[at]))
                    * self.synth_gain
                    + i32::from(streams.wet_r[at]) * self.reverb_gain;
                at += 1;
                (
                    self.left.process(in_l >> GAIN_FRACTION_BITS),
                    self.right.process(in_r >> GAIN_FRACTION_BITS),
                )
            };
            *frame = (clip(left), clip(right));
        }
    }
}

/// Into range, or the saturation with the sign's own limit.
pub(crate) fn clip(sample: i32) -> i16 {
    if (-0x8000..=0x7FFF).contains(&sample) {
        sample as i16
    } else {
        ((sample >> 31) ^ 0x7FFF) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digital-only is a wire: the combine and the clip, nothing else,
    /// one output frame per input frame at the native rate.
    #[test]
    fn digital_only_is_a_wire() {
        let mut analog = Analog::new(AnalogMode::DigitalOnly, false);
        assert_eq!(analog.output_sample_rate(), 32_000);
        assert_eq!(analog.dac_streams_length(100), 100);
        let mut streams = Streams::default();
        streams.clear(4);
        streams.non_l[0] = 1000;
        streams.dry_l[0] = 500;
        streams.wet_l[0] = -400;
        streams.non_r[1] = -30000;
        streams.dry_r[1] = -30000;
        let mut out = [(0i16, 0i16); 4];
        analog.process(&mut out, &streams);
        // (1000 + 500) * 256 + (-400) * 174 >> 8 = 1500 - 272.
        assert_eq!(out[0].0, 1228);
        assert_eq!(out[1].1, -32768, "the sum clips at the jack");
    }

    /// The accurate filter leaves at forty-eight kilohertz, three output
    /// frames for every two in, and its appetite follows its phase.
    #[test]
    fn the_accurate_filter_steps_three_for_two() {
        let mut analog = Analog::new(AnalogMode::Accurate, false);
        assert_eq!(analog.output_sample_rate(), 48_000);
        let mut consumed = 0;
        for _ in 0..100 {
            let n = analog.dac_streams_length(3);
            consumed += n;
            let mut streams = Streams::default();
            streams.clear(n);
            let mut out = [(0i16, 0i16); 3];
            analog.process(&mut out, &streams);
        }
        assert_eq!(consumed, 200, "three out for every two in");
    }

    /// A skipped stretch moves the phase exactly as processing it would.
    #[test]
    fn skipping_keeps_the_phase() {
        let mut skipped = Analog::new(AnalogMode::Accurate, true);
        let mut rendered = Analog::new(AnalogMode::Accurate, true);
        let mut streams = Streams::default();
        for out_len in [1usize, 2, 5, 3, 7] {
            let n = rendered.dac_streams_length(out_len);
            streams.clear(n);
            let mut out = vec![(0i16, 0i16); out_len];
            rendered.process(&mut out, &streams);
            skipped.skip(out_len);
            assert_eq!(
                skipped.dac_streams_length(3),
                rendered.dac_streams_length(3),
                "after {out_len} more frames"
            );
        }
    }
}

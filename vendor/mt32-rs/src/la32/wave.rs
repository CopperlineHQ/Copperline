// SPDX-License-Identifier: LGPL-2.1-or-later

//! The LA32 wave generator: square, sawtooth and PCM partials, computed in
//! the log-space the chip works in.
//!
//! A square wave is two sine segments joined by linear runs, their lengths
//! set by the cutoff and pulse width; resonance is a decaying sine laid on
//! top, windowed by cosine at its ends; a sawtooth is the square multiplied
//! by a synchronous cosine. PCM partials read the unwoven sample store and
//! interpolate between neighbours. Everything stays logarithmic -- a 16-bit
//! value with a 12-bit fraction, plus a sign -- until the pair mixer unlogs
//! and combines, exactly where the reference does.

use crate::tables::Tables;

const SINE_SEGMENT_RELATIVE_LENGTH: u32 = 1 << 18;
const MIDDLE_CUTOFF_VALUE: u32 = 128 << 18;
const RESONANCE_DECAY_THRESHOLD_CUTOFF_VALUE: u32 = 144 << 18;
const MAX_CUTOFF_VALUE: u32 = 240 << 18;

/// A sample in the chip's log-space: no sign in the value, the sign beside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSample {
    pub log_value: u16,
    pub negative: bool,
}

const SILENCE: LogSample = LogSample {
    log_value: 65535,
    negative: false,
};

/// The exponent table, interpolated between entries by the low three bits.
fn interpolate_exp(tables: &Tables, fract: u16) -> u16 {
    let index = usize::from(fract >> 3);
    let extra_bits = u32::from(!fract & 7);
    let entry2 = 8191 - u32::from(tables.exp9[index]);
    let entry1 = if index == 0 {
        8191
    } else {
        8191 - u32::from(tables.exp9[index - 1])
    };
    (entry2 + (((entry1 - entry2) * extra_bits) >> 3)) as u16
}

/// Out of the log-space: the linear sample the DAC would see.
pub fn unlog(tables: &Tables, log_sample: LogSample) -> i16 {
    let int_log = u32::from(log_sample.log_value) >> 12;
    let frac_log = log_sample.log_value & 4095;
    let sample = (interpolate_exp(tables, frac_log) >> int_log) as i16;
    if log_sample.negative {
        -sample
    } else {
        sample
    }
}

/// Log-space addition is linear-space multiplication: values add and
/// saturate, signs multiply.
fn add_log_samples(a: &mut LogSample, b: LogSample) {
    let sum = u32::from(a.log_value) + u32::from(b.log_value);
    a.log_value = if sum < 65536 { sum as u16 } else { 65535 };
    a.negative = a.negative != b.negative;
}

// The square wave's six segments, in phase order.
const POSITIVE_RISING_SINE_SEGMENT: u32 = 0;
const POSITIVE_LINEAR_SEGMENT: u32 = 1;
const POSITIVE_FALLING_SINE_SEGMENT: u32 = 2;
const NEGATIVE_FALLING_SINE_SEGMENT: u32 = 3;
const NEGATIVE_LINEAR_SEGMENT: u32 = 4;
const NEGATIVE_RISING_SINE_SEGMENT: u32 = 5;

// The resonance sine's four, likewise.
const POSITIVE_FALLING_RESONANCE_SINE_SEGMENT: u32 = 1;
const NEGATIVE_FALLING_RESONANCE_SINE_SEGMENT: u32 = 2;
const NEGATIVE_RISING_RESONANCE_SINE_SEGMENT: u32 = 3;

/// One wave generator: half of a partial pair.
#[derive(Debug, Clone)]
pub struct WaveGenerator {
    active: bool,
    sawtooth_waveform: bool,
    amp: u32,
    pitch: u16,
    pulse_width: u8,
    cutoff_val: u32,
    /// A PCM partial's window into the sample store, `None` for synth.
    pcm: Option<PcmState>,
    wave_position: u32,
    square_wave_position: u32,
    resonance_sine_position: u32,
    resonance_amp_subtraction: u32,
    res_amp_decay_factor: u32,
    pcm_interpolation_factor: u32,
    phase: u32,
    resonance_phase: u32,
    square_log_sample: LogSample,
    resonance_log_sample: LogSample,
    first_pcm_log_sample: LogSample,
    second_pcm_log_sample: LogSample,
}

#[derive(Debug, Clone, Copy)]
struct PcmState {
    /// Where the wave starts in the sample store, in samples.
    addr: u32,
    len: u32,
    looped: bool,
    /// False for the slave of a ring-modulated pair, whose interpolation
    /// circuitry the modulator borrows.
    interpolated: bool,
}

impl WaveGenerator {
    pub fn new() -> WaveGenerator {
        WaveGenerator {
            active: false,
            sawtooth_waveform: false,
            amp: 0,
            pitch: 0,
            pulse_width: 0,
            cutoff_val: 0,
            pcm: None,
            wave_position: 0,
            square_wave_position: 0,
            resonance_sine_position: 0,
            resonance_amp_subtraction: 0,
            res_amp_decay_factor: 0,
            pcm_interpolation_factor: 0,
            phase: POSITIVE_RISING_SINE_SEGMENT,
            resonance_phase: 0,
            square_log_sample: SILENCE,
            resonance_log_sample: SILENCE,
            first_pcm_log_sample: SILENCE,
            second_pcm_log_sample: SILENCE,
        }
    }

    /// Set up for a synth partial: square or sawtooth, with the invariant
    /// pulse width and resonance.
    pub fn init_synth(
        &mut self,
        tables: &Tables,
        sawtooth_waveform: bool,
        pulse_width: u8,
        resonance: u8,
    ) {
        self.sawtooth_waveform = sawtooth_waveform;
        self.pulse_width = pulse_width;
        self.wave_position = 0;
        self.square_wave_position = 0;
        self.phase = POSITIVE_RISING_SINE_SEGMENT;
        self.resonance_sine_position = 0;
        self.resonance_phase = 0;
        self.resonance_amp_subtraction = (32 - u32::from(resonance)) << 10;
        self.res_amp_decay_factor =
            u32::from(tables.res_amp_decay_factors[usize::from(resonance >> 2)]) << 2;
        self.pcm = None;
        self.active = true;
    }

    /// Set up for a PCM partial reading `len` samples at `addr`.
    pub fn init_pcm(&mut self, addr: u32, len: u32, looped: bool, interpolated: bool) {
        self.pcm = Some(PcmState {
            addr,
            len,
            looped,
            interpolated,
        });
        self.wave_position = 0;
        self.active = true;
    }

    fn sample_step(&self, tables: &Tables) -> u32 {
        let mut step = u32::from(interpolate_exp(tables, !self.pitch & 4095));
        step <<= self.pitch >> 12;
        step >>= 8;
        step &= !1;
        step
    }

    fn resonance_wave_length_factor(&self, tables: &Tables, effective_cutoff: u32) -> u32 {
        let mut factor = u32::from(interpolate_exp(tables, (!effective_cutoff & 4095) as u16));
        factor <<= effective_cutoff >> 12;
        factor
    }

    fn high_linear_length(&self, tables: &Tables, effective_cutoff: u32) -> u32 {
        let effective_pulse_width = if self.pulse_width > 128 {
            u32::from(self.pulse_width - 128) << 6
        } else {
            0
        };
        if effective_pulse_width < effective_cutoff {
            let exp_arg = effective_cutoff - effective_pulse_width;
            let mut length = u32::from(interpolate_exp(tables, (!exp_arg & 4095) as u16));
            length <<= 7 + (exp_arg >> 12);
            length.wrapping_sub(2 * SINE_SEGMENT_RELATIVE_LENGTH)
        } else {
            0
        }
    }

    fn compute_positions(
        &mut self,
        high_linear_length: u32,
        low_linear_length: u32,
        resonance_wave_length_factor: u32,
    ) {
        self.square_wave_position =
            (self.wave_position >> 8).wrapping_mul(resonance_wave_length_factor >> 4);
        self.resonance_sine_position = self.square_wave_position;
        if self.square_wave_position < SINE_SEGMENT_RELATIVE_LENGTH {
            self.phase = POSITIVE_RISING_SINE_SEGMENT;
            return;
        }
        self.square_wave_position -= SINE_SEGMENT_RELATIVE_LENGTH;
        if self.square_wave_position < high_linear_length {
            self.phase = POSITIVE_LINEAR_SEGMENT;
            return;
        }
        self.square_wave_position = self.square_wave_position.wrapping_sub(high_linear_length);
        if self.square_wave_position < SINE_SEGMENT_RELATIVE_LENGTH {
            self.phase = POSITIVE_FALLING_SINE_SEGMENT;
            return;
        }
        self.square_wave_position -= SINE_SEGMENT_RELATIVE_LENGTH;
        self.resonance_sine_position = self.square_wave_position;
        if self.square_wave_position < SINE_SEGMENT_RELATIVE_LENGTH {
            self.phase = NEGATIVE_FALLING_SINE_SEGMENT;
            return;
        }
        self.square_wave_position -= SINE_SEGMENT_RELATIVE_LENGTH;
        if self.square_wave_position < low_linear_length {
            self.phase = NEGATIVE_LINEAR_SEGMENT;
            return;
        }
        self.square_wave_position = self.square_wave_position.wrapping_sub(low_linear_length);
        self.phase = NEGATIVE_RISING_SINE_SEGMENT;
    }

    fn advance_position(&mut self, tables: &Tables) {
        self.wave_position += self.sample_step(tables);
        self.wave_position %= 4 * SINE_SEGMENT_RELATIVE_LENGTH;
        let effective_cutoff = if self.cutoff_val > MIDDLE_CUTOFF_VALUE {
            (self.cutoff_val - MIDDLE_CUTOFF_VALUE) >> 10
        } else {
            0
        };
        let resonance_wave_length_factor =
            self.resonance_wave_length_factor(tables, effective_cutoff);
        let high_linear_length = self.high_linear_length(tables, effective_cutoff);
        let low_linear_length = (resonance_wave_length_factor << 8)
            .wrapping_sub(4 * SINE_SEGMENT_RELATIVE_LENGTH)
            .wrapping_sub(high_linear_length);
        self.compute_positions(
            high_linear_length,
            low_linear_length,
            resonance_wave_length_factor,
        );
        self.resonance_phase = ((self.resonance_sine_position >> 18)
            + if self.phase > POSITIVE_FALLING_SINE_SEGMENT {
                2
            } else {
                0
            })
            & 3;
    }

    fn generate_next_square_wave_log_sample(&mut self, tables: &Tables) {
        let mut value = match self.phase {
            POSITIVE_RISING_SINE_SEGMENT | NEGATIVE_FALLING_SINE_SEGMENT => {
                u32::from(tables.logsin9[((self.square_wave_position >> 9) & 511) as usize])
            }
            POSITIVE_FALLING_SINE_SEGMENT | NEGATIVE_RISING_SINE_SEGMENT => {
                u32::from(tables.logsin9[((!(self.square_wave_position >> 9)) & 511) as usize])
            }
            _ => 0,
        };
        value <<= 2;
        value += self.amp >> 10;
        if self.cutoff_val < MIDDLE_CUTOFF_VALUE {
            value += (MIDDLE_CUTOFF_VALUE - self.cutoff_val) >> 9;
        }
        self.square_log_sample = LogSample {
            log_value: if value < 65536 { value as u16 } else { 65535 },
            negative: self.phase >= NEGATIVE_FALLING_SINE_SEGMENT,
        };
    }

    fn generate_next_resonance_wave_log_sample(&mut self, tables: &Tables) {
        let mut value = if self.resonance_phase == POSITIVE_FALLING_RESONANCE_SINE_SEGMENT
            || self.resonance_phase == NEGATIVE_RISING_RESONANCE_SINE_SEGMENT
        {
            u32::from(tables.logsin9[((!(self.resonance_sine_position >> 9)) & 511) as usize])
        } else {
            u32::from(tables.logsin9[((self.resonance_sine_position >> 9) & 511) as usize])
        };
        value <<= 2;
        value += self.amp >> 10;
        let decay_factor = if self.phase < NEGATIVE_FALLING_SINE_SEGMENT {
            self.res_amp_decay_factor
        } else {
            self.res_amp_decay_factor + 1
        };
        value += self.resonance_amp_subtraction
            + (((self.resonance_sine_position >> 4).wrapping_mul(decay_factor)) >> 8);
        if self.phase == POSITIVE_RISING_SINE_SEGMENT || self.phase == NEGATIVE_FALLING_SINE_SEGMENT
        {
            value +=
                u32::from(tables.logsin9[((self.square_wave_position >> 9) & 511) as usize]) << 2;
        } else if self.phase == POSITIVE_FALLING_SINE_SEGMENT
            || self.phase == NEGATIVE_RISING_SINE_SEGMENT
        {
            value +=
                u32::from(tables.logsin9[((!(self.square_wave_position >> 9)) & 511) as usize])
                    << 3;
        }
        if self.cutoff_val < MIDDLE_CUTOFF_VALUE {
            value += 31743 + ((MIDDLE_CUTOFF_VALUE - self.cutoff_val) >> 9);
        } else if self.cutoff_val < RESONANCE_DECAY_THRESHOLD_CUTOFF_VALUE {
            let sine_ix = (self.cutoff_val - MIDDLE_CUTOFF_VALUE) >> 13;
            value += u32::from(tables.logsin9[sine_ix as usize]) << 2;
        }
        value = value.wrapping_sub(1 << 12);
        self.resonance_log_sample = LogSample {
            log_value: if value < 65536 { value as u16 } else { 65535 },
            negative: self.resonance_phase >= NEGATIVE_FALLING_RESONANCE_SINE_SEGMENT,
        };
    }

    fn generate_next_sawtooth_cosine_log_sample(&self, tables: &Tables) -> LogSample {
        let position = self.wave_position.wrapping_add(1 << 18);
        let log_value = if position & (1 << 18) != 0 {
            tables.logsin9[((!(position >> 9)) & 511) as usize]
        } else {
            tables.logsin9[((position >> 9) & 511) as usize]
        };
        LogSample {
            log_value: log_value << 2,
            negative: position & (1 << 19) != 0,
        }
    }

    fn pcm_sample_to_log_sample(&self, pcm_sample: i16) -> LogSample {
        let mut value = u32::from(32787 - (pcm_sample as u16 & 32767)) << 1;
        value += self.amp >> 10;
        LogSample {
            log_value: if value < 65536 { value as u16 } else { 65535 },
            negative: pcm_sample < 0,
        }
    }

    fn generate_next_pcm_wave_log_samples(&mut self, tables: &Tables, pcm: &[i16]) {
        let state = self.pcm.expect("only called for PCM waves");
        self.pcm_interpolation_factor = (self.wave_position & 255) >> 1;
        let mut table_ix = self.wave_position >> 8;
        self.first_pcm_log_sample =
            self.pcm_sample_to_log_sample(pcm[(state.addr + table_ix) as usize]);
        if state.interpolated {
            table_ix += 1;
            if table_ix < state.len {
                self.second_pcm_log_sample =
                    self.pcm_sample_to_log_sample(pcm[(state.addr + table_ix) as usize]);
            } else if state.looped {
                table_ix -= state.len;
                self.second_pcm_log_sample =
                    self.pcm_sample_to_log_sample(pcm[(state.addr + table_ix) as usize]);
            } else {
                self.second_pcm_log_sample = SILENCE;
            }
        } else {
            self.second_pcm_log_sample = SILENCE;
        }
        let mut step = u32::from(interpolate_exp(tables, !self.pitch & 4095));
        step <<= self.pitch >> 12;
        step >>= 9;
        self.wave_position += step;
        if self.wave_position >= state.len << 8 {
            if state.looped {
                self.wave_position -= state.len << 8;
            } else {
                self.deactivate();
            }
        }
    }

    /// One sample's worth of work at this amp, pitch and cutoff, leaving
    /// the pair of log samples ready to take.
    pub fn generate_next_sample(
        &mut self,
        tables: &Tables,
        pcm: &[i16],
        amp: u32,
        pitch: u16,
        cutoff_val: u32,
    ) {
        if !self.active {
            return;
        }
        self.amp = amp;
        self.pitch = pitch;
        if self.pcm.is_some() {
            self.generate_next_pcm_wave_log_samples(tables, pcm);
            return;
        }
        self.cutoff_val = cutoff_val.min(MAX_CUTOFF_VALUE);
        self.generate_next_square_wave_log_sample(tables);
        self.generate_next_resonance_wave_log_sample(tables);
        if self.sawtooth_waveform {
            let cosine = self.generate_next_sawtooth_cosine_log_sample(tables);
            add_log_samples(&mut self.square_log_sample, cosine);
            add_log_samples(&mut self.resonance_log_sample, cosine);
        }
        self.advance_position(tables);
    }

    /// The two log components to mix: square and resonance for synth,
    /// this sample and the next for PCM.
    pub fn output_log_sample(&self, first: bool) -> LogSample {
        if !self.active {
            return SILENCE;
        }
        if self.pcm.is_some() {
            if first {
                self.first_pcm_log_sample
            } else {
                self.second_pcm_log_sample
            }
        } else if first {
            self.square_log_sample
        } else {
            self.resonance_log_sample
        }
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_pcm_wave(&self) -> bool {
        self.pcm.is_some()
    }
}

impl Default for WaveGenerator {
    fn default() -> WaveGenerator {
        WaveGenerator::new()
    }
}

/// Which half of a pair a call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pair {
    Master,
    Slave,
}

/// Two wave generators, mixed or ring modulated, as the chip pairs its
/// partials.
#[derive(Debug, Clone, Default)]
pub struct IntPartialPair {
    master: WaveGenerator,
    slave: WaveGenerator,
    ring_modulated: bool,
    mixed: bool,
}

impl IntPartialPair {
    pub fn new() -> IntPartialPair {
        IntPartialPair::default()
    }

    /// Ring modulation off for mixing and stereo structures; `mixed` folds
    /// the master's own output in beside the modulator's.
    pub fn init(&mut self, ring_modulated: bool, mixed: bool) {
        self.ring_modulated = ring_modulated;
        self.mixed = mixed;
    }

    fn half(&mut self, which: Pair) -> &mut WaveGenerator {
        match which {
            Pair::Master => &mut self.master,
            Pair::Slave => &mut self.slave,
        }
    }

    pub fn init_synth(
        &mut self,
        tables: &Tables,
        which: Pair,
        sawtooth_waveform: bool,
        pulse_width: u8,
        resonance: u8,
    ) {
        self.half(which)
            .init_synth(tables, sawtooth_waveform, pulse_width, resonance);
    }

    pub fn init_pcm(&mut self, which: Pair, addr: u32, len: u32, looped: bool) {
        // The ring modulator borrows the slave's interpolation circuitry.
        let interpolated = which == Pair::Master || !self.ring_modulated;
        self.half(which).init_pcm(addr, len, looped, interpolated);
    }

    pub fn generate_next_sample(
        &mut self,
        tables: &Tables,
        pcm: &[i16],
        which: Pair,
        amp: u32,
        pitch: u16,
        cutoff: u32,
    ) {
        self.half(which)
            .generate_next_sample(tables, pcm, amp, pitch, cutoff);
    }

    fn unlog_and_mix(tables: &Tables, wg: &WaveGenerator) -> i16 {
        if !wg.is_active() {
            return 0;
        }
        let first = unlog(tables, wg.output_log_sample(true));
        let second = unlog(tables, wg.output_log_sample(false));
        if wg.is_pcm_wave() {
            let interpolated = i32::from(first)
                + (((i32::from(second) - i32::from(first)) * wg.pcm_interpolation_factor as i32)
                    >> 7);
            interpolated as i16
        } else {
            first.wrapping_add(second)
        }
    }

    /// One linear output sample from the pair.
    pub fn next_out_sample(&self, tables: &Tables) -> i16 {
        if !self.ring_modulated {
            return Self::unlog_and_mix(tables, &self.master)
                .wrapping_add(Self::unlog_and_mix(tables, &self.slave));
        }
        let master_sample = Self::unlog_and_mix(tables, &self.master);
        // The slave of a ring-modulated pair goes uninterpolated: the
        // multiplier that would interpolate it is busy modulating.
        let slave_sample = if self.slave.is_pcm_wave() {
            unlog(tables, self.slave.output_log_sample(true))
        } else {
            Self::unlog_and_mix(tables, &self.slave)
        };
        let ring_modulated = ((i32::from(distorted(master_sample))
            * i32::from(distorted(slave_sample)))
            >> 13) as i16;
        if self.mixed {
            master_sample.wrapping_add(ring_modulated)
        } else {
            ring_modulated
        }
    }

    pub fn deactivate(&mut self, which: Pair) {
        self.half(which).deactivate();
    }

    pub fn is_active(&self, which: Pair) -> bool {
        match which {
            Pair::Master => self.master.is_active(),
            Pair::Slave => self.slave.is_active(),
        }
    }
}

/// The modulator's fourteen-bit overflow, distorting anything past 8191 as
/// the chip's own multiplier does.
fn distorted(sample: i16) -> i16 {
    if sample & 0x2000 == 0 {
        sample & 0x1FFF
    } else {
        sample | !0x1FFF
    }
}

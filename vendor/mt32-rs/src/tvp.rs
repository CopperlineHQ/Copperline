// SPDX-License-Identifier: LGPL-2.1-or-later

//! The pitch envelope: what the firmware's timer interrupt does to a
//! partial's pitch.
//!
//! Pitch runs on the MCU's software timer, not per sample: roughly four
//! thousand times a second the firmware recomputes an interpolated offset
//! between envelope targets, adds the LFO -- which is not an oscillator but
//! the envelope bouncing between alternating targets -- and writes the
//! result to the chip. The timer does not fire exactly on time on hardware,
//! and the reference reproduces that with a small jittered period, which is
//! the one place randomness exists in the whole engine.
//!
//! The port computes exactly as the reference does, including the elder
//! units' 16-bit overflows (Larry 3's "HIT BOTTOM" leans on the base pitch
//! wrapping; Colonel's Bequest's "Lightning" on the envelope's) and the
//! third generation's faster timer.

use crate::jitter::Jitter;
use crate::layout::Quirks;
use crate::param::PartialParam;

/// What the envelope reads from the world around it. Key and velocity
/// stand for the note; the rest are live values the part and synth own.
#[derive(Debug, Clone, Copy)]
pub struct TvpHost {
    pub key: u32,
    pub velocity: u32,
    /// A PCM partial's tuning word and the low bit of its length byte,
    /// which decides whether master tune reaches it.
    pub pcm: Option<(u16, bool)>,
    pub master_tune_pitch_delta: i32,
    pub pitch_bend: i32,
    pub modulation: u32,
    pub patch_key_shift: u8,
    pub patch_fine_tune: u8,
}

const LOWER_DURATION_TO_DIVISOR: [u16; 8] =
    [34078, 37162, 40526, 44194, 48194, 52556, 57312, 62499];

/// The manual's keyfollow options, in 1/8192ths.
const PITCH_KEYFOLLOW_MULT: [i16; 17] = [
    -8192, -4096, -2048, 0, 1024, 2048, 3072, 4096, 5120, 6144, 7168, 8192, 10240, 12288, 16384,
    8198, 8226,
];

/// `round((key - 60) * 4096 / 12)` by table, symmetric about middle C.
const KEY_TO_PITCH_TABLE: [u16; 68] = [
    0, 341, 683, 1024, 1365, 1707, 2048, 2389, 2731, 3072, 3413, 3755, 4096, 4437, 4779, 5120,
    5461, 5803, 6144, 6485, 6827, 7168, 7509, 7851, 8192, 8533, 8875, 9216, 9557, 9899, 10240,
    10581, 10923, 11264, 11605, 11947, 12288, 12629, 12971, 13312, 13653, 13995, 14336, 14677,
    15019, 15360, 15701, 16043, 16384, 16725, 17067, 17408, 17749, 18091, 18432, 18773, 19115,
    19456, 19797, 20139, 20480, 20821, 21163, 21504, 21845, 22187, 22528, 22869,
];

/// The nominal period between timer firings, in samples.
const NOMINAL_PROCESS_TIMER_PERIOD_SAMPLES: i32 = (crate::SAMPLE_RATE / 4000) as i32;

/// Timer ticks per sample, times sixteen: the first two generations run
/// the software timer at 500 kHz, the third at 750 kHz.
const PROCESS_TIMER_TICKS_PER_SAMPLE_X16_1N2_GEN: i32 =
    ((500_000 << 4) / crate::SAMPLE_RATE) as i32;
const PROCESS_TIMER_TICKS_PER_SAMPLE_X16_3_GEN: i32 = ((750_000 << 4) / crate::SAMPLE_RATE) as i32;

fn key_to_pitch(key: u32) -> i32 {
    let pitch = i32::from(KEY_TO_PITCH_TABLE[(key as i32 - 60).unsigned_abs() as usize]);
    if key < 60 {
        -pitch
    } else {
        pitch
    }
}

fn coarse_to_pitch(coarse: u8) -> i32 {
    (i32::from(coarse) - 36) * 4096 / 12
}

fn fine_to_pitch(fine: u8) -> i32 {
    (i32::from(fine) - 50) * 4096 / 1200
}

fn calc_base_pitch(param: PartialParam, host: &TvpHost, quirks: &Quirks) -> u32 {
    let mut base = key_to_pitch(host.key);
    base = (base * i32::from(PITCH_KEYFOLLOW_MULT[usize::from(param.pitch_keyfollow())])) >> 13;
    base += coarse_to_pitch(param.pitch_coarse());
    base += fine_to_pitch(param.pitch_fine());
    if quirks.key_shift {
        // Done on the MT-32 but not the LAPC-I.
        base += coarse_to_pitch(host.patch_key_shift.wrapping_add(12));
    }
    base += fine_to_pitch(host.patch_fine_tune);
    if let Some((pcm_pitch, _)) = host.pcm {
        base += i32::from(pcm_pitch);
    } else if param.waveform() & 1 == 0 {
        // Puts middle C near 261.64 Hz.
        base += 37133;
    } else {
        // A sawtooth is effectively double the frequency; adding 4096
        // less than the square halves it back.
        base += 33037;
    }
    if quirks.base_pitch_overflow {
        // The eldest units compute this in 16 bits and wrap.
        (base & 0xFFFF) as u32
    } else {
        base.clamp(0, 59392) as u32
    }
}

fn calc_velo_mult(velo_sensitivity: u8, velocity: u32) -> u32 {
    if velo_sensitivity == 0 {
        return 21845;
    }
    let reversed = 127 - velocity;
    let scaled = if velo_sensitivity > 3 {
        // Never reached on the CM-32L, whose limit tables clip at 3; the
        // eldest units run into unspecified behaviour assumed to be this.
        (reversed << 8) >> ((3 - i32::from(velo_sensitivity)) & 0x1F)
    } else {
        reversed << (5 + velo_sensitivity)
    };
    ((32768 - scaled) * 21845) >> 15
}

fn calc_target_pitch_offset_without_lfo(
    param: PartialParam,
    level_index: usize,
    velocity: u32,
) -> i32 {
    let velo_mult = calc_velo_mult(param.pitch_env_velo_sensitivity(), velocity) as i32;
    let target = i32::from(param.pitch_env_level(level_index)) - 50;
    (target * velo_mult) >> (16 - param.pitch_env_depth())
}

/// One partial's pitch envelope.
#[derive(Debug, Clone)]
pub struct Tvp {
    process_timer_ticks_per_sample_x16: i32,
    process_timer_increment: i32,
    counter: i32,
    time_elapsed: u32,
    phase: i32,
    base_pitch: u32,
    target_pitch_offset_without_lfo: i32,
    current_pitch_offset: i32,
    lfo_pitch_offset: i16,
    time_keyfollow_subtraction: i8,
    pitch_offset_change_per_big_tick: i16,
    target_pitch_offset_reached_big_tick: u16,
    shifts: u32,
    pitch: u16,
    /// Set on every pitch write, for the amp envelope's sustain recheck --
    /// the CM-32L recalculates there, so the caller forwards this.
    pitch_updated: bool,
}

impl Tvp {
    pub fn new(quirks: &Quirks) -> Tvp {
        Tvp {
            process_timer_ticks_per_sample_x16: if quirks.fast_pitch_changes {
                PROCESS_TIMER_TICKS_PER_SAMPLE_X16_3_GEN
            } else {
                PROCESS_TIMER_TICKS_PER_SAMPLE_X16_1N2_GEN
            },
            process_timer_increment: 0,
            counter: 0,
            time_elapsed: 0,
            phase: 0,
            base_pitch: 0,
            target_pitch_offset_without_lfo: 0,
            current_pitch_offset: 0,
            lfo_pitch_offset: 0,
            time_keyfollow_subtraction: 0,
            pitch_offset_change_per_big_tick: 0,
            target_pitch_offset_reached_big_tick: 0,
            shifts: 0,
            pitch: 0,
            pitch_updated: false,
        }
    }

    /// Start the envelope for a note.
    pub fn reset(&mut self, param: PartialParam, host: &TvpHost, quirks: &Quirks) {
        self.time_elapsed = 0;
        self.process_timer_increment = 0;
        self.base_pitch = calc_base_pitch(param, host, quirks);
        self.current_pitch_offset = calc_target_pitch_offset_without_lfo(param, 0, host.velocity);
        self.target_pitch_offset_without_lfo = self.current_pitch_offset;
        self.phase = 0;
        self.time_keyfollow_subtraction = if param.pitch_env_time_keyfollow() != 0 {
            ((host.key as i32 - 60) >> (5 - param.pitch_env_time_keyfollow())) as i8
        } else {
            0
        };
        self.lfo_pitch_offset = 0;
        self.counter = 0;
        self.pitch = self.base_pitch as u16;
        self.pitch_offset_change_per_big_tick = 0;
        self.target_pitch_offset_reached_big_tick = 0;
        self.shifts = 0;
    }

    pub fn base_pitch(&self) -> u32 {
        self.base_pitch
    }

    /// Whether a pitch write happened since last asked; asking clears it.
    pub fn take_pitch_updated(&mut self) -> bool {
        std::mem::take(&mut self.pitch_updated)
    }

    fn update_pitch(&mut self, param: PartialParam, host: &TvpHost, quirks: &Quirks) {
        let mut new_pitch = self.base_pitch as i32 + self.current_pitch_offset;
        let master_tune_applies = match host.pcm {
            None => true,
            Some((_, len_low_bit)) => !len_low_bit,
        };
        if master_tune_applies {
            new_pitch += host.master_tune_pitch_delta;
        }
        if param.pitch_bender_enabled() & 1 != 0 {
            new_pitch += host.pitch_bend;
        }
        if quirks.pitch_envelope_overflow {
            // The eldest units wrap here too.
            new_pitch &= 0xFFFF;
        } else if new_pitch < 0 {
            new_pitch = 0;
        }
        if new_pitch > 59392 {
            new_pitch = 59392;
        }
        self.pitch = new_pitch as u16;
        self.pitch_updated = true;
    }

    fn target_pitch_offset_reached(
        &mut self,
        param: PartialParam,
        host: &TvpHost,
        quirks: &Quirks,
    ) {
        self.current_pitch_offset =
            self.target_pitch_offset_without_lfo + i32::from(self.lfo_pitch_offset);
        match self.phase {
            3 | 4 => {
                let mut new_lfo =
                    ((host.modulation * u32::from(param.pitch_lfo_mod_sensitivity())) >> 7) as i32;
                new_lfo = (new_lfo + i32::from(param.pitch_lfo_depth())) << 1;
                if self.pitch_offset_change_per_big_tick > 0 {
                    // The opposite direction to last time: the LFO is the
                    // envelope bouncing.
                    new_lfo = -new_lfo;
                }
                self.lfo_pitch_offset = new_lfo as i16;
                let target =
                    self.target_pitch_offset_without_lfo + i32::from(self.lfo_pitch_offset);
                self.setup_pitch_change(target, 101 - param.pitch_lfo_rate());
                self.update_pitch(param, host, quirks);
            }
            6 => self.update_pitch(param, host, quirks),
            _ => self.next_phase(param, host, quirks),
        }
    }

    fn next_phase(&mut self, param: PartialParam, host: &TvpHost, quirks: &Quirks) {
        self.phase += 1;
        let env_index = if self.phase == 6 {
            4
        } else {
            self.phase as usize
        };
        self.target_pitch_offset_without_lfo =
            calc_target_pitch_offset_without_lfo(param, env_index, host.velocity);
        let change_duration = i32::from(param.pitch_env_time(env_index - 1))
            - i32::from(self.time_keyfollow_subtraction);
        if change_duration > 0 {
            self.setup_pitch_change(self.target_pitch_offset_without_lfo, change_duration as u8);
            self.update_pitch(param, host, quirks);
        } else {
            self.target_pitch_offset_reached(param, host, quirks);
        }
    }

    fn setup_pitch_change(&mut self, target_pitch_offset: i32, change_duration: u8) {
        let negative_delta = target_pitch_offset < self.current_pitch_offset;
        let mut delta = target_pitch_offset - self.current_pitch_offset;
        if !(-32768..=32767).contains(&delta) {
            delta = 32767;
        }
        if negative_delta {
            delta = -delta;
        }
        let mut abs_delta = ((delta as u32) & 0xFFFF) << 16;
        let normalisation_shifts = normalise(&mut abs_delta);
        abs_delta >>= 1;
        let change_duration = change_duration - 1;
        let upper_duration = u32::from(change_duration >> 3);
        self.shifts = u32::from(normalisation_shifts) + upper_duration + 2;
        let divisor = LOWER_DURATION_TO_DIVISOR[usize::from(change_duration & 7)];
        let mut change_per_big_tick =
            (((abs_delta & 0xFFFF_0000) / u32::from(divisor)) >> 1) as i16;
        if negative_delta {
            change_per_big_tick = -change_per_big_tick;
        }
        self.pitch_offset_change_per_big_tick = change_per_big_tick;
        let current_big_tick = (self.time_elapsed >> 8) as u16;
        // The shift count can go to -1 when keyfollow stretches a duration
        // past 104; both hardware families mask the count, which lands the
        // duration at zero.
        let duration_in_big_ticks =
            (u32::from(divisor) >> ((12i32 - upper_duration as i32) & 0x1F)).min(32767);
        // Wrapping past 16 bits is intended.
        self.target_pitch_offset_reached_big_tick =
            current_big_tick.wrapping_add(duration_in_big_ticks as u16);
    }

    /// Fall to the release target.
    pub fn start_decay(&mut self) {
        self.phase = 5;
        self.lfo_pitch_offset = 0;
        self.target_pitch_offset_reached_big_tick = (self.time_elapsed >> 8) as u16;
    }

    /// The pitch this sample, running the timer as the firmware would.
    pub fn next_pitch(
        &mut self,
        param: PartialParam,
        host: &TvpHost,
        quirks: &Quirks,
        jitter: &mut Jitter,
    ) -> u16 {
        if self.counter == 0 {
            self.time_elapsed = self
                .time_elapsed
                .wrapping_add(self.process_timer_increment as u32)
                & 0x00FF_FFFF;
            // The timer is not guaranteed to fire on time; this reproduces
            // the pitch deviations real units show.
            self.counter = NOMINAL_PROCESS_TIMER_PERIOD_SAMPLES + (jitter.next_value() & 3);
            self.process_timer_increment =
                (self.process_timer_ticks_per_sample_x16 * self.counter) >> 4;
            self.process(param, host, quirks);
        }
        self.counter -= 1;
        self.pitch
    }

    fn process(&mut self, param: PartialParam, host: &TvpHost, quirks: &Quirks) {
        if self.phase == 0 {
            self.target_pitch_offset_reached(param, host, quirks);
            return;
        }
        if self.phase == 5 {
            self.next_phase(param, host, quirks);
            return;
        }
        if self.phase > 7 {
            self.update_pitch(param, host, quirks);
            return;
        }
        let negative_big_ticks_remaining = ((self.time_elapsed >> 8) as u16)
            .wrapping_sub(self.target_pitch_offset_reached_big_tick)
            as i16;
        if negative_big_ticks_remaining >= 0 {
            self.target_pitch_offset_reached(param, host, quirks);
            return;
        }
        let mut remaining = i32::from(negative_big_ticks_remaining);
        let mut right_shifts = self.shifts as i32;
        if right_shifts > 13 {
            right_shifts -= 13;
            remaining >>= right_shifts & 0x1F;
            right_shifts = 13;
        }
        let mut new_result =
            (remaining * i32::from(self.pitch_offset_change_per_big_tick)) >> (right_shifts & 0x1F);
        new_result += self.target_pitch_offset_without_lfo + i32::from(self.lfo_pitch_offset);
        self.current_pitch_offset = new_result;
        self.update_pitch(param, host, quirks);
    }
}

/// Shift left until bit 31 is set; how many shifts it took.
fn normalise(value: &mut u32) -> u8 {
    let mut shifts = 0;
    while shifts < 31 {
        if *value & 0x8000_0000 != 0 {
            break;
        }
        *value <<= 1;
        shifts += 1;
    }
    shifts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_param(bytes: &[u8]) -> PartialParam<'_> {
        PartialParam(bytes)
    }

    fn quiet_host() -> TvpHost {
        TvpHost {
            key: 60,
            velocity: 100,
            pcm: None,
            master_tune_pitch_delta: 0,
            pitch_bend: 0,
            modulation: 0,
            patch_key_shift: 24,
            patch_fine_tune: 50,
        }
    }

    /// Middle C on a flat envelope holds the square wave's base pitch and
    /// stays put; the sawtooth base sits 4096 lower.
    #[test]
    fn middle_c_holds_its_base_pitch() {
        let quirks = &LAYOUTS[8].quirks; // v2.07: no elder overflows
        let mut bytes = [0u8; 58];
        bytes[1] = 50; // fine tune centred
        bytes[2] = 11; // keyfollow 1
        for n in 0..5 {
            bytes[15 + n] = 50; // pitch env levels flat at centre
        }
        let mut jitter = Jitter::new();
        let mut tvp = Tvp::new(quirks);
        let host = quiet_host();
        tvp.reset(a_param(&bytes), &host, quirks);
        assert_eq!(tvp.base_pitch() as i32, 37133 + coarse_to_pitch(0));
        let first = tvp.next_pitch(a_param(&bytes), &host, quirks, &mut jitter);
        for _ in 0..2000 {
            assert_eq!(
                tvp.next_pitch(a_param(&bytes), &host, quirks, &mut jitter),
                first,
                "a flat envelope holds"
            );
        }
        bytes[4] = 1; // sawtooth
        let mut saw = Tvp::new(quirks);
        saw.reset(a_param(&bytes), &host, quirks);
        assert_eq!(u32::from(first) - saw.base_pitch(), 4096);
    }

    /// The elder base-pitch overflow wraps where the later units clamp:
    /// the quirk Larry 3 leans on.
    #[test]
    fn the_elder_units_wrap_where_the_later_clamp() {
        let mut bytes = [0u8; 58];
        bytes[0] = 96; // coarse pitch at the top
        bytes[2] = 14; // keyfollow x2
        let host = TvpHost {
            key: 108,
            ..quiet_host()
        };
        let elder = &LAYOUTS[0].quirks;
        let later = &LAYOUTS[8].quirks;
        let mut tvp = Tvp::new(elder);
        tvp.reset(a_param(&bytes), &host, elder);
        let wrapped = tvp.base_pitch();
        tvp = Tvp::new(later);
        tvp.reset(a_param(&bytes), &host, later);
        let clamped = tvp.base_pitch();
        assert_eq!(clamped, 59392, "the later units stop at the ceiling");
        assert!(
            wrapped < 59392,
            "the elder units wrapped past it: {wrapped}"
        );
    }

    /// Decay from a held note falls toward the end level rather than
    /// holding.
    #[test]
    fn decay_leaves_the_held_pitch() {
        let quirks = &LAYOUTS[8].quirks;
        let mut bytes = [0u8; 58];
        bytes[8] = 10; // full envelope depth
        for n in 0..4 {
            bytes[11 + n] = 3; // quick phases
        }
        for n in 0..4 {
            bytes[15 + n] = 50;
        }
        bytes[19] = 0; // end level well below centre
        let mut jitter = Jitter::new();
        let host = quiet_host();
        let mut tvp = Tvp::new(quirks);
        tvp.reset(a_param(&bytes), &host, quirks);
        let held = (0..4000)
            .map(|_| tvp.next_pitch(a_param(&bytes), &host, quirks, &mut jitter))
            .last()
            .unwrap();
        tvp.start_decay();
        let mut released = held;
        for _ in 0..64000 {
            released = tvp.next_pitch(a_param(&bytes), &host, quirks, &mut jitter);
        }
        assert!(released < held, "the release fell: {held} -> {released}");
    }
}

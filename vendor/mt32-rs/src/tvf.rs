// SPDX-License-Identifier: LGPL-2.1-or-later

//! The filter envelope: what drives the cutoff ramp.
//!
//! The filter's base cutoff folds together the two keyfollows, the bias
//! point and level, and the cutoff parameter, then the envelope walks its
//! five levels by starting ramps and advancing a phase on each ramp
//! interrupt. The envelope times convert to ramp increments through the
//! logarithmic time curve; sustain is a ramp with increment zero, held
//! until the note lets go.

use crate::la32::ramp::Ramp;
use crate::layout::Quirks;
use crate::param::PartialParam;
use crate::tables::Tables;

/// Phase numbering, as the reference counts them: the value being entered.
const PHASE_2: i32 = 2;
const PHASE_SUSTAIN: i32 = 5;
const PHASE_RELEASE: i32 = 6;
const PHASE_DONE: i32 = 7;

/// What the envelope reads of the note around it.
#[derive(Debug, Clone, Copy)]
pub struct TvfHost {
    pub key: u32,
    pub velocity: u32,
    /// Whether the poly may sustain, which is what holds phase 5.
    pub can_sustain: bool,
}

/// Matches the values a real LAPC-I uses.
const BIAS_LEVEL_TO_BIAS_MULT: [i8; 15] =
    [85, 42, 21, 16, 10, 5, 2, 0, -2, -5, -10, -16, -21, -74, -85];

/// The manual's keyfollow options, times 21.
const KEYFOLLOW_MULT21: [i8; 17] = [
    -21, -10, -5, 0, 2, 5, 8, 10, 13, 16, 18, 21, 26, 32, 42, 21, 21,
];

fn calc_base_cutoff(
    param: PartialParam,
    base_pitch: u32,
    key: u32,
    quirk_tvf_base_cutoff_limit: bool,
) -> u8 {
    let mut base_cutoff = i32::from(KEYFOLLOW_MULT21[usize::from(param.tvf_keyfollow())])
        - i32::from(KEYFOLLOW_MULT21[usize::from(param.pitch_keyfollow())]);
    base_cutoff *= key as i32 - 60;
    let bias_point = i32::from(param.tvf_bias_point());
    if bias_point & 0x40 == 0 {
        let bias = bias_point + 33 - key as i32;
        if bias > 0 {
            base_cutoff +=
                -bias * i32::from(BIAS_LEVEL_TO_BIAS_MULT[usize::from(param.tvf_bias_level())]);
        }
    } else {
        let bias = bias_point - 31 - key as i32;
        if bias < 0 {
            base_cutoff +=
                bias * i32::from(BIAS_LEVEL_TO_BIAS_MULT[usize::from(param.tvf_bias_level())]);
        }
    }
    base_cutoff += (i32::from(param.tvf_cutoff()) << 4) - 800;
    if base_cutoff >= 0 {
        let pitch_delta_thing = (base_pitch >> 4) as i32 + base_cutoff - 3584;
        if pitch_delta_thing > 0 {
            base_cutoff -= pitch_delta_thing;
        }
    } else if quirk_tvf_base_cutoff_limit {
        // The elder firmware's own limit check, typo and all: past
        // -0x400 it clamps to -400 decimal.
        if base_cutoff <= -0x400 {
            base_cutoff = -400;
        }
    } else if base_cutoff < -2048 {
        base_cutoff = -2048;
    }
    base_cutoff += 2056;
    base_cutoff >>= 4;
    base_cutoff.min(255) as u8
}

/// One partial's filter envelope, driving a cutoff ramp it borrows.
#[derive(Debug, Clone)]
pub struct Tvf {
    base_cutoff: u8,
    target: u8,
    phase: i32,
    level_mult: u32,
    key_time_subtraction: i32,
}

impl Tvf {
    pub fn new() -> Tvf {
        Tvf {
            base_cutoff: 0,
            target: 0,
            phase: 0,
            level_mult: 0,
            key_time_subtraction: 0,
        }
    }

    fn start_ramp(
        &mut self,
        ramp: &mut Ramp,
        tables: &Tables,
        target: u8,
        increment: u8,
        phase: i32,
    ) {
        self.target = target;
        self.phase = phase;
        ramp.start(tables, target, increment);
    }

    /// Start the envelope for a note, resetting the ramp it drives.
    pub fn reset(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvfHost,
        quirks: &Quirks,
        base_pitch: u32,
    ) {
        self.base_cutoff =
            calc_base_cutoff(param, base_pitch, host.key, quirks.tvf_base_cutoff_limit);
        let mut level_mult =
            (host.velocity as i32 * i32::from(param.tvf_env_velo_sensitivity())) >> 6;
        level_mult += 109 - i32::from(param.tvf_env_velo_sensitivity());
        level_mult += (host.key as i32 - 60) >> (4 - param.tvf_env_depth_keyfollow());
        if level_mult < 0 {
            level_mult = 0;
        }
        level_mult *= i32::from(param.tvf_env_depth());
        level_mult >>= 6;
        self.level_mult = level_mult.min(255) as u32;
        self.key_time_subtraction = if param.tvf_env_time_keyfollow() != 0 {
            (host.key as i32 - 60) >> (5 - param.tvf_env_time_keyfollow())
        } else {
            0
        };
        let target = ((self.level_mult * u32::from(param.tvf_env_level(0))) >> 8) as u8;
        let env_time_setting = i32::from(param.tvf_env_time(0)) - self.key_time_subtraction;
        let increment = if env_time_setting <= 0 {
            0x80 | 127
        } else {
            let increment =
                i32::from(tables.env_logarithmic_time[usize::from(target)]) - env_time_setting;
            increment.max(1) as u8
        };
        ramp.reset();
        self.start_ramp(ramp, tables, target, increment, PHASE_2 - 1);
    }

    pub fn base_cutoff(&self) -> u8 {
        self.base_cutoff
    }

    /// The ramp's interrupt arrived: on to the next phase.
    pub fn handle_interrupt(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvfHost,
    ) {
        self.next_phase(tables, ramp, param, host);
    }

    /// The note let go: fall to nothing over the release time.
    pub fn start_decay(&mut self, tables: &Tables, ramp: &mut Ramp, param: PartialParam) {
        if self.phase >= PHASE_RELEASE {
            return;
        }
        if param.tvf_env_time(4) == 0 {
            self.start_ramp(ramp, tables, 0, 1, PHASE_DONE - 1);
        } else {
            self.start_ramp(
                ramp,
                tables,
                0,
                param.tvf_env_time(4).wrapping_neg(),
                PHASE_DONE - 1,
            );
        }
    }

    fn next_phase(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvfHost,
    ) {
        let new_phase = self.phase + 1;
        match new_phase {
            PHASE_DONE => {
                self.start_ramp(ramp, tables, 0, 0, new_phase);
                return;
            }
            PHASE_SUSTAIN | PHASE_RELEASE => {
                if !host.can_sustain {
                    self.phase = new_phase;
                    self.start_decay(tables, ramp, param);
                    return;
                }
                let target = ((self.level_mult * u32::from(param.tvf_env_level(3))) >> 8) as u8;
                self.start_ramp(ramp, tables, target, 0, new_phase);
                return;
            }
            _ => {}
        }
        let env_point_index = self.phase as usize;
        let env_time_setting =
            i32::from(param.tvf_env_time(env_point_index)) - self.key_time_subtraction;
        let mut new_target =
            ((self.level_mult * u32::from(param.tvf_env_level(env_point_index))) >> 8) as i32;
        let increment: u8;
        if env_time_setting > 0 {
            let mut target_delta = new_target - i32::from(self.target);
            if target_delta == 0 {
                if new_target == 0 {
                    target_delta = 1;
                    new_target = 1;
                } else {
                    target_delta = -1;
                    new_target -= 1;
                }
            }
            let mut inc =
                i32::from(tables.env_logarithmic_time[target_delta.unsigned_abs() as usize])
                    - env_time_setting;
            if inc <= 0 {
                inc = 1;
            }
            if target_delta < 0 {
                inc |= 0x80;
            }
            increment = inc as u8;
        } else {
            increment = if new_target >= i32::from(self.target) {
                0x80 | 127
            } else {
                127
            };
        }
        self.start_ramp(ramp, tables, new_target as u8, increment, new_phase);
    }
}

impl Default for Tvf {
    fn default() -> Tvf {
        Tvf::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_param(bytes: &[u8]) -> PartialParam<'_> {
        PartialParam(bytes)
    }

    /// A held note walks the envelope to sustain and stays; releasing
    /// falls to done through the release time; the ramp's interrupts are
    /// what advance it, exactly as the hardware is wired.
    #[test]
    fn the_envelope_walks_to_sustain_and_falls_on_release() {
        let tables = Tables::new();
        let quirks = &LAYOUTS[8].quirks;
        let mut bytes = [0u8; 58];
        bytes[23] = 80; // cutoff
        bytes[28] = 100; // envelope depth
        for n in 0..5 {
            bytes[32 + n] = 30; // times
        }
        bytes[37] = 90;
        bytes[38] = 60;
        bytes[39] = 70;
        bytes[40] = 40; // sustain level
        let host = TvfHost {
            key: 60,
            velocity: 100,
            can_sustain: true,
        };
        let mut tvf = Tvf::new();
        let mut ramp = Ramp::new();
        tvf.reset(&tables, &mut ramp, a_param(&bytes), &host, quirks, 24845);
        let mut interrupts = 0;
        for _ in 0..400_000 {
            ramp.next_value();
            if ramp.check_interrupt() {
                interrupts += 1;
                tvf.handle_interrupt(&tables, &mut ramp, a_param(&bytes), &host);
            }
            if tvf.phase == PHASE_SUSTAIN {
                break;
            }
        }
        assert_eq!(tvf.phase, PHASE_SUSTAIN, "after {interrupts} interrupts");
        let held = ramp.next_value();
        for _ in 0..10_000 {
            assert_eq!(ramp.next_value(), held, "sustain holds");
        }
        tvf.start_decay(&tables, &mut ramp, a_param(&bytes));
        let mut fell = held;
        for _ in 0..400_000 {
            fell = ramp.next_value();
        }
        assert_eq!(fell, 0, "release reached silence");
    }

    /// The elder limit quirk clamps deep-negative cutoffs to the odd -400
    /// where the later units use -2048.
    #[test]
    fn the_elder_cutoff_limit_is_the_odd_one() {
        let mut bytes = [0u8; 58];
        bytes[23] = 0; // cutoff bottom
        bytes[25] = 0; // keyfollow -1
        bytes[2] = 14; // pitch keyfollow 2
        let elder = calc_base_cutoff(a_param(&bytes), 0, 108, true);
        let later = calc_base_cutoff(a_param(&bytes), 0, 108, false);
        assert_eq!(later, 0, "the later clamp floors at zero cutoff");
        assert!(elder > later, "the elder value pops up from -400: {elder}");
    }
}

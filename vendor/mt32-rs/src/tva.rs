// SPDX-License-Identifier: LGPL-2.1-or-later

//! The amplitude envelope: the 8095's arithmetic for the amp ramp, phase
//! by phase.
//!
//! The basic amp folds the master volume, part level, expression, rhythm
//! level, biases, velocity and resonance into one subtraction chain that
//! floors at silence at every step; the envelope then walks its levels on
//! ramp interrupts like the filter's does, with three wrinkles the
//! reference confirms against hardware: an attack time of zero skips
//! straight to the attack level, sustain is re-aimed whenever the pitch
//! timer pings so volume and expression stay live, and a zero sustain
//! level ends the partial outright at the sustain gate.

use crate::la32::ramp::Ramp;
use crate::layout::Quirks;
use crate::param::PartialParam;
use crate::tables::Tables;

pub const TVA_PHASE_BASIC: i32 = 0;
pub const TVA_PHASE_ATTACK: i32 = 1;
pub const TVA_PHASE_2: i32 = 2;
pub const TVA_PHASE_3: i32 = 3;
pub const TVA_PHASE_4: i32 = 4;
pub const TVA_PHASE_SUSTAIN: i32 = 5;
pub const TVA_PHASE_RELEASE: i32 = 6;
pub const TVA_PHASE_DEAD: i32 = 7;

/// What the envelope reads of the world: the note, the part's live levels,
/// and which structure the partial sits in.
#[derive(Debug, Clone, Copy)]
pub struct TvaHost {
    pub key: u32,
    pub velocity: u32,
    pub part_volume: u8,
    pub expression: u8,
    /// A rhythm note's per-key output level; `None` off the rhythm part.
    pub rhythm_output_level: Option<u8>,
    pub master_vol: u8,
    pub can_sustain: bool,
    /// Whether this partial is a ring-modulating slave (the later units'
    /// test) or in a no-mix ring structure (the elder units').
    pub ring_modulating_slave: bool,
    pub ring_modulating_no_mix: bool,
    /// The reference's click-avoiding sustain correction, on by default.
    pub nice_amp_ramp: bool,
}

/// Matches a table in the ROM.
const BIAS_LEVEL_TO_AMP_SUBTRACTION_COEFF: [u8; 13] =
    [255, 187, 137, 100, 74, 54, 40, 29, 21, 15, 10, 5, 0];

fn mult_bias(bias_level: u8, bias: i32) -> i32 {
    (bias * i32::from(BIAS_LEVEL_TO_AMP_SUBTRACTION_COEFF[usize::from(bias_level)])) >> 5
}

fn calc_bias_amp_subtraction(bias_point: u8, bias_level: u8, key: i32) -> i32 {
    if bias_point & 0x40 == 0 {
        let bias = i32::from(bias_point) + 33 - key;
        if bias > 0 {
            return mult_bias(bias_level, bias);
        }
    } else {
        let bias = i32::from(bias_point) - 31 - key;
        if bias < 0 {
            return mult_bias(bias_level, -bias);
        }
    }
    0
}

fn calc_bias_amp_subtractions(param: PartialParam, key: i32) -> i32 {
    let first = calc_bias_amp_subtraction(param.tva_bias_point1(), param.tva_bias_level1(), key);
    if first > 255 {
        return 255;
    }
    let second = calc_bias_amp_subtraction(param.tva_bias_point2(), param.tva_bias_level2(), key);
    if second > 255 {
        return 255;
    }
    (first + second).min(255)
}

fn calc_velo_amp_subtraction(velo_sensitivity: u8, velocity: u32) -> i32 {
    let velocity_mult = i32::from(velo_sensitivity) - 50;
    let abs_velocity_mult = velocity_mult.abs();
    // The reference shifts an unsigned reinterpretation left before the
    // arithmetic shift back down; wrapping reproduces it.
    let velocity_mult = (velocity_mult * (velocity as i32 - 64)).wrapping_shl(2);
    abs_velocity_mult - (velocity_mult >> 8)
}

fn calc_basic_amp(
    tables: &Tables,
    param: PartialParam,
    host: &TvaHost,
    bias_amp_subtraction: i32,
    velo_amp_subtraction: i32,
    quirks: &Quirks,
) -> i32 {
    let mut amp = 155;
    let ring_slave = if quirks.ring_modulation_no_mix {
        host.ring_modulating_no_mix
    } else {
        host.ring_modulating_slave
    };
    if !ring_slave {
        amp -= i32::from(tables.master_vol_to_amp_subtraction[usize::from(host.master_vol)]);
        if amp < 0 {
            return 0;
        }
        amp -= i32::from(tables.level_to_amp_subtraction[usize::from(host.part_volume)]);
        if amp < 0 {
            return 0;
        }
        amp -= i32::from(tables.level_to_amp_subtraction[usize::from(host.expression)]);
        if amp < 0 {
            return 0;
        }
        if let Some(rhythm_level) = host.rhythm_output_level {
            amp -= i32::from(tables.level_to_amp_subtraction[usize::from(rhythm_level)]);
            if amp < 0 {
                return 0;
            }
        }
    }
    amp -= bias_amp_subtraction;
    if amp < 0 {
        return 0;
    }
    amp -= i32::from(tables.level_to_amp_subtraction[usize::from(param.tva_level())]);
    if amp < 0 {
        return 0;
    }
    amp -= velo_amp_subtraction;
    if amp < 0 {
        return 0;
    }
    amp = amp.min(155);
    amp -= i32::from(param.tvf_resonance()) >> 1;
    amp.max(0)
}

fn calc_key_time_subtraction(env_time_keyfollow: u8, key: i32) -> i32 {
    if env_time_keyfollow == 0 {
        return 0;
    }
    (key - 60) >> (5 - env_time_keyfollow)
}

/// One partial's amplitude envelope, driving an amp ramp it borrows.
#[derive(Debug, Clone)]
pub struct Tva {
    playing: bool,
    target: u8,
    phase: i32,
    bias_amp_subtraction: i32,
    velo_amp_subtraction: i32,
    key_time_subtraction: i32,
}

impl Tva {
    pub fn new() -> Tva {
        Tva {
            playing: false,
            target: 0,
            phase: TVA_PHASE_DEAD,
            bias_amp_subtraction: 0,
            velo_amp_subtraction: 0,
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

    fn end(&mut self, phase: i32) {
        self.phase = phase;
        self.playing = false;
    }

    /// Start the envelope for a note, resetting the ramp it drives.
    pub fn reset(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvaHost,
        quirks: &Quirks,
    ) {
        self.playing = true;
        let key = host.key as i32;
        self.key_time_subtraction = calc_key_time_subtraction(param.tva_env_time_keyfollow(), key);
        self.bias_amp_subtraction = calc_bias_amp_subtractions(param, key);
        self.velo_amp_subtraction =
            calc_velo_amp_subtraction(param.tva_velo_sensitivity(), host.velocity);
        let mut target = calc_basic_amp(
            tables,
            param,
            host,
            self.bias_amp_subtraction,
            self.velo_amp_subtraction,
            quirks,
        );
        let phase = if param.tva_env_time(0) == 0 {
            // Straight to the attack level; velocity then never affects
            // this partial's timing.
            target += i32::from(param.tva_env_level(0));
            TVA_PHASE_ATTACK
        } else {
            TVA_PHASE_BASIC
        };
        ramp.reset();
        // Downward as quickly as possible: from zero the ramp lands and
        // interrupts immediately.
        self.start_ramp(ramp, tables, target as u8, 0x80 | 127, phase);
    }

    /// Yank the partial for reassignment: a fast fall from wherever it is.
    pub fn start_abort(&mut self, tables: &Tables, ramp: &mut Ramp) {
        self.start_ramp(ramp, tables, 64, 0x80 | 127, TVA_PHASE_RELEASE);
    }

    /// The note let go.
    pub fn start_decay(&mut self, tables: &Tables, ramp: &mut Ramp, param: PartialParam) {
        if self.phase >= TVA_PHASE_RELEASE {
            return;
        }
        let increment = if param.tva_env_time(4) == 0 {
            1
        } else {
            param.tva_env_time(4).wrapping_neg()
        };
        // When the ramp interrupts next, the release counts as finished
        // and the partial dies.
        self.start_ramp(ramp, tables, 0, increment, TVA_PHASE_RELEASE);
    }

    /// The pitch timer's ping: while sustaining, re-aim at the live
    /// volume and expression.
    pub fn recalc_sustain(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvaHost,
        quirks: &Quirks,
    ) {
        if self.phase != TVA_PHASE_SUSTAIN || param.tva_env_level(3) == 0 {
            return;
        }
        let mut new_target = calc_basic_amp(
            tables,
            param,
            host,
            self.bias_amp_subtraction,
            self.velo_amp_subtraction,
            quirks,
        );
        new_target += i32::from(param.tva_env_level(3));
        let target_delta = new_target - i32::from(self.target);
        let descending = target_delta < 0;
        let mut increment = if descending {
            tables.env_logarithmic_time[(-target_delta) as u8 as usize].wrapping_sub(2) | 0x80
        } else {
            tables.env_logarithmic_time[target_delta as u8 as usize].wrapping_sub(2)
        };
        // The hardware assumes the previous ramp finished and clicks when
        // it had not; the reference corrects the direction against the
        // ramp's real value, and so does this port.
        if host.nice_amp_ramp && descending != ramp.is_below_current(new_target as u8) {
            increment ^= 0x80;
        }
        self.start_ramp(
            ramp,
            tables,
            new_target as u8,
            increment,
            TVA_PHASE_SUSTAIN - 1,
        );
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn phase(&self) -> i32 {
        self.phase
    }

    /// The ramp's interrupt arrived: on to the next phase.
    pub fn handle_interrupt(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvaHost,
        quirks: &Quirks,
    ) {
        self.next_phase(tables, ramp, param, host, quirks);
    }

    fn next_phase(
        &mut self,
        tables: &Tables,
        ramp: &mut Ramp,
        param: PartialParam,
        host: &TvaHost,
        quirks: &Quirks,
    ) {
        if self.phase >= TVA_PHASE_DEAD || !self.playing {
            return;
        }
        let mut new_phase = self.phase + 1;
        if new_phase == TVA_PHASE_DEAD {
            self.end(new_phase);
            return;
        }
        // A run of zero levels to the end means nothing left to hear; the
        // eldest firmware only recognises the simplest case.
        let mut all_levels_zero_from_now_on = false;
        if param.tva_env_level(3) == 0 {
            if new_phase == TVA_PHASE_4 {
                all_levels_zero_from_now_on = true;
            } else if !quirks.tva_zero_env_levels && param.tva_env_level(2) == 0 {
                if new_phase == TVA_PHASE_3 {
                    all_levels_zero_from_now_on = true;
                } else if param.tva_env_level(1) == 0 {
                    if new_phase == TVA_PHASE_2 {
                        all_levels_zero_from_now_on = true;
                    } else if param.tva_env_level(0) == 0 && new_phase == TVA_PHASE_ATTACK {
                        // Missing from the ROM itself; the reference adds it.
                        all_levels_zero_from_now_on = true;
                    }
                }
            }
        }
        let mut new_target;
        let mut new_increment: i32 = 0;
        let env_point_index = self.phase as usize;
        if !all_levels_zero_from_now_on {
            new_target = calc_basic_amp(
                tables,
                param,
                host,
                self.bias_amp_subtraction,
                self.velo_amp_subtraction,
                quirks,
            );
            if new_phase == TVA_PHASE_SUSTAIN || new_phase == TVA_PHASE_RELEASE {
                if param.tva_env_level(3) == 0 {
                    self.end(new_phase);
                    return;
                }
                if !host.can_sustain {
                    new_phase = TVA_PHASE_RELEASE;
                    new_target = 0;
                    new_increment = i32::from(param.tva_env_time(4).wrapping_neg());
                    if new_increment == 0 {
                        // Zero would never interrupt; a tiny upward step
                        // reaches zero at once and brings the phase change.
                        new_increment = 1;
                    }
                } else {
                    new_target += i32::from(param.tva_env_level(3));
                    new_increment = 0;
                }
            } else {
                new_target += i32::from(param.tva_env_level(env_point_index));
            }
        } else {
            new_target = 0;
        }
        if (new_phase != TVA_PHASE_SUSTAIN && new_phase != TVA_PHASE_RELEASE)
            || all_levels_zero_from_now_on
        {
            let mut env_time_setting = i32::from(param.tva_env_time(env_point_index));
            if new_phase == TVA_PHASE_ATTACK {
                env_time_setting -=
                    (host.velocity as i32 - 64) >> (6 - param.tva_env_time_velo_sensitivity());
                if env_time_setting <= 0 && param.tva_env_time(env_point_index) != 0 {
                    env_time_setting = 1;
                }
            } else {
                env_time_setting -= self.key_time_subtraction;
            }
            if env_time_setting > 0 {
                let mut target_delta = new_target - i32::from(self.target);
                if target_delta <= 0 {
                    if target_delta == 0 {
                        // An increment of zero would never interrupt: aim
                        // one below and note the direction.
                        target_delta = -1;
                        new_target -= 1;
                        if new_target < 0 {
                            // The firmware's own bug, kept: flipping the
                            // sign here sends the lookup through index -1
                            // and the direction the wrong way.
                            target_delta = 1;
                            new_target = -new_target;
                        }
                    }
                    if target_delta <= 0 {
                        target_delta = -target_delta;
                        new_increment =
                            i32::from(tables.env_logarithmic_time[target_delta as u8 as usize])
                                - env_time_setting;
                        if new_increment <= 0 {
                            new_increment = 1;
                        }
                        new_increment |= 0x80;
                    } else {
                        new_increment =
                            i32::from(tables.env_logarithmic_time[target_delta as u8 as usize])
                                - env_time_setting;
                        if new_increment <= 0 {
                            new_increment = 1;
                        }
                    }
                } else {
                    new_increment =
                        i32::from(tables.env_logarithmic_time[target_delta as u8 as usize])
                            - env_time_setting;
                    if new_increment <= 0 {
                        new_increment = 1;
                    }
                }
            } else {
                new_increment = if new_target >= i32::from(self.target) {
                    0x80 | 127
                } else {
                    127
                };
            }
            if new_increment == 0 {
                new_increment = 1;
            }
        }
        self.start_ramp(
            ramp,
            tables,
            new_target as u8,
            new_increment as u8,
            new_phase,
        );
    }
}

impl Default for Tva {
    fn default() -> Tva {
        Tva::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_param(bytes: &[u8]) -> PartialParam<'_> {
        PartialParam(bytes)
    }

    fn a_host() -> TvaHost {
        TvaHost {
            key: 60,
            velocity: 100,
            part_volume: 100,
            expression: 100,
            rhythm_output_level: None,
            master_vol: 100,
            can_sustain: true,
            ring_modulating_slave: false,
            ring_modulating_no_mix: false,
            nice_amp_ramp: true,
        }
    }

    /// A held note reaches sustain and its amp stays put; the release
    /// falls to dead through the interrupts, ending the partial.
    #[test]
    fn the_envelope_lives_and_dies_by_its_interrupts() {
        let tables = Tables::new();
        let quirks = &LAYOUTS[8].quirks;
        let mut bytes = [0u8; 58];
        bytes[41] = 100; // tva level
        for n in 0..5 {
            bytes[49 + n] = 40; // times
        }
        bytes[54] = 100;
        bytes[55] = 80;
        bytes[56] = 90;
        bytes[57] = 70; // sustain level
        let host = a_host();
        let mut tva = Tva::new();
        let mut ramp = Ramp::new();
        tva.reset(&tables, &mut ramp, a_param(&bytes), &host, quirks);
        assert!(tva.is_playing());
        for _ in 0..2_000_000 {
            ramp.next_value();
            if ramp.check_interrupt() {
                tva.handle_interrupt(&tables, &mut ramp, a_param(&bytes), &host, quirks);
            }
            if tva.phase() == TVA_PHASE_SUSTAIN {
                break;
            }
        }
        assert_eq!(tva.phase(), TVA_PHASE_SUSTAIN);
        tva.start_decay(&tables, &mut ramp, a_param(&bytes));
        for _ in 0..2_000_000 {
            ramp.next_value();
            if ramp.check_interrupt() {
                tva.handle_interrupt(&tables, &mut ramp, a_param(&bytes), &host, quirks);
            }
            if !tva.is_playing() {
                break;
            }
        }
        assert!(!tva.is_playing(), "the release ran to dead");
        assert_eq!(tva.phase(), TVA_PHASE_DEAD);
    }

    /// A sustain level of zero ends the partial at the sustain gate
    /// rather than holding silence open.
    #[test]
    fn zero_sustain_ends_at_the_gate() {
        let tables = Tables::new();
        let quirks = &LAYOUTS[8].quirks;
        let mut bytes = [0u8; 58];
        bytes[41] = 100;
        for n in 0..5 {
            bytes[49 + n] = 20;
        }
        bytes[54] = 100;
        bytes[55] = 80;
        bytes[56] = 60;
        bytes[57] = 0; // no sustain
        let host = a_host();
        let mut tva = Tva::new();
        let mut ramp = Ramp::new();
        tva.reset(&tables, &mut ramp, a_param(&bytes), &host, quirks);
        for _ in 0..2_000_000 {
            ramp.next_value();
            if ramp.check_interrupt() {
                tva.handle_interrupt(&tables, &mut ramp, a_param(&bytes), &host, quirks);
            }
            if !tva.is_playing() {
                break;
            }
        }
        assert!(!tva.is_playing());
    }

    /// The ring-modulating slave skips the volume chain entirely: master
    /// volume cannot silence it, only its own level can.
    #[test]
    fn a_ring_slave_ignores_the_volume_chain() {
        let tables = Tables::new();
        let quirks = &LAYOUTS[8].quirks;
        let mut bytes = [0u8; 58];
        bytes[41] = 100;
        let mut host = a_host();
        host.master_vol = 0;
        let silent = calc_basic_amp(&tables, a_param(&bytes), &host, 0, 0, quirks);
        assert_eq!(silent, 0, "master volume zero silences a normal partial");
        host.ring_modulating_slave = true;
        let slave = calc_basic_amp(&tables, a_param(&bytes), &host, 0, 0, quirks);
        assert!(slave > 0, "the slave plays on: {slave}");
    }
}

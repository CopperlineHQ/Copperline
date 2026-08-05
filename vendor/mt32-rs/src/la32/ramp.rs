// SPDX-License-Identifier: LGPL-2.1-or-later

//! The LA32's ramps: the chip's own smooth transitions for amplitude and
//! filter cutoff.
//!
//! The firmware starts a ramp by writing a target (0-255) and an increment
//! byte whose top bit is the direction and whose lower seven bits set the
//! speed through the chip's exponent table. The ramp runs from wherever the
//! internal value already is -- usually the end of the previous ramp -- and
//! raises an interrupt a fixed seven steps after the target is reached,
//! which is what the envelopes advance their phases on. An increment of
//! zero freezes the ramp and never interrupts.

use crate::tables::Tables;

/// The internal value keeps the 8-bit target left-shifted by this.
const TARGET_SHIFTS: u32 = 18;
const MAX_CURRENT: u32 = 0xFF << TARGET_SHIFTS;
/// How many steps after reaching the target the interrupt arrives.
const INTERRUPT_TIME: i32 = 7;

/// One ramp generator, as the chip runs one per envelope.
#[derive(Debug, Clone)]
pub struct Ramp {
    current: u32,
    large_target: u32,
    large_increment: u32,
    descending: bool,
    interrupt_countdown: i32,
    interrupt_raised: bool,
}

impl Ramp {
    pub fn new() -> Ramp {
        Ramp {
            current: 0,
            large_target: 0,
            large_increment: 0,
            descending: false,
            interrupt_countdown: 0,
            interrupt_raised: false,
        }
    }

    /// Aim at `target`, at the speed and in the direction `increment`
    /// encodes.
    pub fn start(&mut self, tables: &Tables, target: u8, increment: u8) {
        if increment == 0 {
            self.large_increment = 0;
        } else {
            let exp_arg = u32::from(increment & 0x7F);
            let mut large = 8191 - u32::from(tables.exp9[(!(exp_arg << 6) & 511) as usize]);
            large <<= exp_arg >> 3;
            large += 64;
            large >>= 9;
            self.large_increment = large;
        }
        self.descending = increment & 0x80 != 0;
        if self.descending {
            // A zero increment byte with the direction bit would freeze;
            // with it set, the chip still creeps.
            self.large_increment += 1;
        }
        self.large_target = u32::from(target) << TARGET_SHIFTS;
        self.interrupt_countdown = 0;
        self.interrupt_raised = false;
    }

    /// One step of the ramp: the value now, with the interrupt pending
    /// once the target has stood its seven steps.
    pub fn next_value(&mut self) -> u32 {
        if self.interrupt_countdown > 0 {
            self.interrupt_countdown -= 1;
            if self.interrupt_countdown == 0 {
                self.interrupt_raised = true;
            }
        } else if self.large_increment != 0 {
            if self.descending {
                if self.large_increment > self.current {
                    self.current = self.large_target;
                    self.interrupt_countdown = INTERRUPT_TIME;
                } else {
                    self.current -= self.large_increment;
                    if self.current <= self.large_target {
                        self.current = self.large_target;
                        self.interrupt_countdown = INTERRUPT_TIME;
                    }
                }
            } else if MAX_CURRENT - self.current < self.large_increment {
                self.current = self.large_target;
                self.interrupt_countdown = INTERRUPT_TIME;
            } else {
                self.current += self.large_increment;
                if self.current >= self.large_target {
                    self.current = self.large_target;
                    self.interrupt_countdown = INTERRUPT_TIME;
                }
            }
        }
        self.current
    }

    /// Whether the interrupt fired since last asked; asking clears it.
    pub fn check_interrupt(&mut self) -> bool {
        std::mem::take(&mut self.interrupt_raised)
    }

    pub fn reset(&mut self) {
        *self = Ramp::new();
    }

    /// Whether `target` sits below the value the ramp holds now: how the
    /// amp envelope tells which way its sustain will move.
    pub fn is_below_current(&self, target: u8) -> bool {
        u32::from(target) << TARGET_SHIFTS < self.current
    }
}

impl Default for Ramp {
    fn default() -> Ramp {
        Ramp::new()
    }
}

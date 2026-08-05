// SPDX-License-Identifier: LGPL-2.1-or-later

//! The pitch-envelope jitter source.
//!
//! The hardware's software timer does not fire exactly on time, and the
//! reference engine reproduces the resulting pitch wobble by adding a
//! little randomness to the timer period. It reaches for libc `rand()`,
//! which is process-global and platform-dependent; the oracle build renames
//! that call to a defined generator (see `oracle/README.md`), and this is
//! the same generator -- the C standard's example LCG -- owned per synth so
//! nothing outside a synth changes what it renders.

/// The generator, seeded as a fresh process would be.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    pub fn new() -> Jitter {
        Jitter { state: 1 }
    }

    /// The next value, 0..32768, exactly as the shim's stand-in computes.
    pub fn next_value(&mut self) -> i32 {
        self.state = self.state.wrapping_mul(1103515245).wrapping_add(12345);
        ((self.state / 65536) % 32768) as i32
    }
}

impl Default for Jitter {
    fn default() -> Jitter {
        Jitter::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C standard's own example sequence from seed 1.
    #[test]
    fn the_sequence_is_the_standard_example() {
        let mut jitter = Jitter::new();
        assert_eq!(jitter.next_value(), 16838);
        assert_eq!(jitter.next_value(), 5758);
        assert_eq!(jitter.next_value(), 10113);
    }
}

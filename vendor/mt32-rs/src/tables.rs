// SPDX-License-Identifier: LGPL-2.1-or-later

//! The constant lookup tables: the LA32's own exponent and log-sine ROMs,
//! and the firmware's level curves.
//!
//! Everything here is computed, not read from the ROMs, with the formulas
//! the reference engine settled on against tables recovered from real
//! chips. Each expression mirrors the reference bit for bit -- including
//! which precision each step runs at, since the values are truncated into
//! narrow integers and a step done in the wrong width moves entries. The
//! differential test compares every entry against the engine's.

/// The constant tables, built once and shared.
#[derive(Debug)]
pub struct Tables {
    /// How much a level parameter (an output level, a TVA level, an
    /// expression value) subtracts from an amp target, 0-100 in.
    pub level_to_amp_subtraction: [u8; 101],
    /// The envelope time curve: logarithmic in its argument, offset so
    /// zero maps to 64.
    pub env_logarithmic_time: [u8; 256],
    /// What the master volume subtracts from every amp, 0-100 in; zero
    /// silences outright.
    pub master_vol_to_amp_subtraction: [u8; 101],
    /// A 0-100 pulse width onto the LA32's 0-255.
    pub pulse_width_100_to_255: [u8; 101],
    /// The LA32's internal exponent table: 512 twelve-bit values indexed
    /// by the top nine fractional bits.
    pub exp9: [u16; 512],
    /// The LA32's logarithmic sine table: 512 thirteen-bit values, the
    /// first clamped to the maximum.
    pub logsin9: [u16; 512],
    /// Resonance amp decay factors, found from sample analysis.
    pub res_amp_decay_factors: [u8; 8],
}

impl Tables {
    /// Built in full; callers keep one and share it.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut level_to_amp_subtraction = [0u8; 101];
        for (lf, out) in level_to_amp_subtraction.iter_mut().enumerate() {
            let f_val = (2.0f32 - (lf as f32 + 1.0).log10()) * 128.0;
            *out = (f64::from(f_val) + 1.0).min(255.0) as u8;
        }

        let mut env_logarithmic_time = [0u8; 256];
        env_logarithmic_time[0] = 64;
        for (lf, out) in env_logarithmic_time.iter_mut().enumerate().skip(1) {
            *out = (64.0 + f64::from((lf as f32).log2()) * 8.0).ceil() as u8;
        }

        let mut master_vol_to_amp_subtraction = [0u8; 101];
        master_vol_to_amp_subtraction[0] = 255;
        for (vol, out) in master_vol_to_amp_subtraction.iter_mut().enumerate().skip(1) {
            *out = (106.31 - f64::from(16.0f32 * (vol as f32).log2())) as u8;
        }

        let mut pulse_width_100_to_255 = [0u8; 101];
        for (i, out) in pulse_width_100_to_255.iter_mut().enumerate() {
            *out = ((i * 255) as f32 / 100.0 + 0.5) as u8;
        }

        // The LA32's exponent table: 12-bit values addressed by the nine
        // high fractional bits; the low bits interpolate against a second
        // table of differences the engine derives on the fly.
        let mut exp9 = [0u16; 512];
        for (i, out) in exp9.iter_mut().enumerate() {
            *out = (8191.5 - f64::from((13.0 + !(i as i32) as f32 / 512.0).exp2())) as u16;
        }

        let mut logsin9 = [0u16; 512];
        for (i, out) in logsin9.iter_mut().enumerate().skip(1) {
            let sine = ((i as f32 + 0.5) / 1024.0 * std::f32::consts::PI).sin();
            *out = (0.5 - f64::from(sine.log2()) * 1024.0) as u16;
        }
        // The first value is off the top of thirteen bits; the chip clamps.
        logsin9[0] = 8191;

        Self {
            level_to_amp_subtraction,
            env_logarithmic_time,
            master_vol_to_amp_subtraction,
            pulse_width_100_to_255,
            exp9,
            logsin9,
            res_amp_decay_factors: [31, 16, 12, 8, 5, 3, 2, 1],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corners the formulas pin: silence at zero master volume, the
    /// clamped ends of the LA32 tables, and the level curve's saturation.
    #[test]
    fn the_corners_hold() {
        let t = Tables::new();
        assert_eq!(t.master_vol_to_amp_subtraction[0], 255);
        assert_eq!(t.master_vol_to_amp_subtraction[100], 0);
        assert_eq!(t.level_to_amp_subtraction[0], 255, "clamped at the top");
        assert_eq!(t.level_to_amp_subtraction[100], 0);
        assert_eq!(t.env_logarithmic_time[0], 64);
        assert_eq!(t.logsin9[0], 8191, "the clamp the chip applies");
        assert_eq!(t.pulse_width_100_to_255[0], 0);
        assert_eq!(t.pulse_width_100_to_255[100], 255);
        assert!(t.exp9.iter().all(|&v| v < 4096), "twelve-bit values");
        assert!(t.logsin9.iter().all(|&v| v < 8192), "thirteen-bit values");
    }
}

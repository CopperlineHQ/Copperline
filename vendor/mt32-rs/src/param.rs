// SPDX-License-Identifier: LGPL-2.1-or-later

//! Typed views over the parameter bytes.
//!
//! The memory model owns the bytes; everything that computes reads them
//! through these views, so there is one copy of the truth and the offsets
//! are written down once. Field meanings and ranges are the manual's.

/// One partial's 58 parameter bytes within a timbre.
#[derive(Debug, Clone, Copy)]
pub struct PartialParam<'a>(pub &'a [u8]);

impl PartialParam<'_> {
    // WG, 8 bytes at 0.
    pub fn pitch_coarse(&self) -> u8 {
        self.0[0]
    }
    pub fn pitch_fine(&self) -> u8 {
        self.0[1]
    }
    pub fn pitch_keyfollow(&self) -> u8 {
        self.0[2]
    }
    pub fn pitch_bender_enabled(&self) -> u8 {
        self.0[3]
    }
    pub fn waveform(&self) -> u8 {
        self.0[4]
    }
    pub fn pcm_wave(&self) -> u8 {
        self.0[5]
    }
    pub fn pulse_width(&self) -> u8 {
        self.0[6]
    }
    pub fn pulse_width_velo_sensitivity(&self) -> u8 {
        self.0[7]
    }

    // The pitch envelope, 12 bytes at 8.
    pub fn pitch_env_depth(&self) -> u8 {
        self.0[8]
    }
    pub fn pitch_env_velo_sensitivity(&self) -> u8 {
        self.0[9]
    }
    pub fn pitch_env_time_keyfollow(&self) -> u8 {
        self.0[10]
    }
    pub fn pitch_env_time(&self, n: usize) -> u8 {
        self.0[11 + n]
    }
    pub fn pitch_env_level(&self, n: usize) -> u8 {
        self.0[15 + n]
    }

    // The pitch LFO, 3 bytes at 20.
    pub fn pitch_lfo_rate(&self) -> u8 {
        self.0[20]
    }
    pub fn pitch_lfo_depth(&self) -> u8 {
        self.0[21]
    }
    pub fn pitch_lfo_mod_sensitivity(&self) -> u8 {
        self.0[22]
    }

    // The TVF, 18 bytes at 23.
    pub fn tvf_cutoff(&self) -> u8 {
        self.0[23]
    }
    pub fn tvf_resonance(&self) -> u8 {
        self.0[24]
    }
    pub fn tvf_keyfollow(&self) -> u8 {
        self.0[25]
    }
    pub fn tvf_bias_point(&self) -> u8 {
        self.0[26]
    }
    pub fn tvf_bias_level(&self) -> u8 {
        self.0[27]
    }
    pub fn tvf_env_depth(&self) -> u8 {
        self.0[28]
    }
    pub fn tvf_env_velo_sensitivity(&self) -> u8 {
        self.0[29]
    }
    pub fn tvf_env_depth_keyfollow(&self) -> u8 {
        self.0[30]
    }
    pub fn tvf_env_time_keyfollow(&self) -> u8 {
        self.0[31]
    }
    pub fn tvf_env_time(&self, n: usize) -> u8 {
        self.0[32 + n]
    }
    pub fn tvf_env_level(&self, n: usize) -> u8 {
        self.0[37 + n]
    }

    // The TVA, 17 bytes at 41.
    pub fn tva_level(&self) -> u8 {
        self.0[41]
    }
    pub fn tva_velo_sensitivity(&self) -> u8 {
        self.0[42]
    }
    pub fn tva_bias_point1(&self) -> u8 {
        self.0[43]
    }
    pub fn tva_bias_level1(&self) -> u8 {
        self.0[44]
    }
    pub fn tva_bias_point2(&self) -> u8 {
        self.0[45]
    }
    pub fn tva_bias_level2(&self) -> u8 {
        self.0[46]
    }
    pub fn tva_env_time_keyfollow(&self) -> u8 {
        self.0[47]
    }
    pub fn tva_env_time_velo_sensitivity(&self) -> u8 {
        self.0[48]
    }
    pub fn tva_env_time(&self, n: usize) -> u8 {
        self.0[49 + n]
    }
    pub fn tva_env_level(&self, n: usize) -> u8 {
        self.0[54 + n]
    }
}

/// A part's patch temporary area: the patch it plays, and its level and
/// pan beside it.
#[derive(Debug, Clone, Copy)]
pub struct PatchTemp<'a>(pub &'a [u8]);

impl PatchTemp<'_> {
    pub fn timbre_group(&self) -> u8 {
        self.0[0]
    }
    pub fn timbre_num(&self) -> u8 {
        self.0[1]
    }
    pub fn key_shift(&self) -> u8 {
        self.0[2]
    }
    pub fn fine_tune(&self) -> u8 {
        self.0[3]
    }
    pub fn bender_range(&self) -> u8 {
        self.0[4]
    }
    pub fn assign_mode(&self) -> u8 {
        self.0[5]
    }
    pub fn reverb_switch(&self) -> u8 {
        self.0[6]
    }
    pub fn output_level(&self) -> u8 {
        self.0[8]
    }
    pub fn panpot(&self) -> u8 {
        self.0[9]
    }
}

/// A timbre's common parameters: what precedes its four partials.
#[derive(Debug, Clone, Copy)]
pub struct TimbreCommon<'a>(pub &'a [u8]);

impl TimbreCommon<'_> {
    pub fn name(&self) -> &[u8] {
        &self.0[..10]
    }
    pub fn partial_structure12(&self) -> u8 {
        self.0[10]
    }
    pub fn partial_structure34(&self) -> u8 {
        self.0[11]
    }
    pub fn partial_mute(&self) -> u8 {
        self.0[12]
    }
    pub fn no_sustain(&self) -> u8 {
        self.0[13]
    }
}

/// One rhythm key's four setup bytes.
#[derive(Debug, Clone, Copy)]
pub struct RhythmTemp<'a>(pub &'a [u8]);

impl RhythmTemp<'_> {
    pub fn timbre(&self) -> u8 {
        self.0[0]
    }
    pub fn output_level(&self) -> u8 {
        self.0[1]
    }
    pub fn panpot(&self) -> u8 {
        self.0[2]
    }
    pub fn reverb_switch(&self) -> u8 {
        self.0[3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{TIMBRE_COMMON, TIMBRE_PARTIAL};

    /// The views' offsets tile the structures exactly: the last field of
    /// each area sits flush against the next, and the partial's last
    /// envelope byte is its 58th.
    #[test]
    fn the_offsets_tile_the_structures() {
        let bytes: Vec<u8> = (0..TIMBRE_PARTIAL as u8).collect();
        let p = PartialParam(&bytes);
        assert_eq!(p.pulse_width_velo_sensitivity(), 7);
        assert_eq!(p.pitch_env_depth(), 8);
        assert_eq!(p.pitch_env_level(4), 19);
        assert_eq!(p.pitch_lfo_rate(), 20);
        assert_eq!(p.tvf_cutoff(), 23);
        assert_eq!(p.tvf_env_level(3), 40);
        assert_eq!(p.tva_level(), 41);
        assert_eq!(p.tva_env_level(3), TIMBRE_PARTIAL as u8 - 1);

        let common: Vec<u8> = (0..TIMBRE_COMMON as u8).collect();
        let c = TimbreCommon(&common);
        assert_eq!(c.no_sustain(), TIMBRE_COMMON as u8 - 1);
    }
}

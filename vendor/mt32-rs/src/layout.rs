// SPDX-License-Identifier: LGPL-2.1-or-later

//! Where each control ROM keeps its tables, and how its firmware behaves.
//!
//! The reference engine reads a control ROM through a per-version map --
//! offsets for the PCM table, the timbre banks, the rhythm setup, the limit
//! tables the firmware clamps writes against, and the display's two canned
//! messages -- plus a set of quirk flags recording where the elder firmware
//! genuinely computed differently. Both are transcribed here verbatim; the
//! `..BASE` rows keep what a generation shares in one place so a column can
//! be checked against upstream's table by eye.

use crate::rom::RomInfo;
#[cfg(test)]
use crate::rom::{Family, Kind};

/// How a control ROM's firmware behaves where the generations differ.
///
/// The `quirk_` flags are places the elder firmware's arithmetic overflowed
/// or clamped and the sound depends on reproducing that; the rest pick
/// behaviours -- display wording, note cancellation, the analog filter --
/// that go with the hardware the ROM shipped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quirks {
    pub base_pitch_overflow: bool,
    pub pitch_envelope_overflow: bool,
    pub ring_modulation_no_mix: bool,
    pub tva_zero_env_levels: bool,
    pub pan_mult: bool,
    pub key_shift: bool,
    pub tvf_base_cutoff_limit: bool,
    pub fast_pitch_changes: bool,
    pub display_custom_message_priority: bool,
    pub old_mt32_display_features: bool,
    pub new_gen_note_cancellation: bool,
    /// Not properties of the ROM at all, but of the unit it shipped in;
    /// the ROM version is how the hardware is told apart.
    pub default_reverb_mt32_compatible: bool,
    pub old_mt32_analog_lpf: bool,
}

/// The first-generation firmwares up to v1.05.
const ELDER_MT32: Quirks = Quirks {
    base_pitch_overflow: true,
    pitch_envelope_overflow: true,
    ring_modulation_no_mix: true,
    tva_zero_env_levels: true,
    pan_mult: true,
    key_shift: true,
    tvf_base_cutoff_limit: true,
    fast_pitch_changes: false,
    display_custom_message_priority: true,
    old_mt32_display_features: true,
    new_gen_note_cancellation: false,
    default_reverb_mt32_compatible: true,
    old_mt32_analog_lpf: true,
};

/// v1.06 onward: one display habit changed, the arithmetic did not.
const LATER_MT32: Quirks = Quirks {
    display_custom_message_priority: false,
    ..ELDER_MT32
};

/// The second generation and the CM-32L: none of the elder arithmetic,
/// new-generation note handling, and the newer unit's analog path.
const NEW_GEN: Quirks = Quirks {
    base_pitch_overflow: false,
    pitch_envelope_overflow: false,
    ring_modulation_no_mix: false,
    tva_zero_env_levels: false,
    pan_mult: false,
    key_shift: false,
    tvf_base_cutoff_limit: false,
    fast_pitch_changes: false,
    display_custom_message_priority: false,
    old_mt32_display_features: false,
    new_gen_note_cancellation: true,
    default_reverb_mt32_compatible: false,
    old_mt32_analog_lpf: false,
};

/// The CM-32LN additionally takes pitch changes at the faster rate.
const CM32LN: Quirks = Quirks {
    fast_pitch_changes: true,
    ..NEW_GEN
};

/// Where one control ROM version keeps everything the engine reads out of
/// it. Offsets are into the ROM image; the sizes each field's data
/// occupies are the reference engine's, noted where they are not implied
/// by a count beside them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The registry short name this layout belongs to.
    pub short_name: &'static str,
    pub quirks: Quirks,
    /// 4 bytes per entry, `pcm_count` entries.
    pub pcm_table: u16,
    pub pcm_count: u16,
    /// 128 bytes: which timbre each bank-A slot holds.
    pub timbre_a_map: u16,
    pub timbre_a_offset: u16,
    pub timbre_a_compressed: bool,
    /// 128 bytes, as bank A.
    pub timbre_b_map: u16,
    pub timbre_b_offset: u16,
    pub timbre_b_compressed: bool,
    /// 2 bytes per entry, `timbre_r_count` entries.
    pub timbre_r_map: u16,
    pub timbre_r_count: u16,
    /// 4 bytes per entry, `rhythm_settings_count` entries. The elder ROMs
    /// actually carry 86 entries; the engine reads the 85 both share.
    pub rhythm_settings: u16,
    pub rhythm_settings_count: u16,
    /// 9 bytes: the partial reserve the unit powers up with.
    pub reserve_settings: u16,
    /// 8 bytes.
    pub pan_settings: u16,
    /// 8 bytes.
    pub program_settings: u16,
    /// The limit tables writes are clamped against: 4, 16, 23 and 72
    /// bytes respectively.
    pub rhythm_max_table: u16,
    pub patch_max_table: u16,
    pub system_max_table: u16,
    pub timbre_max_table: u16,
    /// 14 bytes per entry, `sound_groups_count` entries.
    pub sound_groups_table: u16,
    pub sound_groups_count: u16,
    /// 20 characters and a terminator, ready for the display.
    pub startup_message: u16,
    pub sysex_error_message: u16,
}

/// What the five first-generation ROMs share.
const GEN1: Layout = Layout {
    short_name: "",
    quirks: ELDER_MT32,
    pcm_table: 0x3000,
    pcm_count: 128,
    timbre_a_map: 0x8000,
    timbre_a_offset: 0x0000,
    timbre_a_compressed: false,
    timbre_b_map: 0xC000,
    timbre_b_offset: 0x4000,
    timbre_b_compressed: false,
    timbre_r_map: 0x3200,
    timbre_r_count: 30,
    rhythm_settings: 0,
    rhythm_settings_count: 85,
    reserve_settings: 0,
    pan_settings: 0,
    program_settings: 0,
    rhythm_max_table: 0,
    patch_max_table: 0,
    system_max_table: 0,
    timbre_max_table: 0,
    sound_groups_table: 0,
    sound_groups_count: 19,
    startup_message: 0x217A,
    sysex_error_message: 0,
};

/// What the second generation and the CM-32L family share.
const GEN2: Layout = Layout {
    quirks: NEW_GEN,
    pcm_table: 0x8100,
    timbre_a_map: 0x8000,
    timbre_a_offset: 0x8000,
    timbre_a_compressed: true,
    timbre_b_map: 0x8080,
    timbre_b_offset: 0x8000,
    timbre_b_compressed: true,
    timbre_r_map: 0x8500,
    timbre_r_count: 64,
    rhythm_settings: 0x8580,
    startup_message: 0x1EF0,
    ..GEN1
};

/// Every control ROM's layout, transcribed from the reference engine.
pub const LAYOUTS: [Layout; 12] = [
    Layout {
        short_name: "ctrl_mt32_1_04",
        rhythm_settings: 0x73A6,
        reserve_settings: 0x57C7,
        pan_settings: 0x57E2,
        program_settings: 0x57D0,
        rhythm_max_table: 0x5252,
        patch_max_table: 0x525E,
        system_max_table: 0x526E,
        timbre_max_table: 0x520A,
        sound_groups_table: 0x7064,
        sysex_error_message: 0x4BB6,
        ..GEN1
    },
    Layout {
        short_name: "ctrl_mt32_1_05",
        rhythm_settings: 0x7414,
        reserve_settings: 0x57C7,
        pan_settings: 0x57E2,
        program_settings: 0x57D0,
        rhythm_max_table: 0x5252,
        patch_max_table: 0x525E,
        system_max_table: 0x526E,
        timbre_max_table: 0x520A,
        sound_groups_table: 0x70CA,
        sysex_error_message: 0x4BB6,
        ..GEN1
    },
    Layout {
        short_name: "ctrl_mt32_1_06",
        quirks: LATER_MT32,
        rhythm_settings: 0x7414,
        reserve_settings: 0x57D9,
        pan_settings: 0x57F4,
        program_settings: 0x57E2,
        rhythm_max_table: 0x5264,
        patch_max_table: 0x5270,
        system_max_table: 0x5280,
        timbre_max_table: 0x521C,
        sound_groups_table: 0x70CA,
        sysex_error_message: 0x4BBA,
        ..GEN1
    },
    Layout {
        short_name: "ctrl_mt32_1_07",
        quirks: LATER_MT32,
        rhythm_settings: 0x73FE,
        reserve_settings: 0x57B1,
        pan_settings: 0x57CC,
        program_settings: 0x57BA,
        rhythm_max_table: 0x523C,
        patch_max_table: 0x5248,
        system_max_table: 0x5258,
        timbre_max_table: 0x51F4,
        sound_groups_table: 0x70B0,
        sysex_error_message: 0x4B92,
        ..GEN1
    },
    Layout {
        short_name: "ctrl_mt32_bluer",
        quirks: LATER_MT32,
        rhythm_settings: 0x741C,
        reserve_settings: 0x57E5,
        pan_settings: 0x5800,
        program_settings: 0x57EE,
        rhythm_max_table: 0x5270,
        patch_max_table: 0x527C,
        system_max_table: 0x528C,
        timbre_max_table: 0x5228,
        sound_groups_table: 0x70CE,
        sysex_error_message: 0x4BC6,
        ..GEN1
    },
    Layout {
        short_name: "ctrl_mt32_2_03",
        reserve_settings: 0x4F49,
        pan_settings: 0x4F64,
        program_settings: 0x4F52,
        rhythm_max_table: 0x4885,
        patch_max_table: 0x4889,
        system_max_table: 0x48A2,
        timbre_max_table: 0x48B9,
        sound_groups_table: 0x5A44,
        sysex_error_message: 0x4066,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_mt32_2_04",
        reserve_settings: 0x4F5D,
        pan_settings: 0x4F78,
        program_settings: 0x4F66,
        rhythm_max_table: 0x4899,
        patch_max_table: 0x489D,
        system_max_table: 0x48B6,
        timbre_max_table: 0x48CD,
        sound_groups_table: 0x5A58,
        sysex_error_message: 0x406D,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_mt32_2_06",
        reserve_settings: 0x4F69,
        pan_settings: 0x4F84,
        program_settings: 0x4F72,
        rhythm_max_table: 0x48A5,
        patch_max_table: 0x48A9,
        system_max_table: 0x48C2,
        timbre_max_table: 0x48D9,
        sound_groups_table: 0x5A64,
        sysex_error_message: 0x4021,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_mt32_2_07",
        reserve_settings: 0x4F81,
        pan_settings: 0x4F9C,
        program_settings: 0x4F8A,
        rhythm_max_table: 0x48B9,
        patch_max_table: 0x48BD,
        system_max_table: 0x48D6,
        timbre_max_table: 0x48ED,
        sound_groups_table: 0x5A78,
        startup_message: 0x1EE7,
        sysex_error_message: 0x4035,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_cm32l_1_00",
        pcm_count: 256,
        reserve_settings: 0x4F65,
        pan_settings: 0x4F80,
        program_settings: 0x4F6E,
        rhythm_max_table: 0x48A1,
        patch_max_table: 0x48A5,
        system_max_table: 0x48BE,
        timbre_max_table: 0x48D5,
        sound_groups_table: 0x5A6C,
        sysex_error_message: 0x401D,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_cm32l_1_02",
        pcm_count: 256,
        reserve_settings: 0x4F93,
        pan_settings: 0x4FAE,
        program_settings: 0x4F9C,
        rhythm_max_table: 0x48CB,
        patch_max_table: 0x48CF,
        system_max_table: 0x48E8,
        timbre_max_table: 0x48FF,
        sound_groups_table: 0x5A96,
        startup_message: 0x1EE7,
        sysex_error_message: 0x4047,
        ..GEN2
    },
    Layout {
        short_name: "ctrl_cm32ln_1_00",
        quirks: CM32LN,
        pcm_count: 256,
        reserve_settings: 0x4EC7,
        pan_settings: 0x4EE2,
        program_settings: 0x4ED0,
        rhythm_max_table: 0x47FF,
        patch_max_table: 0x4803,
        system_max_table: 0x481C,
        timbre_max_table: 0x4833,
        sound_groups_table: 0x55A2,
        startup_message: 0x1F59,
        sysex_error_message: 0x3F7C,
        ..GEN2
    },
];

/// The layout a control ROM is read with.
pub fn layout_for(info: &RomInfo) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|l| l.short_name == info.short_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom::KNOWN_ROMS;

    /// Every control ROM in the registry reads through exactly one layout,
    /// PCM ROMs through none, and no layout is orphaned.
    #[test]
    fn the_registry_and_the_layouts_agree() {
        for rom in &KNOWN_ROMS {
            match rom.kind {
                Kind::Control => assert!(
                    layout_for(rom).is_some(),
                    "{} has no layout",
                    rom.short_name
                ),
                Kind::Pcm => assert!(layout_for(rom).is_none(), "{}", rom.short_name),
            }
        }
        for layout in &LAYOUTS {
            assert!(
                KNOWN_ROMS.iter().any(|r| r.short_name == layout.short_name),
                "{} maps a ROM the registry does not know",
                layout.short_name
            );
        }
    }

    /// Every offset a layout names, plus what is stored there, fits inside
    /// the image it describes.
    #[test]
    fn every_table_fits_inside_its_rom() {
        for layout in &LAYOUTS {
            let rom = KNOWN_ROMS
                .iter()
                .find(|r| r.short_name == layout.short_name)
                .unwrap();
            let size = rom.size;
            let fits = |name: &str, offset: u16, len: usize| {
                assert!(
                    usize::from(offset) + len <= size,
                    "{}: {name} runs off the image",
                    layout.short_name
                );
            };
            fits(
                "pcm table",
                layout.pcm_table,
                usize::from(layout.pcm_count) * 4,
            );
            fits("timbre A map", layout.timbre_a_map, 128);
            fits("timbre B map", layout.timbre_b_map, 128);
            fits(
                "timbre R map",
                layout.timbre_r_map,
                usize::from(layout.timbre_r_count) * 2,
            );
            fits(
                "rhythm settings",
                layout.rhythm_settings,
                usize::from(layout.rhythm_settings_count) * 4,
            );
            fits("reserve settings", layout.reserve_settings, 9);
            fits("pan settings", layout.pan_settings, 8);
            fits("program settings", layout.program_settings, 8);
            fits("rhythm max", layout.rhythm_max_table, 4);
            fits("patch max", layout.patch_max_table, 16);
            fits("system max", layout.system_max_table, 23);
            fits("timbre max", layout.timbre_max_table, 72);
            fits(
                "sound groups",
                layout.sound_groups_table,
                usize::from(layout.sound_groups_count) * 14,
            );
            fits("startup message", layout.startup_message, 21);
            fits("sysex error message", layout.sysex_error_message, 21);
        }
    }

    /// The generations divide as the hardware did: elder arithmetic and
    /// uncompressed timbres on the 64 KiB MT-32 ROMs, compressed timbres
    /// and the larger rhythm bank from the second generation on, and the
    /// bigger PCM table only where a CM-32L's samples are.
    #[test]
    fn the_generations_divide_as_the_hardware_did() {
        for layout in &LAYOUTS {
            let rom = KNOWN_ROMS
                .iter()
                .find(|r| r.short_name == layout.short_name)
                .unwrap();
            let gen1 = rom.family == Some(Family::Mt32Gen1);
            assert_eq!(
                layout.quirks.base_pitch_overflow, gen1,
                "{}",
                layout.short_name
            );
            assert_eq!(layout.timbre_a_compressed, !gen1, "{}", layout.short_name);
            assert_eq!(
                layout.timbre_r_count,
                if gen1 { 30 } else { 64 },
                "{}",
                layout.short_name
            );
            assert_eq!(
                layout.pcm_count == 256,
                rom.family == Some(Family::Cm32l),
                "{}",
                layout.short_name
            );
        }
    }
}

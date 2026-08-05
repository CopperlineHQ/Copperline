// SPDX-License-Identifier: LGPL-2.1-or-later

//! ROM images: which ones exist, and whether a file is one.
//!
//! A ROM is identified by its size and SHA-1 rather than its filename, so a
//! mislabelled or truncated file is refused before it can open a synth that
//! sounds subtly wrong. The registry is the reference engine's, full images
//! only: the half- and interleaved-dump forms some archives carry are a
//! curation problem, not an emulation one, and a user with halves can join
//! them once rather than teach every consumer to.

use crate::sha1;

/// What a ROM holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The firmware: code the real unit's microcontroller runs, and the
    /// tables -- timbres, rhythm setup, PCM addressing -- the engine reads.
    Control,
    /// The sample store the LA32's PCM partials play from.
    Pcm,
}

/// Which family of unit a control ROM came from, which is what decides the
/// memory layout the engine reads it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// The first-generation MT-32, 64 KiB control ROMs.
    Mt32Gen1,
    /// The second-generation MT-32, 128 KiB control ROMs whose upper half
    /// carries the demo songs.
    Mt32Gen2,
    /// The CM-32L line, 64 KiB control ROMs with the extended sound set.
    Cm32l,
}

/// One known image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RomInfo {
    pub size: usize,
    /// Lowercase hex, as registries quote it.
    pub sha1: &'static str,
    pub kind: Kind,
    /// The reference engine's short name, which the differential tests
    /// compare against directly.
    pub short_name: &'static str,
    pub description: &'static str,
    /// `None` for a PCM image: samples have no family, and either control
    /// generation may address into them.
    pub family: Option<Family>,
}

/// Every full image the engine accepts, as the reference engine lists them.
pub const KNOWN_ROMS: [RomInfo; 14] = [
    control(
        "5a5cb5a77d7d55ee69657c2f870416daed52dea7",
        "ctrl_mt32_1_04",
        "MT-32 Control v1.04",
        Family::Mt32Gen1,
    ),
    control(
        "e17a3a6d265bf1fa150312061134293d2b58288c",
        "ctrl_mt32_1_05",
        "MT-32 Control v1.05",
        Family::Mt32Gen1,
    ),
    control(
        "a553481f4e2794c10cfe597fef154eef0d8257de",
        "ctrl_mt32_1_06",
        "MT-32 Control v1.06",
        Family::Mt32Gen1,
    ),
    control(
        "b083518fffb7f66b03c23b7eb4f868e62dc5a987",
        "ctrl_mt32_1_07",
        "MT-32 Control v1.07",
        Family::Mt32Gen1,
    ),
    control(
        "7b8c2a5ddb42fd0732e2f22b3340dcf5360edf92",
        "ctrl_mt32_bluer",
        "MT-32 Control BlueRidge",
        Family::Mt32Gen1,
    ),
    control_gen2(
        "5837064c9df4741a55f7c4d8787ac158dff2d3ce",
        "ctrl_mt32_2_03",
        "MT-32 Control v2.03",
    ),
    control_gen2(
        "2c16432b6c73dd2a3947cba950a0f4c19d6180eb",
        "ctrl_mt32_2_04",
        "MT-32 Control v2.04",
    ),
    control_gen2(
        "2869cf4c235d671668cfcb62415e2ce8323ad4ed",
        "ctrl_mt32_2_06",
        "MT-32 Control v2.06",
    ),
    control_gen2(
        "47b52adefedaec475c925e54340e37673c11707c",
        "ctrl_mt32_2_07",
        "MT-32 Control v2.07",
    ),
    control(
        "73683d585cd6948cc19547942ca0e14a0319456d",
        "ctrl_cm32l_1_00",
        "CM-32L/LAPC-I Control v1.00",
        Family::Cm32l,
    ),
    control(
        "a439fbb390da38cada95a7cbb1d6ca199cd66ef8",
        "ctrl_cm32l_1_02",
        "CM-32L/LAPC-I Control v1.02",
        Family::Cm32l,
    ),
    control(
        "dc1c5b1b90a4646d00f7daf3679733c7badc7077",
        "ctrl_cm32ln_1_00",
        "CM-32LN/CM-500/LAPC-N Control v1.00",
        Family::Cm32l,
    ),
    RomInfo {
        size: 512 * 1024,
        sha1: "f6b1eebc4b2d200ec6d3d21d51325d5b48c60252",
        kind: Kind::Pcm,
        short_name: "pcm_mt32",
        description: "MT-32 PCM ROM",
        family: None,
    },
    RomInfo {
        size: 1024 * 1024,
        sha1: "289cc298ad532b702461bfc738009d9ebe8025ea",
        kind: Kind::Pcm,
        short_name: "pcm_cm32l",
        description: "CM-32L/CM-64/LAPC-I PCM ROM",
        family: None,
    },
];

const fn control(
    sha1: &'static str,
    short_name: &'static str,
    description: &'static str,
    family: Family,
) -> RomInfo {
    RomInfo {
        size: 64 * 1024,
        sha1,
        kind: Kind::Control,
        short_name,
        description,
        family: Some(family),
    }
}

const fn control_gen2(
    sha1: &'static str,
    short_name: &'static str,
    description: &'static str,
) -> RomInfo {
    RomInfo {
        size: 128 * 1024,
        sha1,
        kind: Kind::Control,
        short_name,
        description,
        family: Some(Family::Mt32Gen2),
    }
}

/// Which known image `image` is, or `None` for anything else -- the wrong
/// size never reaches the hash.
pub fn identify(image: &[u8]) -> Option<&'static RomInfo> {
    if !KNOWN_ROMS.iter().any(|r| r.size == image.len()) {
        return None;
    }
    let digest = sha1::hex_digest(image);
    KNOWN_ROMS.iter().find(|r| r.sha1 == digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registry entry is well-formed: a forty-character digest, a
    /// size a full image of its kind can have, and a family exactly when
    /// it is a control ROM.
    #[test]
    fn the_registry_is_coherent() {
        for rom in &KNOWN_ROMS {
            assert_eq!(rom.sha1.len(), 40, "{}", rom.short_name);
            assert!(
                rom.sha1.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}",
                rom.short_name
            );
            match rom.kind {
                Kind::Control => {
                    assert!(matches!(rom.size, 0x10000 | 0x20000), "{}", rom.short_name);
                    assert!(rom.family.is_some(), "{}", rom.short_name);
                    assert_eq!(
                        rom.family == Some(Family::Mt32Gen2),
                        rom.size == 0x20000,
                        "{}: the second generation is the 128 KiB one",
                        rom.short_name
                    );
                }
                Kind::Pcm => {
                    assert!(matches!(rom.size, 0x80000 | 0x100000), "{}", rom.short_name);
                    assert!(rom.family.is_none(), "{}", rom.short_name);
                }
            }
        }
    }

    /// Junk is refused without being hashed; the right size with the wrong
    /// bytes is refused after.
    #[test]
    fn what_is_not_a_rom_is_not_one() {
        assert!(identify(&[]).is_none());
        assert!(identify(&[0u8; 1234]).is_none());
        assert!(identify(&[0u8; 0x10000]).is_none(), "size alone is not it");
    }
}

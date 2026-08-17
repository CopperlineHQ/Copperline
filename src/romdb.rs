// SPDX-License-Identifier: GPL-3.0-or-later

//! Kickstart ROM identification by checksum.
//!
//! A ROM image is a bare block of 68000 code: nothing inside it says "I am
//! Kickstart 3.1 for the A1200", and the file names people keep them under
//! are whatever their dumper or their collection manager chose. Every
//! released Amiga boot ROM is nonetheless a fixed, known block of bytes, so
//! the image can be named by its checksum. That is what this module does:
//! given the bytes of a ROM file it returns a human label such as
//! "Kickstart 3.1 (40.68) A1200", which the About panel, the start-up
//! banner, the ROM-load OSD, and the machine-configuration ROM tab show
//! beside the file name.
//!
//! The checksum data (CRC-32 and length per image) is derived from WinUAE's
//! `rommgr.cpp` ROM table. WinUAE is GPL-2.0-or-later, so the data is
//! licence-compatible with this GPL-3.0-or-later tree. Only the Amiga boot
//! ROMs are transcribed: the main Kickstarts, the CD32 Kickstart and
//! extended ROM, the CDTV/CDTV-CR extended ROMs and the A1000 bootstrap.
//! The even/odd EPROM halves of the split dumps are left out (they are not
//! loadable images on their own), as are the non-Amiga boards WinUAE also
//! carries (Arcadia, ALG, Cubo, DraCo, Casablanca) and the expansion-board
//! ROMs, which are not boot ROMs.
//!
//! Identification keys on (CRC-32, length) after the same normalisation the
//! ROM load path in `memory.rs` performs, so the forms a real collection
//! holds all resolve to the canonical image: a byte-swapped EPROM-programmer
//! dump, a 256 KiB part dumped through a 512 KiB window (and so stored
//! doubled), or the A1000 bootstrap's 8 KiB part echoed through its 64 KiB
//! window. Amiga Forever's encrypted images cannot be checksummed at all
//! without the user's `rom.key`, so they are reported as encrypted rather
//! than as unknown.

use std::borrow::Cow;
use std::path::Path;

/// One known ROM image: what to call it, and the (length, CRC-32) pair that
/// identifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RomEntry {
    /// Human label, e.g. "Kickstart 3.1 (40.68) A1200". Marketing version
    /// first, the ROM's own revision in parentheses, then the models the
    /// image was shipped for.
    pub label: &'static str,
    /// Length of the canonical image in bytes.
    pub size: usize,
    /// CRC-32 of the canonical image.
    pub crc32: u32,
}

impl RomEntry {
    const fn new(label: &'static str, size: usize, crc32: u32) -> Self {
        Self { label, size, crc32 }
    }
}

/// What a ROM image turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identified {
    /// A ROM in the table.
    Known(&'static RomEntry),
    /// An Amiga Forever encrypted image: an `AMIROMTYPE1` container whose
    /// payload is XORed with the adjacent `rom.key`. The bytes cannot be
    /// checksummed without that key, so this says what the file is rather
    /// than pretending the ROM is unknown.
    Encrypted,
}

impl Identified {
    /// The text a surface shows for this identification.
    pub fn label(&self) -> &'static str {
        match self {
            Identified::Known(entry) => entry.label,
            Identified::Encrypted => "Cloanto-encrypted ROM",
        }
    }
}

/// Amiga Forever's scrambled ROM container tag (see `whdload.rs`, which
/// decodes such images when the user's `rom.key` sits beside them).
const CLOANTO_TAG: &[u8] = b"AMIROMTYPE1";

/// Largest file [`describe_file`] will read. The biggest image in the table
/// is the 1 MiB combined CD32 ROM; twice that leaves room for a doubled
/// dump and the Cloanto tag, and stops the identification reading something
/// that cannot be a boot ROM at all.
const MAX_ROM_FILE_BYTES: u64 = 2 * 1024 * 1024 + 16;

/// Whether an image is an Amiga Forever encrypted container.
pub fn is_encrypted(data: &[u8]) -> bool {
    data.starts_with(CLOANTO_TAG)
}

/// Identify a ROM image from its bytes, after normalising byte order and
/// undoing any whole-image repetition (see the module docs).
pub fn identify(data: &[u8]) -> Option<&'static RomEntry> {
    if is_encrypted(data) {
        return None;
    }
    lookup_keys(data)
        .into_iter()
        .find_map(|(crc, len)| identify_crc(crc, len))
}

/// Identify a ROM image, reporting an encrypted container as such.
pub fn describe(data: &[u8]) -> Option<Identified> {
    if is_encrypted(data) {
        return Some(Identified::Encrypted);
    }
    identify(data).map(Identified::Known)
}

/// Identify the ROM image in a file. `None` for a file that cannot be read,
/// is far too large to be a boot ROM, or is not in the table.
pub fn describe_file(path: &Path) -> Option<Identified> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > MAX_ROM_FILE_BYTES {
        return None;
    }
    describe(&std::fs::read(path).ok()?)
}

/// The version pair a ROM image says for itself, read from the image
/// rather than the checksum table: the ROM header's version.revision
/// words, and the first `$VER:` cookie's own number (exec.library's,
/// in a Kickstart-shaped ROM). Either half may be missing; a file that
/// cannot be read yields nothing. This is how the bundled AROS -- which
/// no checksum table names, and which moves between releases -- gets
/// its numbers into the launcher's identification lines.
pub fn rom_self_versions(path: &Path) -> Option<(String, String)> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > MAX_ROM_FILE_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let header = if data.len() >= 16 {
        let ver = u16::from_be_bytes([data[12], data[13]]);
        let rev = u16::from_be_bytes([data[14], data[15]]);
        format!("{ver}.{rev}")
    } else {
        String::new()
    };
    let cookie = data
        .windows(6)
        .position(|w| w == b"$VER: ")
        .map(|i| {
            let tail = &data[i + 6..(i + 128).min(data.len())];
            let line: String = tail
                .iter()
                .take_while(|&&b| b != 0 && b != b'\r' && b != b'\n')
                .map(|&b| b as char)
                .collect();
            line.split_whitespace()
                .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default();
    Some((header, cookie))
}

/// The raw table lookup: an exact (CRC-32, length) match, with no
/// normalisation of its own.
pub fn identify_crc(crc: u32, len: usize) -> Option<&'static RomEntry> {
    ROMS.iter().find(|e| e.crc32 == crc && e.size == len)
}

/// CRC-32 of a buffer, computed the way [`crate::config::RomId`] does it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

/// Restore big-endian byte order in a byte-swapped image, mirroring
/// `memory::normalize_rom_byte_order`: an image prepared for an EPROM
/// programmer opens $xx11 $F94E instead of the $11xx $4EF9 ROM header, and
/// no big-endian ROM can start that way.
fn byte_order_normalized(data: &[u8]) -> Cow<'_, [u8]> {
    let swapped = data.len() >= 4
        && data.len().is_multiple_of(2)
        && data[1] == 0x11
        && data[2..4] == [0xF9, 0x4E];
    if !swapped {
        return Cow::Borrowed(data);
    }
    let mut out = data.to_vec();
    for pair in out.chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
    Cow::Owned(out)
}

/// One half of a buffer that is exactly two identical halves, else `None`.
/// A 256 KiB part read through a 512 KiB window comes back doubled, and the
/// A1000's 8 KiB bootstrap is echoed eight times across its 64 KiB window.
fn halved(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 2 || !data.len().is_multiple_of(2) {
        return None;
    }
    let (first, second) = data.split_at(data.len() / 2);
    (first == second).then_some(first)
}

/// The (CRC-32, length) keys to try for an image, most specific first: the
/// image as loaded (byte order restored), then each repetition of it undone
/// in turn. Looking the whole image up first keeps an image that is *itself*
/// in the table -- the A1000's 64 KiB bootstrap, which is its 8 KiB part
/// echoed -- identified as the image the user actually holds.
fn lookup_keys(data: &[u8]) -> Vec<(u32, usize)> {
    let normalized = byte_order_normalized(data);
    let mut view: &[u8] = &normalized;
    let mut keys = vec![(crc32(view), view.len())];
    while let Some(half) = halved(view) {
        view = half;
        keys.push((crc32(view), view.len()));
    }
    keys
}

/// The known Amiga boot ROMs (see the module docs for what is in and what is
/// out). Order follows WinUAE's table: main Kickstarts oldest first, then the
/// CD32 pair, the CDTV extended ROMs, and the A1000 bootstrap.
pub const ROMS: [RomEntry; 67] = [
    // Pre-release prototype (the "Velvet" development machine).
    RomEntry::new("Kickstart 23.93 (Velvet prototype)", 131072, 0xADCB44C9),
    // Kickstart 1.x: 256 KiB parts, mirrored into the 512 KiB ROM window.
    RomEntry::new("Kickstart 1.0 A1000 (NTSC)", 262144, 0x299790FF),
    RomEntry::new("Kickstart 1.1 (31.34) A1000 (NTSC)", 262144, 0xD060572A),
    RomEntry::new("Kickstart 1.1 (31.34) A1000 (PAL)", 262144, 0xEC86DAE2),
    RomEntry::new("Kickstart 1.2 (33.166) A1000", 262144, 0x9ED783D0),
    RomEntry::new(
        "Kickstart 1.2 (33.180) A500/A1000/A2000",
        262144,
        0xA6CE1636,
    ),
    RomEntry::new("Kickstart 1.3 (34.5) A500/A1000/A2000", 262144, 0xC4F0F55F),
    RomEntry::new("Kickstart 1.3 (34.5) A3000 (SK)", 262144, 0xE0F37258),
    // Kickstart 1.4/2.x.
    RomEntry::new("Kickstart 1.4 (36.16) A3000", 524288, 0xBC0EC13F),
    RomEntry::new("Kickstart 2.04 (37.175) A500+", 524288, 0xC3BDB240),
    RomEntry::new("Kickstart 2.05 (37.299) A600", 524288, 0x83028FB5),
    RomEntry::new("Kickstart 2.05 (37.300) A600HD", 524288, 0x64466C2A),
    RomEntry::new("Kickstart 2.05 (37.350) A600HD", 524288, 0x43B0DF7B),
    RomEntry::new("Kickstart 2.04 (37.175) A3000", 524288, 0x234A7233),
    // Kickstart 3.0/3.1. The Cloanto entries are Amiga Forever's own
    // re-encoded images of the same ROM, which checksum differently.
    RomEntry::new("Kickstart 3.0 (39.106) A1200", 524288, 0x6C9B07D2),
    RomEntry::new("Kickstart 3.0 (39.106) A4000", 524288, 0x9E6AC152),
    RomEntry::new("Kickstart 3.1 (40.70) A4000", 524288, 0x2B4566F1),
    RomEntry::new("Kickstart 3.1 (40.63) A500/A600/A2000", 524288, 0xFC24AE0D),
    RomEntry::new("Kickstart 3.1 (40.68) A1200", 524288, 0x1483A091),
    RomEntry::new("Kickstart 3.1 (40.68) A3000", 524288, 0xEFB239CC),
    RomEntry::new("Kickstart 3.1 (40.68) A4000 (Cloanto)", 524288, 0x43B6DD22),
    RomEntry::new("Kickstart 3.1 (40.68) A4000", 524288, 0xD6BAE334),
    RomEntry::new("Kickstart 3.1 (40.70) A4000T", 524288, 0x75932C3A),
    RomEntry::new("Kickstart 3.X (45.57) A4000 (Cloanto)", 524288, 0x3AC99EDC),
    // Hyperion's OS 3.1.4: two releases of the same 46.143 revision.
    RomEntry::new("Kickstart 3.1.4-1 (46.143) A1200", 524288, 0xF17FA97F),
    RomEntry::new("Kickstart 3.1.4-1 (46.143) A3000", 524288, 0x50C3529C),
    RomEntry::new("Kickstart 3.1.4-1 (46.143) A4000", 524288, 0xD47E18FD),
    RomEntry::new("Kickstart 3.1.4-1 (46.143) A4000T", 524288, 0x75A2B2A5),
    RomEntry::new("Kickstart 3.1.4-1 (46.143) A500", 524288, 0xD52B52FD),
    RomEntry::new("Kickstart 3.1.4-2 (46.143) A1200", 524288, 0xB87506A7),
    RomEntry::new("Kickstart 3.1.4-2 (46.143) A3000", 524288, 0xBA35F8EB),
    RomEntry::new("Kickstart 3.1.4-2 (46.143) A4000", 524288, 0x1B84CB33),
    RomEntry::new("Kickstart 3.1.4-2 (46.143) A4000T", 524288, 0xD6D0EF3E),
    RomEntry::new("Kickstart 3.1.4-2 (46.143) A500", 524288, 0x568F8786),
    // Hyperion's OS 3.2 line: marketing version in front, ROM revision in
    // parentheses (all of 3.2 is 47.x).
    RomEntry::new("Kickstart 3.2 (47.96) A1200", 524288, 0xBD1FF75E),
    RomEntry::new("Kickstart 3.2 (47.96) A3000", 524288, 0xF3AF46CC),
    RomEntry::new("Kickstart 3.2 (47.96) A4000", 524288, 0x9BB8FC93),
    RomEntry::new("Kickstart 3.2 (47.96) A4000T", 524288, 0x9188A509),
    RomEntry::new(
        "Kickstart 3.2 (47.96) A500/A600/A2000/A1000/CDTV",
        524288,
        0x8173D7B6,
    ),
    RomEntry::new("Kickstart 3.2.1 (47.102) A1200", 524288, 0x2B653371),
    RomEntry::new("Kickstart 3.2.1 (47.102) A3000", 524288, 0x0078F607),
    RomEntry::new("Kickstart 3.2.1 (47.102) A4000", 524288, 0xF3CED3B8),
    RomEntry::new("Kickstart 3.2.1 (47.102) A4000T", 524288, 0xAF3452EC),
    RomEntry::new(
        "Kickstart 3.2.1 (47.102) A500/A600/A2000/A1000/CDTV",
        524288,
        0x4F078456,
    ),
    RomEntry::new("Kickstart 3.2.2 (47.111) A1200", 524288, 0x5C40328A),
    RomEntry::new("Kickstart 3.2.2 (47.111) A3000", 524288, 0x46335B57),
    RomEntry::new("Kickstart 3.2.2 (47.111) A4000", 524288, 0x4BEA9798),
    RomEntry::new("Kickstart 3.2.2 (47.111) A4000T", 524288, 0x36BBCD8A),
    RomEntry::new(
        "Kickstart 3.2.2 (47.111) A500/A600/A2000/A1000/CDTV",
        524288,
        0xE4458462,
    ),
    RomEntry::new("Kickstart 3.2.3 (47.115) A1200", 524288, 0xB18D3B67),
    RomEntry::new("Kickstart 3.2.3 (47.115) A3000", 524288, 0x74C0B23F),
    RomEntry::new("Kickstart 3.2.3 (47.115) A4000", 524288, 0xB6A4698E),
    RomEntry::new("Kickstart 3.2.3 (47.115) A4000T", 524288, 0x588A5E6D),
    RomEntry::new(
        "Kickstart 3.2.3 (47.115) A500/A600/A2000/A1000/CDTV",
        524288,
        0xE1F50B0B,
    ),
    // The Walker prototype's own ROM.
    RomEntry::new("Kickstart 3.2 (43.1) Walker", 524288, 0x261339F8),
    // CD32: the Kickstart part, the extended ROM at $E00000, and the two
    // combined 1 MiB images (the community one and the real 391640-03 dump).
    RomEntry::new("Kickstart 3.1 (40.60) CD32", 524288, 0x1E62D4A5),
    RomEntry::new("CD32 extended ROM (40.60)", 524288, 0x87746BE2),
    RomEntry::new("CD32 Kickstart + extended ROM (40.60)", 1048576, 0xF5D4F3C8),
    RomEntry::new(
        "CD32 Kickstart + extended ROM (40.60, 391640-03)",
        1048576,
        0xA4FBC94A,
    ),
    // CDTV / A570 / CDTV-CR extended ROMs at $F00000.
    RomEntry::new("CDTV extended ROM v1.0", 262144, 0x42BAA124),
    RomEntry::new("CDTV extended ROM v2.7", 262144, 0xCEAE68D2),
    RomEntry::new("CDTV/A570 extended ROM v2.30", 262144, 0x30B54232),
    RomEntry::new("CDTV extended ROM v47.1", 262144, 0xAB6274E7),
    RomEntry::new("CDTV-CR extended ROM v3.32", 262144, 0x581A85CF),
    RomEntry::new("CDTV-CR extended ROM v3.44", 262144, 0x0B7BD64F),
    // The A1000 has no Kickstart ROM: it bootstraps from this part and
    // loads Kickstart into writable control store from disk. The 64 KiB
    // image is the 8 KiB part echoed across its window.
    RomEntry::new("A1000 bootstrap ROM", 65536, 0x0B1AD2D0),
    RomEntry::new("A1000 bootstrap ROM (8K part)", 8192, 0x62F11C04),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic "ROM" body: not a real image, but a deterministic block
    /// to drive the normalisation with. Deliberately not periodic, so a
    /// block does not read as its own repeated halves.
    fn body(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i * 7 + 3) as u8 ^ (i >> 8) as u8)
            .collect()
    }

    #[test]
    fn table_entries_are_unique_plausible_and_ascii() {
        // The lookup key is (size, crc32): two entries sharing one would
        // make an image identify as whichever came first in the table.
        let mut keys: Vec<(usize, u32)> = ROMS.iter().map(|e| (e.size, e.crc32)).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate (size, crc32) in the table");
        // Every Amiga boot ROM is one of these sizes; anything else is a
        // transcription slip rather than a real image.
        const SIZES: [usize; 6] = [8192, 65536, 131072, 262144, 524288, 1048576];
        for e in &ROMS {
            assert!(
                SIZES.contains(&e.size),
                "{}: implausible ROM size {}",
                e.label,
                e.size
            );
            assert!(!e.label.is_empty(), "empty label for crc {:08x}", e.crc32);
            assert!(e.label.is_ascii(), "non-ASCII label {:?}", e.label);
        }
    }

    #[test]
    fn identify_crc_finds_well_known_kickstarts() {
        assert_eq!(
            identify_crc(0xC4F0F55F, 262144).map(|e| e.label),
            Some("Kickstart 1.3 (34.5) A500/A1000/A2000")
        );
        assert_eq!(
            identify_crc(0x1483A091, 524288).map(|e| e.label),
            Some("Kickstart 3.1 (40.68) A1200")
        );
        assert_eq!(
            identify_crc(0xB18D3B67, 524288).map(|e| e.label),
            Some("Kickstart 3.2.3 (47.115) A1200")
        );
        assert_eq!(
            identify_crc(0x1E62D4A5, 524288).map(|e| e.label),
            Some("Kickstart 3.1 (40.60) CD32")
        );
        assert_eq!(
            identify_crc(0x0B1AD2D0, 65536).map(|e| e.label),
            Some("A1000 bootstrap ROM")
        );
        // The same CRC at the wrong length is not that ROM.
        assert_eq!(identify_crc(0x1483A091, 262144), None);
        assert_eq!(identify_crc(0, 524288), None);
    }

    #[test]
    fn byte_swapped_eprom_images_are_restored() {
        // A real ROM header: $11xx then JMP absolute long ($4EF9).
        let mut rom = vec![0x11, 0x14, 0x4E, 0xF9, 0x00, 0xFC, 0x00, 0xD2];
        assert!(matches!(byte_order_normalized(&rom), Cow::Borrowed(_)));
        let mut swapped = rom.clone();
        for pair in swapped.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        assert_eq!(&swapped[..4], &[0x14, 0x11, 0xF9, 0x4E]);
        assert_eq!(byte_order_normalized(&swapped).as_ref(), rom.as_slice());
        // A payload byte that happens to be $11 does not make an image
        // byte-swapped: the check is the whole four-byte header.
        rom[1] = 0x11;
        rom[2] = 0x00;
        assert!(matches!(byte_order_normalized(&rom), Cow::Borrowed(_)));
    }

    #[test]
    fn repeated_images_reduce_to_the_part_they_echo() {
        let part = body(1024);
        let doubled: Vec<u8> = part.iter().chain(part.iter()).copied().collect();
        assert_eq!(halved(&doubled), Some(part.as_slice()));
        assert_eq!(halved(&part), None);
        assert_eq!(halved(&[]), None);
        assert_eq!(halved(&[0x11, 0x22, 0x33]), None);

        // Eight echoes (the A1000 bootstrap's shape) unwind to the part,
        // and the whole image is offered first so an image that is itself
        // in the table wins.
        let mut echoed = part.clone();
        for _ in 0..3 {
            echoed = echoed.iter().chain(echoed.iter()).copied().collect();
        }
        let keys = lookup_keys(&echoed);
        assert_eq!(keys.first().copied(), Some((crc32(&echoed), echoed.len())));
        assert!(
            keys.contains(&(crc32(&part), part.len())),
            "the echoed part is among the keys tried"
        );
        assert_eq!(keys.len(), 4, "8K -> 4K -> 2K -> 1K");
    }

    #[test]
    fn byte_swapped_and_doubled_images_share_the_canonical_key() {
        // A 1 KiB "part" with a plausible ROM header, doubled and then
        // byte-swapped the way an EPROM-programmer dump of a doubled part
        // would be: it must still offer the plain part's key.
        let mut part = body(1024);
        part[..4].copy_from_slice(&[0x11, 0x14, 0x4E, 0xF9]);
        let mut image: Vec<u8> = part.iter().chain(part.iter()).copied().collect();
        for pair in image.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        let keys = lookup_keys(&image);
        assert!(keys.contains(&(crc32(&part), part.len())));
    }

    #[test]
    fn cloanto_encrypted_images_are_reported_as_encrypted() {
        let mut encrypted = CLOANTO_TAG.to_vec();
        encrypted.extend_from_slice(&body(512));
        assert!(is_encrypted(&encrypted));
        assert_eq!(identify(&encrypted), None);
        assert_eq!(describe(&encrypted), Some(Identified::Encrypted));
        assert_eq!(
            describe(&encrypted).map(|id| id.label()),
            Some("Cloanto-encrypted ROM")
        );
        // A plain image is not mistaken for one.
        assert!(!is_encrypted(&body(512)));
        assert_eq!(describe(&body(512)), None);
    }

    #[test]
    fn unknown_images_identify_as_nothing() {
        assert_eq!(identify(&body(524288)), None);
        assert_eq!(identify(&[]), None);
    }

    #[test]
    fn known_labels_are_reported_through_identified() {
        let entry = identify_crc(0x1483A091, 524288).expect("KS 3.1 A1200 is in the table");
        assert_eq!(Identified::Known(entry).label(), entry.label);
    }
}

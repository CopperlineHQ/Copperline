// SPDX-License-Identifier: GPL-3.0-or-later

//! Verifying a captured revolution before the emulator trusts it.
//!
//! A capture in the driver's index-less mode is assembled by pattern-matching
//! where the revolution repeats, and on a small fraction of captures that
//! match is ambiguous -- the cut lands a few dozen bits long and duplicated
//! flux is spliced into the middle of the stream. The result decodes as a
//! perfectly healthy track with exactly one sector's payload failing its
//! checksum, which is not what the head passed over. Serving it once is
//! harmless (the guest retries, as it would a genuine bad read); remembering
//! it is not, because every retry would then be handed the same damage.
//!
//! This scan is how the emulator tells the two apart: a capture that decodes
//! as a complete AmigaDOS track with every checksum passing is a faithful
//! recording -- including across its own wrap point, since the scan reads the
//! capture as the ring the emulator serves -- and can be kept and turned under
//! the head indefinitely, exactly like an image's track. Anything less is
//! served as it stands but fetched afresh when the track is wanted again.
//!
//! The scan is format-aware but not title-aware: AmigaDOS's sector layout and
//! checksums are properties of the format on the platter, published in the
//! hardware manuals, not of any program. A disk that is not AmigaDOS at all
//! (a custom trackloader, a protection track) simply reports
//! [`RevolutionScan::Unrecognised`] and is never judged.

const MASK: u32 = 0x5555_5555;

/// A DD AmigaDOS track carries 11 sectors, an HD track 22.
const DD_SECTORS: usize = 11;
const HD_SECTORS: usize = 22;

/// What a captured revolution decoded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevolutionScan {
    /// A complete AmigaDOS track: the expected sector count for its density,
    /// every header present and every checksum -- header and data -- passing,
    /// read as a ring. A faithful recording of the platter.
    CleanAmigaDos {
        /// Sectors decoded (11 for DD, 22 for HD).
        sectors: usize,
    },
    /// Recognisably AmigaDOS, but incomplete or failing a checksum: a sector
    /// missing, or a payload that does not match its own checksum. One of
    /// these per capture, mid-track, is the signature of a spliced capture.
    DamagedAmigaDos {
        /// Sectors whose header and data both check out.
        good: usize,
        /// The sector count the density implies (11 or 22).
        expected: usize,
    },
    /// Not enough AmigaDOS structure to judge either way. Custom formats and
    /// protection tracks land here and are deliberately not second-guessed.
    Unrecognised,
}

/// Read one bit of the capture, as the ring the emulator serves it as.
#[inline]
fn bit_at(words: &[u16], bit_len: usize, i: usize) -> bool {
    let i = i % bit_len;
    words[i / 16] & (1 << (15 - (i % 16))) != 0
}

/// The 32 bits starting at `start`, crossing the wrap if needed.
fn long_at(words: &[u16], bit_len: usize, start: usize) -> u32 {
    let mut v = 0u32;
    for k in 0..32 {
        v = (v << 1) | u32::from(bit_at(words, bit_len, start + k));
    }
    v
}

/// Recombine an odd/even MFM long pair into the data long it encodes.
#[inline]
fn deinterleave(odd: u32, even: u32) -> u32 {
    ((odd & MASK) << 1) | (even & MASK)
}

/// Decode every AmigaDOS sector in a captured revolution and classify the
/// capture. `words` is the packed MFM, `bit_len` the ring length in bits.
///
/// The layout, from each pair of 0x4489 sync words: info long, label
/// (4 longs), header checksum, data checksum, data (128 longs), each as MFM
/// odd bits then even bits. Both checksums are the XOR of the masked MFM
/// longs of the region they cover.
pub fn scan_revolution(words: &[u16], bit_len: usize) -> RevolutionScan {
    if bit_len < 64 || words.is_empty() || words.len() * 16 < bit_len {
        return RevolutionScan::Unrecognised;
    }

    // Every sync-word position, found with a rolling 16-bit window over the
    // ring (the extra 15 bits close the window across the wrap).
    let mut syncs = Vec::new();
    let mut window: u16 = 0;
    for i in 0..bit_len + 15 {
        window = (window << 1) | u16::from(bit_at(words, bit_len, i));
        if i >= 15 && window == 0x4489 {
            syncs.push((i - 15) % bit_len);
        }
    }

    // A sector body follows a pair of adjacent syncs; step past both. Track
    // which sector numbers arrived intact so a splice that lands in a header
    // (removing the sector entirely) counts as damage, not as a short format.
    let mut headers_valid = 0usize;
    let mut good = [false; HD_SECTORS];
    for &s in &syncs {
        let next_is_sync = syncs.contains(&((s + 16) % bit_len));
        let prev_is_sync = syncs.contains(&((s + bit_len - 16) % bit_len));
        if !next_is_sync || prev_is_sync {
            continue;
        }
        let body = s + 32;
        let info_odd = long_at(words, bit_len, body);
        let info_even = long_at(words, bit_len, body + 32);
        let info = deinterleave(info_odd, info_even);
        let [format, _track, sector, _to_gap] = info.to_be_bytes();
        if format != 0xFF || sector as usize >= HD_SECTORS {
            continue;
        }

        let mut hsum = (info_odd & MASK) ^ (info_even & MASK);
        for l in 0..8 {
            hsum ^= long_at(words, bit_len, body + 64 + l * 32) & MASK;
        }
        let stored_h = deinterleave(
            long_at(words, bit_len, body + 320),
            long_at(words, bit_len, body + 352),
        );
        if hsum != stored_h {
            continue;
        }
        headers_valid += 1;

        let data_start = body + 448;
        let mut dsum = 0u32;
        for l in 0..256 {
            dsum ^= long_at(words, bit_len, data_start + l * 32) & MASK;
        }
        let stored_d = deinterleave(
            long_at(words, bit_len, body + 384),
            long_at(words, bit_len, body + 416),
        );
        if dsum == stored_d {
            good[sector as usize] = true;
        }
    }

    // The density decides the expected count: a DD capture cannot hold 22
    // sectors, and an HD capture with only 11 intact is missing half.
    let expected = if headers_valid > DD_SECTORS {
        HD_SECTORS
    } else {
        DD_SECTORS
    };
    let good = good.iter().filter(|g| **g).count();

    if good == expected {
        RevolutionScan::CleanAmigaDos { sectors: good }
    } else if headers_valid + 1 >= expected {
        // Enough intact headers that this is unmistakably an AmigaDOS track;
        // anything short of complete-and-verified is damage. `+ 1` because a
        // splice can land in a header and take a whole sector with it.
        RevolutionScan::DamagedAmigaDos { good, expected }
    } else {
        RevolutionScan::Unrecognised
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one MFM sector as bits: sync pair, then the odd/even split
    /// regions with correct checksums. Clock bits are left clear except in
    /// the literal sync words, which is all the scan looks at.
    fn encode_sector(track: u8, sector: u8, payload: &[u8; 512]) -> Vec<bool> {
        let mut bits = Vec::new();
        let mut push_word = |bits: &mut Vec<bool>, w: u16| {
            for k in (0..16).rev() {
                bits.push(w & (1 << k) != 0);
            }
        };
        let push_long = |bits: &mut Vec<bool>, l: u32| {
            for k in (0..32).rev() {
                bits.push(l & (1 << k) != 0);
            }
        };
        // Inter-sector gap and the sync pair.
        push_word(&mut bits, 0x2AAA);
        push_word(&mut bits, 0x4489);
        push_word(&mut bits, 0x4489);

        let info = u32::from_be_bytes([0xFF, track, sector, 1]);
        let info_odd = (info >> 1) & MASK;
        let info_even = info & MASK;
        let label = [0u32; 4];
        let label_odd = [0u32; 4];
        let label_even = [0u32; 4];

        let mut hsum = info_odd ^ info_even;
        for l in 0..4 {
            hsum ^= label_odd[l] ^ label_even[l];
        }

        let mut data_longs = [0u32; 128];
        for (i, chunk) in payload.chunks(4).enumerate() {
            data_longs[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let data_odd: Vec<u32> = data_longs.iter().map(|l| (l >> 1) & MASK).collect();
        let data_even: Vec<u32> = data_longs.iter().map(|l| l & MASK).collect();
        let mut dsum = 0u32;
        for l in data_odd.iter().chain(data_even.iter()) {
            dsum ^= l;
        }

        push_long(&mut bits, info_odd);
        push_long(&mut bits, info_even);
        for l in label_odd {
            push_long(&mut bits, l);
        }
        for l in label_even {
            push_long(&mut bits, l);
        }
        push_long(&mut bits, (hsum >> 1) & MASK);
        push_long(&mut bits, hsum & MASK);
        push_long(&mut bits, (dsum >> 1) & MASK);
        push_long(&mut bits, dsum & MASK);
        for l in &data_odd {
            push_long(&mut bits, *l);
        }
        for l in &data_even {
            push_long(&mut bits, *l);
        }
        bits
    }

    /// A full synthetic DD track, with a trailing gap, packed into words.
    fn encode_track(track: u8) -> (Vec<u16>, usize) {
        let mut bits = Vec::new();
        for sector in 0..11u8 {
            let mut payload = [0u8; 512];
            payload[0] = sector;
            payload[511] = track;
            bits.extend(encode_sector(track, sector, &payload));
        }
        // Track gap.
        for _ in 0..700 {
            bits.push(false);
            bits.push(true);
        }
        pack(&bits)
    }

    fn pack(bits: &[bool]) -> (Vec<u16>, usize) {
        let mut words = vec![0u16; bits.len().div_ceil(16)];
        for (i, b) in bits.iter().enumerate() {
            if *b {
                words[i / 16] |= 1 << (15 - (i % 16));
            }
        }
        (words, bits.len())
    }

    fn flip_bit(words: &mut [u16], i: usize) {
        words[i / 16] ^= 1 << (15 - (i % 16));
    }

    #[test]
    fn a_clean_track_scans_clean() {
        let (words, bit_len) = encode_track(40);
        assert_eq!(
            scan_revolution(&words, bit_len),
            RevolutionScan::CleanAmigaDos { sectors: 11 }
        );
    }

    /// The emulator serves a capture as a ring, so the scan must decode one
    /// whose sectors straddle the wrap -- a coherent index-less capture looks
    /// exactly like this.
    #[test]
    fn a_rotated_ring_still_scans_clean() {
        let (words, bit_len) = encode_track(40);
        let rot = bit_len / 3;
        let mut bits = Vec::with_capacity(bit_len);
        for i in 0..bit_len {
            let j = (i + rot) % bit_len;
            bits.push(words[j / 16] & (1 << (15 - (j % 16))) != 0);
        }
        let (rotated, bit_len) = pack(&bits);
        assert_eq!(
            scan_revolution(&rotated, bit_len),
            RevolutionScan::CleanAmigaDos { sectors: 11 }
        );
    }

    #[test]
    fn a_flipped_payload_bit_is_damage() {
        let (mut words, bit_len) = encode_track(40);
        // Inside the first sector's data region: past gap+syncs (48 bits),
        // info (64), label (256), checksums (128), into data -- offset chosen
        // to land on a data (even-mask) bit, not a clock bit.
        flip_bit(&mut words, 48 + 64 + 256 + 128 + 101);
        assert_eq!(
            scan_revolution(&words, bit_len),
            RevolutionScan::DamagedAmigaDos {
                good: 10,
                expected: 11
            }
        );
    }

    #[test]
    fn a_broken_header_is_damage_not_a_short_format() {
        let (mut words, bit_len) = encode_track(40);
        // Inside the first sector's info long, on a data (even-mask) bit: the
        // header checksum fails, the sector vanishes entirely, and ten intact
        // headers remain.
        flip_bit(&mut words, 48 + 11);
        assert_eq!(
            scan_revolution(&words, bit_len),
            RevolutionScan::DamagedAmigaDos {
                good: 10,
                expected: 11
            }
        );
    }

    /// The splice signature seen on hardware: duplicated flux inserted
    /// mid-stream shifts everything after it, corrupting the sector it lands
    /// in while later sectors recover at their own sync marks.
    #[test]
    fn inserted_bits_mid_sector_are_damage() {
        let (words, bit_len) = encode_track(40);
        let mut bits: Vec<bool> = (0..bit_len)
            .map(|i| words[i / 16] & (1 << (15 - (i % 16))) != 0)
            .collect();
        // Duplicate 30 bits inside sector 5's payload.
        let sector_len = bits.len() / 11;
        let at = sector_len * 5 + 1000;
        let dup: Vec<bool> = bits[at..at + 30].to_vec();
        for (k, b) in dup.into_iter().enumerate() {
            bits.insert(at + k, b);
        }
        let (words, bit_len) = pack(&bits);
        let scan = scan_revolution(&words, bit_len);
        assert!(
            matches!(scan, RevolutionScan::DamagedAmigaDos { .. }),
            "expected damage, got {scan:?}"
        );
    }

    #[test]
    fn noise_is_unrecognised() {
        let mut words = vec![0u16; 6000];
        for (i, w) in words.iter_mut().enumerate() {
            *w = (i as u16).wrapping_mul(0x9E37) ^ 0x5A5A;
        }
        assert_eq!(scan_revolution(&words, 96000), RevolutionScan::Unrecognised);
    }

    #[test]
    fn an_empty_capture_is_unrecognised() {
        assert_eq!(scan_revolution(&[], 0), RevolutionScan::Unrecognised);
    }
}

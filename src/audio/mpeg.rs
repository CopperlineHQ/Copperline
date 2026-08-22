// SPDX-License-Identifier: GPL-3.0-or-later

//! MPEG audio (Layer III) frame-header pre-parse, shared by the MHI
//! decoder board (`mhi.rs`), which packetizes a guest-fed byte queue into
//! whole frames, and the cue-sheet MP3 track backend (`cdrom/mp3.rs`),
//! which indexes the frames of a track file. Neither needs a codec to do
//! this: the header alone fixes a frame's byte length, sample rate,
//! channel count, and how many PCM samples it decodes to.

/// The byte length of an MPEG audio frame header.
pub(crate) const HEADER_LEN: usize = 4;

/// A Layer III frame header that passed the sanity checks: enough to cut
/// the frame out of a byte stream and to know what it decodes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    /// Total frame length in bytes, header included.
    pub len: usize,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Single-channel frame (channel mode 11).
    pub mono: bool,
    /// MPEG-1, as opposed to the MPEG-2/2.5 low-sample-rate extensions.
    pub mpeg1: bool,
}

impl FrameHeader {
    /// PCM samples per channel the frame decodes to: 1152 for MPEG-1
    /// Layer III, 576 for the MPEG-2/2.5 half-rate frames.
    pub fn samples(&self) -> u32 {
        if self.mpeg1 {
            1152
        } else {
            576
        }
    }

    /// Length of the side information following the header (ISO 11172-3
    /// section 2.4.1.7 and the ISO 13818-3 extension): where a Xing/Info
    /// tag sits when the frame carries one instead of audio.
    pub fn side_info_len(&self) -> usize {
        match (self.mpeg1, self.mono) {
            (true, false) => 32,
            (true, true) => 17,
            (false, false) => 17,
            (false, true) => 9,
        }
    }
}

/// Pre-parse of a possible Layer III frame header. `None` for anything
/// that is not one -- no sync word, an MPEG version/layer other than
/// Layer III, free format (bitrate index 0), or reserved bitrate/
/// sample-rate indices -- so callers can treat such bytes as junk. The
/// length arithmetic mirrors Symphonia's own header parser (ISO 11172-3
/// section 2.4.3.1: 144 bitrate/sample-rate slots for MPEG-1, 72 for the
/// MPEG-2/2.5 half-rate frames, 1-byte slots for Layer III), so a frame
/// cut to this length is never rejected by the decoder's own
/// packet-length check.
pub(crate) fn parse_frame_header(hdr: [u8; HEADER_LEN]) -> Option<FrameHeader> {
    let h = u32::from_be_bytes(hdr);
    if h & 0xFFE0_0000 != 0xFFE0_0000 {
        return None;
    }
    let version = (h >> 19) & 0x3; // 00 = MPEG-2.5, 10 = MPEG-2, 11 = MPEG-1
    let layer = (h >> 17) & 0x3; // 01 = Layer III
    let bitrate_idx = ((h >> 12) & 0xF) as usize;
    let sr_idx = ((h >> 10) & 0x3) as usize;
    if version == 0b01 || layer != 0b01 || bitrate_idx == 0 || bitrate_idx == 0xF || sr_idx == 3 {
        return None;
    }
    // Bit rates in kbit/s, indexed by the header's bitrate field.
    const KBPS_MPEG1_L3: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const KBPS_MPEG2_L3: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const RATES: [[u32; 3]; 4] = [
        [11_025, 12_000, 8_000],  // 00: MPEG-2.5
        [0, 0, 0],                // 01: reserved (rejected above)
        [22_050, 24_000, 16_000], // 10: MPEG-2
        [44_100, 48_000, 32_000], // 11: MPEG-1
    ];
    let mpeg1 = version == 0b11;
    let kbps = if mpeg1 {
        KBPS_MPEG1_L3[bitrate_idx]
    } else {
        KBPS_MPEG2_L3[bitrate_idx]
    };
    let rate = RATES[version as usize][sr_idx];
    let padding = (h >> 9) & 1;
    let factor: u32 = if mpeg1 { 144 } else { 72 };
    let len = (factor * (kbps * 1000) / rate + padding) as usize;
    let mono = (h >> 6) & 0x3 == 0b11;
    Some(FrameHeader {
        len,
        rate,
        mono,
        mpeg1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpeg1_stereo_128k_44k_frame_is_417_bytes() {
        // Sync, MPEG-1, Layer III, no CRC; 128 kbps, 44.1 kHz, no padding;
        // joint stereo.
        let h = parse_frame_header([0xFF, 0xFB, 0x90, 0x40]).unwrap();
        assert_eq!(h.len, 417);
        assert_eq!(h.rate, 44_100);
        assert!(!h.mono);
        assert!(h.mpeg1);
        assert_eq!(h.samples(), 1152);
        assert_eq!(h.side_info_len(), 32);
        // The padding bit adds one slot.
        assert_eq!(
            parse_frame_header([0xFF, 0xFB, 0x92, 0x40]).unwrap().len,
            418
        );
    }

    #[test]
    fn mpeg2_mono_frame_decodes_to_576_samples() {
        // MPEG-2, Layer III; 64 kbps (index 8), 22.05 kHz; mono.
        let h = parse_frame_header([0xFF, 0xF3, 0x80, 0xC0]).unwrap();
        assert_eq!(h.rate, 22_050);
        assert!(h.mono);
        assert!(!h.mpeg1);
        assert_eq!(h.samples(), 576);
        assert_eq!(h.side_info_len(), 9);
        assert_eq!(h.len, 72 * 64_000 / 22_050);
    }

    #[test]
    fn junk_free_format_and_other_layers_are_rejected() {
        assert!(parse_frame_header([0x00, 0x00, 0x00, 0x00]).is_none());
        // Free format (bitrate index 0).
        assert!(parse_frame_header([0xFF, 0xFB, 0x00, 0x40]).is_none());
        // Bad bitrate index (15).
        assert!(parse_frame_header([0xFF, 0xFB, 0xF0, 0x40]).is_none());
        // Reserved sample-rate index (3).
        assert!(parse_frame_header([0xFF, 0xFB, 0x9C, 0x40]).is_none());
        // Layer II (layer bits 10).
        assert!(parse_frame_header([0xFF, 0xFD, 0x90, 0x40]).is_none());
        // Reserved MPEG version (01).
        assert!(parse_frame_header([0xFF, 0xEB, 0x90, 0x40]).is_none());
    }
}

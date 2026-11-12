// SPDX-License-Identifier: GPL-3.0-or-later

//! CD subcode: the eight one-bit channels P-W the pickup reads alongside
//! every sector.
//!
//! A sector carries 98 subcode symbols: two sync patterns and 96 data
//! symbols, each one byte with a bit per channel (P in bit 7, Q in bit 6,
//! R-W in bits 5-0). A [`SUBCODE_FRAME_BYTES`]-byte frame in that
//! symbol-per-byte layout is what a drive delivers and what every consumer
//! here takes: Akiko's subcode DMA, and the CDTV drive's subcode port.
//!
//! Q is the position channel and is regenerated from the track layout
//! (an ADR 1 packet) for any image, which is all an ordinary disc needs.
//! R-W carry CD+G graphics and CD-TEXT, and exist only on discs that have
//! them, so they are served only from images that stored them:
//!
//! - a CloneCD-style `.sub` file beside a cue sheet or bare image: 96
//!   bytes per sector with the channels stored one after the other
//!   (twelve bytes of P, twelve of Q, ... twelve of W);
//! - a CHD whose track metadata says `SUBTYPE:RW_RAW`: 96 bytes per frame
//!   in the drive's own symbol-per-byte layout.
//!
//! Stored P and Q are passed through when the stored Q packet's CRC
//! checks out; otherwise (rippers commonly leave them blank) the
//! regenerated position takes their place and the stored R-W stay.

use super::{to_bcd, CdTrack, LEADIN_SECTORS};

/// One subcode frame per sector: 98 symbols less the two sync patterns.
pub const SUBCODE_FRAME_BYTES: usize = 96;

/// The P and Q bits of a subcode symbol.
const P_BIT: u8 = 0x80;
const Q_BIT: u8 = 0x40;

/// The Q-channel CRC: CRC-16-CCITT (x^16 + x^12 + x^5 + 1, zero seed)
/// over the ten payload bytes, transmitted inverted (Red Book).
pub(crate) fn q_channel_crc(payload: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in payload {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    !crc
}

fn write_bcd_msf(out: &mut [u8], frames: u32) {
    out[0] = to_bcd((frames / (60 * 75)) as u8);
    out[1] = to_bcd(((frames / 75) % 60) as u8);
    out[2] = to_bcd((frames % 75) as u8);
}

/// The twelve Q-channel bytes of the subcode frame at disc sector
/// `sector`: an ADR 1 position packet (control nibble, track, index,
/// track-relative MSF, zero, absolute MSF, CRC), all BCD. A sector in
/// the gap before a track's INDEX 01 belongs to that track as index 0
/// with the relative time counting down, as the pickup sees it.
pub(crate) fn q_channel_position(tracks: &[CdTrack], sector: u32) -> [u8; 12] {
    let mut q = [0u8; 12];
    let (control, number, index, relative) =
        match tracks.iter().rposition(|t| sector >= t.start_sector) {
            Some(i) => {
                let track = &tracks[i];
                let next = tracks.get(i + 1);
                match next {
                    Some(next) if sector >= track.start_sector + track.sector_count => {
                        (next.kind, next.number, 0, next.start_sector - sector)
                    }
                    _ => (track.kind, track.number, 1, sector - track.start_sector),
                }
            }
            // Lead-in side of the first track: its pregap, counting down.
            None => match tracks.first() {
                Some(first) => (first.kind, first.number, 0, first.start_sector - sector),
                None => return q,
            },
        };
    q[0] = if control.is_data() { 0x41 } else { 0x01 };
    q[1] = to_bcd(number);
    q[2] = to_bcd(index);
    write_bcd_msf(&mut q[3..6], relative);
    write_bcd_msf(&mut q[7..10], sector + LEADIN_SECTORS);
    let crc = q_channel_crc(&q[..10]);
    q[10] = (crc >> 8) as u8;
    q[11] = crc as u8;
    q
}

/// Set channel `bit` of a frame from that channel's twelve bytes.
fn put_channel(frame: &mut [u8; SUBCODE_FRAME_BYTES], bit: u8, channel: &[u8]) {
    for (i, symbol) in frame.iter_mut().enumerate() {
        if channel[i / 8] & (0x80 >> (i % 8)) != 0 {
            *symbol |= bit;
        } else {
            *symbol &= !bit;
        }
    }
}

/// The twelve bytes of the Q channel out of a frame.
pub(crate) fn q_channel_of(frame: &[u8; SUBCODE_FRAME_BYTES]) -> [u8; 12] {
    let mut q = [0u8; 12];
    for (i, symbol) in frame.iter().enumerate() {
        if symbol & Q_BIT != 0 {
            q[i / 8] |= 0x80 >> (i % 8);
        }
    }
    q
}

/// The frame of a sector whose image stored no subcode: the regenerated
/// position in Q, P and R-W blank.
pub(crate) fn regenerated_frame(tracks: &[CdTrack], sector: u32) -> [u8; SUBCODE_FRAME_BYTES] {
    let mut frame = [0u8; SUBCODE_FRAME_BYTES];
    put_channel(&mut frame, Q_BIT, &q_channel_position(tracks, sector));
    frame
}

/// Turn a channel-after-channel frame (the `.sub` file layout: P's twelve
/// bytes, then Q's, ... then W's) into the symbol-per-byte one.
pub(crate) fn from_channel_order(stored: &[u8; SUBCODE_FRAME_BYTES]) -> [u8; SUBCODE_FRAME_BYTES] {
    let mut frame = [0u8; SUBCODE_FRAME_BYTES];
    for channel in 0..8 {
        put_channel(
            &mut frame,
            0x80 >> channel,
            &stored[channel * 12..channel * 12 + 12],
        );
    }
    frame
}

/// Finish a frame an image stored: keep its P and Q when the Q packet's
/// CRC verifies, and otherwise put the regenerated position there. R-W
/// are the image's either way.
pub(crate) fn with_position(
    mut frame: [u8; SUBCODE_FRAME_BYTES],
    tracks: &[CdTrack],
    sector: u32,
) -> [u8; SUBCODE_FRAME_BYTES] {
    let q = q_channel_of(&frame);
    let stored_crc = u16::from_be_bytes([q[10], q[11]]);
    // An all-zero packet has a non-zero CRC, so a blank Q never passes.
    if q_channel_crc(&q[..10]) != stored_crc {
        put_channel(&mut frame, P_BIT, &[0u8; 12]);
        put_channel(&mut frame, Q_BIT, &q_channel_position(tracks, sector));
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdrom::TrackKind;

    fn tracks() -> Vec<CdTrack> {
        vec![
            CdTrack {
                number: 1,
                kind: TrackKind::Mode1_2048,
                start_sector: 0,
                sector_count: 2,
            },
            CdTrack {
                number: 2,
                kind: TrackKind::Audio,
                start_sector: 3,
                sector_count: 10,
            },
        ]
    }

    #[test]
    fn q_channel_crc_is_inverted_crc16_ccitt() {
        // CRC-16/XMODEM's check value for "123456789" is $31C3; the Q
        // channel transmits the complement.
        assert_eq!(q_channel_crc(b"123456789"), !0x31C3);
    }

    #[test]
    fn q_channel_position_places_sectors_by_track_index_and_msf() {
        let tracks = tracks();
        // Data track 1, one sector in: control 4 / ADR 1, index 1,
        // 00:00:01 into the track, absolute 00:02:01 (lead-in offset).
        let q = q_channel_position(&tracks, 1);
        assert_eq!(
            &q[..10],
            &[0x41, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x01]
        );
        assert_eq!([q[10], q[11]], q_channel_crc(&q[..10]).to_be_bytes());
        // Audio track 2, two sectors in: the relative time restarts and
        // the data control bit clears.
        let q = q_channel_position(&tracks, 5);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x02, 0x05]
        );
    }

    #[test]
    fn q_channel_counts_a_pregap_down_as_index_zero_of_the_next_track() {
        // Sector 2 is past track 1's two sectors and before track 2's
        // INDEX 01 at sector 3.
        let q = q_channel_position(&tracks(), 2);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02],
            "pregap sector: track 2 index 0, one frame before INDEX 01"
        );
    }

    #[test]
    fn a_regenerated_frame_carries_only_the_position_in_q() {
        let tracks = tracks();
        let frame = regenerated_frame(&tracks, 5);
        assert!(frame.iter().all(|&symbol| symbol & !Q_BIT == 0));
        assert_eq!(q_channel_of(&frame), q_channel_position(&tracks, 5));
    }

    #[test]
    fn channel_order_frames_become_one_symbol_per_byte() {
        // P all ones, Q a walking pattern, W's first and last bits.
        let mut stored = [0u8; SUBCODE_FRAME_BYTES];
        stored[..12].fill(0xFF);
        stored[12] = 0b1010_0000;
        stored[84] = 0x80;
        stored[95] = 0x01;
        let frame = from_channel_order(&stored);
        assert!(frame.iter().all(|&symbol| symbol & P_BIT != 0));
        assert_eq!(frame[0] & Q_BIT, Q_BIT);
        assert_eq!(frame[1] & Q_BIT, 0);
        assert_eq!(frame[2] & Q_BIT, Q_BIT);
        assert_eq!(frame[0] & 0x01, 0x01, "W bit of the first symbol");
        assert_eq!(frame[95] & 0x01, 0x01, "W bit of the last symbol");
        assert_eq!(frame[1] & 0x3F, 0, "no R-W bits where none were stored");
    }

    #[test]
    fn stored_p_and_q_survive_only_with_a_valid_q_packet() {
        let tracks = tracks();
        // A frame with R-W content and a valid Q packet for some other
        // position: passed through untouched.
        let mut stored = [0x2Au8; SUBCODE_FRAME_BYTES];
        put_channel(&mut stored, Q_BIT, &q_channel_position(&tracks, 9));
        put_channel(&mut stored, P_BIT, &[0xFF; 12]);
        assert_eq!(with_position(stored, &tracks, 5), stored);

        // The same R-W with a blank Q (as rippers leave it): the position
        // is regenerated for the sector asked for and P cleared.
        let blank_q = [0x2Au8 | P_BIT; SUBCODE_FRAME_BYTES];
        let frame = with_position(blank_q, &tracks, 5);
        assert_eq!(q_channel_of(&frame), q_channel_position(&tracks, 5));
        assert!(frame.iter().all(|&symbol| symbol & P_BIT == 0));
        assert!(frame.iter().all(|&symbol| symbol & 0x3F == 0x2A));
    }
}

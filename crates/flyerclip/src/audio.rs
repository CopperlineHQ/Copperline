// SPDX-License-Identifier: GPL-3.0-or-later

//! Clip audio: 16-bit big-endian samples, one 3-block chunk per mono channel
//! per field, 735 samples used per chunk.
//!
//! Layout per frame (AudVer 2, flyer/src/Audio.c:1028-1044, 1095-1101): the
//! audio starts at the block after the CFRM header, channel-major:
//! `L1 L2 L3 L4 R1 R2 R3 R4`, each `AudioLength` (3) blocks. AudVer 1 clips
//! interleave by field instead: `L1 R1 L2 R2 ...`.

use crate::clip::Clip;
use crate::header::BLOCK;

/// Samples per field per channel (MakeAudClip.c:43; `AY0 = 735` in the DSP
/// code, exe/2C/DSP.bin word 630).
pub const SAMPLES_PER_FIELD: usize = 735;
/// `MONOAUDFIELD` (flyer/inc/flyer.h:119): blocks per channel per field.
pub const BLOCKS_PER_FIELD: usize = 3;
/// Slots in one chunk; the 33 past `SAMPLES_PER_FIELD` are unused (zero).
pub const SLOTS_PER_FIELD: usize = BLOCKS_PER_FIELD * BLOCK / 2;
/// 735 samples x 59.94 fields/s = 44100 x 1000/1001 Hz (INFERRED from the
/// 84 587 402 Hz audio clock, flyer/src/main.c:131, = 1920 x this rate).
pub const SAMPLE_RATE: f64 = 44100.0 * 1000.0 / 1001.0;

/// Block offset (from the frame header) of channel `chan`, field `fld` (0-3).
pub fn chunk_offset(aud_ver: u8, audio_length: u8, chans: u8, chan: u8, fld: u8) -> u32 {
    let len = audio_length as u32;
    if aud_ver == 1 {
        1 + (fld as u32 * chans as u32 + chan as u32) * len
    } else {
        1 + (chan as u32 * 4 + fld as u32) * len
    }
}

pub(crate) fn extract(clip: &Clip) -> (u8, Vec<i16>) {
    let chans = clip.header.audio_chans;
    let mut out: Vec<[i16; SAMPLES_PER_FIELD]> = Vec::new();
    if chans == 0 {
        return (0, Vec::new());
    }
    let data = clip.bytes();
    let nfields = clip.field_count();
    for n in 0..nfields {
        let fr = &clip.frames[(n / 4) as usize];
        let h = &fr.header;
        for c in 0..chans {
            let mut samples = [0i16; SAMPLES_PER_FIELD];
            if c < h.audio_chans && h.audio_length > 0 {
                let off = (fr.block
                    + chunk_offset(h.aud_ver, h.audio_length, h.audio_chans, c, (n % 4) as u8))
                    as usize
                    * BLOCK;
                for (i, s) in samples.iter_mut().enumerate() {
                    let o = off + 2 * i;
                    if o + 2 <= data.len() {
                        *s = i16::from_be_bytes([data[o], data[o + 1]]);
                    }
                }
            }
            // stash per channel then interleave below
            out.push(samples);
        }
    }
    let mut inter = Vec::with_capacity(out.len() * SAMPLES_PER_FIELD);
    for fieldset in out.chunks(chans as usize) {
        for i in 0..SAMPLES_PER_FIELD {
            for ch in fieldset {
                inter.push(ch[i]);
            }
        }
    }
    (chans, inter)
}

/// Build one channel's chunk (3 blocks) from 735 samples.
pub fn chunk_bytes(samples: &[i16]) -> [u8; BLOCKS_PER_FIELD * BLOCK] {
    let mut b = [0u8; BLOCKS_PER_FIELD * BLOCK];
    for (i, s) in samples.iter().take(SAMPLES_PER_FIELD).enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&s.to_be_bytes());
    }
    b
}

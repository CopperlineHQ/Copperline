// SPDX-License-Identifier: LGPL-2.1-or-later

//! The PCM sample store: unweaving the ROM, and the wave directory the
//! control ROM keeps into it.
//!
//! The PCM ROMs store each 16-bit sample as two bytes with the bits in bus
//! order rather than value order -- the order the board's address and data
//! lines were routed, not a cipher. Decoding is a fixed permutation, done
//! once at open. The samples themselves are 16-bit words in the module's
//! log-PCM form; the LA32 consumes them as they stand, so no further
//! conversion happens here.

use crate::layout::Layout;

/// One wave the control ROM points into the sample store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wave {
    /// Where it starts, in samples.
    pub addr: u32,
    /// How long it runs, in samples: always a power of two times 0x800.
    pub len: u32,
    /// Whether it loops, or plays once through.
    pub looped: bool,
    /// The tuning word, MSB high.
    pub pitch: u16,
    /// The length byte's low bit: set, the wave ignores master tune.
    pub master_tune_immune: bool,
}

/// Which source bit feeds each output bit, MSB first: entry `u` names the
/// bit of the two-byte pair (0-7 in the first byte, 8-15 in the second)
/// that lands at output bit `15 - u`.
const BIT_ORDER: [u8; 16] = [0, 9, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15, 8];

/// The sample store, unwoven: one i16 per two ROM bytes, in order.
pub fn decode(image: &[u8]) -> Vec<i16> {
    image
        .chunks_exact(2)
        .map(|pair| {
            let mut log: u16 = 0;
            for (u, &source) in BIT_ORDER.iter().enumerate() {
                let byte = pair[usize::from(source / 8)];
                let bit = (byte >> (7 - source % 8)) & 1;
                log |= u16::from(bit) << (15 - u);
            }
            log as i16
        })
        .collect()
}

/// The wave directory `layout` says `control` carries, each entry checked
/// against a sample store of `pcm_samples` samples.
///
/// The reference engine's reader has no working error path -- it reports
/// success and failure with the same value and its caller ignores both --
/// so an out-of-range entry there plays from whatever memory follows.
/// Refusing here is the same judgement the engine's own bounds check
/// intended.
pub fn waves(control: &[u8], layout: &Layout, pcm_samples: usize) -> Result<Vec<Wave>, String> {
    let start = usize::from(layout.pcm_table);
    let count = usize::from(layout.pcm_count);
    let table = control
        .get(start..start + 4 * count)
        .ok_or_else(|| "the wave table runs off the control ROM".to_string())?;
    let mut waves = Vec::with_capacity(count);
    for (i, entry) in table.chunks_exact(4).enumerate() {
        let [pos, len_byte, pitch_lsb, pitch_msb] = [entry[0], entry[1], entry[2], entry[3]];
        let addr = u32::from(pos) * 0x800;
        let len = 0x800u32 << ((len_byte & 0x70) >> 4);
        if addr + len > pcm_samples as u32 {
            return Err(format!(
                "wave {i} points outside the sample store: 0x{addr:X}+0x{len:X}"
            ));
        }
        waves.push(Wave {
            addr,
            len,
            looped: len_byte & 0x80 != 0,
            pitch: u16::from_le_bytes([pitch_lsb, pitch_msb]),
            master_tune_immune: len_byte & 0x01 != 0,
        });
    }
    Ok(waves)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The permutation, pinned bit by bit at its corners: the first byte's
    /// top bit is the sample's top bit, the second byte's top bit is the
    /// sample's bottom bit, and the second byte's next bit lands at 14.
    #[test]
    fn the_unweave_puts_each_bit_where_the_engine_does() {
        assert_eq!(decode(&[0x80, 0x00]), vec![i16::MIN]);
        assert_eq!(decode(&[0x00, 0x80]), vec![1]);
        assert_eq!(decode(&[0x00, 0x40]), vec![0x4000]);
        assert_eq!(decode(&[0x01, 0x00]), vec![0x0080]);
        assert_eq!(decode(&[0xFF, 0xFF]), vec![-1]);
        assert_eq!(decode(&[0x00, 0x00]), vec![0]);
        // Two samples decode independently.
        assert_eq!(decode(&[0x80, 0x00, 0x00, 0x80]), vec![i16::MIN, 1]);
    }

    /// A directory entry decodes as the engine reads it: position in 2 KiB
    /// steps, a power-of-two length from the exponent nibble, the loop
    /// flag in the top bit, and one that reaches too far is refused.
    #[test]
    fn the_directory_decodes_and_minds_its_bounds() {
        let mut control = vec![0u8; 0x4000];
        // Entry 0: pos 2, exponent 1 with loop; entry 1: pos 0, exponent 0.
        control[0x3000..0x3008].copy_from_slice(&[2, 0x91, 0x34, 0x12, 0, 0x00, 0, 0]);
        let layout = Layout {
            pcm_count: 2,
            ..crate::layout::LAYOUTS[0]
        };
        let waves = waves(&control, &layout, 0x2800).expect("both fit");
        assert_eq!(
            waves[0],
            Wave {
                addr: 0x1000,
                len: 0x1000,
                looped: true,
                pitch: 0x1234,
                master_tune_immune: true,
            }
        );
        assert_eq!(
            waves[1],
            Wave {
                addr: 0,
                len: 0x800,
                looped: false,
                pitch: 0,
                master_tune_immune: false,
            }
        );
        assert!(
            super::waves(&control, &layout, 0x1000).is_err(),
            "a store too small for entry 0 refuses"
        );
    }
}

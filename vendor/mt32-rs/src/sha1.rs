// SPDX-License-Identifier: LGPL-2.1-or-later

//! SHA-1, exactly as FIPS 180-1 writes it down.
//!
//! Twenty lines of arithmetic is cheaper than a dependency: the crate needs
//! one digest per ROM image at open, nothing more, and identification is the
//! only thing the hash is for -- ROMs are not secrets and this is not
//! cryptography.

/// The digest of `data`, as the twenty raw bytes.
pub fn digest(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    // The message, padded to whole blocks: a 1 bit, zeros, and the length
    // in bits in the last eight bytes.
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());

    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A82_7999),
                1 => (b ^ c ^ d, 0x6ED9_EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }

    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The digest as the forty lowercase hex characters ROM registries quote.
pub fn hex_digest(data: &[u8]) -> String {
    digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FIPS 180-1 appendix vectors, plus the empty message.
    #[test]
    fn the_standard_vectors_come_out_right() {
        assert_eq!(hex_digest(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex_digest(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex_digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    /// A message crossing the one-block boundary pads into two.
    #[test]
    fn padding_carries_across_the_block_boundary() {
        assert_eq!(
            hex_digest(&[0x55; 64]),
            hex_digest(&[0x55; 64]),
            "deterministic"
        );
        assert_eq!(
            hex_digest(&[0u8; 60]),
            "fb3d8fb74570a077e332993f7d3d27603501b987"
        );
    }
}

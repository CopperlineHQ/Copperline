// SPDX-License-Identifier: GPL-3.0-or-later

//! SHA-1, for identifying a WHDLoad package by what is inside it.
//!
//! Written out rather than pulled in: it is forty lines of a fully
//! specified algorithm (FIPS 180-4), it is wanted for exactly one thing,
//! and a dependency that reaches the network at build time to hash a
//! kilobyte of slave header is a poor trade. Nothing here is a security
//! decision -- the digests are catalogue keys, and SHA-1 is what the
//! catalogue keys on.

/// The digest of `data`, as the forty lowercase hex characters the
/// catalogue writes.
pub fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(40);
    for byte in digest(data) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The twenty raw bytes.
pub fn digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    // The message, then a 1 bit, then zeros to 56 mod 64, then the length
    // in bits as a big-endian u64.
    let mut block = [0u8; 64];
    let mut chunks = data.chunks_exact(64);
    for chunk in &mut chunks {
        block.copy_from_slice(chunk);
        compress(&mut h, &block);
    }
    let tail = chunks.remainder();
    let mut last = [0u8; 128];
    last[..tail.len()].copy_from_slice(tail);
    last[tail.len()] = 0x80;
    let bits = (data.len() as u64).wrapping_mul(8);
    // One final block, or two when the padding will not fit in one.
    let len = if tail.len() + 1 + 8 <= 64 { 64 } else { 128 };
    last[len - 8..len].copy_from_slice(&bits.to_be_bytes());
    for chunk in last[..len].chunks_exact(64) {
        block.copy_from_slice(chunk);
        compress(&mut h, &block);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, word) in w.iter_mut().take(16).enumerate() {
        *word = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (i, &word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, value) in h.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_vectors() {
        // FIPS 180-2 appendix A, and the two lengths where the padding
        // needs a second block.
        assert_eq!(hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            hex(&b"a".repeat(1_000_000)),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
        // The block boundaries, which is where padding goes wrong: 55 is
        // the last length whose padding fits, 56 is the first that needs a
        // second block, 64 is exactly one block, and 119/120 are the same
        // pair one block along.
        for (len, want) in [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (119, "ee971065aaa017e0632a8ca6c77bb3bf8b1dfc56"),
            (120, "f34c1488385346a55709ba056ddd08280dd4c6d6"),
            (127, "89d95fa32ed44a7c610b7ee38517ddf57e0bb975"),
            (128, "ad5b3fdbcb526778c2839d2f151ea753995e26a0"),
        ] {
            assert_eq!(hex(&b"a".repeat(len)), want, "{len} bytes");
        }
    }
}

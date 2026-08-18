// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic random bit generator -- designed, currently dormant.
//!
//! No SDK consumer needs board-side randomness today: P-256/X25519 scalars
//! are generated on the 68k (the AmiSSL provider's OpenSSL RAND) and KEYGEN
//! is strictly scalar*G. This DRBG exists so a future entropy-consuming op
//! has a deterministic source wired to the board's `seed` config key. The
//! default (seed unset) is a fixed constant, so the board is byte-for-byte
//! reproducible either way -- any move to host-entropy seeding would be a
//! deliberate determinism-contract change in docs/internals/zz9k.md, not a
//! default. Its state lives in the Board (linear memory), so save states
//! capture it exactly.

use chacha20::cipher::{KeyIvInit, StreamCipher};

// Dormant by design (see the module comment): nothing draws from the DRBG
// until an op needs entropy.
#[allow(dead_code)]
pub struct Drbg {
    key: [u8; 32],
    counter: u64,
}

impl Drbg {
    /// Seed from the `seed` config value: up to 64 hex digits, zero-padded.
    /// An unset or malformed seed falls back to a fixed constant, keeping
    /// the board deterministic no matter what.
    pub fn from_seed_hex(seed: Option<&str>) -> Self {
        let mut key = [0u8; 32];
        if let Some(seed) = seed {
            let digits: Vec<u8> = seed
                .trim()
                .trim_start_matches("0x")
                .bytes()
                .filter_map(|b| (b as char).to_digit(16).map(|d| d as u8))
                .collect();
            for (i, pair) in digits.chunks(2).take(32).enumerate() {
                key[i] = if pair.len() == 2 {
                    (pair[0] << 4) | pair[1]
                } else {
                    pair[0] << 4
                };
            }
        }
        Drbg { key, counter: 0 }
    }

    #[allow(dead_code)]
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);
        let mut cipher = chacha20::ChaCha20::new((&self.key).into(), (&nonce).into());
        out.fill(0);
        cipher.apply_keystream(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream_and_no_repeats() {
        let mut a = Drbg::from_seed_hex(Some("00112233445566778899aabbccddeeff"));
        let mut b = Drbg::from_seed_hex(Some("0x00112233445566778899AABBCCDDEEFF"));
        let mut x = [0u8; 32];
        let mut y = [0u8; 32];
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        b.fill(&mut y);
        assert_ne!(x, y);
        // Unset seed is still deterministic.
        let mut c = Drbg::from_seed_hex(None);
        let mut d = Drbg::from_seed_hex(None);
        c.fill(&mut x);
        d.fill(&mut y);
        assert_eq!(x, y);
    }
}

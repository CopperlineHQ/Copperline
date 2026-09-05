// SPDX-License-Identifier: GPL-3.0-or-later

//! Streaming SHA-256 for archive and player-payload pins. The public adapter
//! keeps callers' lowercase-hex output while the digest comes from RustCrypto.

use sha2::Digest;
use std::fmt::Write;

#[derive(Default)]
pub struct Sha256(sha2::Sha256);

impl Sha256 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finalize(self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.0.finalize() {
            write!(hex, "{byte:02x}").expect("writing to String");
        }
        hex
    }
}

/// SHA-256 of `data` in one call, as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        // The three every implementation is checked against, plus the
        // block-boundary lengths where the padding is easiest to get wrong.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 55, 56 and 64 bytes: either side of the point where the length
        // no longer fits in the block it padded, and exactly one block.
        assert_eq!(
            sha256_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    /// Streaming must not depend on where the input is split: every cut of
    /// a two-block message -- including cuts inside the buffered tail and
    /// exactly on the block boundary -- digests identically to one call.
    #[test]
    fn streaming_is_split_invariant() {
        let message: Vec<u8> = (0u16..130).map(|i| (i * 7 % 251) as u8).collect();
        let whole = sha256_hex(&message);
        for cut in 0..=message.len() {
            let mut hasher = Sha256::new();
            hasher.update(&message[..cut]);
            hasher.update(&message[cut..]);
            assert_eq!(hasher.finalize(), whole, "split at {cut}");
        }
        // And byte-at-a-time, the worst case for the buffer bookkeeping.
        let mut hasher = Sha256::new();
        for byte in &message {
            hasher.update(std::slice::from_ref(byte));
        }
        assert_eq!(hasher.finalize(), whole);
    }
}

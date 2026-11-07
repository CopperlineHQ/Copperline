// SPDX-License-Identifier: GPL-3.0-or-later

//! `#[serde(with = "crate::bytes_serde")]` for a large `Vec<u8>` (RAM, ROM,
//! a board's memory): written as one byte string instead of element by
//! element.
//!
//! serde's own `Vec<u8>` goes through `collect_seq`, one call per byte.
//! bincode, which the reverse-debug ring, run-ahead and netplay snapshots
//! use, then writes a length and the bytes one at a time, so serialising an
//! A4000's 18 MB took tens of milliseconds every few frames whenever a
//! control client had armed the ring. `serialize_bytes` writes the same
//! length and the same bytes in one go: the encoding is unchanged, so
//! snapshots and save files from before read back as they did. (The save
//! file's MessagePack dialect already wrote a non-empty `Vec<u8>` as `bin`,
//! which is what `serialize_bytes` writes; an empty one stays an empty
//! array.) Reading accepts a byte string or a sequence of bytes.

use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::ser::Serializer;

pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    if bytes.is_empty() {
        // As serde's own impl writes it (an empty array in MessagePack).
        serializer.collect_seq(bytes)
    } else {
        serializer.serialize_bytes(bytes)
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;

    impl<'de> Visitor<'de> for Bytes {
        type Value = Vec<u8>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a byte string")
        }

        fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
            Ok(v.to_vec())
        }

        fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
            Ok(v)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 24));
            while let Some(b) = seq.next_element::<u8>()? {
                out.push(b);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_byte_buf(Bytes)
}

/// The same for an `Arc<Vec<u8>>`.
pub mod arc {
    use serde::{Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(
        bytes: &Arc<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::serialize(bytes, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<Vec<u8>>, D::Error> {
        super::deserialize(deserializer).map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Plain {
        a: Vec<u8>,
        b: Vec<u8>,
    }

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Fast {
        #[serde(with = "super")]
        a: Vec<u8>,
        #[serde(with = "super")]
        b: Vec<u8>,
    }

    #[test]
    fn the_encoding_is_the_same_as_serdes_own_in_both_state_formats() {
        let plain = Plain {
            a: (0..=255u8).cycle().take(1000).collect(),
            b: Vec::new(),
        };
        let fast = Fast {
            a: plain.a.clone(),
            b: Vec::new(),
        };
        let bincode_plain = bincode::serialize(&plain).unwrap();
        assert_eq!(bincode::serialize(&fast).unwrap(), bincode_plain);
        let mut msgpack_plain = Vec::new();
        crate::savestate::chunk::encode(&plain, &mut msgpack_plain).unwrap();
        let mut msgpack_fast = Vec::new();
        crate::savestate::chunk::encode(&fast, &mut msgpack_fast).unwrap();
        assert_eq!(msgpack_fast, msgpack_plain);
        // And each reads back the other's.
        let back: Fast = bincode::deserialize(&bincode_plain).unwrap();
        assert_eq!(back, fast);
        let back: Plain = bincode::deserialize(&bincode::serialize(&fast).unwrap()).unwrap();
        assert_eq!(back, plain);
        let back: Fast = rmp_serde::from_slice(&msgpack_plain).unwrap();
        assert_eq!(back, fast);
    }
}

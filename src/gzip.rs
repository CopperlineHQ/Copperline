// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading gzip streams the way disk images in the wild present them.
//!
//! ADZ and HDZ are gzip wrapped around an ADF or a hardfile, and they arrive
//! from wherever the user found them. Two shapes turn up often enough to
//! matter, and they pull in opposite directions: several members
//! concatenated (`cat a.gz b.gz`, which is one valid gzip stream whose
//! payload is the concatenation), and a single member with bytes stuck on
//! the end (block padding, an appended note). `flate2`'s two readers each
//! get one of them wrong -- `GzDecoder` stops after the first member and
//! silently returns a fraction of the disk, `MultiGzDecoder` decodes them
//! all but then fails the whole image over the trailing bytes.
//!
//! So decode members in a loop and stop at the first byte that does not
//! begin another one. A concatenated image comes back whole, and a padded
//! one keeps working.

use std::io::{Read, Result};

/// The two bytes every gzip member starts with.
pub const SIGNATURE: [u8; 2] = [0x1F, 0x8B];

/// Whether `data` opens with a gzip member.
pub fn is_gzip(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE)
}

/// Decompress every gzip member at the front of `data`, concatenated.
///
/// Trailing bytes that do not begin another member end the stream and are
/// ignored, which is what keeps a padded image readable. `limit`, when
/// given, stops the decompression one byte past the cap so a decompression
/// bomb cannot take the host's memory; the caller compares the returned
/// length against its own cap and words the error, because the caller is
/// what knows the image was a disk.
pub fn inflate_members(data: &[u8], limit: Option<u64>) -> Result<Vec<u8>> {
    let mut rest = data;
    let mut out = Vec::new();
    while is_gzip(rest) {
        let mut decoder = flate2::bufread::GzDecoder::new(rest);
        match limit {
            Some(limit) => {
                // One byte past the cap, so an image exactly at it still
                // reads and only a genuinely larger one trips the caller.
                let headroom = (limit + 1).saturating_sub(out.len() as u64);
                if headroom == 0 {
                    break;
                }
                decoder.by_ref().take(headroom).read_to_end(&mut out)?;
            }
            None => {
                decoder.read_to_end(&mut out)?;
            }
        }
        let remaining = decoder.into_inner();
        // A member that consumed nothing would spin here forever. Nothing
        // known produces one, but the loop is driven by attacker-supplied
        // bytes, so it does not get to depend on that.
        if remaining.len() == rest.len() {
            break;
        }
        rest = remaining;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn member(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn single_member_round_trips() {
        assert_eq!(inflate_members(&member(b"hello"), None).unwrap(), b"hello");
    }

    #[test]
    fn concatenated_members_decode_as_one_payload() {
        let mut packed = member(b"first ");
        packed.extend_from_slice(&member(b"second"));
        assert_eq!(inflate_members(&packed, None).unwrap(), b"first second");
    }

    #[test]
    fn trailing_bytes_after_the_last_member_are_ignored() {
        // Padding and appended notes are not another member, and an image
        // that carries them is still the image. MultiGzDecoder fails these.
        for tail in [&[0u8; 16][..], &b"JUNKJUNK"[..], &[0x1F][..]] {
            let mut packed = member(b"payload");
            packed.extend_from_slice(tail);
            assert_eq!(
                inflate_members(&packed, None).unwrap(),
                b"payload",
                "tail {tail:?}"
            );
        }
    }

    #[test]
    fn a_truncated_member_is_an_error_not_a_short_read() {
        let packed = member(b"payload");
        let err = inflate_members(&packed[..packed.len() - 4], None)
            .expect_err("a cut-off member cannot be decoded");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn the_limit_stops_one_byte_past_the_cap() {
        let packed = member(&vec![0u8; 4096]);
        // Exactly at the cap: whole payload, nothing extra.
        assert_eq!(inflate_members(&packed, Some(4096)).unwrap().len(), 4096);
        // Over it: the caller sees cap+1 and knows to refuse.
        assert_eq!(inflate_members(&packed, Some(1024)).unwrap().len(), 1025);
    }

    #[test]
    fn the_limit_spans_members_rather_than_resetting_per_member() {
        let mut packed = member(&vec![0u8; 1024]);
        packed.extend_from_slice(&member(&vec![0u8; 1024]));
        assert_eq!(inflate_members(&packed, Some(1500)).unwrap().len(), 1501);
    }

    #[test]
    fn not_gzip_at_all_yields_nothing() {
        assert!(inflate_members(b"DOS\x01 not compressed", None)
            .unwrap()
            .is_empty());
    }
}

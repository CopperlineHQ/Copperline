// SPDX-License-Identifier: GPL-3.0-or-later

//! The crypto primitives behind the CRYPTO service, on RustCrypto.
//!
//! Every function is a pure mapping from guest-supplied bytes to output
//! bytes or a wire status code -- no randomness, no host state -- which is
//! what keeps the whole board deterministic and replay-safe. Guest data
//! must never panic these functions: structural failures map to a status
//! (or, for VERIFY, to "signature invalid", which is the cryptographically
//! correct answer for a malformed key or signature).

use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use hmac::Mac;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rsa::traits::PublicKeyParts;
use sha2::Digest;

use crate::wire::*;

pub fn digest_len(alg: u32) -> Option<u32> {
    match alg {
        HASH_SHA1 => Some(20),
        HASH_SHA256 => Some(32),
        HASH_SHA384 => Some(48),
        HASH_SHA512 => Some(64),
        HASH_BLAKE2S => Some(32),
        HASH_POLY1305 => Some(16),
        _ => None,
    }
}

fn hmac_digest<D>(key: &[u8], src: &[u8]) -> Vec<u8>
where
    D: sha2::digest::Digest + sha2::digest::core_api::BlockSizeUser + Clone,
{
    // Hmac::new_from_slice accepts any key length (hashing long keys down),
    // matching RFC 2104.
    let mut mac = <hmac::SimpleHmac<D> as Mac>::new_from_slice(key).expect("any key length");
    mac.update(src);
    mac.finalize().into_bytes().to_vec()
}

/// HASH: plain digest, HMAC (flags bit 0, key triple set), or the Poly1305
/// one-shot MAC filed under hash algorithm 6 with a mandatory 32-byte key.
pub fn hash(alg: u32, hmac: bool, src: &[u8], key: &[u8]) -> Result<Vec<u8>, u16> {
    if alg == HASH_POLY1305 {
        if hmac || key.len() != 32 {
            return Err(STATUS_BAD_REQUEST);
        }
        let key = poly1305::Key::from_slice(key);
        return Ok(poly1305::Poly1305::new(key).compute_unpadded(src).to_vec());
    }
    if hmac {
        if key.is_empty() {
            return Err(STATUS_BAD_REQUEST);
        }
        return match alg {
            HASH_SHA1 => Ok(hmac_digest::<sha1::Sha1>(key, src)),
            HASH_SHA256 => Ok(hmac_digest::<sha2::Sha256>(key, src)),
            HASH_SHA384 => Ok(hmac_digest::<sha2::Sha384>(key, src)),
            HASH_SHA512 => Ok(hmac_digest::<sha2::Sha512>(key, src)),
            // BLAKE2s keying is its native keyed mode, not HMAC; the real
            // firmware does not offer HMAC-BLAKE2s either.
            _ => Err(STATUS_UNSUPPORTED),
        };
    }
    match alg {
        HASH_SHA1 => Ok(sha1::Sha1::digest(src).to_vec()),
        HASH_SHA256 => Ok(sha2::Sha256::digest(src).to_vec()),
        HASH_SHA384 => Ok(sha2::Sha384::digest(src).to_vec()),
        HASH_SHA512 => Ok(sha2::Sha512::digest(src).to_vec()),
        HASH_BLAKE2S => Ok(blake2::Blake2s256::digest(src).to_vec()),
        _ => Err(STATUS_UNSUPPORTED),
    }
}

/// STREAM: ChaCha20 (RFC 8439), 32-byte key, 12-byte nonce, explicit
/// initial block counter.
pub fn chacha20_stream(
    key: &[u8],
    nonce: &[u8],
    counter: u32,
    data: &[u8],
) -> Result<Vec<u8>, u16> {
    if key.len() != 32 || nonce.len() != 12 {
        return Err(STATUS_BAD_REQUEST);
    }
    let mut cipher = chacha20::ChaCha20::new(key.into(), nonce.into());
    // Fallible seek + keystream application: a counter near u32::MAX with a
    // multi-block source runs off the end of ChaCha20's 32-bit-block-counter
    // keystream, and the infallible variants panic there -- which would trap
    // the plugin and fault the whole board off one hostile descriptor.
    cipher
        .try_seek(u64::from(counter) * 64)
        .map_err(|_| STATUS_BAD_REQUEST)?;
    let mut out = data.to_vec();
    cipher
        .try_apply_keystream(&mut out)
        .map_err(|_| STATUS_BAD_REQUEST)?;
    Ok(out)
}

fn aead_key_len(alg: u32) -> Option<usize> {
    match alg {
        AEAD_CHACHA20_POLY1305 | AEAD_AES256_GCM => Some(32),
        AEAD_AES128_GCM => Some(16),
        _ => None,
    }
}

fn aead_apply(
    alg: u32,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    msg: &[u8],
    decrypt: bool,
) -> Result<Vec<u8>, u16> {
    let payload = Payload { msg, aad };
    let run = |res: Result<Vec<u8>, chacha20poly1305::aead::Error>| {
        // Encrypt cannot fail for in-range lengths; decrypt failure is a
        // tag mismatch, reported as an I/O error status (the AmiSSL
        // provider maps any non-OK status to its software fallback and the
        // record is then rejected there too).
        res.map_err(|_| STATUS_IO_ERROR)
    };
    match alg {
        AEAD_CHACHA20_POLY1305 => {
            let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
            let nonce = chacha20poly1305::Nonce::from_slice(nonce);
            run(if decrypt {
                cipher.decrypt(nonce, payload)
            } else {
                cipher.encrypt(nonce, payload)
            })
        }
        AEAD_AES128_GCM => {
            let cipher = aes_gcm::Aes128Gcm::new(key.into());
            let nonce = aes_gcm::Nonce::from_slice(nonce);
            run(if decrypt {
                cipher.decrypt(nonce, payload)
            } else {
                cipher.encrypt(nonce, payload)
            })
        }
        AEAD_AES256_GCM => {
            let cipher = aes_gcm::Aes256Gcm::new(key.into());
            let nonce = aes_gcm::Nonce::from_slice(nonce);
            run(if decrypt {
                cipher.decrypt(nonce, payload)
            } else {
                cipher.encrypt(nonce, payload)
            })
        }
        _ => Err(STATUS_UNSUPPORTED),
    }
}

/// AEAD encrypt: plaintext in, ciphertext||tag out.
pub fn aead_encrypt(
    alg: u32,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    pt: &[u8],
) -> Result<Vec<u8>, u16> {
    if Some(key.len()) != aead_key_len(alg) || nonce.len() != 12 {
        return Err(STATUS_BAD_REQUEST);
    }
    aead_apply(alg, key, nonce, aad, pt, false)
}

/// AEAD decrypt: ciphertext||tag in, plaintext out; tag mismatch is
/// STATUS_IO_ERROR.
pub fn aead_decrypt(
    alg: u32,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    ct_tag: &[u8],
) -> Result<Vec<u8>, u16> {
    if Some(key.len()) != aead_key_len(alg)
        || nonce.len() != 12
        || ct_tag.len() < AEAD_TAG_BYTES as usize
    {
        return Err(STATUS_BAD_REQUEST);
    }
    aead_apply(alg, key, nonce, aad, ct_tag, true)
}

/// KX: X25519 derive, P-256 derive, or P-256 keygen (scalar*G, KEYGEN
/// flag). The only legal nonzero flags word is KEYGEN on P-256, matching
/// the firmware's handle_crypto_kx.
pub fn kx(alg: u32, flags: u32, scalar: &[u8], point: &[u8]) -> Result<Vec<u8>, u16> {
    match (alg, flags) {
        (KX_X25519, 0) => {
            if scalar.len() != 32 || point.len() != 32 {
                return Err(STATUS_BAD_REQUEST);
            }
            let out = x25519_dalek::x25519(
                <[u8; 32]>::try_from(scalar).unwrap(),
                <[u8; 32]>::try_from(point).unwrap(),
            );
            // An all-zero shared secret means a low-order peer point;
            // RFC 7748 requires rejecting it.
            if out == [0u8; 32] {
                return Err(STATUS_BAD_REQUEST);
            }
            Ok(out.to_vec())
        }
        (KX_P256, 0) => {
            if scalar.len() != 32 || point.len() != P256_POINT_BYTES as usize {
                return Err(STATUS_BAD_REQUEST);
            }
            let secret = p256::SecretKey::from_slice(scalar).map_err(|_| STATUS_BAD_REQUEST)?;
            let public = p256::PublicKey::from_sec1_bytes(point).map_err(|_| STATUS_BAD_REQUEST)?;
            let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            Ok(shared.raw_secret_bytes().to_vec())
        }
        (KX_P256, KX_FLAG_KEYGEN) => {
            if scalar.len() != 32 {
                return Err(STATUS_BAD_REQUEST);
            }
            let secret = p256::SecretKey::from_slice(scalar).map_err(|_| STATUS_BAD_REQUEST)?;
            let point = secret.public_key().to_encoded_point(false);
            Ok(point.as_bytes().to_vec())
        }
        (KX_X25519, _) | (KX_P256, _) => Err(STATUS_UNSUPPORTED),
        _ => Err(STATUS_UNSUPPORTED),
    }
}

/// VERIFY: prehashed-SHA-256 signature check. Two failure classes,
/// deliberately distinct (docs/internals/zz9k.md "Services"): lengths that
/// violate the wire contract (wrong digest/signature/key sizes) complete
/// with BAD_REQUEST, which the AmiSSL provider answers by falling back to
/// its software implementation -- the authoritative verdict for shapes
/// this op does not model. Parseable-but-invalid content (an off-curve
/// point, out-of-range r/s, a signature that simply does not verify) is a
/// *successful* verification whose payload carries valid = 0.
pub fn verify(alg: u32, digest: &[u8], sig: &[u8], key: &[u8]) -> Result<bool, u16> {
    match alg {
        VERIFY_ECDSA_P256_SHA256 => {
            if digest.len() != 32 || sig.len() != 64 || key.len() != P256_POINT_BYTES as usize {
                return Err(STATUS_BAD_REQUEST);
            }
            let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(key) else {
                return Ok(false);
            };
            let Ok(sig) = p256::ecdsa::Signature::from_slice(sig) else {
                return Ok(false);
            };
            Ok(vk.verify_prehash(digest, &sig).is_ok())
        }
        VERIFY_RSA_PKCS1_2048_SHA256 => {
            // key = modulus (big-endian, key_length - 4 bytes) followed by a
            // 4-byte big-endian public exponent; the one algorithm id covers
            // 2048/3072/4096-bit moduli, and the signature must be exactly
            // modulus-sized.
            if digest.len() != 32 || key.len() < 5 {
                return Err(STATUS_BAD_REQUEST);
            }
            let (n_bytes, e_bytes) = key.split_at(key.len() - 4);
            if !matches!(n_bytes.len(), 256 | 384 | 512) || sig.len() != n_bytes.len() {
                return Err(STATUS_BAD_REQUEST);
            }
            let n = rsa::BigUint::from_bytes_be(n_bytes);
            let e = rsa::BigUint::from_bytes_be(e_bytes);
            let Ok(public) = rsa::RsaPublicKey::new(n, e) else {
                return Ok(false);
            };
            if public.size() != n_bytes.len() {
                return Ok(false);
            }
            let scheme = rsa::Pkcs1v15Sign::new::<sha2::Sha256>();
            Ok(public.verify(scheme, digest, sig).is_ok())
        }
        _ => Err(STATUS_UNSUPPORTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn hash_known_vectors() {
        // FIPS 180 "abc" vectors, plus BLAKE2s-256("abc") from the
        // reference implementation.
        assert_eq!(
            hash(HASH_SHA1, false, b"abc", &[]).unwrap(),
            hex("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert_eq!(
            hash(HASH_SHA256, false, b"abc", &[]).unwrap(),
            hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            hash(HASH_SHA384, false, b"abc", &[]).unwrap(),
            hex("cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7")
        );
        assert_eq!(
            hash(HASH_SHA512, false, b"abc", &[]).unwrap(),
            hex("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
        );
        assert_eq!(
            hash(HASH_BLAKE2S, false, b"abc", &[]).unwrap(),
            hex("508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982")
        );
    }

    #[test]
    fn hmac_sha256_rfc4231_case_2() {
        assert_eq!(
            hash(HASH_SHA256, true, b"what do ya want for nothing?", b"Jefe").unwrap(),
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
    }

    #[test]
    fn poly1305_rfc8439_vector() {
        let key = hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b");
        let tag = hash(
            HASH_POLY1305,
            false,
            b"Cryptographic Forum Research Group",
            &key,
        )
        .unwrap();
        assert_eq!(tag, hex("a8061dc1305136c6c22b8baf0c0127a9"));
        assert!(hash(HASH_POLY1305, false, b"x", &key[..16]).is_err());
    }

    #[test]
    fn chacha20_rfc8439_block_counter_one() {
        // RFC 8439 section 2.4.2: keystream with counter = 1 encrypting the
        // "Ladies and Gentlemen" sunscreen plaintext.
        let key = hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let nonce = hex("000000000000004a00000000");
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let ct = chacha20_stream(&key, &nonce, 1, pt).unwrap();
        assert_eq!(&ct[..16], &hex("6e2e359a2568f98041ba0728dd0d6981")[..]);
        // Round-trips.
        assert_eq!(chacha20_stream(&key, &nonce, 1, &ct).unwrap(), pt.to_vec());
    }

    #[test]
    fn aead_chacha20_poly1305_round_trip_and_tag_failure() {
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        let ct = aead_encrypt(AEAD_CHACHA20_POLY1305, &key, &nonce, b"aad", b"secret").unwrap();
        assert_eq!(ct.len(), 6 + 16);
        let pt = aead_decrypt(AEAD_CHACHA20_POLY1305, &key, &nonce, b"aad", &ct).unwrap();
        assert_eq!(pt, b"secret");
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert_eq!(
            aead_decrypt(AEAD_CHACHA20_POLY1305, &key, &nonce, b"aad", &bad),
            Err(STATUS_IO_ERROR)
        );
        // Wrong AAD is also a tag failure.
        assert_eq!(
            aead_decrypt(AEAD_CHACHA20_POLY1305, &key, &nonce, b"oad", &ct),
            Err(STATUS_IO_ERROR)
        );
    }

    #[test]
    fn aead_aes_gcm_both_key_sizes() {
        let nonce = [1u8; 12];
        let ct = aead_encrypt(AEAD_AES128_GCM, &[2u8; 16], &nonce, &[], b"data").unwrap();
        assert_eq!(
            aead_decrypt(AEAD_AES128_GCM, &[2u8; 16], &nonce, &[], &ct).unwrap(),
            b"data"
        );
        let ct = aead_encrypt(AEAD_AES256_GCM, &[3u8; 32], &nonce, &[], b"data").unwrap();
        assert_eq!(
            aead_decrypt(AEAD_AES256_GCM, &[3u8; 32], &nonce, &[], &ct).unwrap(),
            b"data"
        );
        // Key-length/algorithm mismatch is a bad request, not a panic.
        assert_eq!(
            aead_encrypt(AEAD_AES128_GCM, &[2u8; 32], &nonce, &[], b"x"),
            Err(STATUS_BAD_REQUEST)
        );
    }

    #[test]
    fn x25519_rfc7748_vector() {
        let scalar = hex("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let point = hex("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        let out = kx(KX_X25519, 0, &scalar, &point).unwrap();
        assert_eq!(
            out,
            hex("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
        );
        // The all-zero output of a low-order point is rejected.
        assert_eq!(
            kx(KX_X25519, 0, &scalar, &[0u8; 32]),
            Err(STATUS_BAD_REQUEST)
        );
        // X25519 has no KEYGEN flag; nonzero flags are unsupported.
        assert_eq!(kx(KX_X25519, 1, &scalar, &point), Err(STATUS_UNSUPPORTED));
    }

    #[test]
    fn p256_keygen_and_derive_agree() {
        // Two private scalars; ECDH from each side must agree, and keygen
        // must produce each side's public point.
        let a = hex("c88f01f510d9ac3f70a292daa2316de544e9aab8afe84049c62a9c57862d1433");
        let b = hex("c6ef9c5d78ae012a011164acb397ce2088685d8f06bf9be0b283ab46476bee53");
        let pub_a = kx(KX_P256, KX_FLAG_KEYGEN, &a, &[]).unwrap();
        let pub_b = kx(KX_P256, KX_FLAG_KEYGEN, &b, &[]).unwrap();
        assert_eq!(pub_a.len(), 65);
        assert_eq!(pub_a[0], 0x04);
        // NIST CAVP ECDH P-256 known answer for these scalars.
        let shared_ab = kx(KX_P256, 0, &a, &pub_b).unwrap();
        let shared_ba = kx(KX_P256, 0, &b, &pub_a).unwrap();
        assert_eq!(shared_ab, shared_ba);
        assert_eq!(
            shared_ab,
            hex("d6840f6b42f6edafd13116e0e12565202fef8e9ece7dce03812464d04b9442de")
        );
        // A zero scalar and an off-curve point are bad requests.
        assert_eq!(kx(KX_P256, 0, &[0u8; 32], &pub_b), Err(STATUS_BAD_REQUEST));
        let mut off_curve = pub_b.clone();
        off_curve[64] ^= 1;
        assert_eq!(kx(KX_P256, 0, &a, &off_curve), Err(STATUS_BAD_REQUEST));
    }

    #[test]
    fn ecdsa_p256_verify_valid_and_invalid() {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let sk = p256::ecdsa::SigningKey::from_slice(&hex(
            "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721",
        ))
        .unwrap();
        let digest = sha2::Sha256::digest(b"sample");
        let sig: p256::ecdsa::Signature = sk.sign_prehash(&digest).unwrap();
        let key = sk
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        assert_eq!(
            verify(VERIFY_ECDSA_P256_SHA256, &digest, &sig.to_bytes(), &key),
            Ok(true)
        );
        let mut bad = sig.to_bytes().to_vec();
        bad[10] ^= 1;
        assert_eq!(
            verify(VERIFY_ECDSA_P256_SHA256, &digest, &bad, &key),
            Ok(false)
        );
        // Structurally hopeless inputs are still "invalid", never a panic.
        assert_eq!(
            verify(VERIFY_ECDSA_P256_SHA256, &digest, &[0u8; 64], &key),
            Ok(false)
        );
        assert_eq!(
            verify(
                VERIFY_ECDSA_P256_SHA256,
                &digest,
                &sig.to_bytes(),
                &[1u8; 65]
            ),
            Ok(false)
        );
    }

    #[test]
    fn rsa_pkcs1_verify_sdk_kat() {
        // The SDK's own RSA-2048 verify KAT (a fixed OpenSSL-generated
        // public key and PKCS#1-v1.5-SHA256 signature), exercising the wire
        // key format: modulus bytes then a 4-byte BE exponent.
        use crate::rsa_kat::*;
        let digest = sha2::Sha256::digest(KAT_MSG);
        let mut key = KAT_RSA_N.to_vec();
        key.extend_from_slice(&KAT_RSA_E.to_be_bytes());
        assert_eq!(
            verify(
                VERIFY_RSA_PKCS1_2048_SHA256,
                &digest,
                &KAT_RSA_SIG_PKCS1,
                &key
            ),
            Ok(true)
        );
        let mut bad = KAT_RSA_SIG_PKCS1.to_vec();
        bad[5] ^= 1;
        assert_eq!(
            verify(VERIFY_RSA_PKCS1_2048_SHA256, &digest, &bad, &key),
            Ok(false)
        );
        // A wrong digest is invalid, not an error.
        let other = sha2::Sha256::digest(b"other");
        assert_eq!(
            verify(
                VERIFY_RSA_PKCS1_2048_SHA256,
                &other,
                &KAT_RSA_SIG_PKCS1,
                &key
            ),
            Ok(false)
        );
        // Signature length must match the modulus.
        assert_eq!(
            verify(
                VERIFY_RSA_PKCS1_2048_SHA256,
                &digest,
                &KAT_RSA_SIG_PKCS1[..255],
                &key
            ),
            Err(STATUS_BAD_REQUEST)
        );
    }
}

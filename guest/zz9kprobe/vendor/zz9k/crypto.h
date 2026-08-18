/*
 * Header-only crypto descriptor helpers for SDK callers.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_CRYPTO_H
#define ZZ9K_CRYPTO_H

#include "zz9k/abi.h"
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ZZ9K_CRYPTO_POLY1305_KEY_BYTES 32U
#define ZZ9K_CRYPTO_CHACHA20_KEY_BYTES 32U
#define ZZ9K_CRYPTO_CHACHA20_NONCE_BYTES 12U
#define ZZ9K_CRYPTO_CHACHA20_POLY1305_TAG_BYTES 16U

static inline uint32_t zz9k_crypto_digest_length(uint32_t algorithm)
{
  switch (algorithm) {
  case ZZ9K_CRYPTO_HASH_SHA1:
    return 20U;
  case ZZ9K_CRYPTO_HASH_SHA256:
  case ZZ9K_CRYPTO_HASH_BLAKE2S:
    return 32U;
  case ZZ9K_CRYPTO_HASH_SHA384:
    return 48U;
  case ZZ9K_CRYPTO_HASH_SHA512:
    return 64U;
  case ZZ9K_CRYPTO_HASH_POLY1305:
    return 16U;
  default:
    return 0U;
  }
}

static inline int zz9k_crypto_is_hash_algorithm(uint32_t algorithm)
{
  return zz9k_crypto_digest_length(algorithm) != 0U &&
         algorithm != ZZ9K_CRYPTO_HASH_POLY1305;
}

static inline int zz9k_crypto_build_hash_desc(ZZ9KCryptoHashDesc *desc,
                                              uint32_t algorithm,
                                              uint32_t src_handle,
                                              uint32_t src_offset,
                                              uint32_t src_length,
                                              uint32_t dst_handle,
                                              uint32_t dst_offset)
{
  if (!desc || !zz9k_crypto_is_hash_algorithm(algorithm) ||
      src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->algorithm = algorithm;
  return 1;
}

static inline int zz9k_crypto_build_hmac_desc(ZZ9KCryptoHashDesc *desc,
                                              uint32_t algorithm,
                                              uint32_t src_handle,
                                              uint32_t src_offset,
                                              uint32_t src_length,
                                              uint32_t dst_handle,
                                              uint32_t dst_offset,
                                              uint32_t key_handle,
                                              uint32_t key_offset,
                                              uint32_t key_length)
{
  if (!zz9k_crypto_build_hash_desc(desc, algorithm, src_handle, src_offset,
                                   src_length, dst_handle, dst_offset)) {
    return 0;
  }
  if (key_handle == ZZ9K_INVALID_HANDLE || key_length == 0U) {
    memset(desc, 0, sizeof(*desc));
    return 0;
  }

  desc->key_handle = key_handle;
  desc->key_offset = key_offset;
  desc->key_length = key_length;
  desc->flags = ZZ9K_CRYPTO_HASH_FLAG_HMAC;
  return 1;
}

static inline int zz9k_crypto_build_poly1305_desc(ZZ9KCryptoHashDesc *desc,
                                                  uint32_t src_handle,
                                                  uint32_t src_offset,
                                                  uint32_t src_length,
                                                  uint32_t dst_handle,
                                                  uint32_t dst_offset,
                                                  uint32_t key_handle,
                                                  uint32_t key_offset)
{
  if (!desc || src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE ||
      key_handle == ZZ9K_INVALID_HANDLE) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->key_handle = key_handle;
  desc->key_offset = key_offset;
  desc->key_length = ZZ9K_CRYPTO_POLY1305_KEY_BYTES;
  desc->algorithm = ZZ9K_CRYPTO_HASH_POLY1305;
  return 1;
}

static inline int zz9k_crypto_build_stream_desc(ZZ9KCryptoStreamDesc *desc,
                                                uint32_t algorithm,
                                                uint32_t src_handle,
                                                uint32_t src_offset,
                                                uint32_t src_length,
                                                uint32_t dst_handle,
                                                uint32_t dst_offset,
                                                uint32_t key_handle,
                                                uint32_t key_offset,
                                                uint32_t nonce_handle,
                                                uint32_t nonce_offset,
                                                uint32_t counter,
                                                uint32_t flags)
{
  if (!desc || algorithm != ZZ9K_CRYPTO_STREAM_CHACHA20 ||
      src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE ||
      key_handle == ZZ9K_INVALID_HANDLE ||
      nonce_handle == ZZ9K_INVALID_HANDLE) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->key_handle = key_handle;
  desc->key_offset = key_offset;
  desc->nonce_handle = nonce_handle;
  desc->nonce_offset = nonce_offset;
  desc->counter = counter;
  desc->algorithm = algorithm;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_crypto_build_chacha20_desc(
    ZZ9KCryptoStreamDesc *desc,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t key_handle,
    uint32_t key_offset,
    uint32_t nonce_handle,
    uint32_t nonce_offset,
    uint32_t counter)
{
  return zz9k_crypto_build_stream_desc(
      desc, ZZ9K_CRYPTO_STREAM_CHACHA20, src_handle, src_offset,
      src_length, dst_handle, dst_offset, key_handle, key_offset,
      nonce_handle, nonce_offset, counter, 0U);
}

static inline int zz9k_crypto_build_chacha20_poly1305_desc(
    ZZ9KCryptoAeadDesc *desc,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t aad_handle,
    uint32_t aad_offset,
    uint32_t aad_length,
    uint32_t key_handle,
    uint32_t key_offset,
    uint32_t nonce_handle,
    uint32_t flags)
{
  if (!desc || src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE ||
      key_handle == ZZ9K_INVALID_HANDLE ||
      nonce_handle == ZZ9K_INVALID_HANDLE ||
      (flags & ~ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT) != 0U ||
      (aad_length != 0U && aad_handle == ZZ9K_INVALID_HANDLE)) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  if (aad_length != 0U) {
    desc->aad_handle = aad_handle;
    desc->aad_offset = aad_offset;
    desc->aad_length = aad_length;
  }
  desc->key_handle = key_handle;
  desc->key_offset = key_offset;
  desc->nonce_handle = nonce_handle;
  desc->flags = flags;
  return 1;
}

/* AES-GCM via the AEAD op. key_length selects AES-128 (16 bytes) or AES-256
 * (32 bytes); the chosen algorithm is encoded into the descriptor flags. The
 * nonce is 12 bytes and the 16-byte tag is appended to the ciphertext, exactly
 * as for ChaCha20-Poly1305. `flags` may carry ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT. */
static inline int zz9k_crypto_build_aes_gcm_desc(
    ZZ9KCryptoAeadDesc *desc,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t aad_handle,
    uint32_t aad_offset,
    uint32_t aad_length,
    uint32_t key_handle,
    uint32_t key_offset,
    uint32_t key_length,
    uint32_t nonce_handle,
    uint32_t flags)
{
  uint32_t algorithm;

  if (!desc || src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE ||
      key_handle == ZZ9K_INVALID_HANDLE ||
      nonce_handle == ZZ9K_INVALID_HANDLE ||
      (flags & ~(uint32_t)ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT) != 0U ||
      (aad_length != 0U && aad_handle == ZZ9K_INVALID_HANDLE)) {
    return 0;
  }
  if (key_length == ZZ9K_CRYPTO_AES128_KEY_BYTES) {
    algorithm = ZZ9K_CRYPTO_AEAD_AES128_GCM;
  } else if (key_length == ZZ9K_CRYPTO_AES256_KEY_BYTES) {
    algorithm = ZZ9K_CRYPTO_AEAD_AES256_GCM;
  } else {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  if (aad_length != 0U) {
    desc->aad_handle = aad_handle;
    desc->aad_offset = aad_offset;
    desc->aad_length = aad_length;
  }
  desc->key_handle = key_handle;
  desc->key_offset = key_offset;
  desc->nonce_handle = nonce_handle;
  desc->flags = (flags & (uint32_t)ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT) |
                ZZ9K_CRYPTO_AEAD_FLAG_ALG(algorithm);
  return 1;
}

typedef struct ZZ9KCryptoKxDesc {
  uint32_t scalar_handle;
  uint32_t scalar_offset;
  uint32_t point_handle;
  uint32_t point_offset;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KCryptoKxDesc;

typedef struct ZZ9KCryptoVerifyDesc {
  uint32_t hash_handle;
  uint32_t hash_offset;
  uint32_t hash_length;
  uint32_t sig_handle;
  uint32_t sig_offset;
  uint32_t sig_length;
  uint32_t key_handle;
  uint32_t key_offset;
  uint32_t key_length;
  uint32_t algorithm;
} ZZ9KCryptoVerifyDesc;

/* Fill a key-exchange descriptor for the given scalar-mult algorithm.
 * Used by both the X25519 and P-256 builders below. */
static inline int zz9k_crypto_build_kx_desc(
    ZZ9KCryptoKxDesc *desc, uint32_t algorithm,
    uint32_t scalar_handle, uint32_t scalar_offset,
    uint32_t point_handle, uint32_t point_offset,
    uint32_t dst_handle, uint32_t dst_offset)
{
  if (!desc ||
      scalar_handle == ZZ9K_INVALID_HANDLE ||
      point_handle  == ZZ9K_INVALID_HANDLE ||
      dst_handle    == ZZ9K_INVALID_HANDLE)
    return 0;
  memset(desc, 0, sizeof(*desc));
  desc->scalar_handle = scalar_handle;
  desc->scalar_offset = scalar_offset;
  desc->point_handle  = point_handle;
  desc->point_offset  = point_offset;
  desc->dst_handle    = dst_handle;
  desc->dst_offset    = dst_offset;
  desc->algorithm     = algorithm;
  desc->flags         = 0U;
  return 1;
}

static inline int zz9k_crypto_build_x25519_desc(
    ZZ9KCryptoKxDesc *desc,
    uint32_t scalar_handle, uint32_t scalar_offset,
    uint32_t point_handle, uint32_t point_offset,
    uint32_t dst_handle, uint32_t dst_offset)
{
  return zz9k_crypto_build_kx_desc(desc, ZZ9K_CRYPTO_KX_X25519,
                                   scalar_handle, scalar_offset,
                                   point_handle, point_offset,
                                   dst_handle, dst_offset);
}

/* P-256 ECDH. The point buffer holds the uncompressed peer public key
 * (0x04 || X || Y, ZZ9K_CRYPTO_P256_POINT_BYTES) and dst receives the
 * 32-byte shared X coordinate. */
static inline int zz9k_crypto_build_p256_desc(
    ZZ9KCryptoKxDesc *desc,
    uint32_t scalar_handle, uint32_t scalar_offset,
    uint32_t point_handle, uint32_t point_offset,
    uint32_t dst_handle, uint32_t dst_offset)
{
  return zz9k_crypto_build_kx_desc(desc, ZZ9K_CRYPTO_KX_P256,
                                   scalar_handle, scalar_offset,
                                   point_handle, point_offset,
                                   dst_handle, dst_offset);
}

/* P-256 keygen: scalar*G. `scalar` is the 32-byte private key and `dst`
 * receives the full uncompressed public point (ZZ9K_CRYPTO_P256_POINT_BYTES).
 * There is no peer point, so this builder does not require one; it sets the
 * KEYGEN flag, which only firmware advertising ZZ9K_SERVICE_FLAG_CRYPTO_P256_-
 * KEYGEN honours (older firmware rejects a non-zero flags word). */
static inline int zz9k_crypto_build_p256_keygen_desc(
    ZZ9KCryptoKxDesc *desc,
    uint32_t scalar_handle, uint32_t scalar_offset,
    uint32_t dst_handle, uint32_t dst_offset)
{
  if (!desc ||
      scalar_handle == ZZ9K_INVALID_HANDLE ||
      dst_handle    == ZZ9K_INVALID_HANDLE)
    return 0;
  memset(desc, 0, sizeof(*desc));
  desc->scalar_handle = scalar_handle;
  desc->scalar_offset = scalar_offset;
  desc->point_handle  = ZZ9K_INVALID_HANDLE;   /* no peer point for keygen */
  desc->dst_handle    = dst_handle;
  desc->dst_offset    = dst_offset;
  desc->algorithm     = ZZ9K_CRYPTO_KX_P256;
  desc->flags         = ZZ9K_CRYPTO_KX_FLAG_KEYGEN;
  return 1;
}

/* Signature verification (ECDSA-P256 or RSA-PKCS1-2048, both over SHA-256).
 * hash_* references the message digest, sig_* the signature, key_* the public
 * key (uncompressed P-256 point, or RSA modulus). */
static inline int zz9k_crypto_build_verify_desc(
    ZZ9KCryptoVerifyDesc *desc, uint32_t algorithm,
    uint32_t hash_handle, uint32_t hash_offset, uint32_t hash_length,
    uint32_t sig_handle, uint32_t sig_offset, uint32_t sig_length,
    uint32_t key_handle, uint32_t key_offset, uint32_t key_length)
{
  if (!desc ||
      hash_handle == ZZ9K_INVALID_HANDLE ||
      sig_handle  == ZZ9K_INVALID_HANDLE ||
      key_handle  == ZZ9K_INVALID_HANDLE ||
      (algorithm != ZZ9K_CRYPTO_VERIFY_ECDSA_P256_SHA256 &&
       algorithm != ZZ9K_CRYPTO_VERIFY_RSA_PKCS1_2048_SHA256))
    return 0;
  memset(desc, 0, sizeof(*desc));
  desc->hash_handle = hash_handle;
  desc->hash_offset = hash_offset;
  desc->hash_length = hash_length;
  desc->sig_handle  = sig_handle;
  desc->sig_offset  = sig_offset;
  desc->sig_length  = sig_length;
  desc->key_handle  = key_handle;
  desc->key_offset  = key_offset;
  desc->key_length  = key_length;
  desc->algorithm   = algorithm;
  return 1;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_CRYPTO_H */

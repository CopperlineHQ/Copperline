/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * zz9kprobe: guest-side conformance probe for Copperline's bundled ZZ9000
 * SDK crypto board ([zz9k], docs/internals/zz9k.md). Deliberately built on
 * the REAL SDK transport (vendor/zz9k_host.c, the exact code zz9k.library
 * and the SDK tools link), so a pass means the board satisfies the same
 * Amiga-side code paths real ZZ9000 software exercises: discovery, the
 * bootstrap registers, mailbox attach, submit/poll/call, shared buffers,
 * every crypto opcode against published test vectors (RFC 8439/7748/6979,
 * FIPS 180, RFC 5903, and the SDK's own RSA-2048 KAT), and the completion
 * interrupt's status/ack protocol.
 *
 * Output: "ZZ9K: PASS <name>" / "ZZ9K: FAIL <name> ..." lines and a final
 * "ZZ9K: SUMMARY PASS|FAIL", written to stdout -- the integration test
 * (tests/zz9k.rs) redirects it to a file on the host-mounted boot volume
 * and asserts on the lines, the same harness shape as tests/mhi.rs.
 */

#include "zz9k/caps.h"
#include "zz9k/host.h"
#include "zz9k/crypto.h"
#include "zz9k/shared.h"
#include "zz9k/request.h"
#include <stdio.h>
#include <string.h>

#include <dos/dosextens.h>
#include <exec/execbase.h>
#include <proto/exec.h>

static int failures;
static int checks;

static void pass(const char *name)
{
  checks++;
  printf("ZZ9K: PASS %s\n", name);
  fflush(stdout);
}

static void fail(const char *name, int code)
{
  checks++;
  failures++;
  printf("ZZ9K: FAIL %s (%d)\n", name, code);
  fflush(stdout);
}

static void check(const char *name, int ok, int code)
{
  if (ok) {
    pass(name);
  } else {
    fail(name, code);
  }
}

/* -- Test vectors --------------------------------------------------------- */

static const uint8_t sha256_abc[32] = {
  0xba,0x78,0x16,0xbf,0x8f,0x01,0xcf,0xea,0x41,0x41,0x40,0xde,0x5d,0xae,
  0x22,0x23,0xb0,0x03,0x61,0xa3,0x96,0x17,0x7a,0x9c,0xb4,0x10,0xff,0x61,
  0xf2,0x00,0x15,0xad,
};

/* RFC 4231 test case 2: HMAC-SHA256("Jefe", "what do ya want for nothing?") */
static const uint8_t hmac_jefe[32] = {
  0x5b,0xdc,0xc1,0x46,0xbf,0x60,0x75,0x4e,0x6a,0x04,0x24,0x26,0x08,0x95,
  0x75,0xc7,0x5a,0x00,0x3f,0x08,0x9d,0x27,0x39,0x83,0x9d,0xec,0x58,0xb9,
  0x64,0xec,0x38,0x43,
};

/* RFC 8439 section 2.5.2 Poly1305 vector. */
static const uint8_t poly_key[32] = {
  0x85,0xd6,0xbe,0x78,0x57,0x55,0x6d,0x33,0x7f,0x44,0x52,0xfe,0x42,0xd5,
  0x06,0xa8,0x01,0x03,0x80,0x8a,0xfb,0x0d,0xb2,0xfd,0x4a,0xbf,0xf6,0xaf,
  0x41,0x49,0xf5,0x1b,
};
static const uint8_t poly_tag[16] = {
  0xa8,0x06,0x1d,0xc1,0x30,0x51,0x36,0xc6,0xc2,0x2b,0x8b,0xaf,0x0c,0x01,
  0x27,0xa9,
};

/* RFC 8439 section 2.4.2 ChaCha20 vector (counter = 1). */
static const uint8_t chacha_key[32] = {
  0x00,0x01,0x02,0x03,0x04,0x05,0x06,0x07,0x08,0x09,0x0a,0x0b,0x0c,0x0d,
  0x0e,0x0f,0x10,0x11,0x12,0x13,0x14,0x15,0x16,0x17,0x18,0x19,0x1a,0x1b,
  0x1c,0x1d,0x1e,0x1f,
};
static const uint8_t chacha_nonce[12] = {
  0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x4a,0x00,0x00,0x00,0x00,
};
static const uint8_t chacha_ct_head[16] = {
  0x6e,0x2e,0x35,0x9a,0x25,0x68,0xf9,0x80,0x41,0xba,0x07,0x28,0xdd,0x0d,
  0x69,0x81,
};

/* RFC 7748 section 5.2 X25519 vector 1. */
static const uint8_t x25519_scalar[32] = {
  0xa5,0x46,0xe3,0x6b,0xf0,0x52,0x7c,0x9d,0x3b,0x16,0x15,0x4b,0x82,0x46,
  0x5e,0xdd,0x62,0x14,0x4c,0x0a,0xc1,0xfc,0x5a,0x18,0x50,0x6a,0x22,0x44,
  0xba,0x44,0x9a,0xc4,
};
static const uint8_t x25519_point[32] = {
  0xe6,0xdb,0x68,0x67,0x58,0x30,0x30,0xdb,0x35,0x94,0xc1,0xa4,0x24,0xb1,
  0x5f,0x7c,0x72,0x66,0x24,0xec,0x26,0xb3,0x35,0x3b,0x10,0xa9,0x03,0xa6,
  0xd0,0xab,0x1c,0x4c,
};
static const uint8_t x25519_out[32] = {
  0xc3,0xda,0x55,0x37,0x9d,0xe9,0xc6,0x90,0x8e,0x94,0xea,0x4d,0xf2,0x8d,
  0x08,0x4f,0x32,0xec,0xcf,0x03,0x49,0x1c,0x71,0xf7,0x54,0xb4,0x07,0x55,
  0x77,0xa2,0x85,0x52,
};

/* RFC 5903 section 8.1: P-256 private scalars and the shared secret. */
static const uint8_t p256_da[32] = {
  0xc8,0x8f,0x01,0xf5,0x10,0xd9,0xac,0x3f,0x70,0xa2,0x92,0xda,0xa2,0x31,
  0x6d,0xe5,0x44,0xe9,0xaa,0xb8,0xaf,0xe8,0x40,0x49,0xc6,0x2a,0x9c,0x57,
  0x86,0x2d,0x14,0x33,
};
static const uint8_t p256_db[32] = {
  0xc6,0xef,0x9c,0x5d,0x78,0xae,0x01,0x2a,0x01,0x11,0x64,0xac,0xb3,0x97,
  0xce,0x20,0x88,0x68,0x5d,0x8f,0x06,0xbf,0x9b,0xe0,0xb2,0x83,0xab,0x46,
  0x47,0x6b,0xee,0x53,
};
static const uint8_t p256_shared[32] = {
  0xd6,0x84,0x0f,0x6b,0x42,0xf6,0xed,0xaf,0xd1,0x31,0x16,0xe0,0xe1,0x25,
  0x65,0x20,0x2f,0xef,0x8e,0x9e,0xce,0x7d,0xce,0x03,0x81,0x24,0x64,0xd0,
  0x4b,0x94,0x42,0xde,
};

/* RFC 6979 A.2.5: P-256 public key and the SHA-256("sample") signature. */
static const uint8_t ecdsa_pub[65] = {
  0x04,
  0x60,0xfe,0xd4,0xba,0x25,0x5a,0x9d,0x31,0xc9,0x61,0xeb,0x74,0xc6,0x35,
  0x6d,0x68,0xc0,0x49,0xb8,0x92,0x3b,0x61,0xfa,0x6c,0xe6,0x69,0x62,0x2e,
  0x60,0xf2,0x9f,0xb6,
  0x79,0x03,0xfe,0x10,0x08,0xb8,0xbc,0x99,0xa4,0x1a,0xe9,0xe9,0x56,0x28,
  0xbc,0x64,0xf2,0xf1,0xb2,0x0c,0x2d,0x7e,0x9f,0x51,0x77,0xa3,0xc2,0x94,
  0xd4,0x46,0x22,0x99,
};
static const uint8_t ecdsa_sig[64] = {
  0xef,0xd4,0x8b,0x2a,0xac,0xb6,0xa8,0xfd,0x11,0x40,0xdd,0x9c,0xd4,0x5e,
  0x81,0xd6,0x9d,0x2c,0x87,0x7b,0x56,0xaa,0xf9,0x91,0xc3,0x4d,0x0e,0xa8,
  0x4e,0xaf,0x37,0x16,
  0xf7,0xcb,0x1c,0x94,0x2d,0x65,0x7c,0x41,0xd4,0x36,0xc7,0xa1,0xb6,0xe2,
  0x9f,0x65,0xf3,0xe9,0x00,0xdb,0xb9,0xaf,0xf4,0x06,0x4d,0xc4,0xab,0x2f,
  0x84,0x3a,0xcd,0xa8,
};

/* The SDK's own RSA-2048 verify KAT (tools/rsa_kat_vector.h) and its fixed
 * message (tools/zz9k-cryptoprofile.c's g_msg). */
#include "rsa_kat_vector.h"
static const uint8_t g_msg[32] = {
  0x00,0x11,0x22,0x33,0x44,0x55,0x66,0x77,0x88,0x99,0xaa,0xbb,0xcc,0xdd,
  0xee,0xff,0x0f,0x1e,0x2d,0x3c,0x4b,0x5a,0x69,0x78,0x87,0x96,0xa5,0xb4,
  0xc3,0xd2,0xe1,0xf0,
};

/* -- Shared-buffer pool --------------------------------------------------- */

#define POOL_BUFFERS 6
#define POOL_BYTES 1024

static ZZ9KSharedBuffer pool[POOL_BUFFERS];

static int pool_up(ZZ9KContext *ctx)
{
  int i;
  for (i = 0; i < POOL_BUFFERS; i++) {
    int status = zz9k_alloc_shared(ctx, POOL_BYTES, 16,
                                   ZZ9K_ALLOC_HOST_WINDOW, &pool[i]);
    if (status != ZZ9K_STATUS_OK) {
      return status;
    }
  }
  return ZZ9K_STATUS_OK;
}

static void pool_down(ZZ9KContext *ctx)
{
  int i;
  for (i = 0; i < POOL_BUFFERS; i++) {
    if (pool[i].handle != 0) {
      zz9k_free_shared(ctx, pool[i].handle);
      pool[i].handle = 0;
    }
  }
}

static int buf_matches(const ZZ9KSharedBuffer *buffer, uint32_t offset,
                       const uint8_t *expected, uint32_t length)
{
  uint8_t scratch[POOL_BYTES];
  if (length > sizeof(scratch) ||
      !zz9k_shared_copy_from(scratch, (ZZ9KSharedBuffer *)buffer, offset,
                             length)) {
    return 0;
  }
  return memcmp(scratch, expected, length) == 0;
}

/* -- The checks ----------------------------------------------------------- */

static void check_caps_and_services(ZZ9KContext *ctx)
{
  ZZ9KCaps caps;
  ZZ9KServiceInfo info;
  int status = zz9k_query_caps(ctx, &caps);
  check("query caps", status == ZZ9K_STATUS_OK && caps.abi_major == 2 &&
        (caps.capability_bits & ZZ9K_CAP_MAILBOX) != 0 &&
        (caps.capability_bits & ZZ9K_CAP_CRYPTO) != 0 &&
        (caps.capability_bits & ZZ9K_CAP_IRQ_COMPLETION) != 0,
        status);

  status = zz9k_query_service(ctx, ZZ9K_SERVICE_CRYPTO, &info);
  check("query crypto service", status == ZZ9K_STATUS_OK &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_X25519) != 0 &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_P256) != 0 &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_ECDSA_P256) != 0 &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_RSA_2048) != 0 &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_AES_GCM) != 0 &&
        (info.flags & ZZ9K_SERVICE_FLAG_CRYPTO_P256_KEYGEN) != 0,
        status);

  /* A service this board does not offer reports NOT_FOUND. */
  status = zz9k_query_service(ctx, ZZ9K_SERVICE_SURFACE, &info);
  check("absent service reports NOT_FOUND",
        status == ZZ9K_STATUS_NOT_FOUND, status);
}

static void check_ping(ZZ9KContext *ctx)
{
  static const uint8_t payload[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
  uint8_t echoed[8];
  uint32_t echoed_len = sizeof(echoed);
  int status = zz9k_ping(ctx, payload, sizeof(payload), echoed, &echoed_len);
  check("ping echo", status == ZZ9K_STATUS_OK && echoed_len == 8 &&
        memcmp(echoed, payload, 8) == 0, status);
}

static void check_memory_ops(ZZ9KContext *ctx)
{
  static const uint8_t seq[16] = {
    0x10,0x21,0x32,0x43,0x54,0x65,0x76,0x87,0x98,0xa9,0xba,0xcb,0xdc,0xed,
    0xfe,0x0f,
  };
  int status;
  int ok = zz9k_shared_copy_to(&pool[0], 0, seq, sizeof(seq));
  status = zz9k_mem_copy(ctx, pool[1].handle, 4, pool[0].handle, 0,
                         sizeof(seq));
  check("shared write + mem copy", ok && status == ZZ9K_STATUS_OK &&
        buf_matches(&pool[1], 4, seq, sizeof(seq)), status);

  status = zz9k_mem_fill(ctx, pool[1].handle, 4, sizeof(seq), 0xEE);
  {
    uint8_t expected[16];
    memset(expected, 0xEE, sizeof(expected));
    check("mem fill", status == ZZ9K_STATUS_OK &&
          buf_matches(&pool[1], 4, expected, sizeof(expected)), status);
  }

  /* A freed handle goes stale. */
  {
    ZZ9KSharedBuffer scratch;
    status = zz9k_alloc_shared(ctx, 64, 16, ZZ9K_ALLOC_HOST_WINDOW, &scratch);
    if (status == ZZ9K_STATUS_OK) {
      zz9k_free_shared(ctx, scratch.handle);
      status = zz9k_mem_fill(ctx, scratch.handle, 0, 16, 0);
    }
    check("stale handle rejected", status == ZZ9K_STATUS_BAD_HANDLE, status);
  }
}

static void check_hashes(ZZ9KContext *ctx)
{
  ZZ9KCryptoHashDesc desc;
  ZZ9KCryptoResult result;
  int status;

  zz9k_shared_copy_to(&pool[0], 0, (const uint8_t *)"abc", 3);
  zz9k_crypto_build_hash_desc(&desc, ZZ9K_CRYPTO_HASH_SHA256,
                              pool[0].handle, 0, 3, pool[1].handle, 0);
  status = zz9k_crypto_hash(ctx, &desc, &result);
  check("sha256 abc", status == ZZ9K_STATUS_OK && result.bytes_written == 32 &&
        result.algorithm == ZZ9K_CRYPTO_HASH_SHA256 &&
        buf_matches(&pool[1], 0, sha256_abc, 32), status);

  zz9k_shared_copy_to(&pool[0], 0,
                      (const uint8_t *)"what do ya want for nothing?", 28);
  zz9k_shared_copy_to(&pool[2], 0, (const uint8_t *)"Jefe", 4);
  zz9k_crypto_build_hmac_desc(&desc, ZZ9K_CRYPTO_HASH_SHA256,
                              pool[0].handle, 0, 28, pool[1].handle, 0,
                              pool[2].handle, 0, 4);
  status = zz9k_crypto_hash(ctx, &desc, &result);
  check("hmac-sha256 jefe", status == ZZ9K_STATUS_OK &&
        buf_matches(&pool[1], 0, hmac_jefe, 32), status);

  zz9k_shared_copy_to(&pool[0], 0,
                      (const uint8_t *)"Cryptographic Forum Research Group",
                      34);
  zz9k_shared_copy_to(&pool[2], 0, poly_key, 32);
  zz9k_crypto_build_poly1305_desc(&desc, pool[0].handle, 0, 34,
                                  pool[1].handle, 0, pool[2].handle, 0);
  status = zz9k_crypto_hash(ctx, &desc, &result);
  check("poly1305 rfc8439", status == ZZ9K_STATUS_OK &&
        result.bytes_written == 16 &&
        buf_matches(&pool[1], 0, poly_tag, 16), status);
}

static void check_chacha20(ZZ9KContext *ctx)
{
  static const char pt[] =
      "Ladies and Gentlemen of the class of '99: If I could offer you "
      "only one tip for the future, sunscreen would be it.";
  const uint32_t pt_len = (uint32_t)(sizeof(pt) - 1);
  ZZ9KCryptoStreamDesc desc;
  ZZ9KCryptoResult result;
  int status;

  zz9k_shared_copy_to(&pool[0], 0, (const uint8_t *)pt, pt_len);
  zz9k_shared_copy_to(&pool[2], 0, chacha_key, 32);
  zz9k_shared_copy_to(&pool[3], 0, chacha_nonce, 12);
  zz9k_crypto_build_chacha20_desc(&desc, pool[0].handle, 0, pt_len,
                                  pool[1].handle, 0, pool[2].handle, 0,
                                  pool[3].handle, 0, 1);
  status = zz9k_crypto_stream(ctx, &desc, &result);
  check("chacha20 rfc8439", status == ZZ9K_STATUS_OK &&
        result.bytes_written == pt_len &&
        buf_matches(&pool[1], 0, chacha_ct_head, 16), status);

  /* Round trip: decrypting the ciphertext restores the plaintext. */
  zz9k_crypto_build_chacha20_desc(&desc, pool[1].handle, 0, pt_len,
                                  pool[4].handle, 0, pool[2].handle, 0,
                                  pool[3].handle, 0, 1);
  status = zz9k_crypto_stream(ctx, &desc, &result);
  check("chacha20 round trip", status == ZZ9K_STATUS_OK &&
        buf_matches(&pool[4], 0, (const uint8_t *)pt, pt_len), status);
}

static void check_aead(ZZ9KContext *ctx)
{
  static const uint8_t key[32] = { 7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
                                   7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7 };
  static const uint8_t nonce[12] = { 9,9,9,9,9,9,9,9,9,9,9,9 };
  static const uint8_t msg[14] = { 's','e','c','r','e','t',' ','m','e','s','s','a','g','e' };
  ZZ9KCryptoAeadDesc desc;
  ZZ9KCryptoResult result;
  uint8_t byte;
  int status;

  zz9k_shared_copy_to(&pool[0], 0, msg, sizeof(msg));
  zz9k_shared_copy_to(&pool[2], 0, key, 32);
  zz9k_shared_copy_to(&pool[3], 0, nonce, 12);
  zz9k_shared_copy_to(&pool[5], 0, (const uint8_t *)"aad", 3);
  zz9k_crypto_build_chacha20_poly1305_desc(&desc, pool[0].handle, 0,
                                           sizeof(msg), pool[1].handle, 0,
                                           pool[5].handle, 0, 3,
                                           pool[2].handle, 0,
                                           pool[3].handle, 0);
  status = zz9k_crypto_aead(ctx, &desc, &result);
  check("aead encrypt", status == ZZ9K_STATUS_OK &&
        result.bytes_written == sizeof(msg) + 16 &&
        result.algorithm == ZZ9K_CRYPTO_AEAD_CHACHA20_POLY1305 &&
        result.flags == 0, status);

  zz9k_crypto_build_chacha20_poly1305_desc(&desc, pool[1].handle, 0,
                                           sizeof(msg), pool[4].handle, 0,
                                           pool[5].handle, 0, 3,
                                           pool[2].handle, 0,
                                           pool[3].handle, 0);
  desc.flags |= ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT;
  status = zz9k_crypto_aead(ctx, &desc, &result);
  check("aead decrypt", status == ZZ9K_STATUS_OK &&
        result.bytes_written == sizeof(msg) &&
        result.flags == ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT &&
        buf_matches(&pool[4], 0, msg, sizeof(msg)), status);

  /* Corrupt the tag: decrypt must report an error status, and the
   * provider-visible contract is "any non-OK means reject". */
  zz9k_shared_copy_from(&byte, &pool[1], sizeof(msg), 1);
  byte ^= 1;
  zz9k_shared_copy_to(&pool[1], sizeof(msg), &byte, 1);
  status = zz9k_crypto_aead(ctx, &desc, &result);
  check("aead tag mismatch rejected", status != ZZ9K_STATUS_OK, status);

  /* AES-128-GCM through the same op. */
  zz9k_crypto_build_aes_gcm_desc(&desc, pool[0].handle, 0, sizeof(msg),
                                 pool[1].handle, 0, ZZ9K_INVALID_HANDLE, 0,
                                 0, pool[2].handle, 0, 16, pool[3].handle,
                                 0);
  status = zz9k_crypto_aead(ctx, &desc, &result);
  if (status == ZZ9K_STATUS_OK) {
    zz9k_crypto_build_aes_gcm_desc(&desc, pool[1].handle, 0, sizeof(msg),
                                   pool[4].handle, 0, ZZ9K_INVALID_HANDLE,
                                   0, 0, pool[2].handle, 0, 16,
                                   pool[3].handle,
                                   ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT);
    status = zz9k_crypto_aead(ctx, &desc, &result);
  }
  check("aes-128-gcm round trip", status == ZZ9K_STATUS_OK &&
        result.algorithm == ZZ9K_CRYPTO_AEAD_AES128_GCM &&
        buf_matches(&pool[4], 0, msg, sizeof(msg)), status);
}

static void check_kx(ZZ9KContext *ctx)
{
  ZZ9KCryptoKxDesc desc;
  ZZ9KCryptoResult result;
  uint8_t shared_ab[32];
  uint8_t shared_ba[32];
  int status;

  zz9k_shared_copy_to(&pool[0], 0, x25519_scalar, 32);
  zz9k_shared_copy_to(&pool[2], 0, x25519_point, 32);
  zz9k_crypto_build_x25519_desc(&desc, pool[0].handle, 0, pool[2].handle, 0,
                                pool[1].handle, 0);
  status = zz9k_crypto_kx(ctx, &desc, &result);
  check("x25519 rfc7748", status == ZZ9K_STATUS_OK &&
        result.bytes_written == 32 &&
        buf_matches(&pool[1], 0, x25519_out, 32), status);

  /* P-256: keygen both sides, derive both ways, compare with RFC 5903. */
  zz9k_shared_copy_to(&pool[0], 0, p256_da, 32);
  zz9k_shared_copy_to(&pool[2], 0, p256_db, 32);
  zz9k_crypto_build_p256_keygen_desc(&desc, pool[0].handle, 0,
                                     pool[3].handle, 0);
  status = zz9k_crypto_kx(ctx, &desc, &result);
  if (status == ZZ9K_STATUS_OK && result.bytes_written == 65) {
    zz9k_crypto_build_p256_keygen_desc(&desc, pool[2].handle, 0,
                                       pool[4].handle, 0);
    status = zz9k_crypto_kx(ctx, &desc, &result);
  }
  check("p256 keygen", status == ZZ9K_STATUS_OK && result.bytes_written == 65,
        status);

  zz9k_crypto_build_p256_desc(&desc, pool[0].handle, 0, pool[4].handle, 0,
                              pool[1].handle, 0);
  status = zz9k_crypto_kx(ctx, &desc, &result);
  if (status == ZZ9K_STATUS_OK) {
    zz9k_shared_copy_from(shared_ab, &pool[1], 0, 32);
    zz9k_crypto_build_p256_desc(&desc, pool[2].handle, 0, pool[3].handle, 0,
                                pool[1].handle, 0);
    status = zz9k_crypto_kx(ctx, &desc, &result);
    zz9k_shared_copy_from(shared_ba, &pool[1], 0, 32);
  }
  check("p256 derive rfc5903", status == ZZ9K_STATUS_OK &&
        memcmp(shared_ab, shared_ba, 32) == 0 &&
        memcmp(shared_ab, p256_shared, 32) == 0, status);
}

static void check_verify(ZZ9KContext *ctx)
{
  ZZ9KCryptoHashDesc hash_desc;
  ZZ9KCryptoVerifyDesc desc;
  ZZ9KCryptoResult result;
  int valid;
  int status;
  uint8_t byte;

  /* ECDSA: board-hash "sample", then verify the RFC 6979 signature over
   * that digest. */
  zz9k_shared_copy_to(&pool[0], 0, (const uint8_t *)"sample", 6);
  zz9k_crypto_build_hash_desc(&hash_desc, ZZ9K_CRYPTO_HASH_SHA256,
                              pool[0].handle, 0, 6, pool[1].handle, 0);
  status = zz9k_crypto_hash(ctx, &hash_desc, &result);
  zz9k_shared_copy_to(&pool[2], 0, ecdsa_sig, 64);
  zz9k_shared_copy_to(&pool[3], 0, ecdsa_pub, 65);
  zz9k_crypto_build_verify_desc(&desc, ZZ9K_CRYPTO_VERIFY_ECDSA_P256_SHA256,
                                pool[1].handle, 0, 32, pool[2].handle, 0, 64,
                                pool[3].handle, 0, 65);
  valid = 0;
  if (status == ZZ9K_STATUS_OK) {
    status = zz9k_crypto_verify(ctx, &desc, &valid);
  }
  check("ecdsa p256 verify", status == ZZ9K_STATUS_OK && valid == 1, status);

  /* A corrupted signature is a *successful* verification with valid = 0. */
  zz9k_shared_copy_from(&byte, &pool[2], 10, 1);
  byte ^= 1;
  zz9k_shared_copy_to(&pool[2], 10, &byte, 1);
  valid = 1;
  status = zz9k_crypto_verify(ctx, &desc, &valid);
  check("ecdsa invalid sig reports valid=0",
        status == ZZ9K_STATUS_OK && valid == 0, status);

  /* RSA-2048: the SDK's own KAT. Key wire format: modulus || 4-byte BE
   * exponent. */
  {
    static const uint8_t exp_be[4] = { 0x00, 0x01, 0x00, 0x01 };
    zz9k_shared_copy_to(&pool[0], 0, g_msg, 32);
    zz9k_crypto_build_hash_desc(&hash_desc, ZZ9K_CRYPTO_HASH_SHA256,
                                pool[0].handle, 0, 32, pool[1].handle, 0);
    status = zz9k_crypto_hash(ctx, &hash_desc, &result);
    zz9k_shared_copy_to(&pool[2], 0, kat_rsa_sig_pkcs1, 256);
    zz9k_shared_copy_to(&pool[3], 0, kat_rsa_n, 256);
    zz9k_shared_copy_to(&pool[3], 256, exp_be, 4);
    zz9k_crypto_build_verify_desc(&desc,
                                  ZZ9K_CRYPTO_VERIFY_RSA_PKCS1_2048_SHA256,
                                  pool[1].handle, 0, 32, pool[2].handle, 0,
                                  256, pool[3].handle, 0, 260);
    valid = 0;
    if (status == ZZ9K_STATUS_OK) {
      status = zz9k_crypto_verify(ctx, &desc, &valid);
    }
    check("rsa-2048 verify kat", status == ZZ9K_STATUS_OK && valid == 1,
          status);
  }
}

static void check_completion_irq(ZZ9KContext *ctx)
{
  static const uint8_t payload[4] = { 0xAA, 0x55, 0xAA, 0x55 };
  uint8_t echoed[4];
  uint32_t echoed_len = sizeof(echoed);
  int status;

  if (!zz9k_completion_irq_supported(ctx)) {
    fail("completion irq supported", 0);
    return;
  }
  /* The full interrupt path, exactly as the SDK arms it: install the
   * completion interrupt server on the line the ZZ9000.CFG key selects
   * (INT6 here), enable the board interrupt, and let a synchronous call
   * ride the armed Wait()-on-Signal path instead of the busy poll. Never
   * enable the board interrupt without a server installed: the line is
   * level-sensitive, and an unclaimed interrupt storms the machine -- on
   * real hardware just the same. */
  status = zz9k_arm_completion_irq(ctx);
  check("arm completion irq", status == ZZ9K_STATUS_OK, status);
  if (status != ZZ9K_STATUS_OK) {
    return;
  }
  status = zz9k_ping(ctx, payload, sizeof(payload), echoed, &echoed_len);
  check("irq-driven call completes", status == ZZ9K_STATUS_OK &&
        echoed_len == sizeof(payload) &&
        memcmp(echoed, payload, sizeof(payload)) == 0, status);
  zz9k_disarm_completion_irq(ctx);
  /* And the board keeps working in polled mode after disarm. */
  echoed_len = sizeof(echoed);
  status = zz9k_ping(ctx, payload, sizeof(payload), echoed, &echoed_len);
  check("polled call after disarm", status == ZZ9K_STATUS_OK, status);
}

static void check_diag(ZZ9KContext *ctx)
{
  ZZ9KDiagInfo diag;
  int status = zz9k_read_diag(ctx, &diag);
  check("diag counters", status == ZZ9K_STATUS_OK &&
        diag.requests_completed > 20 && diag.pending_requests == 0 &&
        diag.mailbox_ring_entries == 32, status);
}

int main(void)
{
  ZZ9KContext *ctx = 0;
  struct Process *self = (struct Process *)FindTask(NULL);
  APTR old_window_ptr = self->pr_WindowPtr;
  int status;

  /* Suppress DOS system requesters for this process: the transport's
   * interrupt-line query opens ENV:ZZ9K_INT2, and on the minimal test boot
   * volume no ENV: assign exists -- without this, DOS would put up an
   * "insert volume ENV:" requester and the headless run would hang
   * forever waiting for a click. */
  self->pr_WindowPtr = (APTR)-1;

  status = zz9k_open(&ctx);
  printf("ZZ9K: probe start\n");
  fflush(stdout);
  if (status != ZZ9K_STATUS_OK) {
    printf("ZZ9K: FAIL open (%d)\nZZ9K: SUMMARY FAIL\n", status);
    return 20;
  }
  pass("open");

  status = pool_up(ctx);
  if (status != ZZ9K_STATUS_OK) {
    fail("shared pool alloc", status);
  } else {
    pass("shared pool alloc");
    check_caps_and_services(ctx);
    check_ping(ctx);
    check_memory_ops(ctx);
    check_hashes(ctx);
    check_chacha20(ctx);
    check_aead(ctx);
    check_kx(ctx);
    check_verify(ctx);
    check_completion_irq(ctx);
    check_diag(ctx);
  }
  pool_down(ctx);
  zz9k_close(ctx);

  self->pr_WindowPtr = old_window_ptr;
  printf("ZZ9K: %d checks, %d failures\n", checks, failures);
  printf("ZZ9K: SUMMARY %s\n", failures == 0 ? "PASS" : "FAIL");
  return failures == 0 ? 0 : 20;
}

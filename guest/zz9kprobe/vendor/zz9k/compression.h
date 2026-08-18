/*
 * Header-only compression/decompression descriptor helpers.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_COMPRESSION_H
#define ZZ9K_COMPRESSION_H

#include "zz9k/abi.h"
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline int zz9k_compression_algorithm_known(uint32_t algorithm)
{
  switch (algorithm) {
  case ZZ9K_COMPRESSION_DEFLATE_RAW:
  case ZZ9K_COMPRESSION_ZLIB:
  case ZZ9K_COMPRESSION_GZIP:
  case ZZ9K_COMPRESSION_LZ4_BLOCK:
  case ZZ9K_COMPRESSION_LZMA_ALONE:
  case ZZ9K_COMPRESSION_LZMA2:
  case ZZ9K_COMPRESSION_LH1:
  case ZZ9K_COMPRESSION_LH5:
  case ZZ9K_COMPRESSION_LH6:
  case ZZ9K_COMPRESSION_LH7:
    return 1;
  default:
    return 0;
  }
}

static inline uint32_t zz9k_compression_required_service_flags(
    uint32_t algorithm)
{
  switch (algorithm) {
  case ZZ9K_COMPRESSION_DEFLATE_RAW:
    return ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_RAW;
  case ZZ9K_COMPRESSION_ZLIB:
    return ZZ9K_SERVICE_FLAG_CODEC_ZLIB;
  case ZZ9K_COMPRESSION_GZIP:
    return ZZ9K_SERVICE_FLAG_CODEC_GZIP;
  case ZZ9K_COMPRESSION_LZ4_BLOCK:
    return ZZ9K_SERVICE_FLAG_CODEC_LZ4_BLOCK;
  case ZZ9K_COMPRESSION_LZMA_ALONE:
    return ZZ9K_SERVICE_FLAG_CODEC_LZMA_ALONE;
  case ZZ9K_COMPRESSION_LZMA2:
    return ZZ9K_SERVICE_FLAG_CODEC_LZMA2;
  case ZZ9K_COMPRESSION_LH1:
  case ZZ9K_COMPRESSION_LH5:
  case ZZ9K_COMPRESSION_LH6:
  case ZZ9K_COMPRESSION_LH7:
    return ZZ9K_SERVICE_FLAG_CODEC_LZH;
  default:
    return 0U;
  }
}

static inline uint32_t zz9k_compression_required_feed_service_flags(
    uint32_t algorithm)
{
  switch (algorithm) {
  case ZZ9K_COMPRESSION_DEFLATE_RAW:
    return ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_RAW |
           ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED |
           ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_FEED;
  case ZZ9K_COMPRESSION_ZLIB:
    return ZZ9K_SERVICE_FLAG_CODEC_ZLIB |
           ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED |
           ZZ9K_SERVICE_FLAG_CODEC_ZLIB_FEED;
  case ZZ9K_COMPRESSION_GZIP:
    return ZZ9K_SERVICE_FLAG_CODEC_GZIP |
           ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED |
           ZZ9K_SERVICE_FLAG_CODEC_GZIP_FEED;
  case ZZ9K_COMPRESSION_LZMA_ALONE:
    return ZZ9K_SERVICE_FLAG_CODEC_LZMA_ALONE |
           ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED;
  case ZZ9K_COMPRESSION_LZMA2:
    return ZZ9K_SERVICE_FLAG_CODEC_LZMA2 |
           ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED;
  default:
    return 0U;
  }
}

static inline int zz9k_compression_build_decompress_desc(
    ZZ9KDecompressDesc *desc,
    uint32_t algorithm,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t dst_capacity,
    uint32_t flags)
{
  if (!desc || !zz9k_compression_algorithm_known(algorithm) ||
      src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE || dst_capacity == 0U ||
      (flags & ~ZZ9K_DECOMPRESS_FLAG_EXPECT_END) != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->dst_capacity = dst_capacity;
  desc->algorithm = algorithm;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_compression_build_decompress_test_desc(
    ZZ9KDecompressTestDesc *desc,
    uint32_t algorithm,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t output_limit,
    uint32_t flags)
{
  if (!desc || !zz9k_compression_algorithm_known(algorithm) ||
      src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      output_limit == 0U ||
      (flags & ~ZZ9K_DECOMPRESS_FLAG_EXPECT_END) != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->output_limit = output_limit;
  desc->algorithm = algorithm;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_compression_build_decompress_stream_begin_desc(
    ZZ9KDecompressStreamBeginDesc *desc,
    uint32_t algorithm,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t output_limit,
    uint32_t flags)
{
  uint32_t allowed_flags;
  int feed_input;

  allowed_flags = ZZ9K_DECOMPRESS_FLAG_EXPECT_END |
                  ZZ9K_DECOMPRESS_FLAG_FEED_INPUT;
  feed_input = (flags & ZZ9K_DECOMPRESS_FLAG_FEED_INPUT) != 0U;
  if (!desc || !zz9k_compression_algorithm_known(algorithm) ||
      output_limit == 0U || (flags & ~allowed_flags) != 0U) {
    return 0;
  }
  if (feed_input) {
    if (src_handle != ZZ9K_INVALID_HANDLE || src_offset != 0U ||
        src_length != 0U) {
      return 0;
    }
  } else if (src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->output_limit = output_limit;
  desc->algorithm = algorithm;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_compression_build_decompress_stream_feed_desc(
    ZZ9KDecompressStreamFeedDesc *desc,
    uint32_t session,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t flags)
{
  if (!desc || session == 0U || src_handle == ZZ9K_INVALID_HANDLE ||
      src_length == 0U ||
      (flags & ~ZZ9K_DECOMPRESS_STREAM_FEED_EOF) != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->session = session;
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_compression_build_decompress_stream_read_desc(
    ZZ9KDecompressStreamReadDesc *desc,
    uint32_t session,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t dst_capacity,
    uint32_t flags)
{
  if (!desc || session == 0U || dst_handle == ZZ9K_INVALID_HANDLE ||
      dst_capacity == 0U || flags != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->session = session;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->dst_capacity = dst_capacity;
  desc->flags = flags;
  return 1;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_COMPRESSION_H */

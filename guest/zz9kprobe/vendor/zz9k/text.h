/*
 * Header-only text helpers for SDK callers that do not link zz9k_host.o.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_TEXT_H
#define ZZ9K_TEXT_H

#include "zz9k/abi.h"

#ifdef __cplusplus
extern "C" {
#endif

static inline const char *zz9k_status_text(int status)
{
  switch (status) {
  case ZZ9K_STATUS_OK:
    return "ok";
  case ZZ9K_STATUS_QUEUED:
    return "queued";
  case ZZ9K_STATUS_BUSY:
    return "busy";
  case ZZ9K_STATUS_UNSUPPORTED:
    return "unsupported";
  case ZZ9K_STATUS_BAD_REQUEST:
    return "bad-request";
  case ZZ9K_STATUS_BAD_HANDLE:
    return "bad-handle";
  case ZZ9K_STATUS_NO_MEMORY:
    return "no-memory";
  case ZZ9K_STATUS_TIMEOUT:
    return "timeout";
  case ZZ9K_STATUS_CANCELLED:
    return "cancelled";
  case ZZ9K_STATUS_IO_ERROR:
    return "io-error";
  case ZZ9K_STATUS_NOT_FOUND:
    return "not-found";
  case ZZ9K_STATUS_INTERNAL_ERROR:
    return "internal-error";
  default:
    return "error";
  }
}

static inline const char *zz9k_surface_format_text(uint32_t format)
{
  switch (format) {
  case ZZ9K_SURFACE_FORMAT_RGB565:
    return "RGB565";
  case ZZ9K_SURFACE_FORMAT_ARGB8888:
    return "ARGB8888";
  case ZZ9K_SURFACE_FORMAT_RGBA8888:
    return "RGBA8888";
  case ZZ9K_SURFACE_FORMAT_INDEX8:
    return "INDEX8";
  case ZZ9K_SURFACE_FORMAT_PLANAR:
    return "PLANAR";
  case ZZ9K_SURFACE_FORMAT_RGB555:
    return "RGB555";
  case ZZ9K_SURFACE_FORMAT_BGRA8888:
    return "BGRA8888";
  case ZZ9K_SURFACE_FORMAT_RGB888:
    return "RGB888";
  case ZZ9K_SURFACE_FORMAT_UNKNOWN:
  default:
    return "unknown";
  }
}

static inline const char *zz9k_image_codec_text(uint32_t codec)
{
  switch (codec) {
  case ZZ9K_IMAGE_CODEC_JPEG:
    return "JPEG";
  case ZZ9K_IMAGE_CODEC_PNG:
    return "PNG";
  case ZZ9K_IMAGE_CODEC_GIF:
    return "GIF";
  default:
    return "unknown";
  }
}

static inline const char *zz9k_compression_algorithm_text(uint32_t algorithm)
{
  switch (algorithm) {
  case ZZ9K_COMPRESSION_DEFLATE_RAW:
    return "deflate-raw";
  case ZZ9K_COMPRESSION_ZLIB:
    return "zlib";
  case ZZ9K_COMPRESSION_GZIP:
    return "gzip";
  case ZZ9K_COMPRESSION_LZ4_BLOCK:
    return "lz4-block";
  case ZZ9K_COMPRESSION_LZMA_ALONE:
    return "lzma-alone";
  case ZZ9K_COMPRESSION_LZMA2:
    return "lzma2";
  case ZZ9K_COMPRESSION_LH1:
    return "lh1";
  case ZZ9K_COMPRESSION_LH5:
    return "lh5";
  case ZZ9K_COMPRESSION_LH6:
    return "lh6";
  case ZZ9K_COMPRESSION_LH7:
    return "lh7";
  default:
    return "unknown";
  }
}

static inline uint32_t zz9k_known_service_count(void)
{
  return 12U;
}

static inline uint32_t zz9k_known_service_id(uint32_t index)
{
  switch (index) {
  case 0:
    return ZZ9K_SERVICE_CORE;
  case 1:
    return ZZ9K_SERVICE_MEMORY;
  case 2:
    return ZZ9K_SERVICE_SURFACE;
  case 3:
    return ZZ9K_SERVICE_GFX;
  case 4:
    return ZZ9K_SERVICE_IMAGE;
  case 5:
    return ZZ9K_SERVICE_AUDIO;
  case 6:
    return ZZ9K_SERVICE_CODEC;
  case 7:
    return ZZ9K_SERVICE_STORAGE;
  case 8:
    return ZZ9K_SERVICE_CRYPTO;
  case 9:
    return ZZ9K_SERVICE_DIAG;
  case 10:
    return ZZ9K_SERVICE_MODULE;
  case 11:
    return ZZ9K_SERVICE_VIDEO;
  default:
    return 0xffffffffUL;
  }
}

static inline const char *zz9k_service_text(uint32_t service_id)
{
  switch (service_id) {
  case ZZ9K_SERVICE_CORE:
    return "core";
  case ZZ9K_SERVICE_MEMORY:
    return "memory";
  case ZZ9K_SERVICE_SURFACE:
    return "surface";
  case ZZ9K_SERVICE_GFX:
    return "gfx";
  case ZZ9K_SERVICE_IMAGE:
    return "image";
  case ZZ9K_SERVICE_AUDIO:
    return "audio";
  case ZZ9K_SERVICE_CODEC:
    return "codec";
  case ZZ9K_SERVICE_STORAGE:
    return "storage";
  case ZZ9K_SERVICE_CRYPTO:
    return "crypto";
  case ZZ9K_SERVICE_DIAG:
    return "diag";
  case ZZ9K_SERVICE_MODULE:
    return "module";
  case ZZ9K_SERVICE_VIDEO:
    return "video";
  default:
    return "unknown";
  }
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_TEXT_H */

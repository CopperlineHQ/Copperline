/*
 * Header-only capability helpers for SDK callers.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_CAPS_H
#define ZZ9K_CAPS_H

#include "zz9k/abi.h"

#ifdef __cplusplus
extern "C" {
#endif

static inline int zz9k_has_capability(uint32_t available_bits,
                                      uint32_t capability_bit)
{
  return capability_bit != 0U &&
         (available_bits & capability_bit) == capability_bit;
}

static inline int zz9k_has_capabilities(uint32_t available_bits,
                                        uint32_t required_bits)
{
  return (available_bits & required_bits) == required_bits;
}

static inline uint32_t zz9k_missing_capabilities(uint32_t available_bits,
                                                uint32_t required_bits)
{
  return required_bits & ~available_bits;
}

static inline uint32_t zz9k_service_capability_mask(uint32_t service_id)
{
  switch (service_id) {
  case ZZ9K_SERVICE_CORE:
    return ZZ9K_CAP_MAILBOX;
  case ZZ9K_SERVICE_MEMORY:
    return ZZ9K_CAP_SHARED_ALLOC | ZZ9K_CAP_MEMORY_OPS;
  case ZZ9K_SERVICE_SURFACE:
    return ZZ9K_CAP_SURFACES | ZZ9K_CAP_FRAMEBUFFER_SURFACE;
  case ZZ9K_SERVICE_GFX:
    return ZZ9K_CAP_GFX_OPS;
  case ZZ9K_SERVICE_IMAGE:
    return ZZ9K_CAP_IMAGE_DECODE | ZZ9K_CAP_IMAGE_SCALE;
  case ZZ9K_SERVICE_AUDIO:
    return ZZ9K_CAP_AUDIO_DECODE;
  case ZZ9K_SERVICE_CODEC:
    return ZZ9K_CAP_COMPRESSION;
  case ZZ9K_SERVICE_STORAGE:
    return ZZ9K_CAP_STORAGE_OPS;
  case ZZ9K_SERVICE_CRYPTO:
    return ZZ9K_CAP_CRYPTO;
  case ZZ9K_SERVICE_DIAG:
    return ZZ9K_CAP_DIAGNOSTICS;
  case ZZ9K_SERVICE_MODULE:
    return ZZ9K_CAP_MODULES;
  case ZZ9K_SERVICE_VIDEO:
    return ZZ9K_CAP_VIDEO_DECODE | ZZ9K_CAP_MEDIA_SESSION;
  default:
    return 0U;
  }
}

static inline int zz9k_service_advertised_by_capabilities(
  uint32_t service_id, uint32_t capability_bits)
{
  uint32_t service_bits;

  service_bits = zz9k_service_capability_mask(service_id);
  return service_bits != 0U && (capability_bits & service_bits) != 0U;
}

static inline int zz9k_has_service_flag(uint32_t available_flags,
                                        uint32_t service_flag)
{
  return service_flag != 0U &&
         (available_flags & service_flag) == service_flag;
}

static inline int zz9k_has_service_flags(uint32_t available_flags,
                                         uint32_t required_flags)
{
  return (available_flags & required_flags) == required_flags;
}

static inline uint32_t zz9k_missing_service_flags(uint32_t available_flags,
                                                 uint32_t required_flags)
{
  return required_flags & ~available_flags;
}

static inline uint32_t zz9k_known_capability_count(void)
{
  return 25U;
}

static inline uint32_t zz9k_known_capability_bit(uint32_t index)
{
  switch (index) {
  case 0:
    return ZZ9K_CAP_MAILBOX;
  case 1:
    return ZZ9K_CAP_IRQ_COMPLETION;
  case 2:
    return ZZ9K_CAP_SHARED_ALLOC;
  case 3:
    return ZZ9K_CAP_SURFACES;
  case 4:
    return ZZ9K_CAP_FRAMEBUFFER_SURFACE;
  case 5:
    return ZZ9K_CAP_IMAGE_DECODE;
  case 6:
    return ZZ9K_CAP_IMAGE_SCALE;
  case 7:
    return ZZ9K_CAP_AUDIO_DECODE;
  case 8:
    return ZZ9K_CAP_CRYPTO;
  case 9:
    return ZZ9K_CAP_MODULES;
  case 10:
    return ZZ9K_CAP_MEMORY_OPS;
  case 11:
    return ZZ9K_CAP_DIAGNOSTICS;
  case 12:
    return ZZ9K_CAP_DOORBELL;
  case 13:
    return ZZ9K_CAP_POLLING_COMPLETION;
  case 14:
    return ZZ9K_CAP_SERVICE_DISCOVERY;
  case 15:
    return ZZ9K_CAP_SURFACE_OPS;
  case 16:
    return ZZ9K_CAP_COMPRESSION;
  case 17:
    return ZZ9K_CAP_GFX_OPS;
  case 18:
    return ZZ9K_CAP_STORAGE_OPS;
  case 19:
    return ZZ9K_CAP_AUDIO_PLAYBACK;
  case 20:
    return ZZ9K_CAP_HOST_WINDOW_HEAP;
  case 21:
    return ZZ9K_CAP_VIDEO_DECODE;
  case 22:
    return ZZ9K_CAP_MEDIA_SESSION;
  case 23:
    return ZZ9K_CAP_AUDIO_STREAM_DRAIN;
  case 24:
    return ZZ9K_CAP_APERTURE_LAYOUT;
  default:
    return 0U;
  }
}

static inline uint32_t zz9k_known_service_flag_count(uint32_t service_id)
{
  if (service_id == ZZ9K_SERVICE_IMAGE) {
    return 16U;
  }
  if (service_id == ZZ9K_SERVICE_AUDIO) {
    return 9U;
  }
  if (service_id == ZZ9K_SERVICE_CODEC) {
    return 19U;
  }
  if (service_id == ZZ9K_SERVICE_CRYPTO) {
    return 5U;
  }
  if (service_id == ZZ9K_SERVICE_VIDEO) {
    return 15U;
  }
  if (service_id == ZZ9K_SERVICE_SURFACE) {
    return 5U;
  }
  return 4U;
}

static inline uint32_t zz9k_known_service_flag(uint32_t service_id,
                                              uint32_t index)
{
  switch (index) {
  case 0:
    return ZZ9K_SERVICE_FLAG_FIRMWARE;
  case 1:
    return ZZ9K_SERVICE_FLAG_MODULE;
  case 2:
    return ZZ9K_SERVICE_FLAG_ASYNC;
  case 3:
    return ZZ9K_SERVICE_FLAG_ZERO_COPY;
  default:
    break;
  }

  if (service_id == ZZ9K_SERVICE_IMAGE) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_IMAGE_JPEG_BASELINE;
    case 5:
      return ZZ9K_SERVICE_FLAG_IMAGE_JPEG_PROGRESSIVE;
    case 6:
      return ZZ9K_SERVICE_FLAG_IMAGE_JPEG_DIRECT_BGRA;
    case 7:
      return ZZ9K_SERVICE_FLAG_IMAGE_JPEG_SCALING;
    case 8:
      return ZZ9K_SERVICE_FLAG_IMAGE_PNG_DIRECT_BGRA;
    case 9:
      return ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BILINEAR;
    case 10:
      return ZZ9K_SERVICE_FLAG_IMAGE_SCALE_CLIPPED;
    case 11:
      return ZZ9K_SERVICE_FLAG_IMAGE_STREAMING_INPUT;
    case 12:
      return ZZ9K_SERVICE_FLAG_IMAGE_TILE_OUTPUT;
    case 13:
      return ZZ9K_SERVICE_FLAG_IMAGE_FRAMEBUFFER_OUTPUT;
    case 14:
      return ZZ9K_SERVICE_FLAG_IMAGE_RGB888_OUTPUT;
    case 15:
      return ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BGRA_TO_RGB555_RGB565;
    default:
      return 0U;
    }
  }

  if (service_id == ZZ9K_SERVICE_CODEC) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_RAW;
    case 5:
      return ZZ9K_SERVICE_FLAG_CODEC_ZLIB;
    case 6:
      return ZZ9K_SERVICE_FLAG_CODEC_GZIP;
    case 7:
      return ZZ9K_SERVICE_FLAG_CODEC_LZ4_BLOCK;
    case 8:
      return ZZ9K_SERVICE_FLAG_CODEC_LZMA_ALONE;
    case 9:
      return ZZ9K_SERVICE_FLAG_CODEC_CHECKSUM;
    case 10:
      return ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_TEST;
    case 11:
      return ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_STREAM;
    case 12:
      return ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED;
    case 13:
      return ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_FEED;
    case 14:
      return ZZ9K_SERVICE_FLAG_CODEC_ZLIB_FEED;
    case 15:
      return ZZ9K_SERVICE_FLAG_CODEC_GZIP_FEED;
    case 16:
      return ZZ9K_SERVICE_FLAG_CODEC_LZMA2;
    case 17:
      return ZZ9K_SERVICE_FLAG_CODEC_LZH;
    case 18:
      return ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_BATCH;
    default:
      return 0U;
    }
  }

  if (service_id == ZZ9K_SERVICE_AUDIO) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_AUDIO_MP3_DECODE;
    case 5:
      return ZZ9K_SERVICE_FLAG_AUDIO_PCM_MIX;
    case 6:
      return ZZ9K_SERVICE_FLAG_AUDIO_RESAMPLE;
    case 7:
      return ZZ9K_SERVICE_FLAG_AUDIO_PCM16_STEREO;
    case 8:
      return ZZ9K_SERVICE_FLAG_AUDIO_MP3_STREAM;
    default:
      return 0U;
    }
  }

  if (service_id == ZZ9K_SERVICE_CRYPTO) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_CRYPTO_X25519;
    default:
      return 0U;
    }
  }

  if (service_id == ZZ9K_SERVICE_SURFACE) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_SURFACE_PALETTE_QUERY;
    default:
      break;
    }
    return 0U;
  }

  if (service_id == ZZ9K_SERVICE_VIDEO) {
    switch (index) {
    case 4:
      return ZZ9K_SERVICE_FLAG_VIDEO_MPEG1;
    case 5:
      return ZZ9K_SERVICE_FLAG_VIDEO_MPEG_PS;
    case 6:
      return ZZ9K_SERVICE_FLAG_VIDEO_DIRECT_OVERLAY;
    case 7:
      return ZZ9K_SERVICE_FLAG_VIDEO_STREAMING_INPUT;
    case 8:
      return ZZ9K_SERVICE_FLAG_VIDEO_CORE1;
    case 9:
      return ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_SESSION;
    case 10:
      return ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_MP2;
    case 11:
      return ZZ9K_SERVICE_FLAG_VIDEO_EXPLICIT_PRESENT;
    case 12:
      return ZZ9K_SERVICE_FLAG_VIDEO_TIMELINE_90KHZ;
    case 13:
      return ZZ9K_SERVICE_FLAG_VIDEO_PCM_RING_STATUS;
    case 14:
      return ZZ9K_SERVICE_FLAG_VIDEO_AUDIO_BIND;
    default:
      return 0U;
    }
  }

  return 0U;
}

static inline const char *zz9k_capability_name(uint32_t capability_bit)
{
  switch (capability_bit) {
  case ZZ9K_CAP_MAILBOX:
    return "mailbox";
  case ZZ9K_CAP_IRQ_COMPLETION:
    return "irq-completion";
  case ZZ9K_CAP_SHARED_ALLOC:
    return "shared-alloc";
  case ZZ9K_CAP_SURFACES:
    return "surfaces";
  case ZZ9K_CAP_FRAMEBUFFER_SURFACE:
    return "framebuffer-surface";
  case ZZ9K_CAP_IMAGE_DECODE:
    return "image-decode";
  case ZZ9K_CAP_IMAGE_SCALE:
    return "image-scale";
  case ZZ9K_CAP_AUDIO_DECODE:
    return "audio-decode";
  case ZZ9K_CAP_CRYPTO:
    return "crypto";
  case ZZ9K_CAP_MODULES:
    return "modules";
  case ZZ9K_CAP_MEMORY_OPS:
    return "memory-ops";
  case ZZ9K_CAP_DIAGNOSTICS:
    return "diagnostics";
  case ZZ9K_CAP_DOORBELL:
    return "doorbell";
  case ZZ9K_CAP_POLLING_COMPLETION:
    return "polling-completion";
  case ZZ9K_CAP_SERVICE_DISCOVERY:
    return "service-discovery";
  case ZZ9K_CAP_SURFACE_OPS:
    return "surface-ops";
  case ZZ9K_CAP_COMPRESSION:
    return "compression";
  case ZZ9K_CAP_GFX_OPS:
    return "gfx-ops";
  case ZZ9K_CAP_STORAGE_OPS:
    return "storage-ops";
  case ZZ9K_CAP_AUDIO_PLAYBACK:
    return "audio-playback";
  case ZZ9K_CAP_HOST_WINDOW_HEAP:
    return "host-window-heap";
  case ZZ9K_CAP_VIDEO_DECODE:
    return "video-decode";
  case ZZ9K_CAP_MEDIA_SESSION:
    return "media-session";
  case ZZ9K_CAP_AUDIO_STREAM_DRAIN:
    return "audio-stream-drain";
  case ZZ9K_CAP_APERTURE_LAYOUT:
    return "aperture-layout";
  default:
    return 0;
  }
}

static inline const char *zz9k_service_flag_name(uint32_t service_id,
                                                uint32_t service_flag)
{
  switch (service_flag) {
  case ZZ9K_SERVICE_FLAG_FIRMWARE:
    return "firmware";
  case ZZ9K_SERVICE_FLAG_MODULE:
    return "module";
  case ZZ9K_SERVICE_FLAG_ASYNC:
    return "async";
  case ZZ9K_SERVICE_FLAG_ZERO_COPY:
    return "zero-copy";
  default:
    break;
  }

  if (service_id == ZZ9K_SERVICE_IMAGE) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_IMAGE_JPEG_BASELINE:
      return "jpeg-baseline";
    case ZZ9K_SERVICE_FLAG_IMAGE_JPEG_PROGRESSIVE:
      return "jpeg-progressive";
    case ZZ9K_SERVICE_FLAG_IMAGE_JPEG_DIRECT_BGRA:
      return "jpeg-direct-bgra";
    case ZZ9K_SERVICE_FLAG_IMAGE_JPEG_SCALING:
      return "jpeg-scaling";
    case ZZ9K_SERVICE_FLAG_IMAGE_STREAMING_INPUT:
      return "streaming-input";
    case ZZ9K_SERVICE_FLAG_IMAGE_TILE_OUTPUT:
      return "tile-output";
    case ZZ9K_SERVICE_FLAG_IMAGE_FRAMEBUFFER_OUTPUT:
      return "framebuffer-output";
    case ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BILINEAR:
      return "scale-bilinear";
    case ZZ9K_SERVICE_FLAG_IMAGE_SCALE_CLIPPED:
      return "scale-clipped";
    case ZZ9K_SERVICE_FLAG_IMAGE_PNG_DIRECT_BGRA:
      return "png-direct-bgra";
    case ZZ9K_SERVICE_FLAG_IMAGE_RGB888_OUTPUT:
      return "rgb888-output";
    case ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BGRA_TO_RGB555_RGB565:
      return "scale-bgra-to-rgb555-rgb565";
    default:
      return 0;
    }
  }

  if (service_id == ZZ9K_SERVICE_CODEC) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_RAW:
      return "deflate-raw";
    case ZZ9K_SERVICE_FLAG_CODEC_ZLIB:
      return "zlib";
    case ZZ9K_SERVICE_FLAG_CODEC_GZIP:
      return "gzip";
    case ZZ9K_SERVICE_FLAG_CODEC_LZ4_BLOCK:
      return "lz4-block";
    case ZZ9K_SERVICE_FLAG_CODEC_LZMA_ALONE:
      return "lzma-alone";
    case ZZ9K_SERVICE_FLAG_CODEC_CHECKSUM:
      return "checksum";
    case ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_TEST:
      return "decompress-test";
    case ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_STREAM:
      return "decompress-stream";
    case ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED:
      return "decompress-feed";
    case ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_FEED:
      return "deflate-feed";
    case ZZ9K_SERVICE_FLAG_CODEC_ZLIB_FEED:
      return "zlib-feed";
    case ZZ9K_SERVICE_FLAG_CODEC_GZIP_FEED:
      return "gzip-feed";
    case ZZ9K_SERVICE_FLAG_CODEC_LZMA2:
      return "lzma2";
    case ZZ9K_SERVICE_FLAG_CODEC_LZH:
      return "lzh";
    case ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_BATCH:
      return "decompress-batch";
    default:
      return 0;
    }
  }

  if (service_id == ZZ9K_SERVICE_AUDIO) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_AUDIO_MP3_DECODE:
      return "mp3-decode";
    case ZZ9K_SERVICE_FLAG_AUDIO_PCM_MIX:
      return "pcm-mix";
    case ZZ9K_SERVICE_FLAG_AUDIO_RESAMPLE:
      return "resample";
    case ZZ9K_SERVICE_FLAG_AUDIO_PCM16_STEREO:
      return "pcm16-stereo";
    case ZZ9K_SERVICE_FLAG_AUDIO_MP3_STREAM:
      return "mp3-stream";
    default:
      return 0;
    }
  }

  if (service_id == ZZ9K_SERVICE_CRYPTO) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_CRYPTO_X25519:
      return "x25519";
    default:
      return 0;
    }
  }

  if (service_id == ZZ9K_SERVICE_SURFACE) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_SURFACE_PALETTE_QUERY:
      return "palette-query";
    default:
      break;
    }
    return 0;
  }

  if (service_id == ZZ9K_SERVICE_VIDEO) {
    switch (service_flag) {
    case ZZ9K_SERVICE_FLAG_VIDEO_MPEG1:
      return "mpeg1";
    case ZZ9K_SERVICE_FLAG_VIDEO_MPEG_PS:
      return "mpeg-ps";
    case ZZ9K_SERVICE_FLAG_VIDEO_DIRECT_OVERLAY:
      return "direct-overlay";
    case ZZ9K_SERVICE_FLAG_VIDEO_STREAMING_INPUT:
      return "streaming-input";
    case ZZ9K_SERVICE_FLAG_VIDEO_CORE1:
      return "core1";
    case ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_SESSION:
      return "media-session";
    case ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_MP2:
      return "media-mp2";
    case ZZ9K_SERVICE_FLAG_VIDEO_EXPLICIT_PRESENT:
      return "explicit-present";
    case ZZ9K_SERVICE_FLAG_VIDEO_TIMELINE_90KHZ:
      return "timeline-90khz";
    case ZZ9K_SERVICE_FLAG_VIDEO_PCM_RING_STATUS:
      return "pcm-ring-status";
    case ZZ9K_SERVICE_FLAG_VIDEO_AUDIO_BIND:
      return "audio-bind";
    default:
      return 0;
    }
  }

  return 0;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_CAPS_H */

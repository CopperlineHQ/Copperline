/*
 * Audio descriptor helpers for ZZ9000 SDK v2.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_AUDIO_H
#define ZZ9K_AUDIO_H

#include "zz9k/abi.h"
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline int zz9k_audio_sample_format_known(uint32_t format)
{
  return format == ZZ9K_AUDIO_SAMPLE_FORMAT_S16LE ||
         format == ZZ9K_AUDIO_SAMPLE_FORMAT_S16BE;
}

static inline int zz9k_audio_build_decode_desc(
    ZZ9KAudioDecodeDesc *desc,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t dst_handle,
    uint32_t dst_offset,
    uint32_t dst_capacity,
    uint32_t output_hz,
    uint32_t output_channels,
    uint32_t output_format,
    uint32_t flags)
{
  if (!desc || src_handle == ZZ9K_INVALID_HANDLE || src_length == 0U ||
      dst_handle == ZZ9K_INVALID_HANDLE || dst_capacity == 0U ||
      !zz9k_audio_sample_format_known(output_format) ||
      (output_channels != 0U && output_channels != 1U &&
       output_channels != 2U) ||
      (flags & ~ZZ9K_AUDIO_DECODE_FLAG_EXPECT_END) != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->src_handle = src_handle;
  desc->src_offset = src_offset;
  desc->src_length = src_length;
  desc->dst_handle = dst_handle;
  desc->dst_offset = dst_offset;
  desc->dst_capacity = dst_capacity;
  desc->output_hz = output_hz;
  desc->output_channels = output_channels;
  desc->output_format = output_format;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_audio_build_stream_begin_desc(
    ZZ9KAudioStreamBeginDesc *desc,
    uint32_t mp3_ring_handle,
    uint32_t mp3_ring_capacity,
    uint32_t pcm_ring_handle,
    uint32_t pcm_ring_capacity,
    uint32_t output_hz,
    uint32_t output_channels,
    uint32_t output_format,
    uint32_t low_water_bytes,
    uint32_t high_water_bytes,
    uint32_t flags)
{
  if (!desc || mp3_ring_handle == ZZ9K_INVALID_HANDLE ||
      mp3_ring_capacity == 0U || pcm_ring_handle == ZZ9K_INVALID_HANDLE ||
      pcm_ring_capacity == 0U ||
      !zz9k_audio_sample_format_known(output_format) ||
      (output_channels != 0U && output_channels != 1U &&
       output_channels != 2U) ||
      /* Both water marks are PCM-ring thresholds: low_water is the
       * playback pump's refill trigger, high_water caps decode output
       * per pass. Neither relates to the compressed input ring. */
      low_water_bytes >= pcm_ring_capacity ||
      high_water_bytes >= pcm_ring_capacity ||
      flags != 0U) {
    return 0;
  }

  memset(desc, 0, sizeof(*desc));
  desc->mp3_ring_handle = mp3_ring_handle;
  desc->mp3_ring_capacity = mp3_ring_capacity;
  desc->pcm_ring_handle = pcm_ring_handle;
  desc->pcm_ring_capacity = pcm_ring_capacity;
  desc->output_hz = output_hz;
  desc->output_channels = output_channels;
  desc->output_format = output_format;
  desc->low_water_bytes = low_water_bytes;
  desc->high_water_bytes = high_water_bytes;
  desc->flags = flags;
  return 1;
}

static inline int zz9k_audio_build_stream_feed_desc(
    ZZ9KAudioStreamFeedDesc *desc,
    uint32_t session,
    uint32_t src_handle,
    uint32_t src_offset,
    uint32_t src_length,
    uint32_t flags)
{
  const uint32_t terminal_flags =
      ZZ9K_AUDIO_STREAM_FEED_EOF | ZZ9K_AUDIO_STREAM_FEED_DRAIN;

  if (!desc || session == 0U || src_handle == ZZ9K_INVALID_HANDLE ||
      (src_length == 0U && (flags & terminal_flags) == 0U) ||
      (flags & ~terminal_flags) != 0U ||
      (flags & terminal_flags) == terminal_flags ||
      ((flags & ZZ9K_AUDIO_STREAM_FEED_DRAIN) != 0U && src_length != 0U)) {
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

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_AUDIO_H */

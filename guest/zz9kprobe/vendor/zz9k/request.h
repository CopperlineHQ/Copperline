/*
 * Header-only typed request builders for SDK mailbox calls.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_REQUEST_H
#define ZZ9K_REQUEST_H

#include "zz9k/host.h"
#include "zz9k/audio.h"
#include "zz9k/compression.h"
#include "zz9k/crypto.h"
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline void zz9k_request_init(ZZ9KRequest *request, uint16_t opcode)
{
  memset(request, 0, sizeof(*request));
  request->entry.opcode = opcode;
  request->entry.flags = ZZ9K_ENTRY_INLINE_PAYLOAD;
}

static inline int zz9k_request_ping(ZZ9KRequest *request,
                                    const uint8_t *payload,
                                    uint32_t payload_len)
{
  if (!request || payload_len > sizeof(request->entry.payload.inline_data) ||
      (payload_len != 0 && !payload)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_PING);
  request->entry.payload_len = (uint16_t)payload_len;
  if (payload_len != 0) {
    memcpy(request->entry.payload.inline_data, payload, payload_len);
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_query_service(ZZ9KRequest *request,
                                             uint32_t service_id)
{
  ZZ9KQueryServicePayload *payload;

  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_QUERY_SERVICE);
  request->entry.payload_len = sizeof(ZZ9KQueryServicePayload);
  payload = (ZZ9KQueryServicePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->service_id, service_id);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_query_caps(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_QUERY_CAPS);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_query_aperture_layout(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_QUERY_APERTURE_LAYOUT);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_alloc_shared(ZZ9KRequest *request,
                                            uint32_t length,
                                            uint32_t alignment,
                                            uint32_t flags)
{
  ZZ9KAllocSharedPayload *payload;

  if (!request || length == 0) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_ALLOC_SHARED);
  request->entry.payload_len = sizeof(ZZ9KAllocSharedPayload);
  payload = (ZZ9KAllocSharedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->length, length);
  zz9k_put_be32(payload->alignment, alignment);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_free_shared(ZZ9KRequest *request,
                                           uint32_t handle)
{
  ZZ9KFreeSharedPayload *payload;

  if (!request || handle == ZZ9K_INVALID_HANDLE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_FREE_SHARED);
  request->entry.payload_len = sizeof(ZZ9KFreeSharedPayload);
  payload = (ZZ9KFreeSharedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->handle, handle);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_mem_fill(ZZ9KRequest *request,
                                        uint32_t handle, uint32_t offset,
                                        uint32_t length, uint8_t value)
{
  ZZ9KMemFillPayload *payload;

  if (!request || handle == ZZ9K_INVALID_HANDLE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_MEM_FILL);
  request->entry.payload_len = sizeof(ZZ9KMemFillPayload);
  payload = (ZZ9KMemFillPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->handle, handle);
  zz9k_put_be32(payload->offset, offset);
  zz9k_put_be32(payload->length, length);
  payload->value = value;
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_mem_copy(ZZ9KRequest *request,
                                        uint32_t dst_handle,
                                        uint32_t dst_offset,
                                        uint32_t src_handle,
                                        uint32_t src_offset,
                                        uint32_t length)
{
  ZZ9KMemCopyPayload *payload;

  if (!request || dst_handle == ZZ9K_INVALID_HANDLE ||
      src_handle == ZZ9K_INVALID_HANDLE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_MEM_COPY);
  request->entry.payload_len = sizeof(ZZ9KMemCopyPayload);
  payload = (ZZ9KMemCopyPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->dst_handle, dst_handle);
  zz9k_put_be32(payload->dst_offset, dst_offset);
  zz9k_put_be32(payload->src_handle, src_handle);
  zz9k_put_be32(payload->src_offset, src_offset);
  zz9k_put_be32(payload->length, length);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_alloc_surface_ex(ZZ9KRequest *request,
                                                uint32_t width,
                                                uint32_t height,
                                                uint32_t format,
                                                uint32_t flags,
                                                uint32_t pitch)
{
  ZZ9KAllocSurfacePayload *payload;

  if (!request || width == 0 || height == 0) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_ALLOC_SURFACE);
  request->entry.payload_len = sizeof(ZZ9KAllocSurfacePayload);
  payload = (ZZ9KAllocSurfacePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->width, width);
  zz9k_put_be32(payload->height, height);
  zz9k_put_be32(payload->format, format);
  zz9k_put_be32(payload->flags, flags);
  zz9k_put_be32(payload->pitch, pitch);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_alloc_surface(ZZ9KRequest *request,
                                             uint32_t width,
                                             uint32_t height,
                                             uint32_t format,
                                             uint32_t flags)
{
  return zz9k_request_alloc_surface_ex(request, width, height, format,
                                       flags, 0);
}

static inline int zz9k_request_free_surface(ZZ9KRequest *request,
                                            uint32_t handle)
{
  ZZ9KFreeSurfacePayload *payload;

  if (!request || handle == ZZ9K_INVALID_HANDLE ||
      handle == ZZ9K_SURFACE_HANDLE_FRAMEBUFFER) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_FREE_SURFACE);
  request->entry.payload_len = sizeof(ZZ9KFreeSurfacePayload);
  payload = (ZZ9KFreeSurfacePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->handle, handle);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_map_framebuffer_surface(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_MAP_FRAMEBUFFER_SURFACE);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_scale_image(ZZ9KRequest *request,
                                           const ZZ9KScaleImageDesc *desc)
{
  ZZ9KScaleImagePayload *payload;

  if (!request || !desc || desc->src_w == 0 || desc->src_h == 0 ||
      desc->dst_w == 0 || desc->dst_h == 0) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_SCALE_IMAGE);
  request->entry.payload_len = sizeof(ZZ9KScaleImagePayload);
  payload = (ZZ9KScaleImagePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_surface, desc->src_surface);
  zz9k_put_be32(payload->dst_surface, desc->dst_surface);
  zz9k_put_be32(payload->src_x, desc->src_x);
  zz9k_put_be32(payload->src_y, desc->src_y);
  zz9k_put_be32(payload->src_w, desc->src_w);
  zz9k_put_be32(payload->src_h, desc->src_h);
  zz9k_put_be32(payload->dst_x, desc->dst_x);
  zz9k_put_be32(payload->dst_y, desc->dst_y);
  zz9k_put_be32(payload->dst_w, desc->dst_w);
  zz9k_put_be32(payload->dst_h, desc->dst_h);
  zz9k_put_be32(payload->filter, desc->filter);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_coord_fits_u16(uint32_t value)
{
  return value <= 0xffffU;
}

static inline int zz9k_request_scale_image_clipped(
    ZZ9KRequest *request, const ZZ9KScaleImageClippedDesc *desc)
{
  ZZ9KScaleImageClippedPayload *payload;

  if (!request || !desc || desc->src_w == 0U || desc->src_h == 0U ||
      desc->dst_w == 0U || desc->dst_h == 0U ||
      desc->clip_w == 0U || desc->clip_h == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (!zz9k_coord_fits_u16(desc->src_x) ||
      !zz9k_coord_fits_u16(desc->src_y) ||
      !zz9k_coord_fits_u16(desc->src_w) ||
      !zz9k_coord_fits_u16(desc->src_h) ||
      !zz9k_coord_fits_u16(desc->dst_x) ||
      !zz9k_coord_fits_u16(desc->dst_y) ||
      !zz9k_coord_fits_u16(desc->dst_w) ||
      !zz9k_coord_fits_u16(desc->dst_h) ||
      !zz9k_coord_fits_u16(desc->clip_x) ||
      !zz9k_coord_fits_u16(desc->clip_y) ||
      !zz9k_coord_fits_u16(desc->clip_w) ||
      !zz9k_coord_fits_u16(desc->clip_h)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_SCALE_IMAGE_CLIPPED);
  request->entry.payload_len = sizeof(ZZ9KScaleImageClippedPayload);
  payload =
      (ZZ9KScaleImageClippedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_surface, desc->src_surface);
  zz9k_put_be32(payload->dst_surface, desc->dst_surface);
  zz9k_put_be16(payload->src_x, (uint16_t)desc->src_x);
  zz9k_put_be16(payload->src_y, (uint16_t)desc->src_y);
  zz9k_put_be16(payload->src_w, (uint16_t)desc->src_w);
  zz9k_put_be16(payload->src_h, (uint16_t)desc->src_h);
  zz9k_put_be16(payload->dst_x, (uint16_t)desc->dst_x);
  zz9k_put_be16(payload->dst_y, (uint16_t)desc->dst_y);
  zz9k_put_be16(payload->dst_w, (uint16_t)desc->dst_w);
  zz9k_put_be16(payload->dst_h, (uint16_t)desc->dst_h);
  zz9k_put_be16(payload->clip_x, (uint16_t)desc->clip_x);
  zz9k_put_be16(payload->clip_y, (uint16_t)desc->clip_y);
  zz9k_put_be16(payload->clip_w, (uint16_t)desc->clip_w);
  zz9k_put_be16(payload->clip_h, (uint16_t)desc->clip_h);
  zz9k_put_be32(payload->filter, desc->filter);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_fill_surface(ZZ9KRequest *request,
                                            const ZZ9KSurfaceFillDesc *desc)
{
  ZZ9KSurfaceFillPayload *payload;

  if (!request || !desc || desc->surface == ZZ9K_INVALID_HANDLE ||
      desc->width == 0U || desc->height == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_FILL_SURFACE);
  request->entry.payload_len = sizeof(ZZ9KSurfaceFillPayload);
  payload = (ZZ9KSurfaceFillPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->surface, desc->surface);
  zz9k_put_be32(payload->x, desc->x);
  zz9k_put_be32(payload->y, desc->y);
  zz9k_put_be32(payload->width, desc->width);
  zz9k_put_be32(payload->height, desc->height);
  zz9k_put_be32(payload->color, desc->color);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_query_palette(
    ZZ9KRequest *request, const ZZ9KPaletteQueryDesc *desc)
{
  ZZ9KQueryPalettePayload *payload;

  if (!request || !desc || desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->count == 0U ||
      desc->count > (uint32_t)ZZ9K_PALETTE_MAX_ENTRIES ||
      desc->start > (uint32_t)ZZ9K_PALETTE_MAX_ENTRIES ||
      desc->start + desc->count > (uint32_t)ZZ9K_PALETTE_MAX_ENTRIES ||
      (desc->flags & ~(uint32_t)ZZ9K_PALETTE_QUERY_FLAG_SECONDARY) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_QUERY_PALETTE);
  request->entry.payload_len = sizeof(ZZ9KQueryPalettePayload);
  payload = (ZZ9KQueryPalettePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->surface, desc->surface);
  zz9k_put_be32(payload->start, desc->start);
  zz9k_put_be32(payload->count, desc->count);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_copy_surface(ZZ9KRequest *request,
                                            const ZZ9KSurfaceCopyDesc *desc)
{
  ZZ9KSurfaceCopyPayload *payload;

  if (!request || !desc || desc->src_surface == ZZ9K_INVALID_HANDLE ||
      desc->dst_surface == ZZ9K_INVALID_HANDLE || desc->width == 0U ||
      desc->height == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_COPY_SURFACE);
  request->entry.payload_len = sizeof(ZZ9KSurfaceCopyPayload);
  payload = (ZZ9KSurfaceCopyPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_surface, desc->src_surface);
  zz9k_put_be32(payload->dst_surface, desc->dst_surface);
  zz9k_put_be32(payload->src_x, desc->src_x);
  zz9k_put_be32(payload->src_y, desc->src_y);
  zz9k_put_be32(payload->dst_x, desc->dst_x);
  zz9k_put_be32(payload->dst_y, desc->dst_y);
  zz9k_put_be32(payload->width, desc->width);
  zz9k_put_be32(payload->height, desc->height);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decode_image(ZZ9KRequest *request,
                                            uint16_t opcode,
                                            const ZZ9KImageDecodeDesc *desc)
{
  ZZ9KImageDecodePayload *payload;

  if (!request || !desc || desc->src_handle == ZZ9K_INVALID_HANDLE ||
      desc->src_length == 0U || desc->dst_surface == ZZ9K_INVALID_HANDLE ||
      desc->dst_width == 0U || desc->dst_height == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (opcode != ZZ9K_OP_DECODE_JPEG && opcode != ZZ9K_OP_DECODE_PNG &&
      opcode != ZZ9K_OP_DECODE_GIF) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, opcode);
  request->entry.payload_len = sizeof(ZZ9KImageDecodePayload);
  payload = (ZZ9KImageDecodePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_surface, desc->dst_surface);
  zz9k_put_be32(payload->dst_x, desc->dst_x);
  zz9k_put_be32(payload->dst_y, desc->dst_y);
  zz9k_put_be32(payload->dst_width, desc->dst_width);
  zz9k_put_be32(payload->dst_height, desc->dst_height);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decode_jpeg(ZZ9KRequest *request,
                                           const ZZ9KImageDecodeDesc *desc)
{
  return zz9k_request_decode_image(request, ZZ9K_OP_DECODE_JPEG, desc);
}

static inline int zz9k_request_decode_png(ZZ9KRequest *request,
                                          const ZZ9KImageDecodeDesc *desc)
{
  return zz9k_request_decode_image(request, ZZ9K_OP_DECODE_PNG, desc);
}

static inline int zz9k_request_decode_gif(ZZ9KRequest *request,
                                          const ZZ9KImageDecodeDesc *desc)
{
  return zz9k_request_decode_image(request, ZZ9K_OP_DECODE_GIF, desc);
}

static inline int zz9k_image_codec_is_known(uint32_t codec)
{
  return codec == ZZ9K_IMAGE_CODEC_JPEG ||
         codec == ZZ9K_IMAGE_CODEC_PNG ||
         codec == ZZ9K_IMAGE_CODEC_GIF;
}

static inline int zz9k_request_image_session_begin(
    ZZ9KRequest *request, const ZZ9KImageSessionBeginDesc *desc)
{
  ZZ9KImageSessionBeginPayload *payload;

  if (!request || !desc || !zz9k_image_codec_is_known(desc->codec) ||
      desc->output_format == ZZ9K_SURFACE_FORMAT_UNKNOWN) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  if (desc->output_mode == ZZ9K_IMAGE_OUTPUT_SURFACE) {
    if (desc->dst_surface == ZZ9K_INVALID_HANDLE ||
        desc->dst_width == 0U || desc->dst_height == 0U) {
      return ZZ9K_STATUS_BAD_REQUEST;
    }
  } else if (desc->output_mode == ZZ9K_IMAGE_OUTPUT_FRAMEBUFFER) {
    if (desc->dst_surface != ZZ9K_SURFACE_HANDLE_FRAMEBUFFER ||
        desc->dst_width == 0U || desc->dst_height == 0U) {
      return ZZ9K_STATUS_BAD_REQUEST;
    }
  } else if (desc->output_mode == ZZ9K_IMAGE_OUTPUT_TILE_BUFFER) {
    if (desc->tile_handle == ZZ9K_INVALID_HANDLE ||
        desc->tile_stride == 0U || desc->tile_rows == 0U) {
      return ZZ9K_STATUS_BAD_REQUEST;
    }
  } else {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_IMAGE_SESSION_BEGIN);
  request->entry.payload_len = sizeof(ZZ9KImageSessionBeginPayload);
  payload = (ZZ9KImageSessionBeginPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->codec, desc->codec);
  zz9k_put_be32(payload->output_mode, desc->output_mode);
  zz9k_put_be32(payload->dst_surface, desc->dst_surface);
  zz9k_put_be32(payload->dst_x, desc->dst_x);
  zz9k_put_be32(payload->dst_y, desc->dst_y);
  zz9k_put_be32(payload->dst_width, desc->dst_width);
  zz9k_put_be32(payload->dst_height, desc->dst_height);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->tile_handle, desc->tile_handle);
  zz9k_put_be32(payload->tile_stride, desc->tile_stride);
  zz9k_put_be32(payload->tile_rows, desc->tile_rows);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_image_session_feed(
    ZZ9KRequest *request, const ZZ9KImageSessionFeedDesc *desc)
{
  ZZ9KImageSessionFeedPayload *payload;

  if (!request || !desc || desc->session == 0U ||
      desc->src_handle == ZZ9K_INVALID_HANDLE ||
      (desc->flags & ~ZZ9K_IMAGE_SESSION_FEED_EOF) != 0U ||
      (desc->src_length == 0U &&
       (desc->flags & ZZ9K_IMAGE_SESSION_FEED_EOF) == 0U)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_IMAGE_SESSION_FEED);
  request->entry.payload_len = sizeof(ZZ9KImageSessionFeedPayload);
  payload = (ZZ9KImageSessionFeedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_image_session_close(ZZ9KRequest *request,
                                                   uint32_t session,
                                                   uint32_t flags)
{
  ZZ9KImageSessionClosePayload *payload;

  if (!request || session == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_IMAGE_SESSION_CLOSE);
  request->entry.payload_len = sizeof(ZZ9KImageSessionClosePayload);
  payload = (ZZ9KImageSessionClosePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decode_mp3(ZZ9KRequest *request,
                                          const ZZ9KAudioDecodeDesc *desc)
{
  ZZ9KAudioDecodePayload *payload;

  if (!request || !desc || desc->src_handle == ZZ9K_INVALID_HANDLE ||
      desc->src_length == 0U || desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->dst_capacity == 0U ||
      !zz9k_audio_sample_format_known(desc->output_format) ||
      (desc->output_channels != 0U && desc->output_channels != 1U &&
       desc->output_channels != 2U) ||
      (desc->flags & ~ZZ9K_AUDIO_DECODE_FLAG_EXPECT_END) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECODE_MP3);
  request->entry.payload_len = sizeof(ZZ9KAudioDecodePayload);
  payload = (ZZ9KAudioDecodePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->dst_capacity, desc->dst_capacity);
  zz9k_put_be32(payload->output_hz, desc->output_hz);
  zz9k_put_be32(payload->output_channels, desc->output_channels);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_begin(
    ZZ9KRequest *request,
    const ZZ9KAudioStreamBeginDesc *desc)
{
  ZZ9KAudioStreamBeginPayload *payload;

  if (!request || !desc ||
      desc->mp3_ring_handle == ZZ9K_INVALID_HANDLE ||
      desc->mp3_ring_capacity == 0U ||
      desc->pcm_ring_handle == ZZ9K_INVALID_HANDLE ||
      desc->pcm_ring_capacity == 0U ||
      !zz9k_audio_sample_format_known(desc->output_format) ||
      (desc->output_channels != 0U && desc->output_channels != 1U &&
       desc->output_channels != 2U) ||
      /* Both water marks are PCM-ring thresholds: low_water is the
       * playback pump's refill trigger, high_water caps decode output
       * per pass. Neither relates to the compressed input ring. */
      desc->low_water_bytes >= desc->pcm_ring_capacity ||
      desc->high_water_bytes >= desc->pcm_ring_capacity ||
      desc->flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_BEGIN);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamBeginPayload);
  payload =
      (ZZ9KAudioStreamBeginPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->mp3_ring_handle, desc->mp3_ring_handle);
  zz9k_put_be32(payload->mp3_ring_capacity, desc->mp3_ring_capacity);
  zz9k_put_be32(payload->pcm_ring_handle, desc->pcm_ring_handle);
  zz9k_put_be32(payload->pcm_ring_capacity, desc->pcm_ring_capacity);
  zz9k_put_be32(payload->output_hz, desc->output_hz);
  zz9k_put_be32(payload->output_channels, desc->output_channels);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->low_water_bytes, desc->low_water_bytes);
  zz9k_put_be32(payload->high_water_bytes, desc->high_water_bytes);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_feed(
    ZZ9KRequest *request,
    const ZZ9KAudioStreamFeedDesc *desc)
{
  ZZ9KAudioStreamFeedPayload *payload;
  ZZ9KAudioStreamFeedDesc validated;

  if (!request || !desc ||
      !zz9k_audio_build_stream_feed_desc(
          &validated, desc->session, desc->src_handle, desc->src_offset,
          desc->src_length, desc->flags)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_FEED);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamFeedPayload);
  payload =
      (ZZ9KAudioStreamFeedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_read(ZZ9KRequest *request,
                                                 uint32_t session,
                                                 uint32_t pcm_read,
                                                 uint32_t flags)
{
  ZZ9KAudioStreamReadPayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_READ);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamReadPayload);
  payload =
      (ZZ9KAudioStreamReadPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->pcm_read, pcm_read);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_close(ZZ9KRequest *request,
                                                  uint32_t session,
                                                  uint32_t flags)
{
  ZZ9KAudioStreamClosePayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_CLOSE);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamClosePayload);
  payload =
      (ZZ9KAudioStreamClosePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_play(ZZ9KRequest *request,
                                                 uint32_t session,
                                                 uint32_t flags)
{
  ZZ9KAudioStreamPlayPayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_PLAY);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamPlayPayload);
  payload =
      (ZZ9KAudioStreamPlayPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_audio_stream_stop(ZZ9KRequest *request,
                                                 uint32_t session,
                                                 uint32_t flags)
{
  ZZ9KAudioStreamStopPayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_AUDIO_STREAM_STOP);
  request->entry.payload_len = sizeof(ZZ9KAudioStreamStopPayload);
  payload =
      (ZZ9KAudioStreamStopPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_video_session_begin(
    ZZ9KRequest *request,
    const ZZ9KVideoSessionBeginDesc *desc)
{
  ZZ9KVideoSessionBeginPayload *payload;

  if (!request || !desc || desc->codec == 0U ||
      desc->container == 0U || desc->output_format == 0U ||
      desc->width == 0U || desc->height == 0U || desc->flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_VIDEO_SESSION_BEGIN);
  request->entry.payload_len = sizeof(ZZ9KVideoSessionBeginPayload);
  payload =
      (ZZ9KVideoSessionBeginPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->codec, desc->codec);
  zz9k_put_be32(payload->container, desc->container);
  zz9k_put_be32(payload->width, desc->width);
  zz9k_put_be32(payload->height, desc->height);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_video_session_write(
    ZZ9KRequest *request,
    const ZZ9KVideoSessionWriteDesc *desc)
{
  ZZ9KVideoSessionWritePayload *payload;

  if (!request || !desc || desc->session == 0U ||
      desc->src_handle == ZZ9K_INVALID_HANDLE ||
      (desc->src_length == 0U &&
       (desc->flags & ZZ9K_VIDEO_SESSION_WRITE_EOF) == 0U) ||
      (desc->flags & ~ZZ9K_VIDEO_SESSION_WRITE_EOF) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_VIDEO_SESSION_WRITE);
  request->entry.payload_len = sizeof(ZZ9KVideoSessionWritePayload);
  payload =
      (ZZ9KVideoSessionWritePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_video_session_decode(
    ZZ9KRequest *request,
    const ZZ9KVideoSessionDecodeDesc *desc)
{
  ZZ9KVideoSessionDecodePayload *payload;

  if (!request || !desc || desc->session == 0U || desc->flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  zz9k_request_init(request, ZZ9K_OP_VIDEO_SESSION_DECODE);
  request->entry.payload_len = sizeof(ZZ9KVideoSessionDecodePayload);
  payload =
      (ZZ9KVideoSessionDecodePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_video_session_close(ZZ9KRequest *request,
                                                    uint32_t session,
                                                    uint32_t flags)
{
  ZZ9KVideoSessionClosePayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  zz9k_request_init(request, ZZ9K_OP_VIDEO_SESSION_CLOSE);
  request->entry.payload_len = sizeof(ZZ9KVideoSessionClosePayload);
  payload =
      (ZZ9KVideoSessionClosePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_media_session_begin(
    ZZ9KRequest *request,
    const ZZ9KMediaSessionBeginDesc *desc)
{
  ZZ9KMediaSessionBeginPayload *payload;

  if (!request || !desc || desc->video_codec == 0U ||
      desc->container == 0U || desc->output_format == 0U ||
      desc->width == 0U || desc->height == 0U || desc->flags != 0U ||
      desc->audio_codec > ZZ9K_MEDIA_AUDIO_MP2 ||
      (desc->audio_codec == ZZ9K_MEDIA_AUDIO_NONE &&
       (desc->pcm_ring_handle != 0U ||
        desc->pcm_ring_capacity != 0U ||
        desc->pcm_low_water_bytes != 0U ||
        desc->pcm_high_water_bytes != 0U)) ||
      (desc->audio_codec == ZZ9K_MEDIA_AUDIO_MP2 &&
       (desc->pcm_ring_handle == 0U ||
        desc->pcm_ring_handle == ZZ9K_INVALID_HANDLE ||
        desc->pcm_ring_capacity == 0U ||
        desc->pcm_low_water_bytes >= desc->pcm_high_water_bytes ||
        desc->pcm_high_water_bytes > desc->pcm_ring_capacity))) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_MEDIA_SESSION_BEGIN);
  request->entry.payload_len = sizeof(*payload);
  payload = (ZZ9KMediaSessionBeginPayload *)
      request->entry.payload.inline_data;
  zz9k_put_be32(payload->video_codec, desc->video_codec);
  zz9k_put_be32(payload->container, desc->container);
  zz9k_put_be32(payload->width, desc->width);
  zz9k_put_be32(payload->height, desc->height);
  zz9k_put_be32(payload->output_format, desc->output_format);
  zz9k_put_be32(payload->audio_codec, desc->audio_codec);
  zz9k_put_be32(payload->pcm_ring_handle, desc->pcm_ring_handle);
  zz9k_put_be32(payload->pcm_ring_capacity, desc->pcm_ring_capacity);
  zz9k_put_be32(payload->pcm_low_water_bytes,
                desc->pcm_low_water_bytes);
  zz9k_put_be32(payload->pcm_high_water_bytes,
                desc->pcm_high_water_bytes);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_media_session_write(
    ZZ9KRequest *request,
    const ZZ9KMediaSessionWriteDesc *desc)
{
  ZZ9KMediaSessionWritePayload *payload;

  if (!request || !desc || desc->session == 0U ||
      desc->src_handle == ZZ9K_INVALID_HANDLE ||
      (desc->src_length == 0U &&
       (desc->flags & ZZ9K_MEDIA_SESSION_WRITE_EOF) == 0U) ||
      (desc->flags & ~ZZ9K_MEDIA_SESSION_WRITE_EOF) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_MEDIA_SESSION_WRITE);
  request->entry.payload_len = sizeof(*payload);
  payload = (ZZ9KMediaSessionWritePayload *)
      request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_media_command_opcode_known(uint16_t opcode)
{
  return opcode == ZZ9K_OP_MEDIA_SESSION_DECODE ||
         opcode == ZZ9K_OP_MEDIA_SESSION_AUDIO_READ ||
         opcode == ZZ9K_OP_MEDIA_SESSION_PRESENT ||
         opcode == ZZ9K_OP_MEDIA_SESSION_DISCARD ||
         opcode == ZZ9K_OP_MEDIA_SESSION_AUDIO_BIND ||
         opcode == ZZ9K_OP_MEDIA_SESSION_AUDIO_UNBIND ||
         opcode == ZZ9K_OP_MEDIA_SESSION_CLOSE;
}

static inline int zz9k_request_media_session_command_value(
    ZZ9KRequest *request, uint16_t opcode, uint32_t session,
    uint64_t value, uint32_t flags)
{
  ZZ9KMediaSessionCommandPayload *payload;
  uint32_t allowed_flags =
      opcode == ZZ9K_OP_MEDIA_SESSION_AUDIO_BIND
          ? ZZ9K_MEDIA_AUDIO_BIND_PAUSE
          : 0U;

  if (!request || !zz9k_media_command_opcode_known(opcode) ||
      session == 0U || (flags & ~allowed_flags) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  zz9k_request_init(request, opcode);
  request->entry.payload_len = sizeof(*payload);
  payload = (ZZ9KMediaSessionCommandPayload *)
      request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_media_u64_to_be(payload->value_hi, payload->value_lo, value);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_media_session_command(
    ZZ9KRequest *request, uint16_t opcode, uint32_t session,
    uint32_t flags)
{
  return zz9k_request_media_session_command_value(
      request, opcode, session, 0U, flags);
}

static inline int zz9k_request_media_session_status(
    ZZ9KRequest *request, uint32_t session, uint32_t page, uint32_t flags)
{
  ZZ9KMediaSessionStatusPayload *payload;

  if (!request || session == 0U ||
      page > ZZ9K_MEDIA_STATUS_PROFILE ||
      flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  zz9k_request_init(request, ZZ9K_OP_MEDIA_SESSION_STATUS);
  request->entry.payload_len = sizeof(*payload);
  payload = (ZZ9KMediaSessionStatusPayload *)
      request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->page, page);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_crypto_hash(ZZ9KRequest *request,
                                           const ZZ9KCryptoHashDesc *desc)
{
  ZZ9KCryptoHashPayload *payload;

  if (!request || !desc || desc->src_handle == ZZ9K_INVALID_HANDLE ||
      desc->src_length == 0U || desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->algorithm == ZZ9K_CRYPTO_HASH_NONE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if ((desc->flags & ZZ9K_CRYPTO_HASH_FLAG_HMAC) != 0U &&
      (desc->key_handle == ZZ9K_INVALID_HANDLE ||
       desc->key_length == 0U)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (desc->algorithm == ZZ9K_CRYPTO_HASH_POLY1305 &&
      (desc->flags != 0U ||
       desc->key_handle == ZZ9K_INVALID_HANDLE ||
       desc->key_length != 32U)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_CRYPTO_HASH);
  request->entry.payload_len = sizeof(ZZ9KCryptoHashPayload);
  payload = (ZZ9KCryptoHashPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->key_handle, desc->key_handle);
  zz9k_put_be32(payload->key_offset, desc->key_offset);
  zz9k_put_be32(payload->key_length, desc->key_length);
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_crypto_stream(ZZ9KRequest *request,
                                             const ZZ9KCryptoStreamDesc *desc)
{
  ZZ9KCryptoStreamPayload *payload;

  if (!request || !desc || desc->src_handle == ZZ9K_INVALID_HANDLE ||
      desc->src_length == 0U || desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->key_handle == ZZ9K_INVALID_HANDLE ||
      desc->nonce_handle == ZZ9K_INVALID_HANDLE ||
      desc->algorithm == ZZ9K_CRYPTO_STREAM_NONE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_CRYPTO_STREAM);
  request->entry.payload_len = sizeof(ZZ9KCryptoStreamPayload);
  payload = (ZZ9KCryptoStreamPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->key_handle, desc->key_handle);
  zz9k_put_be32(payload->key_offset, desc->key_offset);
  zz9k_put_be32(payload->nonce_handle, desc->nonce_handle);
  zz9k_put_be32(payload->nonce_offset, desc->nonce_offset);
  zz9k_put_be32(payload->counter, desc->counter);
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_crypto_aead(ZZ9KRequest *request,
                                           const ZZ9KCryptoAeadDesc *desc)
{
  ZZ9KCryptoAeadPayload *payload;

  if (!request || !desc || desc->src_handle == ZZ9K_INVALID_HANDLE ||
      desc->src_length == 0U || desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->key_handle == ZZ9K_INVALID_HANDLE ||
      desc->nonce_handle == ZZ9K_INVALID_HANDLE ||
      (desc->flags & ~(uint32_t)(ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT |
                                 ZZ9K_CRYPTO_AEAD_ALG_MASK)) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (desc->aad_length != 0U && desc->aad_handle == ZZ9K_INVALID_HANDLE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_CRYPTO_AEAD);
  request->entry.payload_len = sizeof(ZZ9KCryptoAeadPayload);
  payload = (ZZ9KCryptoAeadPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->aad_handle, desc->aad_handle);
  zz9k_put_be32(payload->aad_offset, desc->aad_offset);
  zz9k_put_be32(payload->aad_length, desc->aad_length);
  zz9k_put_be32(payload->key_handle, desc->key_handle);
  zz9k_put_be32(payload->key_offset, desc->key_offset);
  zz9k_put_be32(payload->nonce_handle, desc->nonce_handle);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_crypto_kx(ZZ9KRequest *request,
                                          const ZZ9KCryptoKxDesc *desc)
{
  struct ZZ9KCryptoKxPayload *payload;
  int is_keygen;

  if (!request || !desc) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  /* P-256 keygen (scalar*G) has no peer point: its descriptor carries an
   * invalid point_handle by design (zz9k_crypto_build_p256_keygen_desc), and
   * the firmware's handle_crypto_kx KEYGEN branch likewise validates only the
   * scalar and destination. Every other KX op (X25519, P-256 derive) still
   * needs a peer point, so the point_handle stays required for them. */
  is_keygen = (desc->algorithm == ZZ9K_CRYPTO_KX_P256) &&
              ((desc->flags & ZZ9K_CRYPTO_KX_FLAG_KEYGEN) != 0U);
  if (desc->scalar_handle == ZZ9K_INVALID_HANDLE ||
      desc->dst_handle    == ZZ9K_INVALID_HANDLE ||
      (!is_keygen && desc->point_handle == ZZ9K_INVALID_HANDLE)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_CRYPTO_KX);
  request->entry.payload_len = sizeof(struct ZZ9KCryptoKxPayload);
  payload = (struct ZZ9KCryptoKxPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->scalar_handle, desc->scalar_handle);
  zz9k_put_be32(payload->scalar_offset, desc->scalar_offset);
  zz9k_put_be32(payload->point_handle,  desc->point_handle);
  zz9k_put_be32(payload->point_offset,  desc->point_offset);
  zz9k_put_be32(payload->dst_handle,    desc->dst_handle);
  zz9k_put_be32(payload->dst_offset,    desc->dst_offset);
  zz9k_put_be32(payload->algorithm,     desc->algorithm);
  zz9k_put_be32(payload->flags,         desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_crypto_verify(ZZ9KRequest *request,
                                              const ZZ9KCryptoVerifyDesc *desc)
{
  struct ZZ9KCryptoVerifyPayload *payload;

  if (!request || !desc ||
      desc->hash_handle == ZZ9K_INVALID_HANDLE ||
      desc->sig_handle == ZZ9K_INVALID_HANDLE ||
      desc->key_handle == ZZ9K_INVALID_HANDLE ||
      (desc->algorithm != ZZ9K_CRYPTO_VERIFY_ECDSA_P256_SHA256 &&
       desc->algorithm != ZZ9K_CRYPTO_VERIFY_RSA_PKCS1_2048_SHA256)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_CRYPTO_VERIFY);
  request->entry.payload_len = sizeof(struct ZZ9KCryptoVerifyPayload);
  payload = (struct ZZ9KCryptoVerifyPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->hash_handle, desc->hash_handle);
  zz9k_put_be32(payload->hash_offset, desc->hash_offset);
  zz9k_put_be32(payload->hash_length, desc->hash_length);
  zz9k_put_be32(payload->sig_handle, desc->sig_handle);
  zz9k_put_be32(payload->sig_offset, desc->sig_offset);
  zz9k_put_be32(payload->sig_length, desc->sig_length);
  zz9k_put_be32(payload->key_handle, desc->key_handle);
  zz9k_put_be32(payload->key_offset, desc->key_offset);
  zz9k_put_be32(payload->key_length, desc->key_length);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress(ZZ9KRequest *request,
                                          const ZZ9KDecompressDesc *desc)
{
  ZZ9KDecompressPayload *payload;

  if (!request || !desc ||
      !zz9k_compression_algorithm_known(desc->algorithm) ||
      desc->src_handle == ZZ9K_INVALID_HANDLE || desc->src_length == 0U ||
      desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->dst_capacity == 0U ||
      (desc->flags & ~ZZ9K_DECOMPRESS_FLAG_EXPECT_END) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS);
  request->entry.payload_len = sizeof(ZZ9KDecompressPayload);
  payload = (ZZ9KDecompressPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->dst_capacity, desc->dst_capacity);
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_test(
    ZZ9KRequest *request, const ZZ9KDecompressTestDesc *desc)
{
  ZZ9KDecompressTestPayload *payload;

  if (!request || !desc ||
      !zz9k_compression_algorithm_known(desc->algorithm) ||
      desc->src_handle == ZZ9K_INVALID_HANDLE || desc->src_length == 0U ||
      desc->output_limit == 0U ||
      (desc->flags & ~ZZ9K_DECOMPRESS_FLAG_EXPECT_END) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_TEST);
  request->entry.payload_len = sizeof(ZZ9KDecompressTestPayload);
  payload = (ZZ9KDecompressTestPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->output_limit, desc->output_limit);
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_stream_begin(
    ZZ9KRequest *request, const ZZ9KDecompressStreamBeginDesc *desc)
{
  ZZ9KDecompressStreamBeginPayload *payload;
  uint32_t allowed_flags;
  int feed_input;

  allowed_flags = ZZ9K_DECOMPRESS_FLAG_EXPECT_END |
                  ZZ9K_DECOMPRESS_FLAG_FEED_INPUT;
  feed_input = desc &&
      (desc->flags & ZZ9K_DECOMPRESS_FLAG_FEED_INPUT) != 0U;
  if (!request || !desc ||
      !zz9k_compression_algorithm_known(desc->algorithm) ||
      desc->output_limit == 0U || (desc->flags & ~allowed_flags) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (feed_input) {
    if (desc->src_handle != ZZ9K_INVALID_HANDLE ||
        desc->src_offset != 0U || desc->src_length != 0U) {
      return ZZ9K_STATUS_BAD_REQUEST;
    }
  } else if (desc->src_handle == ZZ9K_INVALID_HANDLE ||
             desc->src_length == 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_STREAM_BEGIN);
  request->entry.payload_len = sizeof(ZZ9KDecompressStreamBeginPayload);
  payload =
      (ZZ9KDecompressStreamBeginPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->output_limit, desc->output_limit);
  zz9k_put_be32(payload->algorithm, desc->algorithm);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_stream_feed(
    ZZ9KRequest *request, const ZZ9KDecompressStreamFeedDesc *desc)
{
  ZZ9KDecompressStreamFeedPayload *payload;

  if (!request || !desc || desc->session == 0U ||
      desc->src_handle == ZZ9K_INVALID_HANDLE || desc->src_length == 0U ||
      (desc->flags & ~ZZ9K_DECOMPRESS_STREAM_FEED_EOF) != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_STREAM_FEED);
  request->entry.payload_len = sizeof(ZZ9KDecompressStreamFeedPayload);
  payload =
      (ZZ9KDecompressStreamFeedPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->src_handle, desc->src_handle);
  zz9k_put_be32(payload->src_offset, desc->src_offset);
  zz9k_put_be32(payload->src_length, desc->src_length);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_stream_read(
    ZZ9KRequest *request, const ZZ9KDecompressStreamReadDesc *desc)
{
  ZZ9KDecompressStreamReadPayload *payload;

  if (!request || !desc || desc->session == 0U ||
      desc->dst_handle == ZZ9K_INVALID_HANDLE ||
      desc->dst_capacity == 0U || desc->flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_STREAM_READ);
  request->entry.payload_len = sizeof(ZZ9KDecompressStreamReadPayload);
  payload =
      (ZZ9KDecompressStreamReadPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, desc->session);
  zz9k_put_be32(payload->dst_handle, desc->dst_handle);
  zz9k_put_be32(payload->dst_offset, desc->dst_offset);
  zz9k_put_be32(payload->dst_capacity, desc->dst_capacity);
  zz9k_put_be32(payload->flags, desc->flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_stream_close(
    ZZ9KRequest *request, uint32_t session, uint32_t flags)
{
  ZZ9KDecompressStreamClosePayload *payload;

  if (!request || session == 0U || flags != 0U) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_STREAM_CLOSE);
  request->entry.payload_len = sizeof(ZZ9KDecompressStreamClosePayload);
  payload =
      (ZZ9KDecompressStreamClosePayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->session, session);
  zz9k_put_be32(payload->flags, flags);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_decompress_batch(
    ZZ9KRequest *request, const ZZ9KDecompressBatchDesc *desc)
{
  ZZ9KDecompressBatchPayload *payload;

  if (!request || !desc || desc->arena_handle == ZZ9K_INVALID_HANDLE ||
      desc->arena_length < ZZ9K_BATCH_HEADER_SIZE) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DECOMPRESS_BATCH);
  request->entry.payload_len = sizeof(ZZ9KDecompressBatchPayload);
  payload = (ZZ9KDecompressBatchPayload *)request->entry.payload.inline_data;
  zz9k_put_be32(payload->arena_handle, desc->arena_handle);
  zz9k_put_be32(payload->arena_offset, desc->arena_offset);
  zz9k_put_be32(payload->arena_length, desc->arena_length);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_diag_read(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DIAG_READ);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_diag_timing(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DIAG_TIMING);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_diag_sched(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DIAG_SCHED);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_request_diag_memory(ZZ9KRequest *request)
{
  if (!request) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  zz9k_request_init(request, ZZ9K_OP_DIAG_MEMORY);
  return ZZ9K_STATUS_OK;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_REQUEST_H */

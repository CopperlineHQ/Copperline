/*
 * Header-only typed reply decoders for SDK mailbox completions.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_REPLY_H
#define ZZ9K_REPLY_H

#include "zz9k/audio.h"
#include "zz9k/host.h"
#include "zz9k/compression.h"
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ZZ9KSharedBufferInfo {
  uint32_t handle;
  uint32_t arm_addr;
  uint32_t length;
  uint32_t flags;
} ZZ9KSharedBufferInfo;

static inline int zz9k_reply_require(const ZZ9KMailboxEntry *reply,
                                     uint16_t opcode,
                                     uint32_t min_payload_len)
{
  if (!reply) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  if (reply->status != ZZ9K_STATUS_OK) {
    return reply->status;
  }
  if (reply->opcode != opcode ||
      reply->payload_len > sizeof(reply->payload.inline_data) ||
      reply->payload_len < min_payload_len) {
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_copy_inline_payload(
    const ZZ9KMailboxEntry *reply, uint16_t opcode, uint8_t *dst,
    uint32_t *dst_len)
{
  uint32_t capacity;
  int status;

  if (!dst_len || (dst && *dst_len == 0U)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  status = zz9k_reply_require(reply, opcode, 0U);
  if (status != ZZ9K_STATUS_OK) {
    if (reply && reply->payload_len <= sizeof(reply->payload.inline_data)) {
      *dst_len = reply->payload_len;
    }
    return status;
  }

  capacity = *dst_len;
  *dst_len = reply->payload_len;
  if (dst) {
    if (capacity < reply->payload_len) {
      return ZZ9K_STATUS_BAD_REQUEST;
    }
    if (reply->payload_len != 0U) {
      memcpy(dst, reply->payload.inline_data, reply->payload_len);
    }
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_caps(const ZZ9KMailboxEntry *reply,
                                  ZZ9KCaps *caps)
{
  const uint8_t *payload;
  int status;

  if (!caps) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(caps, 0, sizeof(*caps));
  status = zz9k_reply_require(reply, ZZ9K_OP_QUERY_CAPS, 32U);
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  caps->magic = zz9k_get_be32(&payload[0]);
  caps->abi_major = zz9k_get_be16(&payload[4]);
  caps->abi_minor = zz9k_get_be16(&payload[6]);
  caps->capability_bits = zz9k_get_be32(&payload[8]);
  caps->max_inline_payload = zz9k_get_be32(&payload[12]);
  caps->max_shared_buffers = zz9k_get_be32(&payload[16]);
  caps->max_surfaces = zz9k_get_be32(&payload[20]);
  caps->firmware_version = zz9k_get_be32(&payload[24]);
  caps->request_ring_entries = zz9k_get_be32(&payload[28]);
  if (reply->payload_len >= 36U) {
    caps->completion_ring_entries = zz9k_get_be32(&payload[32]);
  }
  if (reply->payload_len >= 40U) {
    caps->host_window_heap_size = zz9k_get_be32(&payload[36]);
  }

  if (caps->magic != ZZ9K_ABI_MAGIC ||
      caps->abi_major != ZZ9K_ABI_VERSION_MAJOR) {
    memset(caps, 0, sizeof(*caps));
    return ZZ9K_STATUS_UNSUPPORTED;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_aperture_layout(
    const ZZ9KMailboxEntry *reply, ZZ9KApertureLayout *layout)
{
  const uint8_t *payload;
  int status;

  if (!layout) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(layout, 0, sizeof(*layout));
  status = zz9k_reply_require(reply, ZZ9K_OP_QUERY_APERTURE_LAYOUT,
                              sizeof(ZZ9KApertureLayoutPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  layout->profile = zz9k_get_be32(&payload[0]);
  layout->aperture_size = zz9k_get_be32(&payload[4]);
  layout->framebuffer_base = zz9k_get_be32(&payload[8]);
  layout->framebuffer_size = zz9k_get_be32(&payload[12]);
  layout->pip_base = zz9k_get_be32(&payload[16]);
  layout->pip_size = zz9k_get_be32(&payload[20]);
  layout->template_base = zz9k_get_be32(&payload[24]);
  layout->template_size = zz9k_get_be32(&payload[28]);
  layout->host_base = zz9k_get_be32(&payload[32]);
  layout->host_size = zz9k_get_be32(&payload[36]);
  layout->audio_base = zz9k_get_be32(&payload[40]);
  layout->audio_size = zz9k_get_be32(&payload[44]);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_service_info(const ZZ9KMailboxEntry *reply,
                                          uint32_t expected_service_id,
                                          ZZ9KServiceInfo *service)
{
  const uint8_t *payload;
  int status;

  if (!service) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(service, 0, sizeof(*service));
  status = zz9k_reply_require(reply, ZZ9K_OP_QUERY_SERVICE,
                              sizeof(ZZ9KServiceInfoPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  service->service_id = zz9k_get_be32(&payload[0]);
  service->version = zz9k_get_be32(&payload[4]);
  service->capability_bits = zz9k_get_be32(&payload[8]);
  service->flags = zz9k_get_be32(&payload[12]);
  service->opcode_base = zz9k_get_be32(&payload[16]);
  service->opcode_count = zz9k_get_be32(&payload[20]);
  service->max_inline_payload = zz9k_get_be32(&payload[24]);
  memcpy(service->name, &payload[28], 20);
  service->name[20] = '\0';

  if (service->service_id != expected_service_id) {
    memset(service, 0, sizeof(*service));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_shared_buffer(const ZZ9KMailboxEntry *reply,
                                           ZZ9KSharedBufferInfo *buffer)
{
  const uint8_t *payload;
  int status;

  if (!buffer) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(buffer, 0, sizeof(*buffer));
  status = zz9k_reply_require(reply, ZZ9K_OP_ALLOC_SHARED, 16U);
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  buffer->handle = zz9k_get_be32(&payload[0]);
  buffer->arm_addr = zz9k_get_be32(&payload[4]);
  buffer->length = zz9k_get_be32(&payload[8]);
  if (reply->payload_len >= 16U) {
    buffer->flags = zz9k_get_be32(&payload[12]);
  }

  if (buffer->handle == ZZ9K_INVALID_HANDLE || buffer->length == 0) {
    memset(buffer, 0, sizeof(*buffer));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_surface(const ZZ9KMailboxEntry *reply,
                                     uint16_t opcode, ZZ9KSurface *surface)
{
  const uint8_t *payload;
  int status;

  if (!surface) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(surface, 0, sizeof(*surface));
  status = zz9k_reply_require(reply, opcode, 32U);
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  surface->handle = zz9k_get_be32(&payload[0]);
  surface->arm_addr = zz9k_get_be32(&payload[4]);
  surface->width = zz9k_get_be32(&payload[8]);
  surface->height = zz9k_get_be32(&payload[12]);
  surface->pitch = zz9k_get_be32(&payload[16]);
  surface->format = zz9k_get_be32(&payload[20]);
  surface->flags = zz9k_get_be32(&payload[24]);
  surface->length = zz9k_get_be32(&payload[28]);

  if (surface->handle == ZZ9K_INVALID_HANDLE || surface->width == 0 ||
      surface->height == 0 || surface->pitch == 0 ||
      surface->length == 0) {
    memset(surface, 0, sizeof(*surface));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_diag_info(const ZZ9KMailboxEntry *reply,
                                       ZZ9KDiagInfo *diag)
{
  const uint8_t *payload;
  int status;

  if (!diag) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(diag, 0, sizeof(*diag));
  status = zz9k_reply_require(reply, ZZ9K_OP_DIAG_READ, 40U);
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  diag->requests_completed = zz9k_get_be32(&payload[0]);
  diag->requests_failed = zz9k_get_be32(&payload[4]);
  diag->last_status = zz9k_get_be32(&payload[8]);
  diag->pending_requests = zz9k_get_be32(&payload[12]);
  diag->shared_buffers_used = zz9k_get_be32(&payload[16]);
  diag->shared_heap_total = zz9k_get_be32(&payload[20]);
  diag->shared_heap_free = zz9k_get_be32(&payload[24]);
  diag->shared_heap_largest_free = zz9k_get_be32(&payload[28]);
  diag->mailbox_arm_addr = zz9k_get_be32(&payload[32]);
  diag->mailbox_ring_entries = zz9k_get_be32(&payload[36]);
  if (reply->payload_len >= 44U) {
    diag->surfaces_used = zz9k_get_be32(&payload[40]);
  }
  if (reply->payload_len >= 48U) {
    diag->allocator_invalid_slots = zz9k_get_be32(&payload[44]);
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_diag_timing(const ZZ9KMailboxEntry *reply,
                                         ZZ9KDiagTimingInfo *timing)
{
  const uint8_t *payload;
  int status;

  if (!timing) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(timing, 0, sizeof(*timing));
  status = zz9k_reply_require(reply, ZZ9K_OP_DIAG_TIMING,
                              sizeof(ZZ9KDiagTimingPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  timing->version = zz9k_get_be32(&payload[0]);
  timing->timer_hz = zz9k_get_be32(&payload[4]);
  timing->requests_timed = zz9k_get_be32(&payload[8]);
  timing->total_us = zz9k_get_be32(&payload[12]);
  timing->surface_requests = zz9k_get_be32(&payload[16]);
  timing->surface_us = zz9k_get_be32(&payload[20]);
  timing->audio_requests = zz9k_get_be32(&payload[24]);
  timing->audio_us = zz9k_get_be32(&payload[28]);
  timing->last_opcode = zz9k_get_be32(&payload[32]);
  timing->last_us = zz9k_get_be32(&payload[36]);
  timing->max_opcode = zz9k_get_be32(&payload[40]);
  timing->max_us = zz9k_get_be32(&payload[44]);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_diag_memory(const ZZ9KMailboxEntry *reply,
                                         ZZ9KDiagMemoryInfo *memory)
{
  const uint8_t *payload;
  int status;

  if (!memory) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(memory, 0, sizeof(*memory));
  status = zz9k_reply_require(reply, ZZ9K_OP_DIAG_MEMORY,
                              sizeof(ZZ9KDiagMemoryPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  memory->version = zz9k_get_be32(&payload[0]);
  memory->layout_state = zz9k_get_be32(&payload[4]);
  memory->aperture_size = zz9k_get_be32(&payload[8]);
  memory->aperture_info = zz9k_get_be32(&payload[12]);
  memory->host_board_base = zz9k_get_be32(&payload[16]);
  memory->host_arm_base = zz9k_get_be32(&payload[20]);
  memory->host_total = zz9k_get_be32(&payload[24]);
  memory->host_free = zz9k_get_be32(&payload[28]);
  memory->host_largest_free = zz9k_get_be32(&payload[32]);
  memory->allocations = zz9k_get_be32(&payload[36]);
  memory->allocator_invalid_slots = zz9k_get_be32(&payload[40]);
  memory->reserved = zz9k_get_be32(&payload[44]);
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_diag_sched(const ZZ9KMailboxEntry *reply,
                                        ZZ9KDiagSchedInfo *sched)
{
  const uint8_t *payload;
  int status;

  if (!sched) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(sched, 0, sizeof(*sched));
  /* Require only the version-1 base payload so a firmware that predates the
   * decode-timing counters still decodes; read the version-2 extension when
   * the reply carries it (payload_len >= full struct size). */
  status = zz9k_reply_require(reply, ZZ9K_OP_DIAG_SCHED,
                              ZZ9K_DIAG_SCHED_PAYLOAD_V1_BYTES);
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  sched->version = zz9k_get_be32(&payload[0]);
  sched->core1_online = zz9k_get_be32(&payload[4]);
  sched->tasks_on_core1 = zz9k_get_be32(&payload[8]);
  sched->tasks_on_core0 = zz9k_get_be32(&payload[12]);
  if (reply->payload_len >= sizeof(ZZ9KDiagSchedPayload)) {
    sched->decode_requests = zz9k_get_be32(&payload[16]);
    sched->decode_us = zz9k_get_be32(&payload[20]);
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_image_decode_result(
  const ZZ9KMailboxEntry *reply,
  uint16_t opcode,
  ZZ9KImageDecodeResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KImageDecodeResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->width = zz9k_get_be32(&payload[0]);
  result->height = zz9k_get_be32(&payload[4]);
  result->output_format = zz9k_get_be32(&payload[8]);
  result->flags = zz9k_get_be32(&payload[12]);
  result->bytes_written = zz9k_get_be32(&payload[16]);

  if (result->width == 0U || result->height == 0U) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_image_session_result(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KImageSessionResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KImageSessionResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->image_width = zz9k_get_be32(&payload[8]);
  result->image_height = zz9k_get_be32(&payload[12]);
  result->output_format = zz9k_get_be32(&payload[16]);
  result->tile_x = zz9k_get_be32(&payload[20]);
  result->tile_y = zz9k_get_be32(&payload[24]);
  result->tile_width = zz9k_get_be32(&payload[28]);
  result->tile_height = zz9k_get_be32(&payload[32]);
  result->bytes_consumed = zz9k_get_be32(&payload[36]);
  result->bytes_written = zz9k_get_be32(&payload[40]);
  result->flags = zz9k_get_be32(&payload[44]);

  if (result->session == 0U ||
      result->state < ZZ9K_IMAGE_SESSION_STATE_NEED_INPUT ||
      result->state > ZZ9K_IMAGE_SESSION_STATE_ERROR) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_audio_decode_result(
    const ZZ9KMailboxEntry *reply,
    ZZ9KAudioDecodeResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, ZZ9K_OP_DECODE_MP3,
                              sizeof(ZZ9KAudioDecodeResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->bytes_consumed = zz9k_get_be32(&payload[0]);
  result->bytes_written = zz9k_get_be32(&payload[4]);
  result->sample_rate = zz9k_get_be32(&payload[8]);
  result->channels = zz9k_get_be32(&payload[12]);
  result->sample_format = zz9k_get_be32(&payload[16]);
  result->frames_written = zz9k_get_be32(&payload[20]);
  result->flags = zz9k_get_be32(&payload[24]);

  if (result->bytes_written == 0U || result->sample_rate == 0U ||
      result->channels == 0U ||
      !zz9k_audio_sample_format_known(result->sample_format)) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_audio_stream_result(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KAudioStreamResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result ||
      (opcode != ZZ9K_OP_AUDIO_STREAM_BEGIN &&
       opcode != ZZ9K_OP_AUDIO_STREAM_FEED &&
       opcode != ZZ9K_OP_AUDIO_STREAM_READ &&
       opcode != ZZ9K_OP_AUDIO_STREAM_CLOSE &&
       opcode != ZZ9K_OP_AUDIO_STREAM_PLAY &&
       opcode != ZZ9K_OP_AUDIO_STREAM_STOP)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KAudioStreamResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->sample_rate = zz9k_get_be32(&payload[8]);
  result->channels = zz9k_get_be32(&payload[12]);
  result->sample_format = zz9k_get_be32(&payload[16]);
  result->mp3_read = zz9k_get_be32(&payload[20]);
  result->pcm_write = zz9k_get_be32(&payload[24]);
  result->pcm_read = zz9k_get_be32(&payload[28]);
  result->frames_decoded = zz9k_get_be32(&payload[32]);
  result->bytes_consumed = zz9k_get_be32(&payload[36]);
  result->bytes_produced = zz9k_get_be32(&payload[40]);
  result->flags = zz9k_get_be32(&payload[44]);

  if (result->session == 0U ||
      result->state < ZZ9K_AUDIO_STREAM_STATE_NEED_INPUT ||
      result->state > ZZ9K_AUDIO_STREAM_STATE_ERROR ||
      (result->sample_format != ZZ9K_AUDIO_SAMPLE_FORMAT_NONE &&
       !zz9k_audio_sample_format_known(result->sample_format))) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_video_session_result(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KVideoSessionResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result ||
      (opcode != ZZ9K_OP_VIDEO_SESSION_BEGIN &&
       opcode != ZZ9K_OP_VIDEO_SESSION_WRITE &&
       opcode != ZZ9K_OP_VIDEO_SESSION_DECODE &&
       opcode != ZZ9K_OP_VIDEO_SESSION_CLOSE)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KVideoSessionResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->width = zz9k_get_be32(&payload[8]);
  result->height = zz9k_get_be32(&payload[12]);
  result->frame_rate_milli = zz9k_get_be32(&payload[16]);
  result->frame_number = zz9k_get_be32(&payload[20]);
  result->frame_time_millis = zz9k_get_be32(&payload[24]);
  result->bytes_accepted = zz9k_get_be32(&payload[28]);
  result->bytes_written = zz9k_get_be32(&payload[32]);
  result->flags = zz9k_get_be32(&payload[36]);

  if (result->session == 0U ||
      result->state < ZZ9K_VIDEO_SESSION_STATE_NEED_INPUT ||
      result->state > ZZ9K_VIDEO_SESSION_STATE_ERROR) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_media_main_opcode_known(uint16_t opcode)
{
  return opcode == ZZ9K_OP_MEDIA_SESSION_BEGIN ||
         opcode == ZZ9K_OP_MEDIA_SESSION_WRITE ||
         opcode == ZZ9K_OP_MEDIA_SESSION_DECODE ||
         opcode == ZZ9K_OP_MEDIA_SESSION_PRESENT ||
         opcode == ZZ9K_OP_MEDIA_SESSION_DISCARD ||
         opcode == ZZ9K_OP_MEDIA_SESSION_CLOSE;
}

static inline uint32_t zz9k_media_session_known_result_flags(void)
{
  return ZZ9K_MEDIA_SESSION_RESULT_HEADER_READY |
         ZZ9K_MEDIA_SESSION_RESULT_NEED_INPUT |
         ZZ9K_MEDIA_SESSION_RESULT_FRAME_HELD |
         ZZ9K_MEDIA_SESSION_RESULT_DONE |
         ZZ9K_MEDIA_SESSION_RESULT_DERIVED_TIME |
         ZZ9K_MEDIA_SESSION_RESULT_DISCONTINUITY |
         ZZ9K_MEDIA_SESSION_RESULT_REBASED |
         ZZ9K_MEDIA_SESSION_RESULT_AUDIO_READY |
         ZZ9K_MEDIA_SESSION_RESULT_BACKPRESSURE |
         ZZ9K_MEDIA_SESSION_RESULT_PRESENTED |
         ZZ9K_MEDIA_SESSION_RESULT_DISCARDED |
         ZZ9K_MEDIA_SESSION_RESULT_AUDIO_BOUND |
         ZZ9K_MEDIA_SESSION_RESULT_AUDIO_PLAYING |
         ZZ9K_MEDIA_SESSION_RESULT_AUDIO_DRAINED |
         ZZ9K_MEDIA_SESSION_RESULT_AUDIO_UNDERRUN;
}

/* The presentation page reuses the `flags` word for a different namespace
 * (ZZ9K_MEDIA_PRESENT_*), so it must be validated against its own mask. */
static inline uint32_t zz9k_media_present_known_flags(void)
{
  return ZZ9K_MEDIA_PRESENT_CONFIGURED |
         ZZ9K_MEDIA_PRESENT_ACTIVE |
         ZZ9K_MEDIA_PRESENT_NATIVE |
         ZZ9K_MEDIA_PRESENT_OWNED;
}

/* Each status page owns its `flags` namespace; validating one page against
 * another's mask only appears to work while the bits happen to overlap. */
static inline uint32_t zz9k_media_status_known_flags(uint32_t page)
{
  if (page == ZZ9K_MEDIA_STATUS_PRESENTATION) {
    return zz9k_media_present_known_flags();
  }
  if (page == ZZ9K_MEDIA_STATUS_PROFILE) {
    return ZZ9K_MEDIA_PROFILE_FLAG_NEON_PACK;
  }
  return zz9k_media_session_known_result_flags();
}

static inline int zz9k_reply_media_session_main(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KMediaSessionMainResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result || !zz9k_media_main_opcode_known(opcode)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(
      reply, opcode, sizeof(ZZ9KMediaSessionMainResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->width = zz9k_get_be32(&payload[8]);
  result->height = zz9k_get_be32(&payload[12]);
  result->frame_rate_num = zz9k_get_be32(&payload[16]);
  result->frame_rate_den = zz9k_get_be32(&payload[20]);
  result->frame_number = zz9k_get_be32(&payload[24]);
  result->video_pts = zz9k_media_u64_from_be(&payload[28], &payload[32]);
  result->bytes_accepted = zz9k_get_be32(&payload[36]);
  result->bytes_written = zz9k_get_be32(&payload[40]);
  result->flags = zz9k_get_be32(&payload[44]);

  if (result->session == 0U ||
      result->state < ZZ9K_MEDIA_SESSION_STATE_NEED_INPUT ||
      result->state > ZZ9K_MEDIA_SESSION_STATE_ERROR ||
      (result->flags & ~zz9k_media_session_known_result_flags()) != 0U) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_media_session_audio(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KMediaSessionAudioResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result ||
      (opcode != ZZ9K_OP_MEDIA_SESSION_AUDIO_READ &&
       opcode != ZZ9K_OP_MEDIA_SESSION_AUDIO_BIND &&
       opcode != ZZ9K_OP_MEDIA_SESSION_AUDIO_UNBIND)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(
      reply, opcode, sizeof(ZZ9KMediaSessionAudioResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }
  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->sample_rate = zz9k_get_be32(&payload[8]);
  result->channels = zz9k_get_be32(&payload[12]);
  result->sample_format = zz9k_get_be32(&payload[16]);
  result->pcm_produced = zz9k_media_u64_from_be(&payload[20], &payload[24]);
  result->pcm_acknowledged =
      zz9k_media_u64_from_be(&payload[28], &payload[32]);
  result->audio_pts = zz9k_media_u64_from_be(&payload[36], &payload[40]);
  result->flags = zz9k_get_be32(&payload[44]);
  if (result->session == 0U ||
      result->state < ZZ9K_MEDIA_SESSION_STATE_NEED_INPUT ||
      result->state > ZZ9K_MEDIA_SESSION_STATE_ERROR ||
      (result->flags & ~zz9k_media_session_known_result_flags()) != 0U) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_media_session_status(
    const ZZ9KMailboxEntry *reply,
    ZZ9KMediaSessionStatusResult *result)
{
  const uint8_t *payload;
  int status;
  uint32_t i;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(
      reply, ZZ9K_OP_MEDIA_SESSION_STATUS,
      sizeof(ZZ9KMediaSessionStatusResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }
  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->state = zz9k_get_be32(&payload[4]);
  result->page = zz9k_get_be32(&payload[8]);
  result->flags = zz9k_get_be32(&payload[12]);
  for (i = 0U; i < 4U; i++) {
    result->value[i] =
        zz9k_media_u64_from_be(&payload[16U + i * 8U],
                               &payload[20U + i * 8U]);
  }
  if (result->session == 0U ||
      result->state < ZZ9K_MEDIA_SESSION_STATE_NEED_INPUT ||
      result->state > ZZ9K_MEDIA_SESSION_STATE_ERROR ||
      result->page > ZZ9K_MEDIA_STATUS_PROFILE ||
      (result->flags & ~zz9k_media_status_known_flags(result->page)) !=
          0U) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }
  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_crypto_result(const ZZ9KMailboxEntry *reply,
                                           uint16_t opcode,
                                           ZZ9KCryptoResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KCryptoResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->bytes_written = zz9k_get_be32(&payload[0]);
  result->algorithm = zz9k_get_be32(&payload[4]);
  result->flags = zz9k_get_be32(&payload[8]);

  if (result->bytes_written == 0U) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_crypto_verify(const ZZ9KMailboxEntry *reply,
                                            int *valid)
{
  const uint8_t *payload;
  int status;

  if (!valid) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  *valid = 0;
  status = zz9k_reply_require(reply, ZZ9K_OP_CRYPTO_VERIFY,
                              sizeof(ZZ9KCryptoVerifyPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  *valid = (zz9k_get_be32(&payload[0]) != 0U);

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_decompress_result(
    const ZZ9KMailboxEntry *reply, ZZ9KDecompressResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, ZZ9K_OP_DECOMPRESS,
                              sizeof(ZZ9KDecompressResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    status = zz9k_reply_require(reply, ZZ9K_OP_DECOMPRESS_TEST,
                                sizeof(ZZ9KDecompressResultPayload));
  }
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->bytes_consumed = zz9k_get_be32(&payload[0]);
  result->bytes_written = zz9k_get_be32(&payload[4]);
  result->checksum = zz9k_get_be32(&payload[8]);
  result->algorithm = zz9k_get_be32(&payload[12]);
  result->flags = zz9k_get_be32(&payload[16]);

  if (result->bytes_consumed == 0U || result->bytes_written == 0U ||
      !zz9k_compression_algorithm_known(result->algorithm)) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_decompress_stream_result(
    const ZZ9KMailboxEntry *reply,
    uint16_t opcode,
    ZZ9KDecompressStreamResult *result)
{
  const uint8_t *payload;
  int status;

  if (!result ||
      (opcode != ZZ9K_OP_DECOMPRESS_STREAM_BEGIN &&
       opcode != ZZ9K_OP_DECOMPRESS_STREAM_FEED &&
       opcode != ZZ9K_OP_DECOMPRESS_STREAM_READ)) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }

  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, opcode,
                              sizeof(ZZ9KDecompressStreamResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }

  payload = reply->payload.inline_data;
  result->session = zz9k_get_be32(&payload[0]);
  result->bytes_consumed = zz9k_get_be32(&payload[4]);
  result->bytes_written = zz9k_get_be32(&payload[8]);
  result->checksum = zz9k_get_be32(&payload[12]);
  result->algorithm = zz9k_get_be32(&payload[16]);
  result->flags = zz9k_get_be32(&payload[20]);

  if (result->session == 0U ||
      !zz9k_compression_algorithm_known(result->algorithm)) {
    memset(result, 0, sizeof(*result));
    return ZZ9K_STATUS_INTERNAL_ERROR;
  }

  return ZZ9K_STATUS_OK;
}

static inline int zz9k_reply_decompress_batch_result(
    const ZZ9KMailboxEntry *reply, ZZ9KDecompressBatchResult *result)
{
  const ZZ9KDecompressBatchResultPayload *payload;
  int status;

  if (!result) {
    return ZZ9K_STATUS_BAD_REQUEST;
  }
  memset(result, 0, sizeof(*result));
  status = zz9k_reply_require(reply, ZZ9K_OP_DECOMPRESS_BATCH,
                              sizeof(ZZ9KDecompressBatchResultPayload));
  if (status != ZZ9K_STATUS_OK) {
    return status;
  }
  payload =
      (const ZZ9KDecompressBatchResultPayload *)reply->payload.inline_data;
  result->members_total = zz9k_get_be32(payload->members_total);
  result->members_ok = zz9k_get_be32(payload->members_ok);
  result->members_failed = zz9k_get_be32(payload->members_failed);
  result->flags = zz9k_get_be32(payload->flags);
  return ZZ9K_STATUS_OK;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_REPLY_H */

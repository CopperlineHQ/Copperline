/*
 * ZZ9000 SDK v2 Amiga-side host API.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_HOST_H
#define ZZ9K_HOST_H

#include <stdint.h>
#include "zz9k/abi.h"
#include "zz9k/crypto.h"
#include "zz9k/text.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ZZ9KContext ZZ9KContext;

typedef struct ZZ9KBoard {
  uint32_t board_addr;
  uint32_t board_size;
  uint16_t product;
  uint16_t zorro_version;
  uint32_t firmware_version;
} ZZ9KBoard;

typedef struct ZZ9KRequest {
  ZZ9KMailboxEntry entry;
} ZZ9KRequest;

typedef struct ZZ9KSharedBuffer {
  uint32_t handle;
  volatile void *data;
  uint32_t length;
} ZZ9KSharedBuffer;

typedef struct ZZ9KSurface {
  uint32_t handle;
  uint32_t arm_addr;
  volatile void *data;
  uint32_t width;
  uint32_t height;
  uint32_t pitch;
  uint32_t format;
  uint32_t flags;
  uint32_t length;
} ZZ9KSurface;

typedef struct ZZ9KDiagInfo {
  uint32_t requests_completed;
  uint32_t requests_failed;
  uint32_t last_status;
  uint32_t pending_requests;
  uint32_t shared_buffers_used;
  uint32_t shared_heap_total;
  uint32_t shared_heap_free;
  uint32_t shared_heap_largest_free;
  uint32_t mailbox_arm_addr;
  uint32_t mailbox_ring_entries;
  uint32_t surfaces_used;
  uint32_t allocator_invalid_slots;
} ZZ9KDiagInfo;

typedef struct ZZ9KDiagTimingInfo {
  uint32_t version;
  uint32_t timer_hz;
  uint32_t requests_timed;
  uint32_t total_us;
  uint32_t surface_requests;
  uint32_t surface_us;
  uint32_t audio_requests;
  uint32_t audio_us;
  uint32_t last_opcode;
  uint32_t last_us;
  uint32_t max_opcode;
  uint32_t max_us;
} ZZ9KDiagTimingInfo;

typedef struct ZZ9KDiagSchedInfo {
  uint32_t version;
  uint32_t core1_online;
  uint32_t tasks_on_core1;
  uint32_t tasks_on_core0;
  uint32_t decode_requests;  /* version 2+: decompress decode count; 0 on v1 firmware */
  uint32_t decode_us;        /* version 2+: cumulative decode microseconds; 0 on v1 firmware */
} ZZ9KDiagSchedInfo;

typedef struct ZZ9KApertureLayout {
  uint32_t profile;
  uint32_t aperture_size;
  uint32_t framebuffer_base;
  uint32_t framebuffer_size;
  uint32_t pip_base;
  uint32_t pip_size;
  uint32_t template_base;
  uint32_t template_size;
  uint32_t host_base;
  uint32_t host_size;
  uint32_t audio_base;
  uint32_t audio_size;
} ZZ9KApertureLayout;

typedef struct ZZ9KDiagMemoryInfo {
  uint32_t version;
  uint32_t layout_state;
  uint32_t aperture_size;
  uint32_t aperture_info;
  uint32_t host_board_base;
  uint32_t host_arm_base;
  uint32_t host_total;
  uint32_t host_free;
  uint32_t host_largest_free;
  uint32_t allocations;
  uint32_t allocator_invalid_slots;
  uint32_t reserved;
} ZZ9KDiagMemoryInfo;

typedef struct ZZ9KServiceInfo {
  uint32_t service_id;
  uint32_t version;
  uint32_t capability_bits;
  uint32_t flags;
  uint32_t opcode_base;
  uint32_t opcode_count;
  uint32_t max_inline_payload;
  char name[21];
} ZZ9KServiceInfo;

#define ZZ9K_DEFAULT_TIMEOUT_TICKS 250UL
#define ZZ9K_OFFLOAD_TIMEOUT_MS 250UL
#define ZZ9K_OFFLOAD_ITER_BACKSTOP 1000000UL
#define ZZ9K_CRYPTO_BATCH_MAX_IN_FLIGHT 16U

const char *zz9k_status_name(int status);
const char *zz9k_service_name(uint32_t service_id);
int zz9k_service_advertised_by_caps(uint32_t service_id,
                                    uint32_t capability_bits);

int zz9k_find_board(ZZ9KBoard *board);
int zz9k_open(ZZ9KContext **ctx);
int zz9k_attach_mailbox(ZZ9KContext **ctx, const ZZ9KBoard *board,
                        volatile ZZ9KMailboxDescriptor *mailbox,
                        volatile uint16_t *doorbell,
                        volatile uint16_t *irq_ack);
void zz9k_close(ZZ9KContext *ctx);
/* Enable a real-time timeout budget for unarmed polling calls on this context.
   0 disables the wall-clock budget and restores the legacy tick bound. */
void zz9k_set_offload_timeout_ms(ZZ9KContext *ctx, uint32_t timeout_ms);
/* Spin-count budget before unarmed completion poll number `ticks` (0-based).
 * The head stays fine-grained so short ops are discovered within ~1 ms on a
 * 68060; the tail keeps the pre-existing coarse cadence so long ops poll the
 * Zorro completion ring no more often than before. Pure; exposed so host
 * tests can pin the schedule. */
uint32_t zz9k_idle_backoff_limit(uint32_t ticks);
int zz9k_query_caps(ZZ9KContext *ctx, ZZ9KCaps *caps);
int zz9k_query_aperture_layout(ZZ9KContext *ctx,
                               ZZ9KApertureLayout *layout);
int zz9k_query_service(ZZ9KContext *ctx, uint32_t service_id,
                       ZZ9KServiceInfo *service);
int zz9k_ping(ZZ9KContext *ctx, const uint8_t *payload,
              uint32_t payload_len, uint8_t *reply_payload,
              uint32_t *reply_len);
int zz9k_alloc_shared(ZZ9KContext *ctx, uint32_t length, uint32_t alignment,
                      uint32_t flags, ZZ9KSharedBuffer *buffer);
int zz9k_free_shared(ZZ9KContext *ctx, uint32_t handle);
int zz9k_mem_fill(ZZ9KContext *ctx, uint32_t handle, uint32_t offset,
                  uint32_t length, uint8_t value);
int zz9k_mem_copy(ZZ9KContext *ctx, uint32_t dst_handle, uint32_t dst_offset,
                  uint32_t src_handle, uint32_t src_offset, uint32_t length);
int zz9k_alloc_surface(ZZ9KContext *ctx, uint32_t width, uint32_t height,
                       uint32_t format, uint32_t flags, ZZ9KSurface *surface);
int zz9k_alloc_surface_ex(ZZ9KContext *ctx, uint32_t width, uint32_t height,
                          uint32_t format, uint32_t flags, uint32_t pitch,
                          ZZ9KSurface *surface);
int zz9k_free_surface(ZZ9KContext *ctx, uint32_t handle);
int zz9k_map_framebuffer_surface(ZZ9KContext *ctx, ZZ9KSurface *surface);
int zz9k_scale_image(ZZ9KContext *ctx, const ZZ9KScaleImageDesc *desc);
int zz9k_scale_image_clipped(ZZ9KContext *ctx,
                             const ZZ9KScaleImageClippedDesc *desc);
int zz9k_fill_surface(ZZ9KContext *ctx, const ZZ9KSurfaceFillDesc *desc);
int zz9k_copy_surface(ZZ9KContext *ctx, const ZZ9KSurfaceCopyDesc *desc);
/* Reads `count` primary-CLUT entries from index `start` into the caller's
 * shared buffer as 0x00RRGGBB words. Requires firmware advertising
 * ZZ9K_SERVICE_FLAG_SURFACE_PALETTE_QUERY on the surface service. */
int zz9k_query_palette(ZZ9KContext *ctx, const ZZ9KPaletteQueryDesc *desc);
int zz9k_decode_image(ZZ9KContext *ctx, uint32_t opcode,
                      const ZZ9KImageDecodeDesc *desc,
                      ZZ9KImageDecodeResult *result);
int zz9k_decode_jpeg(ZZ9KContext *ctx, const ZZ9KImageDecodeDesc *desc,
                     ZZ9KImageDecodeResult *result);
int zz9k_decode_png(ZZ9KContext *ctx, const ZZ9KImageDecodeDesc *desc,
                    ZZ9KImageDecodeResult *result);
int zz9k_decode_gif(ZZ9KContext *ctx, const ZZ9KImageDecodeDesc *desc,
                    ZZ9KImageDecodeResult *result);
int zz9k_decode_mp3(ZZ9KContext *ctx, const ZZ9KAudioDecodeDesc *desc,
                    ZZ9KAudioDecodeResult *result);
int zz9k_audio_stream_begin(ZZ9KContext *ctx,
                            const ZZ9KAudioStreamBeginDesc *desc,
                            ZZ9KAudioStreamResult *result);
int zz9k_audio_stream_feed(ZZ9KContext *ctx,
                           const ZZ9KAudioStreamFeedDesc *desc,
                           ZZ9KAudioStreamResult *result);
int zz9k_audio_stream_read(ZZ9KContext *ctx, uint32_t session,
                           uint32_t pcm_read, uint32_t flags,
                           ZZ9KAudioStreamResult *result);
int zz9k_audio_stream_close(ZZ9KContext *ctx, uint32_t session,
                            uint32_t flags,
                            ZZ9KAudioStreamResult *result);
int zz9k_audio_stream_play(ZZ9KContext *ctx, uint32_t session,
                           uint32_t flags,
                           ZZ9KAudioStreamResult *result);
int zz9k_audio_stream_stop(ZZ9KContext *ctx, uint32_t session,
                           uint32_t flags,
                           ZZ9KAudioStreamResult *result);
int zz9k_video_session_begin(ZZ9KContext *ctx,
                             const ZZ9KVideoSessionBeginDesc *desc,
                             ZZ9KVideoSessionResult *result);
int zz9k_video_session_write(ZZ9KContext *ctx,
                             const ZZ9KVideoSessionWriteDesc *desc,
                             ZZ9KVideoSessionResult *result);
int zz9k_video_session_decode(ZZ9KContext *ctx,
                              const ZZ9KVideoSessionDecodeDesc *desc,
                              ZZ9KVideoSessionResult *result);
int zz9k_video_session_close(ZZ9KContext *ctx, uint32_t session,
                             uint32_t flags,
                             ZZ9KVideoSessionResult *result);
int zz9k_media_session_begin(ZZ9KContext *ctx,
                             const ZZ9KMediaSessionBeginDesc *desc,
                             ZZ9KMediaSessionMainResult *result);
int zz9k_media_session_write(ZZ9KContext *ctx,
                             const ZZ9KMediaSessionWriteDesc *desc,
                             ZZ9KMediaSessionMainResult *result);
int zz9k_media_session_decode(ZZ9KContext *ctx, uint32_t session,
                              uint32_t flags,
                              ZZ9KMediaSessionMainResult *result);
int zz9k_media_session_present(ZZ9KContext *ctx, uint32_t session,
                               uint32_t flags,
                               ZZ9KMediaSessionMainResult *result);
int zz9k_media_session_discard(ZZ9KContext *ctx, uint32_t session,
                               uint32_t flags,
                               ZZ9KMediaSessionMainResult *result);
int zz9k_media_session_status(ZZ9KContext *ctx, uint32_t session,
                              uint32_t page, uint32_t flags,
                              ZZ9KMediaSessionStatusResult *result);
int zz9k_media_session_audio_read(ZZ9KContext *ctx, uint32_t session,
                                  uint64_t acknowledged, uint32_t flags,
                                  ZZ9KMediaSessionAudioResult *result);
int zz9k_media_session_audio_bind(ZZ9KContext *ctx, uint32_t session,
                                  uint32_t flags,
                                  ZZ9KMediaSessionAudioResult *result);
int zz9k_media_session_audio_unbind(ZZ9KContext *ctx, uint32_t session,
                                    uint32_t flags,
                                    ZZ9KMediaSessionAudioResult *result);
int zz9k_media_session_close(ZZ9KContext *ctx, uint32_t session,
                             uint32_t flags,
                             ZZ9KMediaSessionMainResult *result);
int zz9k_image_session_begin(ZZ9KContext *ctx,
                             const ZZ9KImageSessionBeginDesc *desc,
                             ZZ9KImageSessionResult *result);
int zz9k_image_session_feed(ZZ9KContext *ctx,
                            const ZZ9KImageSessionFeedDesc *desc,
                            ZZ9KImageSessionResult *result);
int zz9k_image_session_close(ZZ9KContext *ctx, uint32_t session,
                             uint32_t flags);
int zz9k_crypto_hash(ZZ9KContext *ctx, const ZZ9KCryptoHashDesc *desc,
                     ZZ9KCryptoResult *result);
int zz9k_crypto_hash_batch(ZZ9KContext *ctx,
                           const ZZ9KCryptoHashDesc *descs,
                           ZZ9KCryptoResult *results,
                           uint32_t count,
                           uint32_t max_in_flight,
                           uint32_t timeout_ticks);
int zz9k_crypto_stream(ZZ9KContext *ctx,
                       const ZZ9KCryptoStreamDesc *desc,
                       ZZ9KCryptoResult *result);
int zz9k_crypto_stream_batch(ZZ9KContext *ctx,
                             const ZZ9KCryptoStreamDesc *descs,
                             ZZ9KCryptoResult *results,
                             uint32_t count,
                             uint32_t max_in_flight,
                             uint32_t timeout_ticks);
int zz9k_crypto_aead(ZZ9KContext *ctx, const ZZ9KCryptoAeadDesc *desc,
                     ZZ9KCryptoResult *result);
int zz9k_crypto_kx(ZZ9KContext *ctx, const ZZ9KCryptoKxDesc *desc,
                   ZZ9KCryptoResult *result);
int zz9k_crypto_verify(ZZ9KContext *ctx, const ZZ9KCryptoVerifyDesc *desc,
                       int *valid);
int zz9k_crypto_aead_batch(ZZ9KContext *ctx,
                           const ZZ9KCryptoAeadDesc *descs,
                           ZZ9KCryptoResult *results,
                           uint32_t count,
                           uint32_t max_in_flight,
                           uint32_t timeout_ticks);
int zz9k_decompress(ZZ9KContext *ctx, const ZZ9KDecompressDesc *desc,
                    ZZ9KDecompressResult *result);
int zz9k_decompress_batch(ZZ9KContext *ctx,
                          const ZZ9KDecompressBatchDesc *desc,
                          ZZ9KDecompressBatchResult *result);
int zz9k_decompress_test(ZZ9KContext *ctx,
                         const ZZ9KDecompressTestDesc *desc,
                         ZZ9KDecompressResult *result);
int zz9k_decompress_stream_begin(
    ZZ9KContext *ctx,
    const ZZ9KDecompressStreamBeginDesc *desc,
    ZZ9KDecompressStreamResult *result);
int zz9k_decompress_stream_feed(
    ZZ9KContext *ctx,
    const ZZ9KDecompressStreamFeedDesc *desc,
    ZZ9KDecompressStreamResult *result);
int zz9k_decompress_stream_read(
    ZZ9KContext *ctx,
    const ZZ9KDecompressStreamReadDesc *desc,
    ZZ9KDecompressStreamResult *result);
int zz9k_decompress_stream_close(ZZ9KContext *ctx, uint32_t session,
                                 uint32_t flags);
int zz9k_read_diag(ZZ9KContext *ctx, ZZ9KDiagInfo *diag);
int zz9k_read_diag_timing(ZZ9KContext *ctx, ZZ9KDiagTimingInfo *timing);
int zz9k_read_diag_sched(ZZ9KContext *ctx, ZZ9KDiagSchedInfo *sched);
int zz9k_read_diag_memory(ZZ9KContext *ctx, ZZ9KDiagMemoryInfo *memory);
int zz9k_completion_irq_supported(ZZ9KContext *ctx);
int zz9k_completion_irq_enable(ZZ9KContext *ctx, int enable);

/*
 * Should the SDK completion interrupt attach to INT2 (INTB_PORTS)
 * instead of the INT6 default? True when ENV:ZZ9K_INT2 exists, else
 * when the SD card's ZZ9000.CFG has `int2 = on` (firmware ABI >= 2.3;
 * older firmware reports the key absent). Amiga-only; the shared
 * decision point for zz9k.library and the SDK tools so all ZZ9000
 * interrupt consumers pick the same line.
 */
int zz9k_sdk_use_int2(const ZZ9KContext *ctx);

/*
 * Arm this context to block on the SDK completion IRQ inside zz9k_call()
 * instead of busy-polling. Amiga-only; returns ZZ9K_STATUS_UNSUPPORTED on the
 * host or when the firmware does not advertise ZZ9K_CAP_IRQ_COMPLETION. On any
 * failure the context is left unarmed and zz9k_call keeps the poll/spin path.
 * The arming task is captured as the wake target, so arm and every subsequent
 * zz9k_call on this context must run on the same task. Idempotent.
 * While a zz9k_call on an armed context is blocked it holds the shared
 * mailbox lock, so a slow or wedged operation serializes all mailbox
 * clients until it completes or times out. The hard bound is the context's
 * wall-clock offload budget when set (zz9k_set_offload_timeout_ms);
 * otherwise ENV:ZZ9K_SYNC_WAIT_TIMEOUT_MS (default 5s), read once per
 * context.
 * Raw contexts returned by zz9k_open and zz9k_attach_mailbox remain unarmed
 * by default. Call this function explicitly only when one task owns the
 * context for its entire armed lifetime. Shared library/provider contexts
 * whose calls may arrive from different tasks must keep the polling path.
 */
int zz9k_arm_completion_irq(ZZ9KContext *ctx);

/* Reverse of zz9k_arm_completion_irq. Safe on an unarmed context. */
void zz9k_disarm_completion_irq(ZZ9KContext *ctx);

int zz9k_completion_irq_ack(ZZ9KContext *ctx);
int zz9k_interrupt_status(ZZ9KContext *ctx, uint16_t *status);
int zz9k_submit(ZZ9KContext *ctx, ZZ9KRequest *request,
                uint32_t *request_id);
int zz9k_submit_batch(ZZ9KContext *ctx, ZZ9KRequest *requests,
                      uint32_t count, uint32_t *request_ids,
                      uint32_t *submitted);
int zz9k_poll(ZZ9KContext *ctx, ZZ9KMailboxEntry *reply);
int zz9k_poll_batch(ZZ9KContext *ctx, ZZ9KMailboxEntry *replies,
                    uint32_t capacity, uint32_t *completed);
int zz9k_call(ZZ9KContext *ctx, ZZ9KRequest *request, ZZ9KMailboxEntry *reply,
              uint32_t timeout_ticks);

/* Read a positive decimal environment variable (AmigaOS: GetVar on ENV:;
   host: getenv). Returns `fallback` when unset, non-numeric, or <= 0. */
uint32_t zz9k_env_u32(const char *name, uint32_t fallback);

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_HOST_H */

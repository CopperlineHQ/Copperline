/*
 * ZZ9000 SDK v2 shared ABI definitions.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_ABI_H
#define ZZ9K_ABI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ZZ9K_ABI_MAGIC              0x5a5a394bUL /* "ZZ9K" */
#define ZZ9K_ABI_VERSION_MAJOR      2U
#define ZZ9K_ABI_VERSION_MINOR      0U

#define ZZ9K_MAILBOX_ENTRY_SIZE     64U
#define ZZ9K_MAILBOX_DESCRIPTOR_SIZE 128U
#define ZZ9K_INVALID_HANDLE         0xffffffffUL
#define ZZ9K_SURFACE_HANDLE_FRAMEBUFFER 0x80000000UL
#define ZZ9K_MEDIA_NO_PTS           UINT64_C(0xffffffffffffffff)
#define ZZ9K_MEDIA_PTS_MODULUS      (UINT64_C(1) << 33)

typedef struct ZZ9KMediaClock {
  uint64_t ticks;
  uint64_t remainder;
} ZZ9KMediaClock;

#define ZZ9K_MNT_MANUFACTURER       0x6d6eU
#define ZZ9K_PRODUCT_Z2             3U
#define ZZ9K_PRODUCT_Z3             4U

#define ZZ9K_ARM_MEMORY_START       0x00200000UL
#define ZZ9K_AMIGA_MEMORY_OFFSET    0x00010000UL
#define ZZ9K_AMIGA_MEMORY_LIMIT     0x10000000UL
#define ZZ9K_ARM_MEMORY_VISIBLE_END \
  (ZZ9K_ARM_MEMORY_START + (ZZ9K_AMIGA_MEMORY_LIMIT - ZZ9K_AMIGA_MEMORY_OFFSET))

/*
 * Legacy firmware-serviced board window. The current ZZ9000 firmware already
 * maps board offsets 0xa000..0xffff to this ARM buffer for SD/USB proxy I/O.
 * SDK v2 keeps its bootstrap mailbox inside the upper part of that window so
 * early discovery does not depend on generic DDR reads from the Amiga side.
 */
#define ZZ9K_MAPPED_IO_ARM_START    0x3FE40000UL
#define ZZ9K_MAPPED_IO_BOARD_OFFSET 0x0000A000UL
#define ZZ9K_MAPPED_IO_WINDOW_SIZE  0x00006000UL
#define ZZ9K_SDK_MAILBOX_BOARD_OFFSET 0x0000D000UL
#define ZZ9K_SDK_MAILBOX_ARM_ADDRESS \
  (ZZ9K_MAPPED_IO_ARM_START + \
   (ZZ9K_SDK_MAILBOX_BOARD_OFFSET - ZZ9K_MAPPED_IO_BOARD_OFFSET))

/*
 * SDK v2 bootstrap registers. Reads use the low board offsets below so older
 * firmware can expose the mailbox through the ARM-serviced request path.
 * Write doorbells must use the Z3 HDL register aperture when advertised.
 */
#define ZZ9K_Z3_REGISTER_WINDOW_OFFSET 0x00001000U
#define ZZ9K_REG_CONFIG             0x0004U
#define ZZ9K_REG_SDK_MAGIC          0x0100U
#define ZZ9K_REG_SDK_VERSION        0x0102U
#define ZZ9K_REG_SDK_MAILBOX_HI     0x0104U
#define ZZ9K_REG_SDK_MAILBOX_LO     0x0106U
#define ZZ9K_REG_SDK_DOORBELL       0x0108U
#define ZZ9K_REG_SDK_STATUS         0x010aU
#define ZZ9K_REG_SDK_IRQ_ACK        0x010cU
#define ZZ9K_REG_SDK_DIAG_WRITE     0x0110U
#define ZZ9K_REG_SDK_DIAG_DATA      0x0114U
#define ZZ9K_REG_SDK_DIAG_Z3ADDR    0x0118U
#define ZZ9K_REG_SDK_MAGIC_VALUE    0x5a39U

/* Direct FPGA bootstrap pair. Reads return the high/low halves of the
 * compile-time aperture descriptor. After reserving every advertised region,
 * the RTG driver writes the ACK token to the low half; SDK clients only read
 * and validate the acknowledged mailbox layout. Board offsets are 0x111c and
 * 0x111e once the 0x1000 direct-register window base is included. */
#define ZZ9K_REG_APERTURE_INFO_HI    0x011cU
#define ZZ9K_REG_APERTURE_INFO_LO_ACK 0x011eU
#define ZZ9K_APERTURE_ACK_TOKEN      0xa501U
#define ZZ9K_APERTURE_INFO_LEGACY    0x00000000UL
#define ZZ9K_APERTURE_INFO_2M        0x5a010502UL
#define ZZ9K_APERTURE_INFO_4M        0x5a010704UL
#define ZZ9K_APERTURE_INFO_8M        0x5a010708UL

/*
 * ZZ9000.CFG query interface (firmware ABI >= 2.3): write a key id to
 * CONFIG_KEY, then read the value back from CONFIG_KEY and the
 * "present in the config file" flag from CONFIG_PRESENT. On older
 * firmware the group reads as zero, so every key reports absent.
 */
#define ZZ9K_REG_CONFIG_KEY         0x00e8U
#define ZZ9K_REG_CONFIG_PRESENT     0x00eaU
#define ZZ9K_CFG_KEY_INT2           5U

#define ZZ9K_INTERRUPT_ETH          0x0001U
#define ZZ9K_INTERRUPT_AUDIO        0x0002U
#define ZZ9K_INTERRUPT_VBLANK       0x0004U
#define ZZ9K_INTERRUPT_SDK          0x0008U
#define ZZ9K_CONFIG_ACK_MODE        0x0008U
#define ZZ9K_CONFIG_ACK_ETH         0x0010U
#define ZZ9K_CONFIG_ACK_AUDIO       0x0020U
#define ZZ9K_CONFIG_ACK_VBLANK      0x0040U
#define ZZ9K_CONFIG_ACK_SDK         0x0080U

#define ZZ9K_SDK_IRQ_ACK_VALUE      0x0001U
#define ZZ9K_SDK_IRQ_ENABLE_VALUE   0x0002U
#define ZZ9K_SDK_IRQ_DISABLE_VALUE  0x0004U

enum ZZ9KStatus {
  ZZ9K_STATUS_OK = 0,
  ZZ9K_STATUS_QUEUED = 1,
  ZZ9K_STATUS_BUSY = 2,
  ZZ9K_STATUS_UNSUPPORTED = 3,
  ZZ9K_STATUS_BAD_REQUEST = 4,
  ZZ9K_STATUS_BAD_HANDLE = 5,
  ZZ9K_STATUS_NO_MEMORY = 6,
  ZZ9K_STATUS_TIMEOUT = 7,
  ZZ9K_STATUS_CANCELLED = 8,
  ZZ9K_STATUS_IO_ERROR = 9,
  ZZ9K_STATUS_NOT_FOUND = 10,
  ZZ9K_STATUS_INTERNAL_ERROR = 0xffff
};

enum ZZ9KEntryFlags {
  ZZ9K_ENTRY_INLINE_PAYLOAD = 1U << 0,
  ZZ9K_ENTRY_BUFFER_PAYLOAD = 1U << 1,
  ZZ9K_ENTRY_ASYNC = 1U << 2,
  ZZ9K_ENTRY_NEEDS_IRQ = 1U << 3,
  ZZ9K_ENTRY_CANCEL_REQUEST = 1U << 4
};

enum ZZ9KService {
  ZZ9K_SERVICE_CORE = 0x0000,
  ZZ9K_SERVICE_MEMORY = 0x0100,
  ZZ9K_SERVICE_SURFACE = 0x0200,
  ZZ9K_SERVICE_GFX = 0x0300,
  ZZ9K_SERVICE_IMAGE = 0x0400,
  ZZ9K_SERVICE_AUDIO = 0x0500,
  ZZ9K_SERVICE_CODEC = 0x0600,
  ZZ9K_SERVICE_STORAGE = 0x0700,
  ZZ9K_SERVICE_CRYPTO = 0x0800,
  ZZ9K_SERVICE_DIAG = 0x0900,
  ZZ9K_SERVICE_MODULE = 0x0a00,
  ZZ9K_SERVICE_VIDEO = 0x0b00,
  ZZ9K_SERVICE_VENDOR = 0x8000
};

enum ZZ9KOpcode {
  ZZ9K_OP_NOP = ZZ9K_SERVICE_CORE + 0x00,
  ZZ9K_OP_QUERY_CAPS = ZZ9K_SERVICE_CORE + 0x01,
  ZZ9K_OP_PING = ZZ9K_SERVICE_CORE + 0x02,
  ZZ9K_OP_CANCEL = ZZ9K_SERVICE_CORE + 0x03,
  ZZ9K_OP_QUERY_SERVICE = ZZ9K_SERVICE_CORE + 0x04,
  ZZ9K_OP_QUERY_APERTURE_LAYOUT = ZZ9K_SERVICE_CORE + 0x05,

  ZZ9K_OP_ALLOC_SHARED = ZZ9K_SERVICE_MEMORY + 0x00,
  ZZ9K_OP_FREE_SHARED = ZZ9K_SERVICE_MEMORY + 0x01,
  ZZ9K_OP_MEM_FILL = ZZ9K_SERVICE_MEMORY + 0x02,
  ZZ9K_OP_MEM_COPY = ZZ9K_SERVICE_MEMORY + 0x03,

  ZZ9K_OP_ALLOC_SURFACE = ZZ9K_SERVICE_SURFACE + 0x00,
  ZZ9K_OP_FREE_SURFACE = ZZ9K_SERVICE_SURFACE + 0x01,
  ZZ9K_OP_MAP_FRAMEBUFFER_SURFACE = ZZ9K_SERVICE_SURFACE + 0x02,
  ZZ9K_OP_FILL_SURFACE = ZZ9K_SERVICE_SURFACE + 0x03,
  ZZ9K_OP_COPY_SURFACE = ZZ9K_SERVICE_SURFACE + 0x04,
  ZZ9K_OP_QUERY_PALETTE = ZZ9K_SERVICE_SURFACE + 0x05,

  ZZ9K_OP_SCALE_IMAGE = ZZ9K_SERVICE_IMAGE + 0x00,
  ZZ9K_OP_DECODE_JPEG = ZZ9K_SERVICE_IMAGE + 0x01,
  ZZ9K_OP_DECODE_PNG = ZZ9K_SERVICE_IMAGE + 0x02,
  ZZ9K_OP_DECODE_GIF = ZZ9K_SERVICE_IMAGE + 0x03,
  ZZ9K_OP_IMAGE_SESSION_BEGIN = ZZ9K_SERVICE_IMAGE + 0x04,
  ZZ9K_OP_IMAGE_SESSION_FEED = ZZ9K_SERVICE_IMAGE + 0x05,
  ZZ9K_OP_IMAGE_SESSION_CLOSE = ZZ9K_SERVICE_IMAGE + 0x06,
  ZZ9K_OP_SCALE_IMAGE_CLIPPED = ZZ9K_SERVICE_IMAGE + 0x07,

  ZZ9K_OP_DECODE_MP3 = ZZ9K_SERVICE_AUDIO + 0x00,
  ZZ9K_OP_MIX_AUDIO = ZZ9K_SERVICE_AUDIO + 0x01,
  ZZ9K_OP_RESAMPLE_AUDIO = ZZ9K_SERVICE_AUDIO + 0x02,
  ZZ9K_OP_AUDIO_STREAM_BEGIN = ZZ9K_SERVICE_AUDIO + 0x03,
  ZZ9K_OP_AUDIO_STREAM_FEED = ZZ9K_SERVICE_AUDIO + 0x04,
  ZZ9K_OP_AUDIO_STREAM_READ = ZZ9K_SERVICE_AUDIO + 0x05,
  ZZ9K_OP_AUDIO_STREAM_CLOSE = ZZ9K_SERVICE_AUDIO + 0x06,
  ZZ9K_OP_AUDIO_STREAM_PLAY = ZZ9K_SERVICE_AUDIO + 0x07,
  ZZ9K_OP_AUDIO_STREAM_STOP = ZZ9K_SERVICE_AUDIO + 0x08,

  ZZ9K_OP_DECOMPRESS = ZZ9K_SERVICE_CODEC + 0x00,
  ZZ9K_OP_DECOMPRESS_TEST = ZZ9K_SERVICE_CODEC + 0x01,
  ZZ9K_OP_DECOMPRESS_STREAM_BEGIN = ZZ9K_SERVICE_CODEC + 0x02,
  ZZ9K_OP_DECOMPRESS_STREAM_READ = ZZ9K_SERVICE_CODEC + 0x03,
  ZZ9K_OP_DECOMPRESS_STREAM_CLOSE = ZZ9K_SERVICE_CODEC + 0x04,
  ZZ9K_OP_DECOMPRESS_STREAM_FEED = ZZ9K_SERVICE_CODEC + 0x05,
  ZZ9K_OP_DECOMPRESS_BATCH = ZZ9K_SERVICE_CODEC + 0x06,

  ZZ9K_OP_CRYPTO_HASH = ZZ9K_SERVICE_CRYPTO + 0x00,
  ZZ9K_OP_CRYPTO_STREAM = ZZ9K_SERVICE_CRYPTO + 0x01,
  ZZ9K_OP_CRYPTO_AEAD = ZZ9K_SERVICE_CRYPTO + 0x02,
  ZZ9K_OP_CRYPTO_KX        = ZZ9K_SERVICE_CRYPTO + 0x03,
  ZZ9K_OP_CRYPTO_VERIFY    = ZZ9K_SERVICE_CRYPTO + 0x04,

  ZZ9K_OP_VIDEO_SESSION_BEGIN = ZZ9K_SERVICE_VIDEO + 0x00,
  ZZ9K_OP_VIDEO_SESSION_WRITE = ZZ9K_SERVICE_VIDEO + 0x01,
  ZZ9K_OP_VIDEO_SESSION_DECODE = ZZ9K_SERVICE_VIDEO + 0x02,
  ZZ9K_OP_VIDEO_SESSION_CLOSE = ZZ9K_SERVICE_VIDEO + 0x03,
  ZZ9K_OP_MEDIA_SESSION_BEGIN = ZZ9K_SERVICE_VIDEO + 0x04,
  ZZ9K_OP_MEDIA_SESSION_WRITE = ZZ9K_SERVICE_VIDEO + 0x05,
  ZZ9K_OP_MEDIA_SESSION_DECODE = ZZ9K_SERVICE_VIDEO + 0x06,
  ZZ9K_OP_MEDIA_SESSION_AUDIO_READ = ZZ9K_SERVICE_VIDEO + 0x07,
  ZZ9K_OP_MEDIA_SESSION_PRESENT = ZZ9K_SERVICE_VIDEO + 0x08,
  ZZ9K_OP_MEDIA_SESSION_DISCARD = ZZ9K_SERVICE_VIDEO + 0x09,
  ZZ9K_OP_MEDIA_SESSION_STATUS = ZZ9K_SERVICE_VIDEO + 0x0a,
  ZZ9K_OP_MEDIA_SESSION_AUDIO_BIND = ZZ9K_SERVICE_VIDEO + 0x0b,
  ZZ9K_OP_MEDIA_SESSION_AUDIO_UNBIND = ZZ9K_SERVICE_VIDEO + 0x0c,
  ZZ9K_OP_MEDIA_SESSION_CLOSE = ZZ9K_SERVICE_VIDEO + 0x0d,

  ZZ9K_OP_DIAG_READ = ZZ9K_SERVICE_DIAG + 0x00,
  ZZ9K_OP_DIAG_TIMING = ZZ9K_SERVICE_DIAG + 0x01,
  ZZ9K_OP_DIAG_SCHED = ZZ9K_SERVICE_DIAG + 0x02,
  ZZ9K_OP_DIAG_MEMORY = ZZ9K_SERVICE_DIAG + 0x03
};

enum ZZ9KCapability {
  ZZ9K_CAP_MAILBOX = 1U << 0,
  ZZ9K_CAP_IRQ_COMPLETION = 1U << 1,
  ZZ9K_CAP_SHARED_ALLOC = 1U << 2,
  ZZ9K_CAP_SURFACES = 1U << 3,
  ZZ9K_CAP_FRAMEBUFFER_SURFACE = 1U << 4,
  ZZ9K_CAP_IMAGE_DECODE = 1U << 5,
  ZZ9K_CAP_IMAGE_SCALE = 1U << 6,
  ZZ9K_CAP_AUDIO_DECODE = 1U << 7,
  ZZ9K_CAP_CRYPTO = 1U << 8,
  ZZ9K_CAP_MODULES = 1U << 9,
  ZZ9K_CAP_MEMORY_OPS = 1U << 10,
  ZZ9K_CAP_DIAGNOSTICS = 1U << 11,
  ZZ9K_CAP_DOORBELL = 1U << 12,
  ZZ9K_CAP_POLLING_COMPLETION = 1U << 13,
  ZZ9K_CAP_SERVICE_DISCOVERY = 1U << 14,
  ZZ9K_CAP_SURFACE_OPS = 1U << 15,
  ZZ9K_CAP_COMPRESSION = 1U << 16,
  ZZ9K_CAP_GFX_OPS = 1U << 17,
  ZZ9K_CAP_STORAGE_OPS = 1U << 18,
  ZZ9K_CAP_AUDIO_PLAYBACK = 1U << 19,
  /* Firmware serves ZZ9K_ALLOC_HOST_WINDOW allocations from a small heap
   * that is reachable through the Zorro 2 board window. */
  ZZ9K_CAP_HOST_WINDOW_HEAP = 1U << 20,
  ZZ9K_CAP_VIDEO_DECODE = 1U << 21,
  ZZ9K_CAP_MEDIA_SESSION = 1U << 22,
  /* AUDIO_STREAM_FEED_DRAIN preserves partial compressed input while
   * draining complete frames and the bound playback tail. */
  ZZ9K_CAP_AUDIO_STREAM_DRAIN = 1U << 23,
  /* The firmware publishes an aperture-relative Zorro 2 memory layout; the
   * RTG driver acknowledges it before HOST_WINDOW becomes mappable. */
  ZZ9K_CAP_APERTURE_LAYOUT = 1U << 24
};

#define ZZ9K_APERTURE_LAYOUT_GENERATION_SHIFT 16U
#define ZZ9K_APERTURE_LAYOUT_GENERATION_MASK  0xffff0000UL
#define ZZ9K_APERTURE_LAYOUT_FLAGS_MASK       0x0000ffffUL
#define ZZ9K_APERTURE_LAYOUT_GENERATION_1     1U
#define ZZ9K_APERTURE_PROFILE(generation, flags) \
  ((((uint32_t)(generation)) << ZZ9K_APERTURE_LAYOUT_GENERATION_SHIFT) | \
   ((uint32_t)(flags) & ZZ9K_APERTURE_LAYOUT_FLAGS_MASK))

enum ZZ9KApertureFlags {
  ZZ9K_APERTURE_FLAG_VALID = 1U << 0,
  ZZ9K_APERTURE_FLAG_ACKED = 1U << 1,
  ZZ9K_APERTURE_FLAG_HOST_WINDOW = 1U << 2,
  ZZ9K_APERTURE_FLAG_PIP = 1U << 3
};

enum ZZ9KApertureLayoutState {
  ZZ9K_APERTURE_LAYOUT_LEGACY = 0,
  ZZ9K_APERTURE_LAYOUT_UNACKNOWLEDGED = 1,
  ZZ9K_APERTURE_LAYOUT_ACTIVE = 2,
  ZZ9K_APERTURE_LAYOUT_INVALID = 3
};

/*
 * ZZ9K_OP_ALLOC_SHARED flag bits. HOST_WINDOW asks the firmware to place
 * the buffer in the board-window-reachable heap so a Zorro 2 host can map
 * it (the library strips the bit on Zorro 3, where the whole shared heap
 * is mappable). CARD_ONLY declares that the 68k never touches the buffer
 * contents: the library skips the board-window mapping and leaves
 * ZZ9KSharedBuffer.data NULL, so the buffer is usable by handle only.
 * Generation-1 firmware publishes a board-relative layout and requires a
 * bootstrap acknowledgement before the host heap becomes active. The
 * library accepts only the canonical 2, 4, and software-ready 8 MB profiles,
 * requires the AutoConfig size to match, and validates every returned range.
 * Firmware without ZZ9K_CAP_APERTURE_LAYOUT retains the historical fixed
 * 4 MB behavior; legacy 2 MB HOST_WINDOW requests remain unsupported.
 */
enum ZZ9KAllocFlags {
  ZZ9K_ALLOC_HOST_WINDOW = 1U << 0,
  ZZ9K_ALLOC_CARD_ONLY = 1U << 1
};

#define ZZ9K_HOST_WINDOW_MIN_BOARD_SIZE 0x00400000U /* legacy fixed profile */

enum ZZ9KServiceFlags {
  ZZ9K_SERVICE_FLAG_FIRMWARE = 1U << 0,
  ZZ9K_SERVICE_FLAG_MODULE = 1U << 1,
  ZZ9K_SERVICE_FLAG_ASYNC = 1U << 2,
  ZZ9K_SERVICE_FLAG_ZERO_COPY = 1U << 3,
  ZZ9K_SERVICE_FLAG_SURFACE_PALETTE_QUERY = 1U << 16,
  ZZ9K_SERVICE_FLAG_IMAGE_JPEG_BASELINE = 1U << 16,
  ZZ9K_SERVICE_FLAG_IMAGE_JPEG_PROGRESSIVE = 1U << 17,
  ZZ9K_SERVICE_FLAG_IMAGE_JPEG_DIRECT_BGRA = 1U << 18,
  ZZ9K_SERVICE_FLAG_IMAGE_JPEG_SCALING = 1U << 19,
  ZZ9K_SERVICE_FLAG_IMAGE_STREAMING_INPUT = 1U << 20,
  ZZ9K_SERVICE_FLAG_IMAGE_TILE_OUTPUT = 1U << 21,
  ZZ9K_SERVICE_FLAG_IMAGE_FRAMEBUFFER_OUTPUT = 1U << 22,
  ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BILINEAR = 1U << 23,
  ZZ9K_SERVICE_FLAG_IMAGE_SCALE_CLIPPED = 1U << 24,
  ZZ9K_SERVICE_FLAG_IMAGE_PNG_DIRECT_BGRA = 1U << 25,
  ZZ9K_SERVICE_FLAG_IMAGE_RGB888_OUTPUT = 1U << 26,
  ZZ9K_SERVICE_FLAG_IMAGE_SCALE_BGRA_TO_RGB555_RGB565 = 1U << 27,

  ZZ9K_SERVICE_FLAG_AUDIO_MP3_DECODE = 1U << 16,
  ZZ9K_SERVICE_FLAG_AUDIO_PCM_MIX = 1U << 17,
  ZZ9K_SERVICE_FLAG_AUDIO_RESAMPLE = 1U << 18,
  ZZ9K_SERVICE_FLAG_AUDIO_PCM16_STEREO = 1U << 19,
  ZZ9K_SERVICE_FLAG_AUDIO_MP3_STREAM = 1U << 20,

  ZZ9K_SERVICE_FLAG_VIDEO_MPEG1 = 1U << 16,
  ZZ9K_SERVICE_FLAG_VIDEO_MPEG_PS = 1U << 17,
  ZZ9K_SERVICE_FLAG_VIDEO_DIRECT_OVERLAY = 1U << 18,
  ZZ9K_SERVICE_FLAG_VIDEO_STREAMING_INPUT = 1U << 19,
  ZZ9K_SERVICE_FLAG_VIDEO_CORE1 = 1U << 20,
  ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_SESSION = 1U << 21,
  ZZ9K_SERVICE_FLAG_VIDEO_MEDIA_MP2 = 1U << 22,
  ZZ9K_SERVICE_FLAG_VIDEO_EXPLICIT_PRESENT = 1U << 23,
  ZZ9K_SERVICE_FLAG_VIDEO_TIMELINE_90KHZ = 1U << 24,
  ZZ9K_SERVICE_FLAG_VIDEO_PCM_RING_STATUS = 1U << 25,
  ZZ9K_SERVICE_FLAG_VIDEO_AUDIO_BIND = 1U << 26,

  ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_RAW = 1U << 16,
  ZZ9K_SERVICE_FLAG_CODEC_ZLIB = 1U << 17,
  ZZ9K_SERVICE_FLAG_CODEC_GZIP = 1U << 18,
  ZZ9K_SERVICE_FLAG_CODEC_LZ4_BLOCK = 1U << 19,
  ZZ9K_SERVICE_FLAG_CODEC_LZMA_ALONE = 1U << 20,
  ZZ9K_SERVICE_FLAG_CODEC_CHECKSUM = 1U << 21,
  ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_TEST = 1U << 22,
  ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_STREAM = 1U << 23,
  ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_FEED = 1U << 24,
  ZZ9K_SERVICE_FLAG_CODEC_DEFLATE_FEED = 1U << 25,
  ZZ9K_SERVICE_FLAG_CODEC_ZLIB_FEED = 1U << 26,
  ZZ9K_SERVICE_FLAG_CODEC_GZIP_FEED = 1U << 27,
  ZZ9K_SERVICE_FLAG_CODEC_LZMA2 = 1U << 28,
  ZZ9K_SERVICE_FLAG_CODEC_LZH = 1U << 29,
  ZZ9K_SERVICE_FLAG_CODEC_DECOMPRESS_BATCH = 1U << 30,

  ZZ9K_SERVICE_FLAG_CRYPTO_X25519     = 1U << 16,
  ZZ9K_SERVICE_FLAG_CRYPTO_P256       = 1U << 17,
  ZZ9K_SERVICE_FLAG_CRYPTO_ECDSA_P256 = 1U << 18,
  ZZ9K_SERVICE_FLAG_CRYPTO_RSA_2048   = 1U << 19,
  ZZ9K_SERVICE_FLAG_CRYPTO_AES_GCM    = 1U << 20,
  /* P-256 keygen (scalar*G -> full 65-byte point) via the KX op's KEYGEN flag.
   * Distinct from CRYPTO_P256 (derive only): v2.2.0 advertises derive without
   * keygen, so the provider must gate ECDHE keygen offload on this bit. */
  ZZ9K_SERVICE_FLAG_CRYPTO_P256_KEYGEN = 1U << 21
};

enum ZZ9KAudioSampleFormat {
  ZZ9K_AUDIO_SAMPLE_FORMAT_NONE = 0,
  ZZ9K_AUDIO_SAMPLE_FORMAT_S16LE = 1,
  ZZ9K_AUDIO_SAMPLE_FORMAT_S16BE = 2
};

enum ZZ9KAudioDecodeFlags {
  ZZ9K_AUDIO_DECODE_FLAG_EXPECT_END = 1U << 0
};

enum ZZ9KAudioDecodeResultFlags {
  ZZ9K_AUDIO_DECODE_RESULT_END = 1U << 0
};

typedef struct ZZ9KBufferPayload {
  uint32_t handle;
  uint32_t offset;
  uint32_t length;
  uint32_t aux[9];
} ZZ9KBufferPayload;

/*
 * Inline service payloads are stored as big-endian byte arrays. This keeps
 * the mailbox wire format identical on m68k, ARM, and native test hosts.
 */
typedef struct ZZ9KAllocSharedPayload {
  uint8_t length[4];
  uint8_t alignment[4];
  uint8_t flags[4];
  uint8_t reserved[36];
} ZZ9KAllocSharedPayload;

typedef struct ZZ9KSharedBufferInfoPayload {
  uint8_t handle[4];
  uint8_t arm_addr[4];
  uint8_t length[4];
  uint8_t flags[4];
  uint8_t reserved[32];
} ZZ9KSharedBufferInfoPayload;

typedef struct ZZ9KFreeSharedPayload {
  uint8_t handle[4];
  uint8_t reserved[44];
} ZZ9KFreeSharedPayload;

typedef struct ZZ9KMemFillPayload {
  uint8_t handle[4];
  uint8_t offset[4];
  uint8_t length[4];
  uint8_t value;
  uint8_t reserved[35];
} ZZ9KMemFillPayload;

typedef struct ZZ9KMemCopyPayload {
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t length[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KMemCopyPayload;

typedef struct ZZ9KDiagPayload {
  uint8_t requests_completed[4];
  uint8_t requests_failed[4];
  uint8_t last_status[4];
  uint8_t pending_requests[4];
  uint8_t shared_buffers_used[4];
  uint8_t shared_heap_total[4];
  uint8_t shared_heap_free[4];
  uint8_t shared_heap_largest_free[4];
  uint8_t mailbox_arm_addr[4];
  uint8_t mailbox_ring_entries[4];
  uint8_t surfaces_used[4];
  uint8_t allocator_invalid_slots[4];
} ZZ9KDiagPayload;

typedef struct ZZ9KDiagMemoryPayload {
  uint8_t version[4];
  uint8_t layout_state[4];
  uint8_t aperture_size[4];
  uint8_t aperture_info[4];
  uint8_t host_board_base[4];
  uint8_t host_arm_base[4];
  uint8_t host_total[4];
  uint8_t host_free[4];
  uint8_t host_largest_free[4];
  uint8_t allocations[4];
  uint8_t allocator_invalid_slots[4];
  uint8_t reserved[4];
} ZZ9KDiagMemoryPayload;

typedef struct ZZ9KApertureLayoutPayload {
  uint8_t profile[4];
  uint8_t aperture_size[4];
  uint8_t framebuffer_base[4];
  uint8_t framebuffer_size[4];
  uint8_t pip_base[4];
  uint8_t pip_size[4];
  uint8_t template_base[4];
  uint8_t template_size[4];
  uint8_t host_base[4];
  uint8_t host_size[4];
  uint8_t audio_base[4];
  uint8_t audio_size[4];
} ZZ9KApertureLayoutPayload;

typedef struct ZZ9KDiagTimingPayload {
  uint8_t version[4];
  uint8_t timer_hz[4];
  uint8_t requests_timed[4];
  uint8_t total_us[4];
  uint8_t surface_requests[4];
  uint8_t surface_us[4];
  uint8_t audio_requests[4];
  uint8_t audio_us[4];
  uint8_t last_opcode[4];
  uint8_t last_us[4];
  uint8_t max_opcode[4];
  uint8_t max_us[4];
} ZZ9KDiagTimingPayload;

/* DIAG_SCHED (0x0902): dual-core scheduler observability. core1_online is 1 when
 * the core-1 worker is up (else single-core fallback); tasks_on_core{1,0} count
 * crypto tasks executed on each core (actual execution core, not dispatch). */
typedef struct ZZ9KDiagSchedPayload {
  uint8_t version[4];
  uint8_t core1_online[4];
  uint8_t tasks_on_core1[4];
  uint8_t tasks_on_core0[4];
  uint8_t decode_requests[4];  /* version 2+: decompress decode count */
  uint8_t decode_us[4];        /* version 2+: cumulative decode microseconds */
} ZZ9KDiagSchedPayload;

/* The version-1 base payload (core1_online + tasks_on_core{1,0}). Firmware that
 * predates the decode-timing counters sends exactly this; version 2+ appends the
 * decode_* fields. Decoders require the base and read the extension when present. */
#define ZZ9K_DIAG_SCHED_PAYLOAD_V1_BYTES 16U

typedef char ZZ9KDiagSchedPayload_must_be_24_bytes[
  (sizeof(ZZ9KDiagSchedPayload) == 24U) ? 1 : -1];

typedef struct ZZ9KQueryServicePayload {
  uint8_t service_id[4];
  uint8_t reserved[44];
} ZZ9KQueryServicePayload;

typedef struct ZZ9KServiceInfoPayload {
  uint8_t service_id[4];
  uint8_t version[4];
  uint8_t capability_bits[4];
  uint8_t flags[4];
  uint8_t opcode_base[4];
  uint8_t opcode_count[4];
  uint8_t max_inline_payload[4];
  uint8_t name[20];
} ZZ9KServiceInfoPayload;

typedef struct ZZ9KSurfaceInfoPayload {
  uint8_t handle[4];
  uint8_t arm_addr[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t pitch[4];
  uint8_t format[4];
  uint8_t flags[4];
  uint8_t length[4];
  uint8_t reserved[16];
} ZZ9KSurfaceInfoPayload;

typedef struct ZZ9KAllocSurfacePayload {
  uint8_t width[4];
  uint8_t height[4];
  uint8_t format[4];
  uint8_t flags[4];
  uint8_t pitch[4];
  uint8_t reserved[28];
} ZZ9KAllocSurfacePayload;

typedef struct ZZ9KQueryPalettePayload {
  uint8_t surface[4];
  uint8_t start[4];
  uint8_t count[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KQueryPalettePayload;

typedef struct ZZ9KFreeSurfacePayload {
  uint8_t handle[4];
  uint8_t reserved[44];
} ZZ9KFreeSurfacePayload;

typedef struct ZZ9KScaleImagePayload {
  uint8_t src_surface[4];
  uint8_t dst_surface[4];
  uint8_t src_x[4];
  uint8_t src_y[4];
  uint8_t src_w[4];
  uint8_t src_h[4];
  uint8_t dst_x[4];
  uint8_t dst_y[4];
  uint8_t dst_w[4];
  uint8_t dst_h[4];
  uint8_t filter[4];
  uint8_t flags[4];
} ZZ9KScaleImagePayload;

typedef struct ZZ9KScaleImageClippedPayload {
  uint8_t src_surface[4];
  uint8_t dst_surface[4];
  uint8_t src_x[2];
  uint8_t src_y[2];
  uint8_t src_w[2];
  uint8_t src_h[2];
  uint8_t dst_x[2];
  uint8_t dst_y[2];
  uint8_t dst_w[2];
  uint8_t dst_h[2];
  uint8_t clip_x[2];
  uint8_t clip_y[2];
  uint8_t clip_w[2];
  uint8_t clip_h[2];
  uint8_t filter[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KScaleImageClippedPayload;

typedef struct ZZ9KSurfaceFillPayload {
  uint8_t surface[4];
  uint8_t x[4];
  uint8_t y[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t color[4];
  uint8_t flags[4];
  uint8_t reserved[20];
} ZZ9KSurfaceFillPayload;

typedef struct ZZ9KSurfaceCopyPayload {
  uint8_t src_surface[4];
  uint8_t dst_surface[4];
  uint8_t src_x[4];
  uint8_t src_y[4];
  uint8_t dst_x[4];
  uint8_t dst_y[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t flags[4];
  uint8_t reserved[12];
} ZZ9KSurfaceCopyPayload;

typedef struct ZZ9KImageDecodePayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_surface[4];
  uint8_t dst_x[4];
  uint8_t dst_y[4];
  uint8_t dst_width[4];
  uint8_t dst_height[4];
  uint8_t output_format[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KImageDecodePayload;

typedef struct ZZ9KImageDecodeResultPayload {
  uint8_t width[4];
  uint8_t height[4];
  uint8_t output_format[4];
  uint8_t flags[4];
  uint8_t bytes_written[4];
  uint8_t reserved[28];
} ZZ9KImageDecodeResultPayload;

typedef struct ZZ9KImageSessionBeginPayload {
  uint8_t codec[4];
  uint8_t output_mode[4];
  uint8_t dst_surface[4];
  uint8_t dst_x[4];
  uint8_t dst_y[4];
  uint8_t dst_width[4];
  uint8_t dst_height[4];
  uint8_t output_format[4];
  uint8_t tile_handle[4];
  uint8_t tile_stride[4];
  uint8_t tile_rows[4];
  uint8_t flags[4];
} ZZ9KImageSessionBeginPayload;

typedef struct ZZ9KImageSessionFeedPayload {
  uint8_t session[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KImageSessionFeedPayload;

typedef struct ZZ9KImageSessionResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t image_width[4];
  uint8_t image_height[4];
  uint8_t output_format[4];
  uint8_t tile_x[4];
  uint8_t tile_y[4];
  uint8_t tile_width[4];
  uint8_t tile_height[4];
  uint8_t bytes_consumed[4];
  uint8_t bytes_written[4];
  uint8_t flags[4];
} ZZ9KImageSessionResultPayload;

typedef struct ZZ9KImageSessionClosePayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KImageSessionClosePayload;

typedef struct ZZ9KAudioDecodePayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t dst_capacity[4];
  uint8_t output_hz[4];
  uint8_t output_channels[4];
  uint8_t output_format[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KAudioDecodePayload;

typedef struct ZZ9KAudioDecodeResultPayload {
  uint8_t bytes_consumed[4];
  uint8_t bytes_written[4];
  uint8_t sample_rate[4];
  uint8_t channels[4];
  uint8_t sample_format[4];
  uint8_t frames_written[4];
  uint8_t flags[4];
  uint8_t reserved[20];
} ZZ9KAudioDecodeResultPayload;

typedef struct ZZ9KAudioStreamBeginPayload {
  uint8_t mp3_ring_handle[4];
  uint8_t mp3_ring_capacity[4];
  uint8_t pcm_ring_handle[4];
  uint8_t pcm_ring_capacity[4];
  uint8_t output_hz[4];
  uint8_t output_channels[4];
  uint8_t output_format[4];
  uint8_t low_water_bytes[4];
  uint8_t high_water_bytes[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KAudioStreamBeginPayload;

typedef struct ZZ9KAudioStreamFeedPayload {
  uint8_t session[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KAudioStreamFeedPayload;

typedef struct ZZ9KAudioStreamReadPayload {
  uint8_t session[4];
  uint8_t pcm_read[4];
  uint8_t flags[4];
  uint8_t reserved[36];
} ZZ9KAudioStreamReadPayload;

typedef struct ZZ9KAudioStreamClosePayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KAudioStreamClosePayload;

typedef struct ZZ9KAudioStreamPlayPayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KAudioStreamPlayPayload;

typedef struct ZZ9KAudioStreamStopPayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KAudioStreamStopPayload;

typedef struct ZZ9KAudioStreamResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t sample_rate[4];
  uint8_t channels[4];
  uint8_t sample_format[4];
  uint8_t mp3_read[4];
  uint8_t pcm_write[4];
  uint8_t pcm_read[4];
  uint8_t frames_decoded[4];
  uint8_t bytes_consumed[4];
  uint8_t bytes_produced[4];
  uint8_t flags[4];
} ZZ9KAudioStreamResultPayload;

/* Decoder identity and immutable geometry are fixed at BEGIN. DECODE publishes
 * a decoder-owned frame to the active P96 overlay; client-visible bitmap
 * addresses and pitches are deliberately not part of this contract. */
typedef struct ZZ9KVideoSessionBeginPayload {
  uint8_t codec[4];
  uint8_t container[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t output_format[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KVideoSessionBeginPayload;

typedef struct ZZ9KVideoSessionWritePayload {
  uint8_t session[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KVideoSessionWritePayload;

typedef struct ZZ9KVideoSessionDecodePayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KVideoSessionDecodePayload;

typedef struct ZZ9KVideoSessionClosePayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KVideoSessionClosePayload;

typedef struct ZZ9KVideoSessionResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t frame_rate_milli[4];
  uint8_t frame_number[4];
  uint8_t frame_time_millis[4];
  uint8_t bytes_accepted[4];
  uint8_t bytes_written[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KVideoSessionResultPayload;

/* Additive media sessions leave the legacy VIDEO_SESSION_* wire contract
 * untouched. The MP2/ring fields are reserved in U2 so later firmware can
 * enable them without changing BEGIN; requesting MP2 from U2 firmware returns
 * UNSUPPORTED and its service flags remain clear. */
typedef struct ZZ9KMediaSessionBeginPayload {
  uint8_t video_codec[4];
  uint8_t container[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t output_format[4];
  uint8_t audio_codec[4];
  uint8_t pcm_ring_handle[4];
  uint8_t pcm_ring_capacity[4];
  uint8_t pcm_low_water_bytes[4];
  uint8_t pcm_high_water_bytes[4];
  uint8_t flags[4];
  uint8_t reserved[4];
} ZZ9KMediaSessionBeginPayload;

typedef struct ZZ9KMediaSessionWritePayload {
  uint8_t session[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KMediaSessionWritePayload;

/* DECODE/PRESENT/DISCARD/CLOSE and the future AUDIO_READ/ACK and bind
 * operations share this fixed command shape. value is an operation-specific
 * unsigned 64-bit cursor; it is zero for the U2 video-only operations. */
typedef struct ZZ9KMediaSessionCommandPayload {
  uint8_t session[4];
  uint8_t value_hi[4];
  uint8_t value_lo[4];
  uint8_t flags[4];
  uint8_t reserved[32];
} ZZ9KMediaSessionCommandPayload;

typedef struct ZZ9KMediaSessionStatusPayload {
  uint8_t session[4];
  uint8_t page[4];
  uint8_t flags[4];
  uint8_t reserved[36];
} ZZ9KMediaSessionStatusPayload;

typedef struct ZZ9KMediaSessionMainResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t width[4];
  uint8_t height[4];
  uint8_t frame_rate_num[4];
  uint8_t frame_rate_den[4];
  uint8_t frame_number[4];
  uint8_t video_pts_hi[4];
  uint8_t video_pts_lo[4];
  uint8_t bytes_accepted[4];
  uint8_t bytes_written[4];
  uint8_t flags[4];
} ZZ9KMediaSessionMainResultPayload;

typedef struct ZZ9KMediaSessionAudioResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t sample_rate[4];
  uint8_t channels[4];
  uint8_t sample_format[4];
  uint8_t pcm_produced_hi[4];
  uint8_t pcm_produced_lo[4];
  uint8_t pcm_acknowledged_hi[4];
  uint8_t pcm_acknowledged_lo[4];
  uint8_t audio_pts_hi[4];
  uint8_t audio_pts_lo[4];
  uint8_t flags[4];
} ZZ9KMediaSessionAudioResultPayload;

/* STATUS is paged so future counters do not grow the 48-byte mailbox entry.
 * The timing page exposes, in order: first valid PTS origin, current video
 * PTS, current audio PTS, and the most recently observed raw 33-bit PTS. */
typedef struct ZZ9KMediaSessionStatusResultPayload {
  uint8_t session[4];
  uint8_t state[4];
  uint8_t page[4];
  uint8_t flags[4];
  uint8_t value0_hi[4];
  uint8_t value0_lo[4];
  uint8_t value1_hi[4];
  uint8_t value1_lo[4];
  uint8_t value2_hi[4];
  uint8_t value2_lo[4];
  uint8_t value3_hi[4];
  uint8_t value3_lo[4];
} ZZ9KMediaSessionStatusResultPayload;

typedef struct ZZ9KCryptoHashPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t key_handle[4];
  uint8_t key_offset[4];
  uint8_t key_length[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[8];
} ZZ9KCryptoHashPayload;

typedef struct ZZ9KCryptoStreamPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t key_handle[4];
  uint8_t key_offset[4];
  uint8_t nonce_handle[4];
  uint8_t nonce_offset[4];
  uint8_t counter[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
} ZZ9KCryptoStreamPayload;

typedef struct ZZ9KCryptoAeadPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t aad_handle[4];
  uint8_t aad_offset[4];
  uint8_t aad_length[4];
  uint8_t key_handle[4];
  uint8_t key_offset[4];
  uint8_t nonce_handle[4];
  uint8_t flags[4];
} ZZ9KCryptoAeadPayload;

typedef struct ZZ9KCryptoResultPayload {
  uint8_t bytes_written[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[36];
} ZZ9KCryptoResultPayload;

struct ZZ9KCryptoKxPayload {
  uint8_t scalar_handle[4];
  uint8_t scalar_offset[4];
  uint8_t point_handle[4];
  uint8_t point_offset[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[16];
};

typedef struct ZZ9KCryptoVerifyPayload {
  uint8_t algorithm[4];
  uint8_t hash_handle[4];
  uint8_t hash_offset[4];
  uint8_t hash_length[4];
  uint8_t sig_handle[4];
  uint8_t sig_offset[4];
  uint8_t sig_length[4];
  uint8_t key_handle[4];
  uint8_t key_offset[4];
  uint8_t key_length[4];
  uint8_t reserved[8];
} ZZ9KCryptoVerifyPayload;

typedef struct ZZ9KDecompressPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t dst_capacity[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[16];
} ZZ9KDecompressPayload;

typedef struct ZZ9KDecompressTestPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t output_limit[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KDecompressTestPayload;

typedef struct ZZ9KDecompressStreamBeginPayload {
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t output_limit[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KDecompressStreamBeginPayload;

typedef struct ZZ9KDecompressStreamReadPayload {
  uint8_t session[4];
  uint8_t dst_handle[4];
  uint8_t dst_offset[4];
  uint8_t dst_capacity[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KDecompressStreamReadPayload;

typedef struct ZZ9KDecompressStreamFeedPayload {
  uint8_t session[4];
  uint8_t src_handle[4];
  uint8_t src_offset[4];
  uint8_t src_length[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KDecompressStreamFeedPayload;

typedef struct ZZ9KDecompressStreamClosePayload {
  uint8_t session[4];
  uint8_t flags[4];
  uint8_t reserved[40];
} ZZ9KDecompressStreamClosePayload;

typedef struct ZZ9KDecompressResultPayload {
  uint8_t bytes_consumed[4];
  uint8_t bytes_written[4];
  uint8_t checksum[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[28];
} ZZ9KDecompressResultPayload;

typedef struct ZZ9KDecompressStreamResultPayload {
  uint8_t session[4];
  uint8_t bytes_consumed[4];
  uint8_t bytes_written[4];
  uint8_t checksum[4];
  uint8_t algorithm[4];
  uint8_t flags[4];
  uint8_t reserved[24];
} ZZ9KDecompressStreamResultPayload;

/* Batched LZH decode: the 48-byte inline payload only references the
   self-describing arena (one shared buffer); everything else -- the
   member table, compressed blob, optional output region, and per-member
   results -- lives inside the arena itself (see the ZZ9K_BATCH_* layout
   constants). */
typedef struct ZZ9KDecompressBatchPayload {
  uint8_t arena_handle[4];
  uint8_t arena_offset[4];
  uint8_t arena_length[4];
  uint8_t reserved[36];
} ZZ9KDecompressBatchPayload;

typedef struct ZZ9KDecompressBatchResultPayload {
  uint8_t members_total[4];
  uint8_t members_ok[4];
  uint8_t members_failed[4];
  uint8_t flags[4];
  uint8_t reserved[32];
} ZZ9KDecompressBatchResultPayload;

typedef char ZZ9KAllocSharedPayload_must_be_48_bytes[
  (sizeof(ZZ9KAllocSharedPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KSharedBufferInfoPayload_must_be_48_bytes[
  (sizeof(ZZ9KSharedBufferInfoPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KFreeSharedPayload_must_be_48_bytes[
  (sizeof(ZZ9KFreeSharedPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMemFillPayload_must_be_48_bytes[
  (sizeof(ZZ9KMemFillPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMemCopyPayload_must_be_48_bytes[
  (sizeof(ZZ9KMemCopyPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDiagPayload_must_be_48_bytes[
  (sizeof(ZZ9KDiagPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDiagMemoryPayload_must_be_48_bytes[
  (sizeof(ZZ9KDiagMemoryPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KApertureLayoutPayload_must_be_48_bytes[
  (sizeof(ZZ9KApertureLayoutPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDiagTimingPayload_must_be_48_bytes[
  (sizeof(ZZ9KDiagTimingPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KQueryServicePayload_must_be_48_bytes[
  (sizeof(ZZ9KQueryServicePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KServiceInfoPayload_must_be_48_bytes[
  (sizeof(ZZ9KServiceInfoPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KSurfaceInfoPayload_must_be_48_bytes[
  (sizeof(ZZ9KSurfaceInfoPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KAllocSurfacePayload_must_be_48_bytes[
  (sizeof(ZZ9KAllocSurfacePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KQueryPalettePayload_must_be_48_bytes[
  (sizeof(ZZ9KQueryPalettePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KFreeSurfacePayload_must_be_48_bytes[
  (sizeof(ZZ9KFreeSurfacePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KScaleImagePayload_must_be_48_bytes[
  (sizeof(ZZ9KScaleImagePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KScaleImageClippedPayload_must_be_48_bytes[
  (sizeof(ZZ9KScaleImageClippedPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KSurfaceFillPayload_must_be_48_bytes[
  (sizeof(ZZ9KSurfaceFillPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KSurfaceCopyPayload_must_be_48_bytes[
  (sizeof(ZZ9KSurfaceCopyPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageDecodePayload_must_be_48_bytes[
  (sizeof(ZZ9KImageDecodePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageDecodeResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KImageDecodeResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageSessionBeginPayload_must_be_48_bytes[
  (sizeof(ZZ9KImageSessionBeginPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageSessionFeedPayload_must_be_48_bytes[
  (sizeof(ZZ9KImageSessionFeedPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageSessionResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KImageSessionResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KImageSessionClosePayload_must_be_48_bytes[
  (sizeof(ZZ9KImageSessionClosePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KAudioDecodePayload_must_be_48_bytes[
  (sizeof(ZZ9KAudioDecodePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KAudioDecodeResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KAudioDecodeResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KVideoSessionBeginPayload_must_be_48_bytes[
  (sizeof(ZZ9KVideoSessionBeginPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KVideoSessionWritePayload_must_be_48_bytes[
  (sizeof(ZZ9KVideoSessionWritePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KVideoSessionDecodePayload_must_be_48_bytes[
  (sizeof(ZZ9KVideoSessionDecodePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KVideoSessionClosePayload_must_be_48_bytes[
  (sizeof(ZZ9KVideoSessionClosePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KVideoSessionResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KVideoSessionResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionBeginPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionBeginPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionWritePayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionWritePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionCommandPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionCommandPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionStatusPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionStatusPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionMainResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionMainResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionAudioResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionAudioResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KMediaSessionStatusResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KMediaSessionStatusResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KCryptoHashPayload_must_be_48_bytes[
  (sizeof(ZZ9KCryptoHashPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KCryptoStreamPayload_must_be_48_bytes[
  (sizeof(ZZ9KCryptoStreamPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KCryptoAeadPayload_must_be_48_bytes[
  (sizeof(ZZ9KCryptoAeadPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KCryptoResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KCryptoResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KCryptoVerifyPayload_must_be_48_bytes[
  (sizeof(ZZ9KCryptoVerifyPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressTestPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressTestPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressStreamBeginPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressStreamBeginPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressStreamReadPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressStreamReadPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressStreamFeedPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressStreamFeedPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressStreamClosePayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressStreamClosePayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressStreamResultPayload_must_be_48_bytes[
  (sizeof(ZZ9KDecompressStreamResultPayload) == 48U) ? 1 : -1
];
typedef char ZZ9KDecompressBatchPayload_must_be_48_bytes[
    (sizeof(ZZ9KDecompressBatchPayload) == 48U) ? 1 : -1];
typedef char ZZ9KDecompressBatchResultPayload_must_be_48_bytes[
    (sizeof(ZZ9KDecompressBatchResultPayload) == 48U) ? 1 : -1];

typedef union ZZ9KEntryPayload {
  uint8_t inline_data[48];
  ZZ9KBufferPayload buffer;
} ZZ9KEntryPayload;

static inline uint16_t zz9k_get_be16(const volatile void *p)
{
  const volatile uint8_t *b = (const volatile uint8_t *)p;
  return (uint16_t)(((uint16_t)b[0] << 8) | b[1]);
}

static inline uint32_t zz9k_get_be32(const volatile void *p)
{
  const volatile uint8_t *b = (const volatile uint8_t *)p;
  return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) |
         ((uint32_t)b[2] << 8) | b[3];
}

static inline void zz9k_put_be16(volatile void *p, uint16_t value)
{
  volatile uint8_t *b = (volatile uint8_t *)p;
  b[0] = (uint8_t)((value >> 8) & 0xffU);
  b[1] = (uint8_t)(value & 0xffU);
}

static inline void zz9k_put_be32(volatile void *p, uint32_t value)
{
  volatile uint8_t *b = (volatile uint8_t *)p;
  b[0] = (uint8_t)((value >> 24) & 0xffU);
  b[1] = (uint8_t)((value >> 16) & 0xffU);
  b[2] = (uint8_t)((value >> 8) & 0xffU);
  b[3] = (uint8_t)(value & 0xffU);
}

static inline uint64_t zz9k_media_u64_from_be(const volatile void *hi,
                                               const volatile void *lo)
{
  return ((uint64_t)zz9k_get_be32(hi) << 32) | zz9k_get_be32(lo);
}

static inline void zz9k_media_u64_to_be(volatile void *hi,
                                        volatile void *lo,
                                        uint64_t value)
{
  zz9k_put_be32(hi, (uint32_t)(value >> 32));
  zz9k_put_be32(lo, (uint32_t)value);
}

/* Convert arbitrary clock units to the unwrapped 90 kHz media timeline while
 * retaining the division remainder. This prevents cumulative drift for rates
 * such as 44.1 kHz and 30000/1001. */
static inline uint64_t zz9k_media_clock_advance(ZZ9KMediaClock *clock,
                                                uint64_t units,
                                                uint32_t units_per_second)
{
  uint64_t scaled;

  if (!clock || units_per_second == 0U) {
    return ZZ9K_MEDIA_NO_PTS;
  }
  scaled = units * UINT64_C(90000) + clock->remainder;
  clock->ticks += scaled / units_per_second;
  clock->remainder = scaled % units_per_second;
  return clock->ticks;
}

/* Map a raw MPEG 33-bit PTS to the epoch nearest the prior unwrapped value. */
static inline uint64_t zz9k_media_pts_unwrap(uint64_t previous,
                                             uint64_t raw_pts)
{
  const uint64_t mask = ZZ9K_MEDIA_PTS_MODULUS - 1U;
  const uint64_t half = ZZ9K_MEDIA_PTS_MODULUS >> 1;
  uint64_t candidate;

  raw_pts &= mask;
  if (previous == ZZ9K_MEDIA_NO_PTS) {
    return raw_pts;
  }
  candidate = (previous & ~mask) | raw_pts;
  if (candidate < previous && previous - candidate > half) {
    candidate += ZZ9K_MEDIA_PTS_MODULUS;
  } else if (candidate > previous && candidate - previous > half &&
             candidate >= ZZ9K_MEDIA_PTS_MODULUS) {
    candidate -= ZZ9K_MEDIA_PTS_MODULUS;
  }
  return candidate;
}

typedef struct ZZ9KMailboxEntry {
  uint32_t request_id;
  uint16_t opcode;
  uint16_t status;
  uint16_t flags;
  uint16_t payload_len;
  uint32_t user_cookie;
  ZZ9KEntryPayload payload;
} ZZ9KMailboxEntry;

typedef char ZZ9KMailboxEntry_must_be_64_bytes[
  (sizeof(ZZ9KMailboxEntry) == ZZ9K_MAILBOX_ENTRY_SIZE) ? 1 : -1
];

/*
 * Wire entries are always big-endian so the m68k host, ARM firmware, and
 * native tooling can share exactly one representation.
 */
typedef struct ZZ9KMailboxWireEntry {
  uint8_t request_id[4];
  uint8_t opcode[2];
  uint8_t status[2];
  uint8_t flags[2];
  uint8_t payload_len[2];
  uint8_t user_cookie[4];
  uint8_t payload[48];
} ZZ9KMailboxWireEntry;

typedef char ZZ9KMailboxWireEntry_must_be_64_bytes[
  (sizeof(ZZ9KMailboxWireEntry) == ZZ9K_MAILBOX_ENTRY_SIZE) ? 1 : -1
];

typedef struct ZZ9KMailboxDescriptor {
  uint8_t magic[4];
  uint8_t abi_major[2];
  uint8_t abi_minor[2];
  uint8_t descriptor_size[4];
  uint8_t request_ring_offset[4];
  uint8_t request_ring_entries[4];
  uint8_t request_head[4];
  uint8_t request_tail[4];
  uint8_t completion_ring_offset[4];
  uint8_t completion_ring_entries[4];
  uint8_t completion_head[4];
  uint8_t completion_tail[4];
  uint8_t capability_bits[4];
  uint8_t reserved[80];
} ZZ9KMailboxDescriptor;

typedef char ZZ9KMailboxDescriptor_must_be_128_bytes[
  (sizeof(ZZ9KMailboxDescriptor) == ZZ9K_MAILBOX_DESCRIPTOR_SIZE) ? 1 : -1
];

typedef struct ZZ9KCaps {
  uint32_t magic;
  uint16_t abi_major;
  uint16_t abi_minor;
  uint32_t capability_bits;
  uint32_t max_inline_payload;
  uint32_t max_shared_buffers;
  uint32_t max_surfaces;
  uint32_t firmware_version;
  uint32_t request_ring_entries;
  uint32_t completion_ring_entries;
  uint32_t host_window_heap_size;
  uint32_t reserved[5];
} ZZ9KCaps;

typedef struct ZZ9KQueryCapsPayload {
  uint32_t magic;
  uint16_t abi_major;
  uint16_t abi_minor;
  uint32_t capability_bits;
  uint32_t max_inline_payload;
  uint32_t max_shared_buffers;
  uint32_t max_surfaces;
  uint32_t firmware_version;
  uint32_t request_ring_entries;
  uint32_t completion_ring_entries;
  uint32_t host_window_heap_size;
  uint8_t reserved[8];
} ZZ9KQueryCapsPayload;

typedef char ZZ9KQueryCapsPayload_must_fit_inline[
  (sizeof(ZZ9KQueryCapsPayload) <= 48U) ? 1 : -1
];

typedef struct ZZ9KSurfaceDesc {
  uint32_t width;
  uint32_t height;
  uint32_t pitch;
  uint32_t format;
  uint32_t flags;
  uint32_t handle;
  uint32_t offset;
  uint32_t reserved[5];
} ZZ9KSurfaceDesc;

typedef struct ZZ9KScaleImageDesc {
  uint32_t src_surface;
  uint32_t dst_surface;
  uint32_t src_x;
  uint32_t src_y;
  uint32_t src_w;
  uint32_t src_h;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t dst_w;
  uint32_t dst_h;
  uint32_t filter;
  uint32_t flags;
} ZZ9KScaleImageDesc;

typedef struct ZZ9KScaleImageClippedDesc {
  uint32_t src_surface;
  uint32_t dst_surface;
  uint32_t src_x;
  uint32_t src_y;
  uint32_t src_w;
  uint32_t src_h;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t dst_w;
  uint32_t dst_h;
  uint32_t clip_x;
  uint32_t clip_y;
  uint32_t clip_w;
  uint32_t clip_h;
  uint32_t filter;
  uint32_t flags;
} ZZ9KScaleImageClippedDesc;

typedef struct ZZ9KSurfaceFillDesc {
  uint32_t surface;
  uint32_t x;
  uint32_t y;
  uint32_t width;
  uint32_t height;
  uint32_t color;
  uint32_t flags;
} ZZ9KSurfaceFillDesc;

typedef struct ZZ9KSurfaceCopyDesc {
  uint32_t src_surface;
  uint32_t dst_surface;
  uint32_t src_x;
  uint32_t src_y;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t width;
  uint32_t height;
  uint32_t flags;
} ZZ9KSurfaceCopyDesc;

typedef struct ZZ9KImageDecodeDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_surface;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t dst_width;
  uint32_t dst_height;
  uint32_t output_format;
  uint32_t flags;
} ZZ9KImageDecodeDesc;

typedef struct ZZ9KImageDecodeResult {
  uint32_t width;
  uint32_t height;
  uint32_t output_format;
  uint32_t flags;
  uint32_t bytes_written;
} ZZ9KImageDecodeResult;

typedef struct ZZ9KImageSessionBeginDesc {
  uint32_t codec;
  uint32_t output_mode;
  uint32_t dst_surface;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t dst_width;
  uint32_t dst_height;
  uint32_t output_format;
  uint32_t tile_handle;
  uint32_t tile_stride;
  uint32_t tile_rows;
  uint32_t flags;
} ZZ9KImageSessionBeginDesc;

typedef struct ZZ9KImageSessionFeedDesc {
  uint32_t session;
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t flags;
} ZZ9KImageSessionFeedDesc;

typedef struct ZZ9KImageSessionResult {
  uint32_t session;
  uint32_t state;
  uint32_t image_width;
  uint32_t image_height;
  uint32_t output_format;
  uint32_t tile_x;
  uint32_t tile_y;
  uint32_t tile_width;
  uint32_t tile_height;
  uint32_t bytes_consumed;
  uint32_t bytes_written;
  uint32_t flags;
} ZZ9KImageSessionResult;

typedef struct ZZ9KAudioDecodeDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t dst_capacity;
  uint32_t output_hz;
  uint32_t output_channels;
  uint32_t output_format;
  uint32_t flags;
} ZZ9KAudioDecodeDesc;

typedef struct ZZ9KAudioDecodeResult {
  uint32_t bytes_consumed;
  uint32_t bytes_written;
  uint32_t sample_rate;
  uint32_t channels;
  uint32_t sample_format;
  uint32_t frames_written;
  uint32_t flags;
} ZZ9KAudioDecodeResult;

typedef struct ZZ9KAudioStreamBeginDesc {
  uint32_t mp3_ring_handle;
  uint32_t mp3_ring_capacity;
  uint32_t pcm_ring_handle;
  uint32_t pcm_ring_capacity;
  uint32_t output_hz;
  uint32_t output_channels;
  uint32_t output_format;
  uint32_t low_water_bytes;
  uint32_t high_water_bytes;
  uint32_t flags;
} ZZ9KAudioStreamBeginDesc;

typedef struct ZZ9KAudioStreamFeedDesc {
  uint32_t session;
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t flags;
} ZZ9KAudioStreamFeedDesc;

typedef struct ZZ9KAudioStreamResult {
  uint32_t session;
  uint32_t state;
  uint32_t sample_rate;
  uint32_t channels;
  uint32_t sample_format;
  uint32_t mp3_read;
  uint32_t pcm_write;
  uint32_t pcm_read;
  uint32_t frames_decoded;
  uint32_t bytes_consumed;
  uint32_t bytes_produced;
  uint32_t flags;
} ZZ9KAudioStreamResult;

typedef struct ZZ9KVideoSessionBeginDesc {
  uint32_t codec;
  uint32_t container;
  uint32_t width;
  uint32_t height;
  uint32_t output_format;
  uint32_t flags;
} ZZ9KVideoSessionBeginDesc;

typedef struct ZZ9KVideoSessionWriteDesc {
  uint32_t session;
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t flags;
} ZZ9KVideoSessionWriteDesc;

typedef struct ZZ9KVideoSessionDecodeDesc {
  uint32_t session;
  uint32_t flags;
} ZZ9KVideoSessionDecodeDesc;

typedef struct ZZ9KVideoSessionResult {
  uint32_t session;
  uint32_t state;
  uint32_t width;
  uint32_t height;
  uint32_t frame_rate_milli;
  uint32_t frame_number;
  uint32_t frame_time_millis;
  uint32_t bytes_accepted;
  uint32_t bytes_written;
  uint32_t flags;
} ZZ9KVideoSessionResult;

typedef struct ZZ9KMediaSessionBeginDesc {
  uint32_t video_codec;
  uint32_t container;
  uint32_t width;
  uint32_t height;
  uint32_t output_format;
  uint32_t audio_codec;
  uint32_t pcm_ring_handle;
  uint32_t pcm_ring_capacity;
  uint32_t pcm_low_water_bytes;
  uint32_t pcm_high_water_bytes;
  uint32_t flags;
} ZZ9KMediaSessionBeginDesc;

typedef struct ZZ9KMediaSessionWriteDesc {
  uint32_t session;
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t flags;
} ZZ9KMediaSessionWriteDesc;

typedef struct ZZ9KMediaSessionMainResult {
  uint32_t session;
  uint32_t state;
  uint32_t width;
  uint32_t height;
  uint32_t frame_rate_num;
  uint32_t frame_rate_den;
  uint32_t frame_number;
  uint64_t video_pts;
  uint32_t bytes_accepted;
  uint32_t bytes_written;
  uint32_t flags;
} ZZ9KMediaSessionMainResult;

typedef struct ZZ9KMediaSessionAudioResult {
  uint32_t session;
  uint32_t state;
  uint32_t sample_rate;
  uint32_t channels;
  uint32_t sample_format;
  uint64_t pcm_produced;
  uint64_t pcm_acknowledged;
  uint64_t audio_pts;
  uint32_t flags;
} ZZ9KMediaSessionAudioResult;

typedef struct ZZ9KMediaSessionStatusResult {
  uint32_t session;
  uint32_t state;
  uint32_t page;
  uint32_t flags;
  uint64_t value[4];
} ZZ9KMediaSessionStatusResult;

typedef struct ZZ9KCryptoHashDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t key_handle;
  uint32_t key_offset;
  uint32_t key_length;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KCryptoHashDesc;

typedef struct ZZ9KCryptoStreamDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t key_handle;
  uint32_t key_offset;
  uint32_t nonce_handle;
  uint32_t nonce_offset;
  uint32_t counter;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KCryptoStreamDesc;

typedef struct ZZ9KCryptoAeadDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t aad_handle;
  uint32_t aad_offset;
  uint32_t aad_length;
  uint32_t key_handle;
  uint32_t key_offset;
  uint32_t nonce_handle;
  uint32_t flags;
} ZZ9KCryptoAeadDesc;

typedef struct ZZ9KCryptoResult {
  uint32_t bytes_written;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KCryptoResult;

typedef struct ZZ9KDecompressDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t dst_capacity;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KDecompressDesc;

typedef struct ZZ9KDecompressTestDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t output_limit;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KDecompressTestDesc;

typedef struct ZZ9KDecompressStreamBeginDesc {
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t output_limit;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KDecompressStreamBeginDesc;

typedef struct ZZ9KDecompressStreamReadDesc {
  uint32_t session;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t dst_capacity;
  uint32_t flags;
} ZZ9KDecompressStreamReadDesc;

typedef struct ZZ9KDecompressStreamFeedDesc {
  uint32_t session;
  uint32_t src_handle;
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t flags;
} ZZ9KDecompressStreamFeedDesc;

typedef struct ZZ9KDecompressResult {
  uint32_t bytes_consumed;
  uint32_t bytes_written;
  uint32_t checksum;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KDecompressResult;

typedef struct ZZ9KDecompressStreamResult {
  uint32_t session;
  uint32_t bytes_consumed;
  uint32_t bytes_written;
  uint32_t checksum;
  uint32_t algorithm;
  uint32_t flags;
} ZZ9KDecompressStreamResult;

/* Batch arena wire format (all fields big-endian, offsets relative to the
   arena base = arena_handle's buffer + arena_offset):
     [0, 48)                          header
     [desc_offset, +N*32)             member descriptors
     [blob_offset, +blob_length)      concatenated compressed members
     [output_offset, +output_capacity) decoded output (EXTRACT mode only)
     [result_offset, +N*16)           per-member results (firmware-written)
 */
#define ZZ9K_BATCH_ARENA_MAGIC 0x5A424154UL /* 'ZBAT' */
#define ZZ9K_BATCH_ARENA_VERSION 1U
#define ZZ9K_BATCH_MODE_TEST 0U    /* decode-and-discard, CRC only */
#define ZZ9K_BATCH_MODE_EXTRACT 1U /* decode into the output region */
#define ZZ9K_BATCH_HEADER_SIZE 48U
#define ZZ9K_BATCH_DESC_SIZE 32U
#define ZZ9K_BATCH_RESULT_SIZE 16U
#define ZZ9K_BATCH_MEMBER_LIMIT 1024U
#define ZZ9K_BATCH_MEMBER_FLAG_HAVE_CRC (1U << 0)

/* TEST-mode members decode-and-discard, so uncompressed_size is not
   bounded by any arena region. Cap it so a corrupt descriptor cannot pin
   the firmware worker for minutes producing discarded output. Mirrored as
   SDK_BATCH_TEST_MAX_EXPECTED in the firmware. */
#define ZZ9K_BATCH_TEST_MAX_EXPECTED 0x04000000UL /* 64 MB */

/* Header field byte offsets. */
#define ZZ9K_BATCH_HDR_MAGIC 0U
#define ZZ9K_BATCH_HDR_VERSION 4U /* u16 */
#define ZZ9K_BATCH_HDR_MODE 6U    /* u16 */
#define ZZ9K_BATCH_HDR_MEMBER_COUNT 8U
#define ZZ9K_BATCH_HDR_DESC_OFFSET 12U
#define ZZ9K_BATCH_HDR_BLOB_OFFSET 16U
#define ZZ9K_BATCH_HDR_BLOB_LENGTH 20U
#define ZZ9K_BATCH_HDR_OUTPUT_OFFSET 24U
#define ZZ9K_BATCH_HDR_OUTPUT_CAPACITY 28U
#define ZZ9K_BATCH_HDR_RESULT_OFFSET 32U

/* Member descriptor field byte offsets (relative to each 32-byte entry).
   src_offset is relative to blob_offset; dst_offset (EXTRACT only) is
   relative to output_offset. */
#define ZZ9K_BATCH_DESC_ALGORITHM 0U
#define ZZ9K_BATCH_DESC_SRC_OFFSET 4U
#define ZZ9K_BATCH_DESC_SRC_LENGTH 8U
#define ZZ9K_BATCH_DESC_DST_OFFSET 12U
#define ZZ9K_BATCH_DESC_UNCOMPRESSED_SIZE 16U
#define ZZ9K_BATCH_DESC_EXPECTED_CRC 20U
#define ZZ9K_BATCH_DESC_FLAGS 24U

/* Member result field byte offsets (relative to each 16-byte entry). */
#define ZZ9K_BATCH_RESULT_STATUS 0U
#define ZZ9K_BATCH_RESULT_BYTES_WRITTEN 4U
#define ZZ9K_BATCH_RESULT_CHECKSUM 8U

typedef struct ZZ9KBatchMemberDesc {
  uint32_t algorithm; /* ZZ9K_COMPRESSION_LH1/LH5/LH6/LH7 */
  uint32_t src_offset;
  uint32_t src_length;
  uint32_t dst_offset;
  uint32_t uncompressed_size; /* exact expected decoded size */
  uint32_t expected_crc;      /* LHA CRC-16 in the low 16 bits. ADVISORY in
                                 arena v1: the firmware does NOT compare it;
                                 verify the result row's checksum yourself
                                 (members_ok reflects decode completion
                                 only, not CRC correctness). */
  uint32_t flags;             /* ZZ9K_BATCH_MEMBER_FLAG_* */
} ZZ9KBatchMemberDesc;

typedef struct ZZ9KBatchMemberResult {
  uint32_t status; /* ZZ9K_STATUS_* for this member */
  uint32_t bytes_written;
  uint32_t checksum; /* computed CRC-16 in the low 16 bits */
} ZZ9KBatchMemberResult;

typedef struct ZZ9KDecompressBatchDesc {
  uint32_t arena_handle;
  uint32_t arena_offset;
  uint32_t arena_length;
} ZZ9KDecompressBatchDesc;

typedef struct ZZ9KDecompressBatchResult {
  uint32_t members_total;
  uint32_t members_ok;
  uint32_t members_failed;
  uint32_t flags;
} ZZ9KDecompressBatchResult;

enum ZZ9KSurfaceFormat {
  ZZ9K_SURFACE_FORMAT_UNKNOWN = 0,
  ZZ9K_SURFACE_FORMAT_RGB565 = 1,
  ZZ9K_SURFACE_FORMAT_ARGB8888 = 2,
  ZZ9K_SURFACE_FORMAT_RGBA8888 = 3,
  ZZ9K_SURFACE_FORMAT_INDEX8 = 4,
  ZZ9K_SURFACE_FORMAT_PLANAR = 5,
  ZZ9K_SURFACE_FORMAT_RGB555 = 6,
  ZZ9K_SURFACE_FORMAT_BGRA8888 = 7,
  ZZ9K_SURFACE_FORMAT_RGB888 = 8
};

enum ZZ9KSurfaceFlags {
  ZZ9K_SURFACE_FLAG_CPU_VISIBLE = 1U << 0,
  ZZ9K_SURFACE_FLAG_FRAMEBUFFER = 1U << 1,
  ZZ9K_SURFACE_FLAG_DISPLAYED = 1U << 2,
  ZZ9K_SURFACE_FLAG_SHARED_BUFFER = 1U << 3,
  ZZ9K_SURFACE_FLAG_ARM_LOCAL = 1U << 4
};

enum ZZ9KScaleFilter {
  ZZ9K_SCALE_NEAREST = 0,
  ZZ9K_SCALE_BILINEAR = 1,
  ZZ9K_SCALE_BICUBIC = 2,
  ZZ9K_SCALE_LANCZOS3 = 3
};

enum ZZ9KImageDecodeFlags {
  ZZ9K_IMAGE_DECODE_FLAG_FIT = 1U << 0,
  ZZ9K_IMAGE_DECODE_FLAG_PRESERVE_ASPECT = 1U << 1,
  ZZ9K_IMAGE_DECODE_FLAG_DITHER = 1U << 2
};

enum ZZ9KImageDecodeResultFlags {
  ZZ9K_IMAGE_DECODE_RESULT_ALPHA = 1U << 0,
  ZZ9K_IMAGE_DECODE_RESULT_ANIMATED = 1U << 1,
  ZZ9K_IMAGE_DECODE_RESULT_PARTIAL = 1U << 2
};

enum ZZ9KImageCodec {
  ZZ9K_IMAGE_CODEC_JPEG = 1U,
  ZZ9K_IMAGE_CODEC_PNG = 2U,
  ZZ9K_IMAGE_CODEC_GIF = 3U
};

enum ZZ9KImageOutputMode {
  ZZ9K_IMAGE_OUTPUT_SURFACE = 1U,
  ZZ9K_IMAGE_OUTPUT_FRAMEBUFFER = 2U,
  ZZ9K_IMAGE_OUTPUT_TILE_BUFFER = 3U
};

enum ZZ9KImageSessionFeedFlags {
  ZZ9K_IMAGE_SESSION_FEED_EOF = 1U << 0
};

enum ZZ9KImageSessionState {
  ZZ9K_IMAGE_SESSION_STATE_NEED_INPUT = 1U,
  ZZ9K_IMAGE_SESSION_STATE_HEADER_READY = 2U,
  ZZ9K_IMAGE_SESSION_STATE_TILE_READY = 3U,
  ZZ9K_IMAGE_SESSION_STATE_COMPLETE = 4U,
  ZZ9K_IMAGE_SESSION_STATE_ERROR = 5U
};

enum ZZ9KImageSessionResultFlags {
  ZZ9K_IMAGE_SESSION_RESULT_HEADER_READY = 1U << 0,
  ZZ9K_IMAGE_SESSION_RESULT_PARTIAL = 1U << 1,
  ZZ9K_IMAGE_SESSION_RESULT_SCALED = 1U << 2
};

enum ZZ9KAudioStreamFeedFlags {
  ZZ9K_AUDIO_STREAM_FEED_EOF = 1U << 0,
  /* Resumable starvation boundary: decode every complete frame currently
   * buffered, retain any incomplete compressed frame, and drain PCM/output. */
  ZZ9K_AUDIO_STREAM_FEED_DRAIN = 1U << 1
};

enum ZZ9KAudioStreamState {
  ZZ9K_AUDIO_STREAM_STATE_NEED_INPUT = 1U,
  ZZ9K_AUDIO_STREAM_STATE_STREAMING = 2U,
  ZZ9K_AUDIO_STREAM_STATE_DONE = 3U,
  ZZ9K_AUDIO_STREAM_STATE_ERROR = 4U
};

enum ZZ9KAudioStreamResultFlags {
  ZZ9K_AUDIO_STREAM_RESULT_NEED_INPUT = 1U << 0,
  ZZ9K_AUDIO_STREAM_RESULT_PCM_READY = 1U << 1,
  ZZ9K_AUDIO_STREAM_RESULT_DONE = 1U << 2,
  ZZ9K_AUDIO_STREAM_RESULT_BACKPRESSURE = 1U << 3,
  /* The most recent resumable drain reached the real output frontier. */
  ZZ9K_AUDIO_STREAM_RESULT_DRAINED = 1U << 4
};

enum ZZ9KVideoCodec {
  ZZ9K_VIDEO_CODEC_MPEG1 = 1U
};

enum ZZ9KVideoContainer {
  ZZ9K_VIDEO_CONTAINER_MPEG_PS = 1U
};

enum ZZ9KVideoOutputFormat {
  ZZ9K_VIDEO_OUTPUT_DIRECT_OVERLAY = 1U
};

enum ZZ9KVideoSessionWriteFlags {
  ZZ9K_VIDEO_SESSION_WRITE_EOF = 1U << 0
};

enum ZZ9KVideoSessionState {
  ZZ9K_VIDEO_SESSION_STATE_NEED_INPUT = 1U,
  ZZ9K_VIDEO_SESSION_STATE_READY = 2U,
  ZZ9K_VIDEO_SESSION_STATE_FRAME_READY = 3U,
  ZZ9K_VIDEO_SESSION_STATE_DONE = 4U,
  ZZ9K_VIDEO_SESSION_STATE_ERROR = 5U
};

enum ZZ9KVideoSessionResultFlags {
  ZZ9K_VIDEO_SESSION_RESULT_HEADER_READY = 1U << 0,
  ZZ9K_VIDEO_SESSION_RESULT_NEED_INPUT = 1U << 1,
  ZZ9K_VIDEO_SESSION_RESULT_FRAME_READY = 1U << 2,
  ZZ9K_VIDEO_SESSION_RESULT_DONE = 1U << 3
};

enum ZZ9KMediaAudioCodec {
  ZZ9K_MEDIA_AUDIO_NONE = 0U,
  ZZ9K_MEDIA_AUDIO_MP2 = 1U
};

enum ZZ9KMediaSessionWriteFlags {
  ZZ9K_MEDIA_SESSION_WRITE_EOF = 1U << 0
};

enum ZZ9KMediaAudioBindFlags {
  ZZ9K_MEDIA_AUDIO_BIND_PAUSE = 1U << 0
};

enum ZZ9KMediaSessionState {
  ZZ9K_MEDIA_SESSION_STATE_NEED_INPUT = 1U,
  ZZ9K_MEDIA_SESSION_STATE_READY = 2U,
  ZZ9K_MEDIA_SESSION_STATE_FRAME_HELD = 3U,
  ZZ9K_MEDIA_SESSION_STATE_DONE = 4U,
  ZZ9K_MEDIA_SESSION_STATE_ERROR = 5U
};

enum ZZ9KMediaSessionResultFlags {
  ZZ9K_MEDIA_SESSION_RESULT_HEADER_READY = 1U << 0,
  ZZ9K_MEDIA_SESSION_RESULT_NEED_INPUT = 1U << 1,
  ZZ9K_MEDIA_SESSION_RESULT_FRAME_HELD = 1U << 2,
  ZZ9K_MEDIA_SESSION_RESULT_DONE = 1U << 3,
  ZZ9K_MEDIA_SESSION_RESULT_DERIVED_TIME = 1U << 4,
  ZZ9K_MEDIA_SESSION_RESULT_DISCONTINUITY = 1U << 5,
  ZZ9K_MEDIA_SESSION_RESULT_REBASED = 1U << 6,
  ZZ9K_MEDIA_SESSION_RESULT_AUDIO_READY = 1U << 7,
  ZZ9K_MEDIA_SESSION_RESULT_BACKPRESSURE = 1U << 8,
  ZZ9K_MEDIA_SESSION_RESULT_PRESENTED = 1U << 9,
  ZZ9K_MEDIA_SESSION_RESULT_DISCARDED = 1U << 10,
  ZZ9K_MEDIA_SESSION_RESULT_AUDIO_BOUND = 1U << 11,
  ZZ9K_MEDIA_SESSION_RESULT_AUDIO_PLAYING = 1U << 12,
  ZZ9K_MEDIA_SESSION_RESULT_AUDIO_DRAINED = 1U << 13,
  ZZ9K_MEDIA_SESSION_RESULT_AUDIO_UNDERRUN = 1U << 14
};

/* Palette (CLUT) limits for ZZ9K_OP_QUERY_PALETTE. The INDEX8 table has 256
 * entries, each returned as one 0x00RRGGBB word. */
enum ZZ9KPaletteLimits {
  ZZ9K_PALETTE_MAX_ENTRIES = 256,
  ZZ9K_PALETTE_ENTRY_BYTES = 4
};

/* Bit 0 is reserved for the secondary (HI) CLUT, which firmware does not
 * shadow; queries that set it get ZZ9K_STATUS_UNSUPPORTED rather than a
 * silently wrong answer. */
enum ZZ9KPaletteQueryFlags {
  ZZ9K_PALETTE_QUERY_FLAG_SECONDARY = 1U << 0
};

/* ZZ9K_OP_QUERY_PALETTE reads the display CLUT into a caller shared buffer.
 * The 256-entry table cannot fit the fixed 48-byte reply, so - as the crypto
 * hash op does - the result is written to dst_handle/dst_offset as `count`
 * consecutive 0x00RRGGBB words starting at palette index `start`. The palette
 * is display-global: `surface` is carried for future per-surface tables but
 * does not select one, and this op implies nothing about 8-bit overlay
 * composition being available. */
typedef struct ZZ9KPaletteQueryDesc {
  uint32_t surface;
  uint32_t start;
  uint32_t count;
  uint32_t dst_handle;
  uint32_t dst_offset;
  uint32_t flags;
} ZZ9KPaletteQueryDesc;

enum ZZ9KMediaStatusPage {
  ZZ9K_MEDIA_STATUS_TIMING = 0U,
  ZZ9K_MEDIA_STATUS_AUDIO = 1U,
  ZZ9K_MEDIA_STATUS_COUNTERS = 2U,
  /* DMA-retired frames, DMA-queued frames, staged source frames, underruns. */
  ZZ9K_MEDIA_STATUS_AUDIO_OUTPUT = 3U,
  /* Overlay presentation path and geometry: source size, destination size,
   * destination origin, screen size. Firmware that predates this page
   * answers ZZ9K_STATUS_BAD_REQUEST, which is the intended capability gate —
   * a client seeing that reports the presentation path as unavailable rather
   * than failing playback. */
  ZZ9K_MEDIA_STATUS_PRESENTATION = 4U,
  /* Per-stage pipeline timing (U7). Each value packs one stage as
   * (microseconds << 32) | calls, indexed by ZZ9KMediaProfileStage.
   * Firmware without the page answers BAD_REQUEST, so a client reports
   * profiling as unavailable rather than failing. */
  ZZ9K_MEDIA_STATUS_PROFILE = 5U
};

enum ZZ9KMediaProfileStage {
  ZZ9K_MEDIA_PROFILE_VIDEO_DECODE = 0,
  ZZ9K_MEDIA_PROFILE_YUY2_PACK = 1,
  ZZ9K_MEDIA_PROFILE_PRESENT = 2,
  ZZ9K_MEDIA_PROFILE_AUDIO_DECODE = 3,
  ZZ9K_MEDIA_PROFILE_STAGES = 4
};

/* Profile-page flags: names the live YUY2 pack kernel, so a before/after
 * comparison states what it measured rather than assuming. */
enum ZZ9KMediaProfileFlags {
  ZZ9K_MEDIA_PROFILE_FLAG_NEON_PACK = 1U << 0
};

#define ZZ9K_MEDIA_PROFILE_US(v) ((uint32_t)((v) >> 32))
#define ZZ9K_MEDIA_PROFILE_CALLS(v) ((uint32_t)((v) & 0xffffffffU))

/* ZZ9K_MEDIA_STATUS_PRESENTATION `flags` bits. NATIVE distinguishes the FPGA
 * overlay plane from the card-local ARM shadow compositor; OWNED says the
 * queried session is the one feeding that overlay. */
enum ZZ9KMediaPresentFlags {
  ZZ9K_MEDIA_PRESENT_CONFIGURED = 1U << 0,
  ZZ9K_MEDIA_PRESENT_ACTIVE = 1U << 1,
  ZZ9K_MEDIA_PRESENT_NATIVE = 1U << 2,
  ZZ9K_MEDIA_PRESENT_OWNED = 1U << 3
};

/* Each presentation value carries two 16-bit halves: width/height, or x/y.
 * Signed coordinates travel as their two's-complement 16-bit pattern.
 * Mirrored as SDK_MEDIA_PACK_PAIR in the firmware. */
#define ZZ9K_MEDIA_PACK_PAIR(hi, lo) \
  (((uint64_t)(uint16_t)(hi) << 16) | (uint64_t)(uint16_t)(lo))
#define ZZ9K_MEDIA_PAIR_HI(v) ((uint16_t)(((v) >> 16) & 0xffffU))
#define ZZ9K_MEDIA_PAIR_LO(v) ((uint16_t)((v) & 0xffffU))
#define ZZ9K_MEDIA_PAIR_HI_S(v) ((int16_t)ZZ9K_MEDIA_PAIR_HI(v))
#define ZZ9K_MEDIA_PAIR_LO_S(v) ((int16_t)ZZ9K_MEDIA_PAIR_LO(v))

enum ZZ9KCryptoHashAlgorithm {
  ZZ9K_CRYPTO_HASH_NONE = 0,
  ZZ9K_CRYPTO_HASH_SHA1 = 1,
  ZZ9K_CRYPTO_HASH_SHA256 = 2,
  ZZ9K_CRYPTO_HASH_SHA384 = 3,
  ZZ9K_CRYPTO_HASH_SHA512 = 4,
  ZZ9K_CRYPTO_HASH_BLAKE2S = 5,
  ZZ9K_CRYPTO_HASH_POLY1305 = 6
};

enum ZZ9KCryptoHashFlags {
  ZZ9K_CRYPTO_HASH_FLAG_HMAC = 1U << 0
};

enum ZZ9KCryptoStreamAlgorithm {
  ZZ9K_CRYPTO_STREAM_NONE = 0,
  ZZ9K_CRYPTO_STREAM_CHACHA20 = 1
};

enum ZZ9KCryptoAeadAlgorithm {
  ZZ9K_CRYPTO_AEAD_NONE = 0,
  ZZ9K_CRYPTO_AEAD_CHACHA20_POLY1305 = 1,
  ZZ9K_CRYPTO_AEAD_AES128_GCM = 2,
  ZZ9K_CRYPTO_AEAD_AES256_GCM = 3
};

typedef enum ZZ9KCryptoKxAlgorithm {
  ZZ9K_CRYPTO_KX_NONE    = 0,
  ZZ9K_CRYPTO_KX_X25519  = 1U,
  ZZ9K_CRYPTO_KX_P256    = 2U
} ZZ9KCryptoKxAlgorithm;

/* KX descriptor flags. KEYGEN turns a P-256 KX request into a base-point
 * multiply: `scalar` is the private key, `point` is unused, and `dst` receives
 * the full uncompressed public point (ZZ9K_CRYPTO_P256_POINT_BYTES). Firmware
 * that predates the keygen primitive rejects a non-zero flags word with
 * UNSUPPORTED, so callers must gate on ZZ9K_SERVICE_FLAG_CRYPTO_P256_KEYGEN. */
#define ZZ9K_CRYPTO_KX_FLAG_KEYGEN 1U

typedef enum ZZ9KCryptoVerifyAlgorithm {
  ZZ9K_CRYPTO_VERIFY_NONE                     = 0,
  ZZ9K_CRYPTO_VERIFY_ECDSA_P256_SHA256        = 1U,
  ZZ9K_CRYPTO_VERIFY_RSA_PKCS1_2048_SHA256    = 2U
} ZZ9KCryptoVerifyAlgorithm;

#define ZZ9K_CRYPTO_X25519_KEY_BYTES    32U
#define ZZ9K_CRYPTO_X25519_SHARED_BYTES 32U

/* P-256 public point is the uncompressed SEC1 form: 0x04 || X(32) || Y(32). */
#define ZZ9K_CRYPTO_P256_POINT_BYTES   65U
#define ZZ9K_CRYPTO_P256_PRIVATE_BYTES 32U
#define ZZ9K_CRYPTO_P256_SHARED_BYTES  32U

/* AES-GCM (reuses the AEAD op): 96-bit nonce, 128-bit tag, key 16 or 32. */
#define ZZ9K_CRYPTO_AES128_KEY_BYTES    16U
#define ZZ9K_CRYPTO_AES256_KEY_BYTES    32U
#define ZZ9K_CRYPTO_AES_GCM_NONCE_BYTES 12U
#define ZZ9K_CRYPTO_AES_GCM_TAG_BYTES   16U

/* The AEAD payload has no algorithm field, so the AEAD algorithm is carried in
 * the flags field at bits 8-15. A zero algorithm nibble means the legacy
 * default, ChaCha20-Poly1305, so existing callers stay byte-compatible. */
enum ZZ9KCryptoAeadFlags {
  ZZ9K_CRYPTO_AEAD_FLAG_DECRYPT = 1U << 0,
  ZZ9K_CRYPTO_AEAD_ALG_SHIFT = 8,
  ZZ9K_CRYPTO_AEAD_ALG_MASK = 0xFFU << 8
};

/* Encode/decode the AEAD algorithm in the flags field. */
#define ZZ9K_CRYPTO_AEAD_FLAG_ALG(alg) \
  (((uint32_t)(alg) << ZZ9K_CRYPTO_AEAD_ALG_SHIFT) & ZZ9K_CRYPTO_AEAD_ALG_MASK)
#define ZZ9K_CRYPTO_AEAD_FLAG_GET_ALG(flags) \
  (((flags) & ZZ9K_CRYPTO_AEAD_ALG_MASK) >> ZZ9K_CRYPTO_AEAD_ALG_SHIFT)

enum ZZ9KCompressionAlgorithm {
  ZZ9K_COMPRESSION_NONE = 0,
  ZZ9K_COMPRESSION_DEFLATE_RAW = 1,
  ZZ9K_COMPRESSION_ZLIB = 2,
  ZZ9K_COMPRESSION_GZIP = 3,
  ZZ9K_COMPRESSION_LZ4_BLOCK = 4,
  ZZ9K_COMPRESSION_LZMA_ALONE = 5,
  ZZ9K_COMPRESSION_LZMA2 = 6,
  ZZ9K_COMPRESSION_LH1 = 7,
  ZZ9K_COMPRESSION_LH5 = 8,
  ZZ9K_COMPRESSION_LH6 = 9,
  ZZ9K_COMPRESSION_LH7 = 10
};

enum ZZ9KDecompressFlags {
  ZZ9K_DECOMPRESS_FLAG_EXPECT_END = 1U << 0,
  ZZ9K_DECOMPRESS_FLAG_FEED_INPUT = 1U << 1
};

enum ZZ9KDecompressStreamFeedFlags {
  ZZ9K_DECOMPRESS_STREAM_FEED_EOF = 1U << 0
};

enum ZZ9KDecompressResultFlags {
  ZZ9K_DECOMPRESS_RESULT_STREAM_END = 1U << 0,
  ZZ9K_DECOMPRESS_RESULT_CHECKSUM_VALID = 1U << 1,
  ZZ9K_DECOMPRESS_RESULT_NEED_INPUT = 1U << 2
};

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_ABI_H */

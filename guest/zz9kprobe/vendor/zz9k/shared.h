/*
 * Header-only helpers for CPU-visible ZZ9000 shared buffers.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef ZZ9K_SHARED_H
#define ZZ9K_SHARED_H

#include "zz9k/host.h"

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline int zz9k_shared_range_fits(const ZZ9KSharedBuffer *buffer,
                                         uint32_t offset,
                                         uint32_t length)
{
  if (!buffer || buffer->handle == ZZ9K_INVALID_HANDLE ||
      buffer->data == 0 || offset > buffer->length) {
    return 0;
  }
  if (length > (buffer->length - offset)) {
    return 0;
  }
  return 1;
}

static inline int zz9k_shared_ptr_aligned(const volatile void *ptr,
                                          uintptr_t mask)
{
  return (((uintptr_t)ptr) & mask) == 0U;
}

static inline int zz9k_shared_copy_to(ZZ9KSharedBuffer *buffer,
                                      uint32_t offset,
                                      const void *src,
                                      uint32_t length)
{
  volatile uint8_t *dst8;
  const uint8_t *src8;

  if (!zz9k_shared_range_fits(buffer, offset, length) ||
      (length != 0U && !src)) {
    return 0;
  }

  dst8 = (volatile uint8_t *)buffer->data + offset;
  src8 = (const uint8_t *)src;
  if (zz9k_shared_ptr_aligned(dst8, 3U) &&
      zz9k_shared_ptr_aligned(src8, 3U)) {
    volatile uint32_t *dst32 = (volatile uint32_t *)dst8;
    const uint32_t *src32 = (const uint32_t *)src8;

    while (length >= 4U) {
      *dst32++ = *src32++;
      length -= 4U;
    }
    dst8 = (volatile uint8_t *)dst32;
    src8 = (const uint8_t *)src32;
  }
  if (zz9k_shared_ptr_aligned(dst8, 1U) &&
      zz9k_shared_ptr_aligned(src8, 1U)) {
    volatile uint16_t *dst16 = (volatile uint16_t *)dst8;
    const uint16_t *src16 = (const uint16_t *)src8;

    while (length >= 2U) {
      *dst16++ = *src16++;
      length -= 2U;
    }
    dst8 = (volatile uint8_t *)dst16;
    src8 = (const uint8_t *)src16;
  }
  while (length != 0U) {
    *dst8++ = *src8++;
    length--;
  }
  return 1;
}

static inline int zz9k_shared_copy_from(void *dst,
                                        const ZZ9KSharedBuffer *buffer,
                                        uint32_t offset,
                                        uint32_t length)
{
  uint8_t *dst8;
  volatile const uint8_t *src8;

  if (!zz9k_shared_range_fits(buffer, offset, length) ||
      (length != 0U && !dst)) {
    return 0;
  }

  dst8 = (uint8_t *)dst;
  src8 = (volatile const uint8_t *)buffer->data + offset;
  if (zz9k_shared_ptr_aligned(dst8, 3U) &&
      zz9k_shared_ptr_aligned(src8, 3U)) {
    uint32_t *dst32 = (uint32_t *)dst8;
    volatile const uint32_t *src32 = (volatile const uint32_t *)src8;

    while (length >= 4U) {
      *dst32++ = *src32++;
      length -= 4U;
    }
    dst8 = (uint8_t *)dst32;
    src8 = (volatile const uint8_t *)src32;
  }
  if (zz9k_shared_ptr_aligned(dst8, 1U) &&
      zz9k_shared_ptr_aligned(src8, 1U)) {
    uint16_t *dst16 = (uint16_t *)dst8;
    volatile const uint16_t *src16 = (volatile const uint16_t *)src8;

    while (length >= 2U) {
      *dst16++ = *src16++;
      length -= 2U;
    }
    dst8 = (uint8_t *)dst16;
    src8 = (volatile const uint8_t *)src16;
  }
  while (length != 0U) {
    *dst8++ = *src8++;
    length--;
  }
  return 1;
}

static inline int zz9k_shared_set(ZZ9KSharedBuffer *buffer,
                                  uint32_t offset,
                                  uint8_t value,
                                  uint32_t length)
{
  volatile uint8_t *dst;
  uint32_t i;

  if (!zz9k_shared_range_fits(buffer, offset, length)) {
    return 0;
  }

  dst = (volatile uint8_t *)buffer->data + offset;
  for (i = 0; i < length; i++) {
    dst[i] = value;
  }
  return 1;
}

static inline int zz9k_shared_move(ZZ9KSharedBuffer *buffer,
                                   uint32_t dst_offset,
                                   uint32_t src_offset,
                                   uint32_t length)
{
  volatile uint8_t *bytes;
  uint32_t i;

  if (!zz9k_shared_range_fits(buffer, dst_offset, length) ||
      !zz9k_shared_range_fits(buffer, src_offset, length)) {
    return 0;
  }
  if (length == 0U || dst_offset == src_offset) {
    return 1;
  }

  bytes = (volatile uint8_t *)buffer->data;
  if (dst_offset < src_offset) {
    for (i = 0U; i < length; i++) {
      bytes[dst_offset + i] = bytes[src_offset + i];
    }
  } else {
    for (i = length; i > 0U; i--) {
      bytes[dst_offset + i - 1U] = bytes[src_offset + i - 1U];
    }
  }
  return 1;
}

#ifdef __cplusplus
}
#endif

#endif /* ZZ9K_SHARED_H */

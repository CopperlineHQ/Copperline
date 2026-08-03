/* Copperline's static answer to config.h.in, which upstream fills in from
 * CMake. build.rs compiles these sources directly into the emulator, so the
 * build is always static, unversioned, and offers both API flavours.
 *
 * See ../README.md. Everything else under src/ is upstream's, untouched.
 */

#ifndef MT32EMU_CONFIG_H
#define MT32EMU_CONFIG_H

#define MT32EMU_VERSION      "2.8.2"
#define MT32EMU_VERSION_MAJOR 2
#define MT32EMU_VERSION_MINOR 8
#define MT32EMU_VERSION_PATCH 2

/* 3: both the C++ and the C API are available. Copperline uses the C one. */
#define MT32EMU_EXPORTS_TYPE 3

/* Static build: MT32EMU_SHARED stays undefined, so no export attributes. */

/* Version tagging guards a shared object against a mismatched client. There
 * is no shared object here, so there is nothing to guard. */
#define MT32EMU_WITH_VERSION_TAGGING 0

#endif /* #ifndef MT32EMU_CONFIG_H */

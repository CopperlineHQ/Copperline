//
// Copyright 2022-2025 Stefan Reinauer
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifndef __NDK_COMPAT_H
#define __NDK_COMPAT_H

#include <inttypes.h>

/* Some Amiga NDK/CRT combinations define uint32_t as unsigned long while
 * inttypes.h still uses unsigned int formats for PRI*32. Keep the macros
 * aligned with the actual 32-bit type we compile against.
 */
#if (INCLUDE_VERSION < 47) || (defined(__CLIB2__) && (__SIZEOF_LONG__ == 4))
#undef PRIu32
#define PRIu32 "lu"
#undef PRId32
#define PRId32 "ld"
#undef PRIx32
#define PRIx32 "lx"
#endif

#define GCC_VERSION (__GNUC__ * 10000 + __GNUC_MINOR__ * 100 + __GNUC_PATCHLEVEL__)

#endif

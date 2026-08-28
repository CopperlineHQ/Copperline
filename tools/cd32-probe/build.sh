#!/bin/sh
# Build the CD32 probe as a plain AmigaDOS executable. entry() must stay
# the first function in probe.c: with -nostartfiles, AmigaDOS jumps to
# the first byte of the first hunk.
set -e
CC="${CC:-/opt/amiga/bin/m68k-amigaos-gcc}"
"$CC" -noixemul -nostartfiles -Os -fomit-frame-pointer -fno-toplevel-reorder \
    -m68020 -Wall -o PROBE start.s probe.c
ls -la PROBE

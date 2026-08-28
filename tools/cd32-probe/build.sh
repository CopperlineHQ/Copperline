#!/bin/sh
# Build the CD32 probe as a plain AmigaDOS executable. AmigaDOS jumps to
# the first byte of the first hunk, which start.s owns: it must stay
# first on the link line, and only forwards to probe.c's entry().
set -e
CC="${CC:-/opt/amiga/bin/m68k-amigaos-gcc}"
"$CC" -noixemul -nostartfiles -Os -fomit-frame-pointer -fno-toplevel-reorder \
    -m68020 -Wall -o PROBE start.s probe.c
ls -la PROBE

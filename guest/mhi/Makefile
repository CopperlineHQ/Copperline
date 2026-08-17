# Builds mhi_copperline.library, the ported MHI guest-side driver for
# Copperline's virtual MHI decoder board (docs/internals/mhi.md), with the
# dockerized m68k-amigaos cross-GCC (see ../toolchain.mk).
#
# Unlike guest/hostsocket or guest/services, this board has no boot ROM at
# all (mhi.md's own "Zorro identity": "no autoboot ROM") -- the library is
# an ordinary disk-loaded AmigaOS shared library AmigaAMP LoadSeg()s out of
# LIBS:mhi/, exactly like mhiuae.library's own build (test-assets/host-tools/
# drivers/mhi/Makefile). So there is no -mpcrel/no-relocations/no-data-bss
# discipline to enforce here (that's a PIC-ROM-only concern): the compiler's
# ordinary relocatable hunk output is exactly what LoadSeg() wants.
#
# -m68000: the repo-wide floor (see AGENTS.md's `--cpu 68000..68060`, and
# guest/hostsocket, guest/services, guest/hostfs-test all target it) --
# deliberately narrower than mhiuae.library's own -mcpu=68020, since nothing
# in the board-access layer needs anything past plain 68000 word moves.
# -noixemul/-nostartfiles: match mhiuae.library's own Makefile -- this is a
# hand-rolled RTF_AUTOINIT library (startup.c), not a linked ixemul/libnix
# program, so no C runtime startup is linked in at all.

LIB     = mhi_copperline.library
# 1.1: M4 -- MHIQuery's tone/volume/output flags now key off the board's
# CAPS bit 6 instead of a hardcoded MHIF_UNSUPPORTED (docs/internals/
# mhi.md's "M4: the DSP chain"), a real behavior change against an M4
# board while staying compatible with M1-M3 boards.
VERSION_STR = 1.1
DATE_STR    = 2026-08-17
LIB_VERSION = 1
LIB_REVISION = 1

include ../toolchain.mk

CFLAGS  = -Os -m68000 -noixemul -nostartfiles -std=c99 -fomit-frame-pointer \
          -Wall -Wextra -Wstrict-prototypes \
          -DVERSION_STR="\"$(VERSION_STR)\"" -DDATE_STR="\"$(DATE_STR)\"" \
          -DLIB_VERSION=$(LIB_VERSION) -DLIB_REVISION=$(LIB_REVISION)

SOURCES = startup.c mhi_copperline.c board.c int_handler.s
HEADERS = board.h mhi_abi.h mhi_copperline.h

.PHONY: all clean check

all: $(LIB)

# int_handler.s needs cpp for its #include "board.h"/#define MHI_ASSEMBLY --
# -x assembler-with-cpp forces that (a plain ".s" name isn't preprocessed by
# default), then -x c switches back for the remaining C sources, same
# two-mode-in-one-invocation idiom as guest/hostsocket/Makefile.
$(LIB): $(SOURCES) $(HEADERS)
	$(CC) $(CFLAGS) startup.c mhi_copperline.c board.c \
	  -x assembler-with-cpp int_handler.s -o $@

# Sanity check, mirroring the relocation/section checks the ROM-building
# guest Makefiles run (guest/hostsocket, guest/services), adapted for a
# relocatable library instead of a PIC ROM: a real hunk executable with
# actual relocations is *expected* here (LoadSeg fixes them up at load
# time), so this only checks that every referenced symbol resolves to
# something other than a dangling external (i.e. the OS-library LVO calls
# stay unresolved-by-design -- those are patched by the linker into
# relocation-relative jumps, not real symbol references) and that the
# object contains the expected hunks.
check: $(LIB)
	$(OBJDUMP) -h $(LIB)
	$(OBJDUMP) -t $(LIB) | grep -i "mhi_int_handler\|ROMTag\|InitTab"

clean:
	rm -f $(LIB)

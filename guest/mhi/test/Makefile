# Builds mhitest (WP5 M1 harness probe) and mhiparam (WP5 M4 harness
# probe, MHI-PLAN-M3-M4.md WP4.5), the guest-side test clients for
# mhi_copperline.library, with the dockerized m68k-amigaos cross-GCC (see
# ../../toolchain.mk). Same shape as guest/hostfs-test/Makefile: normal
# relocatable hunk executables (LoadSeg relocates them), not ROMs, so no
# -mpcrel/relocation checks apply here.

include ../../toolchain.mk

# -noixemul (not -nostdlib, unlike guest/hostfs-test/mkfile.c): mhitest.c's
# unsigned ULONG/10 decimal formatting needs 32-bit software divide/modulo
# (__udivsi3/__umodsi3 -- the 68000 floor has no hardware instruction for
# it) and its strlen_local() loop gets recognized and rewritten into a real
# strlen() call regardless; both need a real libc.a linked in to resolve,
# which -noixemul provides (the same non-ixemul m68000 libc.a the main
# library links via -noixemul, see ../Makefile) without pulling in
# ixemul.library's own startup/base-library machinery. -nostartfiles still
# suppresses crt0 -- this file's own start.s is the real entry point, same
# as guest/hostfs-test/mkfile.c.
CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra \
          -noixemul -nostartfiles

.PHONY: all clean
all: mhitest mhiparam

# The docker mount (see ../../toolchain.mk's DOCKER) only covers this
# directory, so mhi_abi.h -- otherwise shared read-only from .. -- is
# staged here as a build-local copy rather than widening the mount for one
# header; not committed (see ../../../.gitignore).
mhi_abi.h: ../mhi_abi.h
	cp $< $@

# start.s first: the CLI enters at the start of the first hunk, and the
# compiler may place rodata ahead of the C entry function.
mhitest: start.s mhitest.c mhi_abi.h
	$(CC) $(CFLAGS) start.s mhitest.c -o $@

mhiparam: start.s mhiparam.c mhi_abi.h
	$(CC) $(CFLAGS) start.s mhiparam.c -o $@

clean:
	rm -f mhitest mhiparam mhi_abi.h

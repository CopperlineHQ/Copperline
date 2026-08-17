# Builds mhitest (WP5 M1 harness probe) and mhiseek (WP5 M3 harness probe,
# MHI-PLAN-M3-M4.md WP3.3), the guest-side test clients for
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
all: mhitest mhiseek

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

# -fno-builtin: without it, GCC's loop-to-memcpy idiom recognition at -Os
# rewrites split_two_args' byte-copy loops into calls to libnix's memcpy,
# which (unlike every function this program calls directly through an LVO
# table or an EXEC_BASE_NAME-bound inline stub) references the global
# `SysBase` symbol -- never defined in a -nostartfiles build with no crt0
# to set it, so the link fails with "undefined reference to SysBase".
# mhitest.c's own copy loop happens not to trigger the substitution; this
# one does, so it gets the flag mhitest.c doesn't need.
mhiseek: start.s mhiseek.c mhi_abi.h
	$(CC) $(CFLAGS) -fno-builtin start.s mhiseek.c -o $@

clean:
	rm -f mhitest mhiseek mhi_abi.h

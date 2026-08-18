# Builds zz9kprobe, the guest-side conformance probe for the bundled zz9k
# crypto board (docs/internals/zz9k.md), with the dockerized m68k-amigaos
# cross-GCC (see ../toolchain.mk). A normal relocatable hunk executable
# with the standard -noixemul startup (unlike the -nostartfiles probes:
# this one deliberately links the SDK's own transport, which expects an
# ordinary C runtime).

include ../toolchain.mk

# -fcommon is load-bearing: the vendored SDK transport declares DOSBase as
# a tentative definition, relying on it merging with the DOSBase the
# -noixemul startup code defines and initializes (zz9k_host.c's own comment
# says so). This image's GCC defaults to -fno-common, which turns the
# tentative definition into a second strong zero definition; the startup's
# initialized DOSBase loses the link-time merge and every stdio call jumps
# through a NULL library base (verified: an F-line crash at startup on both
# AROS and Kickstart 3.1 without this flag). The SDK's own build image is a
# GCC 6 era toolchain where -fcommon was still the default.
CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra \
          -noixemul -fcommon -Ivendor

zz9kprobe: zz9kprobe.c vendor/zz9k_host.c
	$(CC) $(CFLAGS) -s zz9kprobe.c vendor/zz9k_host.c -o $@

clean:
	rm -f zz9kprobe

.PHONY: clean

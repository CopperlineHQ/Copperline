# Builds modemtest, the guest-side probe for the modem integration test
# (tests/modem_e2e.rs), with the dockerized m68k-amigaos cross-GCC (see
# ../toolchain.mk). A normal relocatable hunk executable with the
# standard -noixemul startup, the same style guest/zz9kprobe uses and
# verified against both AROS and Kickstart 3.1 -- this probe only calls
# dos.library/exec.library, so it carries none of zz9kprobe's -fcommon
# DOSBase-merge concern.

include ../toolchain.mk

CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul

modemtest: modemtest.c
	$(CC) $(CFLAGS) -s modemtest.c -o $@

clean:
	rm -f modemtest

.PHONY: clean

# Builds chftest, the guest-side probe for copperhf.device's M2
# OpenDevice/CMD_READ/CMD_WRITE round trip (tests/copperhf_device.rs), with
# the dockerized m68k-amigaos cross-GCC (see ../toolchain.mk). A normal
# relocatable hunk executable with the standard -noixemul startup, the same
# style guest/modem-test and guest/zz9kprobe use and verified against both
# AROS and Kickstart 3.1 -- this probe only calls exec.library, so it
# carries none of zz9kprobe's -fcommon DOSBase-merge concern.

include ../toolchain.mk

CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul

chftest: chftest.c
	$(CC) $(CFLAGS) -s chftest.c -o $@

clean:
	rm -f chftest

.PHONY: clean

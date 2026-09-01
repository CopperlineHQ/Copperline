# Builds chftest (M2) and chftest_m4 (M4), the guest-side probes for
# copperhf.device (tests/copperhf_device.rs, tests/copperhf_m4_guest.rs),
# with the dockerized m68k-amigaos cross-GCC (see ../toolchain.mk). Normal
# relocatable hunk executables with the standard -noixemul startup, the same
# style guest/modem-test and guest/zz9kprobe use and verified against both
# AROS and Kickstart 3.1 -- these probes only call exec.library, so they
# carry none of zz9kprobe's -fcommon DOSBase-merge concern.

include ../toolchain.mk

CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul

all: chftest chftest_m4

chftest: chftest.c
	$(CC) $(CFLAGS) -s chftest.c -o $@

chftest_m4: chftest_m4.c
	$(CC) $(CFLAGS) -s chftest_m4.c -o $@

clean:
	rm -f chftest chftest_m4

.PHONY: clean all

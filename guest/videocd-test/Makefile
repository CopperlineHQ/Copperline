# Builds the guest-side videocd.library ABI probe used by the asset-gated
# CD32 integration test. The program is a normal relocatable Amiga HUNK and
# writes its machine-readable result beside itself through the --run hostfs
# mount.

include ../toolchain.mk

CFLAGS = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -Werror \
	-noixemul -nostartfiles -fno-builtin

.PHONY: all clean
all: videocdtest

videocdtest: start.s videocdtest.c
	$(CC) $(CFLAGS) start.s videocdtest.c -o $@

clean:
	rm -f videocdtest VIDEOCD-RESULT

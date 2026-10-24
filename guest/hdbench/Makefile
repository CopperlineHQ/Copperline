# Hard-drive timing benchmark; its disk and configuration come from the host
# test (tests/hd_timing.rs).
include ../toolchain.mk
CFLAGS = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul -nostdlib -nostartfiles

hdbench: start.s hdbench.c
	$(CC) $(CFLAGS) start.s hdbench.c -o $@ -lc -lamiga -lgcc

.PHONY: clean
clean:
	rm -f hdbench

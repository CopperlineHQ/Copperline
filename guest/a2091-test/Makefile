# Raw-device DMA regression probe; its disk image is supplied by the host test.
include ../toolchain.mk
CFLAGS = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul -nostdlib -nostartfiles

dmatest: start.s dmatest.c
	$(CC) $(CFLAGS) start.s dmatest.c -o $@ -lc -lamiga -lgcc

.PHONY: clean
clean:
	rm -f dmatest

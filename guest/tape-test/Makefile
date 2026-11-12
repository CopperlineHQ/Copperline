# SCSI tape regression probe (tests/scsi_tape.rs); its cartridge directory is
# supplied by the host test.
include ../toolchain.mk
CFLAGS = -Os -m68000 -fomit-frame-pointer -Wall -Wextra -noixemul -nostdlib -nostartfiles

tapetest: start.s tapetest.c
	$(CC) $(CFLAGS) start.s tapetest.c -o $@ -lc -lamiga -lgcc

.PHONY: clean
clean:
	rm -f tapetest

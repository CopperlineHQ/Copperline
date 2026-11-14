# OS-heavy host-performance workload; see README.md. It uses no NDK headers,
# so any m68k-amigaos GCC builds it (CC=/opt/amiga/bin/m68k-amigaos-gcc).
include ../toolchain.mk
CFLAGS = -O2 -m68020 -fomit-frame-pointer -Wall -Wextra -noixemul -nostdlib -nostartfiles -ffreestanding

osbench: start.s osbench.c
	$(CC) $(CFLAGS) start.s osbench.c -o $@ -lgcc

.PHONY: clean
clean:
	rm -f osbench

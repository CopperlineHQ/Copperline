# Builds the uaelib guest probe for the uaelib-trap integration tests with
# the dockerized m68k-amigaos cross-GCC (see guest/toolchain.mk). Like the
# hostfs probe it is a normal relocatable hunk executable (LoadSeg relocates
# it), so it needs none of the services ROM's PC-relative/no-data checks.

include ../toolchain.mk

# Freestanding: no libc, and no loop-idiom rewriting either, or GCC turns
# the probe's own strlen/memset loops into calls to themselves.
CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra \
          -nostdlib -nostartfiles -ffreestanding \
          -fno-tree-loop-distribute-patterns

# start.s first: the CLI enters at the start of the first hunk, and the
# compiler may place rodata ahead of the C entry function.
uaelibtest: start.s support.s uaelibtest.c
	$(CC) $(CFLAGS) start.s support.s uaelibtest.c -o $@

clean:
	rm -f uaelibtest

.PHONY: clean

# Builds the mkfile guest probe for the hostfs integration tests with the
# dockerized m68k-amigaos cross-GCC (see guest/toolchain.mk). The committed
# artifact is a normal relocatable hunk executable -- LoadSeg relocates it --
# so unlike the services ROM it needs no PC-relative or no-data checks.

include ../toolchain.mk

CFLAGS  = -Os -m68000 -fomit-frame-pointer -Wall -Wextra \
          -nostdlib -nostartfiles

# start.s first: the CLI enters at the start of the first hunk, and the
# compiler may place rodata ahead of the C entry function.
mkfile: start.s mkfile.c
	$(CC) $(CFLAGS) start.s mkfile.c -o $@

clean:
	rm -f mkfile

.PHONY: clean

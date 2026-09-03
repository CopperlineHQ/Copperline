# Builds the DAP adapter's debug-information probes, both read by the
# unit tests in src/debuginfo/ (rebuild and re-check them together).
#
# `lines` is vasm output with LINE debug hunks (-linedebug), the format
# assembly projects get, from the shared dockerized toolchain
# (guest/toolchain.mk).
#
# `hello` is C with DWARF (-g -O0): a relocatable hunk executable whose
# trailing HUNK_DEBUG block carries the DWARF sections with a symbol
# table naming each section's start -- the format src/debuginfo/hunk.rs
# reads. Only bebbo's gcc 6.5 toolchain with binutils 2.39 writes that
# block; the current amiga-gcc docker images (gcc 6.5 with binutils
# 2.46, and the gcc 13/15/16 images) drop the DWARF sections from hunk
# output, so `hello` builds with a host-installed toolchain named by
# HELLO_CC (an amiga-gcc `make all` install, the /opt/amiga default) and
# is committed built. No NDK headers are needed.

include ../toolchain.mk

HELLO_CC ?= /opt/amiga/bin/m68k-amigaos-gcc
VASM      = $(DOCKER) vasmm68k_mot

# -O0 keeps every local in a stack slot with a plain frame-base
# location, which is what the adapter's variable support covers.
CFLAGS  = -g -O0 -m68000 -Wall -Wextra -nostdlib -nostartfiles -ffreestanding \
          -fno-tree-loop-distribute-patterns

all: hello lines

# start.s first: the CLI enters at the start of the first hunk.
hello: start.s hello.c
	$(HELLO_CC) $(CFLAGS) start.s hello.c -o $@

lines: lines.s
	$(VASM) -Fhunkexe -linedebug -o $@ $<

clean:
	rm -f hello lines

.PHONY: all clean

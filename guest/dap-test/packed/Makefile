# SPDX-License-Identifier: GPL-3.0-or-later
AMIGA_CC ?= m68k-amigaos-gcc
PROBES = program-dwarf2 program-dwarf4 program-dwarf5

.PHONY: all clean
all: $(PROBES)

program-dwarf%: main.c worker.c
	$(AMIGA_CC) -m68020 -O0 -g -gdwarf-$* -ffreestanding \
		-fno-omit-frame-pointer -fdebug-prefix-map=$(CURDIR)=. \
		-nostdlib -Wl,-e,_entry main.c worker.c -o $@

clean:
	rm -f $(PROBES)

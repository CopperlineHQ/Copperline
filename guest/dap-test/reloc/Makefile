# Committed fixtures: tests need neither the compiler nor external ROMs.
# Rebuild with Bartman's m68k-amiga-elf-gcc and elf2hunk on PATH, or set
# ELF_CC and ELF2HUNK to their installed paths. No SDK headers required.
ELF_CC ?= m68k-amiga-elf-gcc
ELF2HUNK ?= elf2hunk
CFLAGS = -g -O0 -m68000 -nostdlib -ffreestanding -fno-omit-frame-pointer \
         -fno-common -fno-pic -Wall -Wextra -fdebug-prefix-map=$(CURDIR)=.

all: program-dwarf4 program-dwarf5 program-linked

program-dwarf4.elf: main.c worker.c
	$(ELF_CC) $(CFLAGS) -gdwarf-4 -r $^ -o $@

program-dwarf5.elf: main.c worker.c
	$(ELF_CC) $(CFLAGS) -gdwarf-5 -r $^ -o $@

# Retained relocations in a fully linked file have already been applied.
program-linked.elf: main.c worker.c
	$(ELF_CC) $(CFLAGS) -gdwarf-5 -Wl,--emit-relocs,-Ttext=0,-e,entry $^ -o $@

%: %.elf
	$(ELF2HUNK) $< $@

clean:
	rm -f program-dwarf4 program-dwarf4.elf program-dwarf5 program-dwarf5.elf \
	      program-linked program-linked.elf

.PHONY: all clean

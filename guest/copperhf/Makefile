# Builds copperhf.device's boot ROM with the dockerized m68k-amigaos
# cross-GCC, and installs it as the committed artifact
# assets/copperhf/copperhf_rom.bin (rebuilding needs docker; a plain cargo
# build just embeds the committed ROM).
#
# The ROM must be position-independent and self-contained: -mpcrel makes all
# code/rodata references PC-relative, and the checks below fail the build if
# the linked executable still contains relocations or data/bss hunks
# (objcopy would silently emit a ROM that breaks at any other base). It must
# also fit below CHF_MAGIC (0x4000, copperhf_board.h): the register block
# starts there and the ROM is served read-only across 0x0000-0x3FFF only.

ROM        = ../../assets/copperhf/copperhf_rom.bin
ROM_BUDGET = 0x4000

include ../toolchain.mk

# -fno-strict-aliasing: the ROM addresses shared guest/host structures
# (IORequest blocks, RDB blocks) through recast byte pointers by design;
# with strict aliasing GCC dead-store-eliminated mounter.c's request field
# writes outright (see chf_read_block's own comment for the confirmed
# failure).
CFLAGS  = -Os -m68000 -mpcrel -fomit-frame-pointer -fno-jump-tables \
          -fno-builtin-memset -fno-builtin-memcpy \
          -fno-tree-loop-distribute-patterns -fno-strict-aliasing \
          -Wall -Wextra -nostdlib -nostartfiles

$(ROM): device.bin
	@size=$$(wc -c < device.bin); \
	  if [ $$size -gt $$(($(ROM_BUDGET))) ]; then \
	    echo "device.bin: $$size bytes exceeds the $(ROM_BUDGET)-byte ROM budget (CHF_MAGIC)"; \
	    exit 1; \
	  fi
	cp device.bin $(ROM)

# The docker mount only covers this directory, so objcopy writes a local
# intermediate and the install step above copies it into assets/.
device.bin: device.exe
	@! $(OBJDUMP) -r device.exe | grep -q RELOC \
	  || { echo "device.exe: has relocations; code must be PC-relative"; exit 1; }
	@! $(OBJDUMP) -h device.exe | grep -Eq '[.](data|bss)' \
	  || { echo "device.exe: has data/bss; keep state on the stack"; exit 1; }
	$(OBJCOPY) -O binary device.exe $@

# entry.s first: the entry table must sit at the start of the code hunk.
# int_handler.s needs -x assembler-with-cpp (a plain ".s" name isn't
# preprocessed by default) so it can #include copperhf_board.h instead of
# duplicating register offsets; entry.s itself has no #includes, so it
# stays plain asm; -x c switches back for device.c and mounter.c (M3).
device.exe: entry.s device.c mounter.c int_handler.s copperhf_board.h
	$(CC) $(CFLAGS) entry.s -x c device.c mounter.c \
	  -x assembler-with-cpp int_handler.s -x none -o $@

clean:
	rm -f device.exe device.bin

.PHONY: clean

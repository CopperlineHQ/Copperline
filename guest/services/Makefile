# Builds the guest-side handler ROM for Copperline's services board with the
# dockerized m68k-amigaos cross-GCC, and installs it as the committed artifact
# assets/services/services_rom.bin (rebuilding needs docker; a plain cargo
# build just embeds the committed ROM).
#
# The ROM must be position-independent and self-contained: -mpcrel makes all
# code/rodata references PC-relative, and the checks below fail the build if
# the linked executable still contains relocations or data/bss hunks
# (objcopy would silently emit a ROM that breaks at any other base).

ROM     = ../../assets/services/services_rom.bin

include ../toolchain.mk

CFLAGS  = -Os -m68000 -mpcrel -fomit-frame-pointer -fno-jump-tables \
          -Wall -Wextra -nostdlib -nostartfiles

$(ROM): handler.bin
	cp handler.bin $(ROM)

# The docker mount only covers this directory, so objcopy writes a local
# intermediate and the install step above copies it into assets/.
handler.bin: handler.exe
	@! $(OBJDUMP) -r handler.exe | grep -q RELOC \
	  || { echo "handler.exe: has relocations; code must be PC-relative"; exit 1; }
	@! $(OBJDUMP) -h handler.exe | grep -Eq '[.](data|bss)' \
	  || { echo "handler.exe: has data/bss; keep state on the stack"; exit 1; }
	$(OBJCOPY) -O binary handler.exe $@

# entry.s first: the entry table must sit at the start of the code hunk.
handler.exe: entry.s handler.c copperline_board.h
	$(CC) $(CFLAGS) entry.s handler.c -o $@

clean:
	rm -f handler.exe handler.bin

.PHONY: clean

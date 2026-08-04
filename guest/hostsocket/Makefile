# Builds the guest-side bsdsocket.library stub ROM for Copperline's bundled
# HostSocket board with the dockerized m68k-amigaos cross-GCC, and installs
# it as the committed artifact assets/hostsocket/hostsocket_rom.bin
# (rebuilding needs docker; a plain cargo build just embeds the committed
# ROM).
#
# The ROM must be position-independent and self-contained: -mpcrel makes all
# code/rodata references PC-relative, and the checks below fail the build if
# the linked executable still contains relocations or data/bss hunks
# (objcopy would silently emit a ROM that breaks at any other base).

ROM     = ../../assets/hostsocket/hostsocket_rom.bin

include ../toolchain.mk

CFLAGS  = -Os -m68000 -mpcrel -fomit-frame-pointer -fno-jump-tables \
          -Wall -Wextra -nostdlib -nostartfiles

$(ROM): stub.bin
	cp stub.bin $(ROM)

# The docker mount only covers this directory, so objcopy writes a local
# intermediate and the install step above copies it into assets/.
stub.bin: stub.exe
	@! $(OBJDUMP) -r stub.exe | grep -q RELOC \
	  || { echo "stub.exe: has relocations; code must be PC-relative"; exit 1; }
	@! $(OBJDUMP) -h stub.exe | grep -Eq '[.](data|bss)' \
	  || { echo "stub.exe: has data/bss; keep state on the stack"; exit 1; }
	$(OBJCOPY) -O binary stub.exe $@

# entry.s first: the entry table must sit at the start of the code hunk.
# -x assembler-with-cpp forces cpp over entry.s (a plain ".s" name isn't
# preprocessed by default) so it can #include hostsocket_board.h instead of
# duplicating register offsets; -x c switches back for stub.c.
stub.exe: entry.s stub.c hostsocket_board.h
	$(CC) $(CFLAGS) -x assembler-with-cpp entry.s -x c stub.c -o $@

clean:
	rm -f stub.exe stub.bin

.PHONY: clean

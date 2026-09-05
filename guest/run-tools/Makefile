# Redistributable 68000 commands for the bare-ROM --run boot volume.
include ../toolchain.mk

CFLAGS = -Os -m68000 -Wall -Wextra -Werror -nostdlib -nostartfiles \
         -ffreestanding -fno-tree-loop-distribute-patterns
TOOLS = FailAt CD Stack Echo Execute

all: $(TOOLS) probe

$(TOOLS): start.s commands.c

FailAt:
	$(CC) $(CFLAGS) -DCOMMAND=1 start.s commands.c -o $@
CD:
	$(CC) $(CFLAGS) -DCOMMAND=2 start.s commands.c -o $@
Stack:
	$(CC) $(CFLAGS) -DCOMMAND=3 start.s commands.c -o $@
Echo:
	$(CC) $(CFLAGS) -DCOMMAND=4 start.s commands.c -o $@
Execute:
	$(CC) $(CFLAGS) -DCOMMAND=5 start.s commands.c -o $@

probe: start.s probe.c
	$(CC) $(CFLAGS) start.s probe.c -o $@

clean:
	rm -f $(TOOLS) probe

.PHONY: all clean

PYTHON ?= python3
ROM := build/copperline-fmv.rom
DRIVER := build/cd32mpeg.device
include ../guest/toolchain.mk

CFLAGS := -Os -m68000 -noixemul -nostartfiles -std=c99 \
	-fomit-frame-pointer -Wall -Wextra -Werror -Wstrict-prototypes

.PHONY: all bundle check clean

all: $(ROM)

$(ROM): build_rom.py hunk.py $(DRIVER)
	$(PYTHON) build_rom.py --driver-hunk $(DRIVER) --output $@

$(DRIVER): src/cd32mpeg.c src/decoder.c src/videocd.c src/player.c \
		src/cd32mpeg.h src/videocd.h src/player.h
	mkdir -p build
	$(CC) $(CFLAGS) src/cd32mpeg.c src/decoder.c src/videocd.c src/player.c \
		-lamiga -o $@

bundle: $(ROM)
	mkdir -p ../assets/fmv
	install -m 644 $(ROM) ../assets/fmv/copperline-fmv.rom

check: $(DRIVER)
	FMV_DRIVER_HUNK=$(DRIVER) PYTHONDONTWRITEBYTECODE=1 \
		$(PYTHON) -m unittest discover -s tests -v

clean:
	rm -f $(ROM) $(DRIVER)

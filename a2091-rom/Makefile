PYTHON ?= python3
FULL_VERSION ?= 42.40
DEVICE_VERSION := 42
DEVICE_REVISION := 40
AMIGA_DATE := 31.8.2026

include ../guest/toolchain.mk

VASM  := $(DOCKER) vasmm68k_mot
VLINK := $(DOCKER) vlink
STRIP := $(DOCKER) m68k-amigaos-strip

BUILD := build
OBJ := $(BUILD)/obj
ROM := $(BUILD)/copperline-a2091.rom
NODRIVER_ROM := $(BUILD)/copperline-a2091-nodriver.rom
DRIVER := $(BUILD)/scsi.device
BOOT := $(BUILD)/boot.bin
NDK := /opt/amiga/m68k-amigaos/ndk-include

SRCS := src/device.c src/version.c src/port.c src/attach.c \
	src/cmdhandler.c src/printf.c src/sd.c src/scsipi_base.c \
	src/scsipiconf.c src/scsiconf.c src/scsimsg.c \
	thirdparty/mounter/mounter.c src/bootmenu.c src/romfile.c \
	src/a2091_scsi.c
OBJS := $(patsubst %.c,$(OBJ)/%.o,$(SRCS)) $(OBJ)/src/reloc.o

CFLAGS := -DBUILD_DATE=\"2026-08-31\" -DBUILD_TIME=\"00:00:00\" \
	-DAMIGA_DATE=\"$(AMIGA_DATE)\" -D_KERNEL -DPORT_AMIGA \
	-DDRIVER_A2091 -DDEVNAME=scsi -DHAVE_ROM=1 \
	-DDEVICE_VERSION=$(DEVICE_VERSION) -DDEVICE_REVISION=$(DEVICE_REVISION) \
	-DFULL_VERSION=$(FULL_VERSION) -Isrc -Ithirdparty/mounter \
	-Os -fomit-frame-pointer -noixemul -msmall-code -m68000 \
	-Wall -Wextra -Wno-pointer-sign
LDFLAGS := -nostartfiles -nostdlib -ldebug -lgcc -lc -lamiga -ramiga-dev

ifeq ($(DEBUG_A2091),1)
CFLAGS += -DDEBUG_DEVICE -DDEBUG_CMDHANDLER -DDEBUG_SCSIPI \
	-DDEBUG_SCSIPI_BASE -DDEBUG_SCSICONF -DDEBUG_SD -DDEBUG_MOUNTER
endif

ifeq ($(MOUNTER_DEBUG),1)
CFLAGS += -DDEBUG_MOUNTER
endif

.PHONY: all bundle check clean scaffold-images

all: $(ROM) $(NODRIVER_ROM)

$(OBJ)/%.o: %.c
	mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -c $< -o $@

$(OBJ)/src/reloc.o: src/reloc.S
	mkdir -p $(dir $@)
	$(VASM) -quiet -m68000 -Fhunk -o $@ $< -I src -I . -I $(NDK)

$(DRIVER): $(OBJS)
	mkdir -p $(dir $@)
	$(CC) $(CFLAGS) $(OBJS) $(LDFLAGS) -Wl,-Map=$(BUILD)/scsi.device.map -o $@
	$(STRIP) $@

$(OBJ)/rom.o: src/rom.S src/reloc.S src/version.i
	mkdir -p $(dir $@)
	$(VASM) -quiet -m68000 -Fhunk -o $@ $< -I src -I . -I $(NDK)

$(BOOT): $(OBJ)/rom.o rom.ld
	mkdir -p $(dir $@)
	$(VLINK) -Trom.ld -brawbin1 -o $@ $<

$(ROM): build_rom.py hunk.py $(BOOT) $(DRIVER)
	$(PYTHON) build_rom.py --boot $(BOOT) --driver $(DRIVER) --size 64 --output $@

$(NODRIVER_ROM): build_rom.py hunk.py $(BOOT)
	$(PYTHON) build_rom.py --boot $(BOOT) --size 64 --output $@

scaffold-images: $(BOOT)
	$(PYTHON) build_rom.py --boot $(BOOT) --size 16 --output $(BUILD)/a2091-scaffold-16.rom
	$(PYTHON) build_rom.py --boot $(BOOT) --size 32 --output $(BUILD)/a2091-scaffold-32.rom
	$(PYTHON) build_rom.py --boot $(BOOT) --size 64 --output $(BUILD)/a2091-scaffold-64.rom

bundle: $(ROM)
	mkdir -p ../assets/a2091
	install -m 644 $(ROM) ../assets/a2091/copperline-a2091.rom

check: $(ROM) scaffold-images
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) -m unittest discover -s tests -v

clean:
	rm -rf $(BUILD)

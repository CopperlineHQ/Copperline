PROGRAM ?= demo
TOOLCHAIN ?= auto
SOURCES := main.c uaelib.c

ifeq ($(TOOLCHAIN),auto)
  ifneq ($(shell command -v m68k-amiga-elf-gcc 2>/dev/null),)
    TOOLCHAIN := bartman
  else ifneq ($(shell command -v m68k-amigaos-gcc 2>/dev/null),)
    TOOLCHAIN := bebbo
  else ifneq ($(shell command -v vc 2>/dev/null),)
    TOOLCHAIN := vasm
  else
    $(error No Amiga toolchain found: install m68k-amiga-elf-gcc, m68k-amigaos-gcc, or vbcc/vasm)
  endif
endif

CFLAGS := -g -O2 -m68000 -Wall -Wextra

ifeq ($(TOOLCHAIN),bartman)
  CC := m68k-amiga-elf-gcc
  SOURCES += startup.s format_gcc.s runtime.c
  CFLAGS += -nostdlib -ffreestanding -ffunction-sections -fdata-sections -DCOPPERLINE_BARE_STARTUP
  LDFLAGS := -Wl,--emit-relocs,--gc-sections,-Ttext=0,-e,_start,-Map=$(PROGRAM).map
  TARGET := $(PROGRAM).elf
else ifeq ($(TOOLCHAIN),bebbo)
  CC := m68k-amigaos-gcc
  SOURCES += format_gcc.s
  TARGET := $(PROGRAM)
else ifeq ($(TOOLCHAIN),vasm)
  CC := vc
  SOURCES += format_vasm.asm
  CFLAGS := +kick13 -g -O2 -cpu=68000
  TARGET := $(PROGRAM)
else
  $(error TOOLCHAIN must be auto, bartman, bebbo, or vasm)
endif

.PHONY: all clean adf size
all: $(PROGRAM)

ifeq ($(TOOLCHAIN),bartman)
$(PROGRAM): $(TARGET)
	elf2hunk $< $@
endif

$(TARGET): $(SOURCES) uaelib.h
	$(CC) $(CFLAGS) $(LDFLAGS) $(SOURCES) -o $@

adf: $(PROGRAM)
	copperline-ctl exe2adf $(PROGRAM) --boot --out $(PROGRAM).adf

size: $(PROGRAM)
	copperline-ctl size-report $(PROGRAM) $(if $(wildcard $(PROGRAM).elf),--elf $(PROGRAM).elf) --out $(PROGRAM).size.cpuprofile

clean:
	$(RM) $(PROGRAM) $(PROGRAM).elf $(PROGRAM).map $(PROGRAM).adf $(PROGRAM).size.cpuprofile

# Copperline A2091/A590 open boot ROM

This directory builds Copperline's clean-room, freely redistributable A2091
and A590 SCSI autoboot ROM. It contains a 68000-compatible `scsi.device`, an
RDB automounter with Kickstart 1.3 support, a WD33C93 PIO/DMA transport, and
Commodore DMAC glue. It does not contain Commodore ROM code and does not probe
the A590 XT interface.

The common device/SCSI/mounter code is derived from A4091 software v42.39.
Board-specific code and the rotated image builder are BSD-2-Clause. See
`THIRD_PARTY_NOTICES.txt` for the exact source revisions and notices.

## Build

The repository's standard Amiga Docker toolchain is used, so no host m68k
compiler is required:

```sh
make -C a2091-rom
make -C a2091-rom check
make -C a2091-rom bundle
```

The build produces:

- `build/copperline-a2091.rom`: 64 KiB merged emulator/EPROM image;
- `build/copperline-a2091-U13.bin`: even-byte EPROM half;
- `build/copperline-a2091-U12.bin`: odd-byte EPROM half;
- `build/scsi.device`: disk-loadable HUNK executable;
- `build/copperline-a2091-nodriver.rom`: loader-only diagnostic image.

`build_rom.py` can also create 16 KiB and 32 KiB scaffold images. Its input
is board-linear from board offset `$2000`; it rotates that payload into the
physical EPROM address space, leaves the shadowed first 8 KiB of a 64 KiB
image erased, validates the DiagArea/Resident/HUNK/TOC, and splits U13/U12.

## Driver policy

Short control transfers use asynchronous PIO. Sector transfers use the DMAC
when the buffer address and length are even and the entire range lies below
16 MiB. The shared SCSI layer allocates a Chip RAM bounce buffer otherwise,
including for accelerator or Zorro III RAM. DMA completion uses a shared
`INTB_PORTS` server; disconnect is drained with interrupts gated.

The ROM has been exercised headlessly against Copperline's A2091 model with
Kickstart 3.1 and a real RDB Workbench installation. Real A2091/A590 EPROM
testing remains a separate hardware acceptance step; verify board jumpers and
EPROM type before programming the split outputs.

The ROM builders share the HUNK reader and relocator in
`../tools/amiga_hunk.py`; keep the repository layout when running them.
Run its asset-free tests with `python3 -m unittest discover -s tools/tests -v`
from the repository root. Each ROM Makefile also tracks this helper as a build
dependency.

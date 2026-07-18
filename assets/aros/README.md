# Bundled AROS ROM

Copperline boots these AROS m68k ROM images when the user supplies no
Kickstart of their own (see `src/romsearch.rs`). AROS (the AROS Research
Operating System) is an open-source, freely redistributable re-implementation
of the AmigaOS API, licensed under the AROS Public License (`LICENSE`). Unlike
a real Kickstart it can legally ship with the program.

## Files

| File                          | Size      | Maps at  | Role                          |
|-------------------------------|-----------|----------|-------------------------------|
| `aros-amiga-m68k-rom.bin`     | 512 KiB   | $F80000  | Kickstart-replacement ROM     |
| `aros-amiga-m68k-ext.bin`     | 512 KiB   | $E00000  | Extended ROM                  |

The two halves are consumed exactly as WinUAE and FS-UAE take them.

## Provenance

Built from source on 2026-07-18 from the AROS boot-time-optimization branch
of upstream pull request 829 (https://github.com/aros-development-team/AROS/pull/829),
branch `optimize-68k-boot-rom` of https://github.com/warpdesign/AROS at commit
137d07c5. That branch cuts the m68k boot to the insert-disk screen from
roughly 25-30 s to under 10 s (single-pass romtag scan, fast memory clearing,
blitter-drawn boot animation), which also shortens every AROS-booted golden
probe run in CI (tests/probe_golden.rs).

Build recipe (Linux, or a Linux container; the AROS crosstools do not build
cleanly on macOS):

    git clone --branch optimize-68k-boot-rom https://github.com/warpdesign/AROS.git
    cd AROS && git submodule update --init   # catalog strings live in submodules
    mkdir ../build && cd ../build
    ../AROS/configure --target=amiga-m68k
    make kernel-link-amiga-m68k
    # ROMs land in bin/amiga-m68k/gen/boot/aros-amiga-m68k-{rom,ext}.bin

Once PR 829 is merged, refreshing from the official nightly again is simpler:
download `AROS-<date>-amiga-m68k-boot-iso.zip` from
https://sourceforge.net/projects/aros/files/nightly2/, extract the ISO, and
pull `boot/amiga/aros-rom.bin` and `boot/amiga/aros-ext.bin` (renamed to the
WinUAE/FS-UAE convention used here). Both files must be exactly 524288 bytes
(512 KiB). Also refresh `LICENSE` and `ACKNOWLEDGEMENTS` from the same
source tree.

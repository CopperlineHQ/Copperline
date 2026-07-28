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

Built from source on 2026-07-28 from AROS upstream master
(https://github.com/aros-development-team/AROS) at commit d0370bd757,
plus two not-yet-merged fixes:

- the NTSC boot fix of pull request 876
  (https://github.com/aros-development-team/AROS/pull/876, commit
  c4780bddbd): dosboot and intuition probed BestModeID for a 640x480 mode
  before opening their screens and dead-ended with alert 84000009
  ("unknown type of system screen") when it was absent, which made every
  NTSC machine guru and reboot-loop at boot because an NTSC-only display
  database holds nothing taller than 400 lines.
- the input-event-loss fix of pull request 878
  (https://github.com/aros-development-team/AROS/pull/878, commit
  03a6393257): input events delivered before the first consumer
  registered with the input subsystem were dropped, so the keyboard's
  power-up key stream (the codes of keys held during boot, drained the
  moment the driver starts handshaking) never reached keyboard.device's
  matrix, KBD_READMATRIX read all zeros, and dosboot's hold-SPACE/HELP
  Early Startup menu check could not fire (Copperline issue 317). The
  fix buffers pre-consumer events in the input subsystem and replays
  them to the first consumer that attaches.
Master includes the boot-time optimizations of pull request 829
(https://github.com/aros-development-team/AROS/pull/829: single-pass
romtag scan, fast memory clearing, blitter-drawn boot animation), which cut
the m68k boot to the insert-disk screen from roughly 25-30 s to under 10 s
and shorten every AROS-booted golden probe run in CI (tests/probe_golden.rs),
the boot-animation rendering fix of pull request 848
(https://github.com/aros-development-team/AROS/pull/848: reverts an unsafe
OCS rollover display change in the amigavideo driver), the m68k
Workbench/console rendering speedups of pull request 844, and the fix for
issue 849 (https://github.com/aros-development-team/AROS/issues/849,
commit 747405ba10): the early-startup Boot Options page formatted its
device list with a 64-bit UQUAD block count under a 32-bit `%d` specifier,
so every following argument read from the wrong varargs offset and the Exec
Bootstrap Task crashed on machines with RDB drives attached.

Build recipe (Linux, or a Linux container; the AROS crosstools do not build
cleanly on macOS):

    git clone https://github.com/aros-development-team/AROS.git
    cd AROS && git submodule update --init   # catalog strings live in submodules
    mkdir ../build && cd ../build
    ../AROS/configure --target=amiga-m68k    # needs python3-mako
    make kernel-link-amiga-m68k
    # ROMs land in bin/amiga-m68k/gen/boot/aros-amiga-m68k-{rom,ext}.bin

Refreshing from the official nightly is a simpler alternative:
download `AROS-<date>-amiga-m68k-boot-iso.zip` from
https://sourceforge.net/projects/aros/files/nightly2/, extract the ISO, and
pull `boot/amiga/aros-rom.bin` and `boot/amiga/aros-ext.bin` (renamed to the
WinUAE/FS-UAE convention used here). Both files must be exactly 524288 bytes
(512 KiB). Also refresh `LICENSE` and `ACKNOWLEDGEMENTS` from the same
source tree.

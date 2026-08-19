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

Built from source on 2026-08-19 from AROS upstream master
(https://github.com/aros-development-team/AROS) at commit d07902ab
(the merge of the cd.device CD32 CD-boot series, pull request 1018),
plus the low-chip boot-console series of pull request 1022
(https://github.com/aros-development-team/AROS/pull/1022, through commit
89a6f60e), which is not yet merged. Fixes Copperline contributed or
depends on, in master unless noted:

- the low-chip boot-console series of pull request 1022 (in flight):
  a boot console that cannot get its window latches the failure and
  degrades to a sink instead of re-attempting a 40 KiB Workbench screen
  on every packet (the loop a CD32 game that fills all of chip RAM used
  to trigger), the bitmap-allocation failure paths gain diagnostics
  including free-chip figures, and a window closed on a failed
  console.device open no longer leaves a dangling pointer.
- the cd.device CD32 CD-boot series of pull request 1018 (merged
  2026-08-19):
  CD0: registered with the DosType CDVDFS actually claims (a 2019
  regression had left it unmountable), latched-completion handling in
  the Akiko command loop, the disc probe moved off the boot task onto
  the unit task, timeouts on the drive waits, and a DosEnvec that no
  longer carries stack garbage. With these, CD32 discs mount through
  CDVDFS and boot.
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
- the EnableAGA low-memory fix (commit 7df15c66cb): SetChipRev rebuilt
  every composited screen's copper list in place with AGA-sized content,
  overrunning the pre-AGA-sized chip RAM allocation and trampling low
  memory including AbsExecBase, so the first program run after
  C:SetPatch on an AGA machine jumped through a garbage ExecBase.
- the amigavideo / graphics.library correctness batch merged 2026-07-29
  to 2026-07-31 (pull requests 879, 886, 895, 896, 902, 903 and 906):
  AGA palette writes through NULL copper pointers, RectFill drawmode
  handling, `rp->Mask` reaching the drivers, plane counts in the HIDD
  BltBitMap path, pattern/template fill masks, blitter edge- and
  write-mask handling in the Amiga driver, and a blitter-matching line
  tie-break.
- the follow-up batch merged 2026-08-01 to 2026-08-06: further
  graphics.library blit semantics (the minterm applied over pens rather
  than resolved colours, the plane mask applied to source pens, FRST_DOT
  polyline complement handling, COMPLEMENT JAM2 pattern fills, bitmaps
  freed with their allocated size), amigavideo taking BltPattern's word
  masks from the mask and flushing the pixel cache before plane
  readback, three exec fixes (task registration without preempting the
  creator, named callers in bad-free alerts, C runtime taken from the
  static linklib), a con-handler rework (served over a device rather
  than a console window, clean ACTION_DIE shutdown, no requesters
  without a window), DOS treating a short read as a failure, afs/fat
  filesystem hardening, and a new Paula serial hidd for m68k-amiga.

Master also includes the boot-time optimizations of pull request 829
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

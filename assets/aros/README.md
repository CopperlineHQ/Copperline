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

Built from source on 2026-09-05 from AROS upstream master
(https://github.com/aros-development-team/AROS) at commit `a3cfa659ed`,
plus one local patch, `patches/0001-m68k-amiga-dos-keep-the-8-KiB-process-stack-floor.patch`:

- arch/m68k-amiga/dos/dos_platform.h keeps `PROC_MINSTACKSIZE` at 8 KiB.
  Upstream commit `e9c4ecde99` (pull request 1109) lowered the m68k DOS
  process stack floor from 16 KiB to 4 KiB and pinned the Shell-seg,
  "Boot Mount" and lddemon processes at 4 KiB, sized from a CD32 disc
  boot, which runs no Startup-Sequence. A boot that does run one -- the
  `--run` staging volume with its `Run`/`Execute` line, plus the
  host-directory and copperhf handler processes -- overflows those stacks:
  the program never starts and the boot process warm-reboots a minute
  later (every copperhf autoboot integration test failed). Restoring the
  8 KiB floor fixes all of them. Two stacks overflow independently:
  AROS's own Shell-seg pin (raising Copperline's handler request while
  leaving the pin at 4 KiB still fails) and the 6000-byte stack
  Copperline's host-directory handler used to request (raising every AROS
  pin to 8 KiB while leaving that request still fails; the same
  refresh raises it to 16 KiB in `guest/services/handler.c`). Drop the
  patch once upstream carries an equivalent floor or unpins the shell.

Plain machine boots (the "Waiting for bootable media" screen), CD32 game
boots and the Cannon Fodder FMV regression pass with and without the patch;
only Startup-Sequence boots with handler processes need it.

Upstream changes since the previous refresh (master `6b5933dc` plus the
then-draft pull request 1089):

- pull request 1089 (https://github.com/aros-development-team/AROS/pull/1089,
  merged 2026-09-01) is now in master: the open `cd32mpeg.device`, Mode-2
  continuous `CD_READXL`, and the diagnostic-ROM gate which prevents
  Commodore's cartridge from replacing the AROS `cd.device`. Copperline's
  CDXL ordering fix, previously carried as draft commit `ebfc7d9`, landed
  as commit `64eb7ed1a8` ("cd: preserve CDXL sector arrival order"): each
  PBX snapshot is sorted by absolute MSF before it is copied into the CDXL
  stream, because Akiko services the highest armed PBX slot first and
  slot-number order can otherwise swap adjacent MPEG sectors after a high
  slot is re-armed. Commit `0a5afc83e5` adds the device to the ROM's
  module dependency list so the ext bank actually builds it.
- the CD32 chip-memory and Microcosm series of pull request 1109
  (https://github.com/aros-development-team/AROS/pull/1109, merged
  2026-09-03): bootstrap stack allocation units fixed, measured m68k
  system-task stacks reduced, CDVDFS readahead shrunk, CD32 services (the
  FMV worker, phantom floppy support) created only when first needed, and
  the Akiko EEPROM used directly instead of mounting a RAM-backed NVRAM
  filesystem -- free chip RAM at Microcosm startup rises from 1,236,496 to
  1,562,168 bytes, which lets the game's CDXL arena avoid its
  producer/consumer wrap deadlock on a stock 2 MB CD32. The same series
  honours zero-length CDXL transfer terminators, fixes Microcosm's CDXL
  startup, and restarts the CIA timer after an aborted timer.device
  request.
- the CDXL presentation series of pull request 1125
  (https://github.com/aros-development-team/AROS/pull/1125, merged
  2026-09-04): PBX sector copying moves to the cd.device task with
  completed-node callbacks kept in softint context, palette updates are
  batched and published with the plane changes as one guarded copper-list
  transaction, DBufInfo replies are deferred to the next graphics VBlank
  (Alien Breed: Tower Assault no longer shows a new palette over an old
  bitmap), `CD_READXL` node-list handling is fixed, classic planar viewport
  display updates are supported, stale ColorMap ViewPortExtra references
  are cleared, repeated `LoadView(NULL)` behaviour is preserved, and the
  CD32 filesystem readahead is 64 KiB.
- ROM footprint work by Nick Andrews (commits `31fceee88e`, `c92a242d9a`,
  `7405611e23`, `21d72f3c45`, `f2f0885604`, `bb0ebd96b9`): every kickstart
  module of the amiga-m68k ROM is now built for size through
  `ROM_OPTIMIZATION_CFLAGS` (before, only exec, dos, Shell and the shell
  commands were), static-library code whose only callers live in the ext
  bank is placed in the ext bank (18.6 KB out of the main bank), genmodule's
  per-module init and expunge sequences are shared in libautoinit instead
  of being emitted inline into every module, and exec, intuition and a
  set of modules that forced `DEBUG` on no longer carry their runtime
  debug logs.
- the exec locking series of pull request 1086
  (https://github.com/aros-development-team/AROS/pull/1086, merged
  2026-08-29): the port, resource, semaphore and library paths take the
  matching system-list spinlock, and SumLibrary holds the library lock
  across the checksum; the m68k ROM picks these up as common exec code.
  Commits `1d1fe254cd` and `119c3206c2` add `SD_ACTION_REBOOT`, serialise
  the reset-callback chain with a timer-bounded wait per handler, and keep
  that state in IntExecBase (a kickstart module has no `.bss`).
- dos.library: the first access through a late assign with ADD/PREPEND
  entries now searches every directory (commit `1913822229`, the
  `IMAGES:`/`THEME:Images` case in the Startup-Sequence), and the
  security.library hooks of pull request 1084 stay inert on this ROM,
  which does not link the library.
- filesystem work that reaches the ROM's afs-handler and partition.library:
  file replacement no longer corrupts the directory, the volume node is
  created before it is published, runs of consecutive blocks are read in
  one request with read-ahead into free cache buffers, and the default
  buffer count is arch-overridable (amiga-m68k keeps 20).

Earlier refreshes picked up these fixes Copperline contributed or depends
on, all in master:

- the cd.device interrupt-source fix of pull request 1070
  (https://github.com/aros-development-team/AROS/pull/1070, merged
  2026-08-26): cd32.c CD32_Interrupt masks CDINTREQ with the driver's
  own enable state (`status = readl(AKIKO_CDINTREQ) & cu->cu_IntEnable`).
  The Akiko
  CDINTREQ read returns the raw request latches (WinUAE and MAME agree,
  and CD32 titles that drive the DRIVE port directly poll latches they
  never enable), and the completion bits stay latched until the matching
  comparator register is rewritten, so an INT2 server that reacts to
  sources it has not armed signals its unit task at unexpected moments
  and desynchronises the command exchange -- a permanent CD wedge once
  the task closes the receive comparator behind a half-delivered
  response.
- the ciab.resource port-direction fix of pull request 1063
  (https://github.com/aros-development-team/AROS/pull/1063, merged
  2026-08-24): cia_init programmed CIA-B DDRA to 0xff, driving all
  eight port A pins as outputs, where only /DTR and /RTS are outputs
  on the machine. PA0-2 are the parallel port's Centronics status
  inputs and PA3-5 the serial port's /DSR, /CTS and /CD, so an AROS
  guest could never read printer status or see a serial control line
  change -- under Copperline's serial bridges, which drive those lines
  (carrier follows the TCP connection), every input read back 1. DDRA
  is now 0xc0, matching Kickstart, and the same guest probe reads the
  same values under this ROM as under Kickstart 3.1.
- the dosboot stale-IORequest fix of pull request 1051
  (https://github.com/aros-development-team/AROS/pull/1051, merged
  2026-08-22): dosboot closed the trackdisk request and deleted it and
  its reply port before calling the init code a boot block hands back
  (dos.library's init, for a DOS disk), assuming that init never
  returns. It does return whenever CliInit cannot mount the boot
  volume -- a disk whose root block cannot be read, for instance -- and
  the strap then closed and deleted the same request again, by which
  time exec had reused the memory as a library jump table, so
  CloseDevice jumped through a JMP opcode and the machine died with an
  illegal address access in the Exec Bootstrap Task. Such a disk now
  falls through to the "Waiting for bootable media" screen like any
  other unbootable node.
- the exec SMP series of pull requests 1046, 1048 and 1049 (merged
  2026-08-21 to 2026-08-22), which reworks the shared exec memory
  allocator, task state, signalling and scheduling arbitration, and
  ETask lifetime handling; the m68k ROM picks these up as common exec
  code.
- the m68k chip-RAM footprint series of pull request 1034
  (https://github.com/aros-development-team/AROS/pull/1034, merged
  2026-08-20): the boot-time chip RAM cost of the ROM drops by well over
  a megabyte on an unexpanded machine. The CD32 boot node asked CDVDFS
  for 32 buffers, which CDVDFS multiplies into 16-sector chunks - a 1 MB
  cache allocated from MEMF_24BITDMA the moment CD0: mounts, which on a
  fast-RAM-less CD32 is chip RAM (and why adding fast RAM used to "fix"
  CD games); it now asks for 4 (128 KiB). CreatePool committed a full
  puddle up front, so the ~45 pools alive after boot pinned 324 KiB that
  was 97% empty; the pool header is now a chunk-sized allocation and the
  first data puddle is sized to the first request. And exec's
  NewCreateTaskA floored every task stack at 16 KiB on m68k - roughly
  200 KiB across the resident tasks against measured per-task peaks in
  the hundreds of bytes; the floor is now 4 KiB there (interrupts run on
  the supervisor stack on m68k, so task stacks carry only user-mode
  frames), the resident system tasks take measured sizes with generous
  headroom, and the boot shell and default CLI command stacks are 8 KiB
  (Kickstart's default is 4000 bytes). Free chip RAM at the boot
  handoff: A1200 1.30 MB -> 1.72 MB, CD32 with a disc in the drive
  164 KiB -> 1.48 MB, and a 1 MB A500 keeps 236 KiB of its slow RAM free
  where before the OS left 64 bytes. Chip-only CD32 games that needed a
  fast-RAM expansion under the AROS ROM now boot without one.
- the CD32 quiet-boot and requester-gate series of pull request 1032
  (https://github.com/aros-development-team/AROS/pull/1032, merged
  2026-08-19): a CD boot runs appliance-quiet like the CD32 Kickstart
  (the boot process keeps pr_WindowPtr at -1 through the whole run and
  the initial CLI gets NIL: instead of a console window, so no "Please
  insert volume" requester can ever block a pad-only machine);
  cd.device no longer aborts the command exchange when the drive
  volunteers a play-status packet, which desynchronised every later
  command by one reply and dumped Pinball Fantasies to the shell when a
  table started its music; CD_PLAYTRACK sends a real M:S:F lead-out end
  position and CD_ATTENUATE handles mute/unmute/query correctly;
  ReadJoyPort's controller probe drives the right port's POTGO bits;
  and lowlevel.library implements the Kickstart-private requester-gate
  vectors at LVO -120/-126 (a nested EasyRequestArgs suppress/restore
  pair, semantics recovered from the real 40.34 ROM) that CD32 titles
  call blind at startup - Pinball Illusions crashed into genmodule's
  poison vector there and hung on its loading screen. With these, both
  Pinball Fantasies and Pinball Illusions cold-boot from CD to playable
  tables on the AROS ROM.
- the low-chip boot-console series of pull request 1022
  (https://github.com/aros-development-team/AROS/pull/1022, merged
  2026-08-19):
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
    ../AROS/configure --target=amiga-m68k    # needs python3-mako and python3-yaml
    make kernel-link-amiga-m68k
    # ROMs land in bin/amiga-m68k/gen/boot/aros-amiga-m68k-{rom,ext}.bin

Refreshing from the official nightly is a simpler alternative:
download `AROS-<date>-amiga-m68k-boot-iso.zip` from
https://sourceforge.net/projects/aros/files/nightly2/, extract the ISO, and
pull `boot/amiga/aros-rom.bin` and `boot/amiga/aros-ext.bin` (renamed to the
WinUAE/FS-UAE convention used here). Both files must be exactly 524288 bytes
(512 KiB). Also refresh `LICENSE` and `ACKNOWLEDGEMENTS` from the same
source tree.

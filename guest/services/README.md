# Guest-side filesys handler

`services_rom.bin` is the m68k code Copperline maps into its services board
(Zorro II, manufacturer 0x1448 "dec0de Consulting", product 5) to provide
host-directory mounts (`HOSTFS0:`, `HOSTFS1:`, ...). It is deliberately tiny:
all filesystem semantics live in the emulator (`src/filesys.rs`); the handler
only mounts the DOS devices and pumps DosPackets to the host through its
unit's doorbell register in the board window (each mount unit has its own
register bank, so handler processes never synchronize with each other).

Two entry points (see `entry.s` and `copperline_board.h`):

- `resident_init()` -- rt_Init of the Romtag the DiagArea's DiagPoint
  patches into the diag copy; Kickstart's cold-start resident scan calls it
  once DOS-list surgery is safe. It hands off to `mount_boards()`, which
  builds one DeviceNode per entry in the mount table the emulator wrote
  into the board window and adds it with `AddBootNode` at the priority from
  the mount's `bootpri` config (default -128: mounted at DOS init, never a
  boot candidate). On Kickstart 1.3 (expansion.library V34) it hand-builds
  the BootNode `AddBootNode` would have built, so `bootpri` works there
  too; the handler likewise falls back from the V36 DosList calls to a
  Forbid-protected splice when dos.library is V34.
- `handler_main()` -- the DOS handler process, started by DOS at mount time
  (`ADNF_STARTPROC`) or by V34's boot path. `WaitPort`/`GetMsg`, ring the
  packet in through the unit's doorbell register (the emulator fills
  `dp_Res1`/`dp_Res2` within the write), reply the packet. The startup
  packet locates the board via dp_Arg3's DeviceNode, or -- on Kickstart
  1.3's boot path, which sends V34 BCPL process parameters with dp_Arg3
  NULL -- via dp_Arg2's FileSysStartupMsg inside the board window.

## Rebuilding

The ROM is a committed artifact, rebuilt by hand when the handler changes:

```sh
make        # dockerized m68k-amigaos-gcc -> handler.exe -> services_rom.bin
```

The Makefile uses the `stefanreinauer/amiga-gcc` container (GCC 16 with the
AmigaOS hunk patches; https://github.com/reinauer/container-amiga-gcc), so no
local cross-toolchain is needed. The ROM must be position-independent: it
runs at whatever base autoconfig assigns, so everything is compiled with
`-mpcrel`, and the Makefile fails the build if the linked executable contains
relocations or data/bss hunks (checked with objdump before objcopy extracts
the flat code hunk).

`copperline_board.h` pins the board-window layout and host registers shared
with `src/filesys.rs`; keep the two in sync (Rust unit tests lock the layout).

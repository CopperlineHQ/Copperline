# Guest-side filesys handler

`services_rom.bin` is the m68k code Copperline maps into its services board
(Zorro II, manufacturer 0x1448 "dec0de Consulting", product 5) to provide
host-directory mounts (`HOSTFS0:`, `HOSTFS1:`, ...). It is deliberately tiny:
all filesystem semantics live in the emulator (`src/filesys.rs`); the handler
only mounts the DOS devices and pumps DosPackets to the host through one
reserved A-line trap per packet.

Two entry points (see `entry.s` and `copperline_board.h`):

- `mount_boards()` -- called at expansion init from the board's DiagArea
  with the documented DiagPoint context. Builds one DeviceNode per entry in
  the mount table the emulator wrote into the board window and adds it with
  `AddBootNode` (priority -128: mounted at DOS init, never a boot candidate).
- `handler_main()` -- the DOS handler process, started by DOS on first
  reference to a mount. `WaitPort`/`GetMsg`, trap to the emulator (which
  fills `dp_Res1`/`dp_Res2`), reply the packet.

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

`copperline_board.h` pins the board-window layout and trap opcodes shared with
`src/filesys.rs`; keep the two in sync (Rust unit tests lock the layout).

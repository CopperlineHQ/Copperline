# copperhf.device boot ROM

The 68k side of `copperhf.device` (see `COPPERHF-DEVICE-PLAN.md`'s M2):
a DiagArea + Romtag that builds an exec device against the register
protocol in `copperhf_board.h`, served read-only from the board window's
`0x0000-0x3FFF` range (`src/copperhf.rs::COPPERHF_ROM` /
`crate::zorro::BoardSpec::copperhf`'s `diag_vec`).

- `entry.s` -- entry table, DiagArea, and Romtag (adapted from
  `guest/services/entry.s` / `guest/hostsocket/entry.s`). Its `da_BootPoint`
  (`_boot_point`) boots dos.library on Kickstart 1.3 (V34) the same way
  `guest/services/entry.s`'s does -- V36+ never calls it, `AddBootNode`
  handles strap integration itself.
- `device.c` -- device construction (MakeLibrary + AddDevice, deferred to
  rt_Init) and the Open/Close/Expunge/BeginIO/AbortIO vectors. `resident_init`
  calls `mounter.c`'s `chf_mount_all` right after `AddDevice()` and strictly
  before `AddIntServer()`/`CHF_IRQ_ENABLE` (see `mounter.c`'s header comment
  for why the ordering matters). M4 adds three guest-side BeginIO
  interceptions that never reach the doorbell: `NSCMD_DEVICEQUERY` (fills a
  `NSDeviceQueryResult` from a ROM-resident supported-command table),
  `TD_ADDCHANGEINT` (queues the request on a per-device pending list under
  Disable()/Enable()), and `TD_REMCHANGEINT` (removes and replies the held
  ADDCHANGEINT, matching by `io_Data`). Every other M4 command (TD64/NSD
  64-bit reads and writes, `HD_SCSICMD`, `TD_CHANGENUM`/`CHANGESTATE`/
  `PROTSTATUS`/`EJECT`) is unchanged from the guest's point of view -- it
  goes to the doorbell exactly like a CMD_READ, and the host (`src/
  copperhf.rs`) answers it.
- `mounter.c` (M3) -- the boot-time partition mounter: walks each present
  unit's RDSK/PART chain (polled I/O, its own doorbell/completion spin, no
  MsgPort involved), hand-builds a `DeviceNode` + `FileSysStartupMsg` +
  `DosEnvec` per partition (honoring `PBFF_NOMOUNT`/`PBFF_BOOTABLE`), and
  adds it via `AddBootNode` (V36+) or the hand-built `eb_MountList`
  `BootNode` / `AddDosNode` fallback (V34, mirroring
  `guest/services/handler.c`'s proven recipe). Separately walks
  `rdb_FileSysHeaderList`: any `FSHD` whose dostype matches a mounted
  partition and isn't already in `FileSystem.resource` has its `LSEG` chain
  loaded through a minimal hunk loader (`HUNK_CODE`/`DATA`/`BSS`/`RELOC32`
  long and short forms/`SYMBOL`/`DEBUG`/`END`) and added as a `FileSysEntry`
  (`FileSystem.resource` is created if absent). One narrow entry point,
  `chf_mount_all(sysbase, board, cd)`, so MIRAGE's ROM can link this object
  file unchanged. Behavioural reference: LIV2/lide.device (GPL-2.0-only,
  read for behaviour only -- see the file's own header comment).
- `int_handler.s` -- the INT2 completion-drain server (must be assembly:
  see its own header comment on the Z-flag contract). M4 adds a second
  interrupt source on the same shared handler: `CHF_IRQ_STATUS` bit 1
  (`CHF_CHANGED_MASK` non-zero) is drained by calling `device.c`'s
  `chf_drain_changes(dev)`, which acks the mask at the real board and
  `Cause()`s every pending `TD_ADDCHANGEINT` interrupt whose unit the mask
  names. `chf_drain_changes` takes `dev` as an ordinary stack argument
  (`int_handler.s` pushes it before `bsr.w` and pops it after) -- an
  earlier version tried a `__asm("a5")` register-bound parameter, matching
  the four exec vectors' own idiom, but that idiom only holds for functions
  GCC never generates a caller for (MakeLibrary's vector table, BeginIO's
  hardware-documented entry contract); for an ordinary internal function
  reached by hand-written `bsr`, this GCC target silently ignores an
  `__asm` binding to a callee-saved register (A5) and falls back to
  stack-passing, so the unpushed `bsr` was reading garbage off the stack --
  see `device.c`'s own header comment on `chf_drain_changes` for the full
  post-mortem (confirmed via objdump and a temporary instrumented build
  that counted 6000+ endlessly-retriggering interrupt entries in 13
  emulated seconds, none of them reaching the real board).
- `device_layout.h` -- the one M4 guest-only (not shared with the host)
  layout constant, `CHF_DEV_BOARDBASE_OFFSET`: the byte offset of
  `dev_BoardBase` within `struct CopperhfDevice` (`device.c`), needed by
  `int_handler.s` because M4's `AddIntServer` `is_Data` is the whole device
  pointer, not just the board pointer (M1-M3). Cross-checked against
  `offsetof()` by a `_Static_assert` in `device.c` at every build.
- `copperhf_board.h` -- the register map, shared with `src/copperhf.rs`.

Because `src/harddrive.rs` guarantees every attached image (RDB or bare)
presents as a valid RDSK within the first 16 sectors, the mounter has
exactly one input shape -- there is no bare-partition path to handle.

## Building

```sh
make
```

Needs Docker (`stefanreinauer/amiga-gcc:gcc-v16.1`, pulled automatically);
see `guest/toolchain.mk`. Installs `assets/copperhf/copperhf_rom.bin`. A
plain `cargo build` never needs Docker -- it embeds the committed artifact
via `include_bytes!`.

The build fails if the linked executable has any relocations or a
`.data`/`.bss` section (the ROM must be pure PC-relative code -- see
`entry.s`'s header comment for the two hard-won traps that discipline
guards against) or if it exceeds the 0x4000-byte budget below the
register block (`CHF_MAGIC` in `copperhf_board.h`).

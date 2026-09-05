# copperhf.device: register protocol and boot ROM

Copperline's virtual hardfile controller exposes seven units through
`copperhf.device` (`[copperhf]`, `src/copperhf.rs`). The guest boot ROM lives
in `guest/copperhf/`. This chapter describes the register protocol, device
vectors, autoboot mounter, and asynchronous I/O. Register constants are
maintained in both the Rust implementation and
`guest/copperhf/copperhf_board.h`; keep them consistent when changing the ABI.

## Zorro identity

- Zorro II slave, one 64 KiB register/ROM window.
- Manufacturer **5192** / `0x1448` (the Copperline manufacturer ID; see
  [](../zorro)), product **8**.
- Autoboot ROM: `er_InitDiagVec` points at a DiagArea
  embedded in the ROM (`guest/copperhf/entry.s`'s `_diag_area`), so the
  board appears in `FindConfigDev` scans and its Romtag is picked up by
  Kickstart's cold-start resident scan like any other autoboot device.

## Window layout

```
0x0000-0x3FFF  boot ROM (read-only), DiagArea inside it
0x4000-0x40FF  register block
```

The ROM occupies `0x0000..0x3FFF` but the actual code starts at window
offset `ROM_OFFSET` (`0x0008`, `src/copperhf.rs::ROM_OFFSET`) -- the first
eight bytes of the window are unused, kept only for consistency with the
services/HostSocket boards' own ROM layout convention. Reads inside
`0x0000..0x3FFF` that land past the end of the committed ROM image (or in
those first eight bytes) return the `0xFFFF` "nothing here" pattern, the
same fallback every other unmapped offset on this board uses; writes
anywhere in the ROM window are silently dropped. `DIAG_OFFSET`
(`src/copperhf.rs`) is `ROM_OFFSET + 0x40`, matching `entry.s`'s
`_diag_area` placement (`.org 0x40`) -- a unit test in `src/copperhf.rs`
locks the byte at that offset to the DiagArea's `da_Config` value so the
Rust constant and the ROM's own layout cannot silently drift apart.

(copperhf-register-map)=
## Register map

See `guest/copperhf/copperhf_board.h` for the full, authoritative table
(offsets, widths, access, and the exact doorbell/completion/ACK protocol
description):

| Register | Offset | Width | Access | Meaning |
|---|---|---|---|---|
| `CHF_MAGIC` | 0x4000 | 32 | RO | `"CPHF"` (0x43504846) |
| `CHF_VERSION` | 0x4004 | 16 | RO | register-protocol version (2) |
| `CHF_UNITS` | 0x4006 | 16 | RO | unit slot count (7) |
| `CHF_UNIT_PRESENT` | 0x4008 | 16 | RO | bit *n* set = unit *n* configured (a slot stays present after its media is ejected/hot-detached -- see `CHF_UNIT_MEDIA`) |
| `CHF_UNIT_RDONLY` | 0x400A | 16 | RO | bit *n* set = unit *n* read-only (always 0; read-only hardfiles are not implemented) |
| `CHF_UNIT_SELECT` | 0x400C | 16 | RW | selects the unit `CHF_CHANGE_COUNT`/`CHF_UNIT_BLOCKS` report on |
| `CHF_CHANGE_COUNT` | 0x400E | 16 | RO | disk-change counter of the selected unit |
| `CHF_UNIT_BLOCKS` | 0x4010 | 32 | RO | total 512-byte blocks of the selected unit |
| `CHF_CHANGED_MASK` | 0x4014 | 16 | RO | bit *n* set = unit *n*'s media changed (eject, hot attach/detach) and the guest has not yet acked it |
| `CHF_CHANGED_ACK` | 0x4016 | 16 | WO | write a mask; clears those `CHF_CHANGED_MASK` bits |
| `CHF_UNIT_MEDIA` | 0x4018 | 16 | RO | bit *n* set = unit *n* currently has media (distinct from `CHF_UNIT_PRESENT`, "slot configured") |
| `CHF_DOORBELL` | 0x4020 | 32 | WO | guest pointer to an IOStdReq; queues work for ordered completion at the next board tick |
| `CHF_COMPLETE_GET` | 0x4028 | 32 | RO | oldest completed request pointer, 0 if empty (idempotent -- does not pop) |
| `CHF_COMPLETE_ACK` | 0x402C | 16 | WO | any write pops the oldest completion |
| `CHF_IRQ_STATUS` | 0x4030 | 16 | RO | bit 0 = completion queue non-empty; bit 1 = `CHF_CHANGED_MASK` non-zero |
| `CHF_IRQ_ENABLE` | 0x4032 | 16 | RW | bit 0 = enable INT2 while `CHF_IRQ_STATUS` is non-zero, any bit (reset: 0) |

`io_Unit` on a request is the raw copperhf unit **number** (0..6), not a
guest `Unit` pointer -- this device has no per-unit `Unit` structures on
the guest side.

A unit's boot-time `[copperhf]` config attach bumps neither
`CHF_CHANGE_COUNT` nor `CHF_CHANGED_MASK`: a unit configured before the
guest ever booted has never changed from the guest's point of view, and
older guest drivers predate the changed-mask protocol and
never acknowledge it -- flagging one at boot would latch `CHF_IRQ_STATUS` bit 1
(and, once the guest enables `CHF_IRQ_ENABLE`, INT2 itself) permanently
set. Only a *runtime* change -- the guest's own `TD_EJECT`, or a hot
attach/detach through the control protocol
(`copperhf.attach`/`copperhf.eject`, [](../debugger/control)) -- bumps
either register.

### Commands

| Command | Value | Semantics |
|---|---|---|
| `CMD_READ` | 2 | read; `IOERR_BADADDRESS` (no wrap) if `io_Offset + io_Length` overflows 32 bits |
| `CMD_WRITE` | 3 | write, same overflow rule |
| `CMD_UPDATE` | 4 | flush |
| `CMD_CLEAR` | 5 | no-op success |
| `TD_MOTOR` | 9 | tracked, no I/O effect; `io_Actual` = previous state |
| `TD_FORMAT` | 11 | treated as `CMD_WRITE` |
| `TD_CHANGENUM` | 13 | `io_Actual` = the unit's change counter |
| `TD_CHANGESTATE` | 14 | `io_Actual` = 0 media present, 1 absent |
| `TD_PROTSTATUS` | 15 | `io_Actual` = 0 writable, 1 read-only |
| `TD_GETGEOMETRY` | 22 | `struct DriveGeometry` at `io_Data`, `io_Actual` = 0 |
| `TD_EJECT` | 23 | `io_Length != 0` ejects (drops media, bumps the change counter, sets `CHF_CHANGED_MASK`); `io_Length == 0` is a no-op "insert" |
| `TD_READ64` | 24 | 64-bit read; `io_Actual` on entry is the upper 32 bits of the byte offset (`io_HighOffset`), `io_Offset` the lower 32; no 4 GiB ceiling |
| `TD_WRITE64` | 25 | 64-bit write, same offset convention |
| `TD_SEEK64` | 26 | no-op success |
| `TD_FORMAT64` | 27 | treated as `TD_WRITE64` |
| `HD_SCSICMD` | 28 | `io_Data` -> `struct SCSICmd`; see below |
| `NSCMD_TD_READ64`/`WRITE64`/`SEEK64`/`FORMAT64` | 0xC000-0xC003 | identical to their `TD_*64` counterparts, only the command number differs (NSD's `newstyle.h`) |

Commands targeting a unit whose `CHF_UNIT_PRESENT` bit is clear (unit
number out of range, or a slot never attached) fail with `IOERR_OPENFAIL`.
Commands targeting a present unit whose `CHF_UNIT_MEDIA` bit is clear
(ejected/hot-detached) fail with `TDERR_DiskChanged` (29) for every I/O and
geometry command; `TD_CHANGENUM`/`TD_CHANGESTATE`/`TD_PROTSTATUS` and
`TD_EJECT` still answer regardless of media state. Any other command sets
`io_Error = IOERR_NOCMD` -- including `NSCMD_DEVICEQUERY`,
`TD_ADDCHANGEINT`, and `TD_REMCHANGEINT`, which are guest-side (answered by
`device.c`'s `BeginIO` directly) and never reach the doorbell at all.

### `HD_SCSICMD`

`io_Data` points at a `struct SCSICmd` (`devices/scsidisk.h`, 30 bytes on
m68k). The board answers the CDB in `scsi_Command` against the unit's own
image with no SCSI bus underneath, reusing `src/scsi.rs::ScsiDisk`'s CDB
machinery (the same target model the A2091/A4091 boards drive over the
WD33C93A): READ/WRITE(6/10/12/16), INQUIRY, READ CAPACITY(10/16), TEST UNIT
READY, MODE SENSE/SELECT(6/10) (stubs), REQUEST SENSE. `scsi_Actual`,
`scsi_CmdActual`, and `scsi_Status` are always filled in; on CHECK
CONDITION, `scsi_SenseData` is filled too when `scsi_Flags` requests
`SCSIF_AUTOSENSE`/`SCSIF_OLDAUTOSENSE`, honouring `scsi_SenseLength`.

## The device stub (`guest/copperhf/`)

The boot ROM contains an Exec device built from:

- `entry.s` -- entry table, DiagArea, and Romtag (`rt_Type = NT_DEVICE`).
  Follows the same PC-relative discipline and DiagPoint/rt_Init deferral
  recipe as `guest/services/entry.s` and `guest/hostsocket/entry.s`
  (real device construction never happens from `da_DiagPoint` itself --
  see that file's header comment for why 1.3's boot corrupts otherwise).
- `device.c` -- device construction (`MakeLibrary` + `AddDevice`, called
  from `rt_Init`) and the `Open`/`Close`/`Expunge`/`ExtFunc`/`BeginIO`/
  `AbortIO` vectors, each an ordinary C function with `__asm("reg")`-bound
  parameters matching exec's documented device-vector register contract
  (verified against `exec.doc`, not assumed).
- `int_handler.s` -- the INT2 completion-drain server, installed on
  `INTB_PORTS` via `AddIntServer`. Hand-written assembly, not C: per
  `AddIntServer`'s own autodoc warning, a plain C function cannot reliably
  control the 68000 Z flag its "was this interrupt mine" contract depends
  on. Reads `CHF_IRQ_STATUS`; if clear, returns with Z set so the shared
  chain (real hardware shares `INTB_PORTS` with CIA-A) passes the
  interrupt on untouched. Otherwise it drains `CHF_COMPLETE_GET` in a
  loop -- `ReplyMsg`-ing each completed IORequest and writing
  `CHF_COMPLETE_ACK` to pop it -- until it reads back 0, then returns
  with Z clear.

`BeginIO` never calls `ReplyMsg` itself: it clears `IOF_QUICK`, writes 0 to
`io_Error`, and rings `CHF_DOORBELL` with the request pointer as a single
32-bit write. The request completes through the INT2 handler after the
host worker has finished and the board applies its result.

(asynchronous-io-m5)=
## Asynchronous I/O

Each request passes through three stages:

1. **Doorbell** (`dispatch_request`, on the emulation thread): validate the
   request against cached unit state and copy any write payload out of guest
   RAM. Submit file I/O to the worker through a bounded channel (64 requests).
   Requests answered from cached state join the same ordered completion queue.
2. **Worker**: one FIFO worker per board performs sector reads, writes, and
   SCSI commands using its backing files. It never accesses guest memory.
3. **Drain** (`tick`, on the emulation thread): wait for each queued result in
   order, copy read data into guest RAM, update `io_Error`/`io_Actual`, enqueue
   the completion pointer, and raise INT2. Media changes are applied in the
   same order.

### The determinism model

A request completes on the first board `tick()` after its doorbell write.
The tick waits for the worker result, so slow host I/O delays the host
without changing the emulated completion time. The bus samples the board's
IRQ after that tick. File-I/O and board-answered requests share one FIFO,
preserving doorbell order.

`TD_EJECT` records the guest's requested action at doorbell time, then
closes the backing file at its position in the worker queue. It cannot
race earlier I/O on that unit.

A full worker channel blocks the emulation thread until space is available.
The worker never calls back into emulation, so it can continue draining
jobs while emulation waits. In the browser build, jobs run inline and their
results wait in the same completion queue until the next tick.

### Quiesce-on-save

`CopperhfBoard::quiesce` drains all in-flight work before a save or media
change. `Emulator::save_state` and `save_state_bytes` call it before
serialization; the `copperhf.attach` and `copperhf.eject` control methods
call it before changing a unit. Snapshots therefore contain an empty work
queue and the resulting guest-visible state, including cached unit sizes.

Runtime media changes use the control protocol. There is no window menu or
drag-and-drop target for copperhf units, though a windowed session displays
an on-screen notice when CCP changes a unit.

`tests/copperhf_m5.rs` checks ordered asynchronous I/O, repeated-boot
save-state and screenshot equality, and resumption from a save made during
boot-volume I/O.

`Open` fails with `IOERR_OPENFAIL` unless the requested unit is below
`CHF_UNITS` and its `CHF_UNIT_PRESENT` bit is set; on success it sets
`io_Unit` to the raw unit number, `io_Device` to the device base, and
`io_Error` to 0. `Close` decrements the open count and never expunges --
this is a ROM-resident device, so `Expunge` unconditionally refuses
(returns 0) regardless of open count. `AbortIO` always reports
`IOERR_NOCMD`; cancellation is not implemented, so queued work continues
to completion.

The stub is V34-clean: no V36+ exec/expansion calls anywhere on this path,
68000-only instructions, word-aligned structures throughout.

(milestone-status)=
## Autoboot and integration coverage

`guest/copperhf/mounter.c` walks RDSK/PART blocks and creates a `DeviceNode`,
`FileSysStartupMsg`, and `DosEnvec` for each partition. It uses `AddBootNode`
on V36+ and the `eb_MountList` fallback on V34. FSHD/LSEG chains load
filesystem code into `FileSystem.resource`, allowing attached units to
mount and autoboot.

The integration matrix includes:

- `tests/copperhf_m6.rs`: bundled AROS and a synthetic LSEG fixture exercise
  filesystem registration, relocation, partition-node construction, and
  autoboot without external assets.
- `tests/copperhf_kickstarts.rs`: ignored tests for Kickstart 1.3, 3.1, and
  3.2, covering RDB and bare-OFS autoboot. Additional cases use a real
  `FastFileSystem` binary for FFS-from-LSEG and a PFS3-DS binary for a
  limited >4 GiB boot smoke test. The latter does not prove formatting or
  mounting the full large partition.

See `tests/README.md` for required assets and commands. A skipped test is
not evidence that its configuration works.

## See also

- `guest/copperhf/copperhf_board.h` -- the authoritative register map.
- `guest/copperhf/README.md` -- building the boot ROM.
- [](../guide/configuration) -- the `[copperhf]` config section.
- [](../zorro) -- the Copperline manufacturer ID and product numbering.

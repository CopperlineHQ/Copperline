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
  embedded in the ROM (`guest/copperhf/entry.s`'s `_diag_area`), so
  Kickstart's cold-start resident scan picks up its Romtag like any other
  autoboot device's.

## Window layout

```
0x0000-0x3FFF  boot ROM (read-only), DiagArea inside it
0x4000-0x40FF  register block
```

The ROM occupies `0x0000..0x3FFF`, but the code starts at window offset
`ROM_OFFSET` (`0x0008`, `src/copperhf.rs::ROM_OFFSET`). The first eight
bytes of the window are unused, following the services and HostSocket
boards' ROM layout convention. Reads inside `0x0000..0x3FFF` that land
past the end of the committed ROM image (or in those first eight bytes)
return the `0xFFFF` "nothing here" pattern, the same fallback every other
unmapped offset on this board uses; writes anywhere in the ROM window are
silently dropped. `DIAG_OFFSET` (`src/copperhf.rs`) is `ROM_OFFSET +
0x40`, matching `entry.s`'s `_diag_area` placement (`.org 0x40`). A unit
test in `src/copperhf.rs` locks the byte at that offset to the DiagArea's
`da_Config` value, so the Rust constant and the ROM's layout cannot drift
apart unnoticed.

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
| `CHF_UNIT_RDONLY` | 0x400A | 16 | RO | bit *n* set = unit *n*'s image refuses writes (a CHD with no write overlay, a read-only session copy, a physical disk attached read-only); 0 for an ordinary image file |
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
`CHF_CHANGE_COUNT` nor `CHF_CHANGED_MASK`. A unit configured before the
guest ever booted has never changed from the guest's point of view, and
older guest drivers predate the changed-mask protocol and never
acknowledge it: flagging a change at boot would latch `CHF_IRQ_STATUS`
bit 1 (and, once the guest enables `CHF_IRQ_ENABLE`, INT2 itself)
permanently. Only a *runtime* change -- the guest's own `TD_EJECT`, or a
hot attach/detach through the control protocol
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
| `TD_GETGEOMETRY` | 22 | `struct DriveGeometry` at `io_Data` (`io_Length` >= 32), `io_Actual` = 0 |
| `TD_EJECT` | 23 | `io_Length != 0` ejects (drops media, bumps the change counter, sets `CHF_CHANGED_MASK`); `io_Length == 0` is a no-op "insert" |
| `TD_READ64` | 24 | 64-bit read; `io_Actual` on entry is the upper 32 bits of the byte offset (`io_HighOffset`), `io_Offset` the lower 32; no 4 GiB ceiling |
| `TD_WRITE64` | 25 | 64-bit write, same offset convention |
| `TD_SEEK64` | 26 | no-op success |
| `TD_FORMAT64` | 27 | treated as `TD_WRITE64` |
| `HD_SCSICMD` | 28 | `io_Data` -> `struct SCSICmd`; see below |
| `NSCMD_TD_READ64`/`WRITE64`/`SEEK64`/`FORMAT64` | 0xC000-0xC003 | identical to their `TD_*64` counterparts, only the command number differs (NSD's `newstyle.h`) |

Reads and writes must transfer a non-zero whole number of 512-byte
sectors from a sector-aligned offset and stay within the unit; otherwise
they fail with `IOERR_BADLENGTH`. Writes to a unit whose `CHF_UNIT_RDONLY` bit is set
fail with `TDERR_WriteProt` (28).

Commands targeting a unit whose `CHF_UNIT_PRESENT` bit is clear (unit
number out of range, or a slot never attached) fail with `IOERR_OPENFAIL`.
Commands targeting a present unit whose `CHF_UNIT_MEDIA` bit is clear
(ejected/hot-detached) fail with `TDERR_DiskChanged` (29) for every I/O and
geometry command; `TD_CHANGENUM`/`TD_CHANGESTATE`/`TD_PROTSTATUS` and
`TD_EJECT` still answer regardless of media state. The board answers any
other command with `io_Error = IOERR_NOCMD`. `NSCMD_DEVICEQUERY`,
`TD_ADDCHANGEINT`, and `TD_REMCHANGEINT` are answered guest-side by
`device.c`'s `BeginIO` and never reach the doorbell.

### `HD_SCSICMD`

`io_Data` points at a `struct SCSICmd` (`devices/scsidisk.h`, 30 bytes on
m68k). The board answers the CDB in `scsi_Command` against the unit's own
image with no SCSI bus underneath, reusing `src/scsi.rs::ScsiDisk`'s CDB
machinery (the same target model the A2091/A4091 boards drive over the
WD33C93A): TEST UNIT READY, REQUEST SENSE, INQUIRY (including the EVPD
supported-pages page), READ/WRITE(6/10/12/16), VERIFY(10), READ
CAPACITY(10/16), MODE SENSE(6/10) (geometry pages 3 and 4, with the
write-protect bit set for a read-only unit), MODE SELECT(6/10) (parameter
list accepted and ignored), SYNCHRONIZE CACHE(10), READ DEFECT DATA(10)
(no defects), and no-op FORMAT UNIT, SEEK, RESERVE/RELEASE, and START STOP
UNIT. `scsi_Actual`, `scsi_CmdActual`, and `scsi_Status` are always filled
in; on CHECK CONDITION, `scsi_SenseData` is filled too when `scsi_Flags`
requests `SCSIF_AUTOSENSE`/`SCSIF_OLDAUTOSENSE`, honouring
`scsi_SenseLength`. A non-GOOD `scsi_Status` also sets `io_Error` to
`HFERR_BadStatus` (45), and a `scsi_Length` above 16 MiB fails with
`IOERR_BADLENGTH`.

## The device stub (`guest/copperhf/`)

The boot ROM contains an Exec device built from:

- `entry.s` -- entry table, DiagArea, and Romtag (`rt_Type = NT_DEVICE`).
  Follows the same PC-relative discipline and DiagPoint/rt_Init deferral
  recipe as `guest/services/entry.s` and `guest/hostsocket/entry.s`. Real
  device construction never happens from `da_DiagPoint` itself; the
  header comment in `entry.s` explains why doing so corrupts a Kickstart
  1.3 boot.
- `device.c` -- device construction (`MakeLibrary` + `AddDevice`, called
  from `rt_Init`) and the `Open`/`Close`/`Expunge`/`ExtFunc`/`BeginIO`/
  `AbortIO` vectors, each an ordinary C function with `__asm("reg")`-bound
  parameters matching exec's documented device-vector register contract
  (verified against `exec.doc`, not assumed).
- `mounter.c` -- the autoboot mounter (see below), run from `rt_Init`
  after `AddDevice` and before the INT2 server is installed.
- `int_handler.s` -- the INT2 server, installed on `INTB_PORTS` via
  `AddIntServer`. It is hand-written assembly, not C: per `AddIntServer`'s
  own autodoc warning, a plain C function cannot reliably control the
  68000 Z flag that the "was this interrupt mine" contract depends on. It
  reads `CHF_IRQ_STATUS`; if that is clear, it returns with Z set so the
  shared chain (real hardware shares `INTB_PORTS` with CIA-A) passes the
  interrupt on untouched. Otherwise it drains `CHF_COMPLETE_GET` in a
  loop -- running `chf_post_dma` on, then `ReplyMsg`-ing, each completed
  IORequest and writing `CHF_COMPLETE_ACK` to pop it -- until it reads
  back 0. If status bit 1 is set, it then calls `device.c`'s
  `chf_drain_changes`, which acknowledges `CHF_CHANGED_MASK` and
  `Cause()`s every pending `TD_ADDCHANGEINT` interrupt for a changed
  unit. It returns with Z clear.

`BeginIO` never calls `ReplyMsg` itself: it clears `IOF_QUICK`, writes 0 to
`io_Error`, and rings `CHF_DOORBELL` with the request pointer as a single
32-bit write. The request completes through the INT2 handler after the
host worker has finished and the board applies its result.

`Open` fails with `IOERR_OPENFAIL` unless the requested unit is below
`CHF_UNITS` and its `CHF_UNIT_PRESENT` bit is set; on success it sets
`io_Unit` to the raw unit number, `io_Device` to the device base, and
`io_Error` to 0. `Close` decrements the open count and never expunges:
this is a ROM-resident device, so `Expunge` unconditionally refuses
(returns 0) regardless of open count. `AbortIO` always reports
`IOERR_NOCMD`; cancellation is not implemented, so queued work continues
to completion.

The stub runs on Kickstart 1.3 (V34): it is built for the 68000, keeps its
structures word-aligned, and guards its V36+/V37+ calls (`AddBootNode`,
`CachePreDMA`/`CachePostDMA`) with version checks.

The board is a busmaster, so the driver performs exec's DMA cache
maintenance (V37+; skipped on Kickstart 1.3, which has neither the vectors
nor the cached CPUs): `BeginIO` calls `CachePreDMA` over the request block
and its data buffers before ringing the doorbell, and the INT2 drain calls
`CachePostDMA` (device.c's `chf_post_dma`) on each completed request before
`ReplyMsg`, invalidating the host-written `io_Actual`/`io_Error` fields and
any read payload -- the request block first, so the fields the buffer pass
reads are themselves fresh. The mounter's polled pre-interrupt path
(`chf_do_io`) brackets its own I/O the same way. Without this, a copyback
data cache (a real 68040, or the emulator's `[cpu] dcache` model) serves
stale lines over the DMA'd bytes. The emulator also drops its modelled
data cache when any device tick writes guest memory (see the
[CPU chapter's Caches section](cpu.md#caches)), so either half alone
covers emulated runs; the driver half also keeps the device correct on
real cached CPUs.

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
change. `Emulator::save_state`, `save_state_bytes`, and
`machine_state_bytes` call it before serialization; the `copperhf.attach`
and `copperhf.eject` control methods
call it before changing a unit. Snapshots therefore contain an empty work
queue and the resulting guest-visible state, including cached unit sizes.

Runtime media changes use the control protocol. There is no window menu or
drag-and-drop target for copperhf units, though a windowed session displays
an on-screen notice when CCP changes a unit.

`tests/copperhf_m5.rs` checks ordered asynchronous I/O, repeated-boot
save-state and screenshot equality, and resumption from a save made during
boot-volume I/O.

(milestone-status)=
## Autoboot and integration coverage

`guest/copperhf/mounter.c` walks RDSK/PART blocks and creates a `DeviceNode`,
`FileSysStartupMsg`, and `DosEnvec` for each partition. It uses `AddBootNode`
on V36+ and the `eb_MountList` fallback on V34. FSHD/LSEG chains load
filesystem code into `FileSystem.resource`, allowing attached units to
mount and autoboot. A bare (RDB-less) hardfile reaches the guest wrapped
in an RDB that the host synthesizes (`HardDriveImage::open` in
`src/harddrive.rs`), so the mounter has no separate bare-partition path.

Its I/O is polled (`chf_do_io` spins on `CHF_COMPLETE_GET`/`ACK` itself),
which is why it runs before `int_handler.s` is installed: its request
blocks have no reply port for the INT2 server to `ReplyMsg` to.

The integration matrix includes:

- `tests/copperhf_device.rs`, `tests/copperhf_m4_guest.rs`,
  `tests/copperhf_mounter.rs`, and `tests/copperhf_machine.rs`: asset-free
  checks over the bundled AROS ROM, covering an `OpenDevice` read/write
  round trip, the extended command set (NSD query, TD64, `HD_SCSICMD`,
  change interrupts and `TD_EJECT`) driven by a guest probe, RDB and
  RDB-less autoboot, and `[copperhf]` machine wiring. The AROS-boot
  suites skip themselves in debug builds.
- `tests/copperhf_m6.rs`: bundled AROS and a synthetic LSEG fixture exercise
  filesystem registration, relocation, and partition-node construction
  without external assets.
- `tests/copperhf_kickstarts.rs`: ignored tests for Kickstart 1.3, 3.1, and
  3.2, covering RDB and RDB-less OFS autoboot. Additional cases use a real
  `FastFileSystem` binary for FFS-from-LSEG and a PFS3-DS binary for a
  limited >4 GiB boot smoke test. The latter does not prove formatting or
  mounting the full large partition.

See `tests/README.md` for required assets and commands. A skipped test is
not evidence that its configuration works.

## See also

- `guest/copperhf/copperhf_board.h` -- the authoritative register map.
- `guest/copperhf/README.md` -- building the boot ROM.
- `COPPERHF-DEVICE-PLAN.md` (repository root) -- the M1-M6 milestone plan
  that the source comments and test file names refer to.
- [](../guide/configuration) -- the `[copperhf]` config section.
- [](../zorro) -- the Copperline manufacturer ID and product numbering.

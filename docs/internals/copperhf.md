# copperhf.device: register protocol and boot ROM

This page documents Copperline's virtual hardfile controller,
`copperhf.device` (`[copperhf]` in the config, `src/copperhf.rs`), and its
guest-side boot ROM (`guest/copperhf/`). The authoritative register map is
`guest/copperhf/copperhf_board.h`, shared verbatim (by hand-kept convention,
like every other virtual board in this project) between the Rust host side
and the 68k guest side; this page summarizes it and adds the milestone
status and the device stub's behaviour, which the header does not cover.
Where this page and the header disagree, the header wins for register
semantics -- fix whichever drifted.

See `COPPERHF-DEVICE-PLAN.md` in the repository root for the full milestone
plan this board is being built against.

## Zorro identity

- Zorro II slave, one 64 KiB register/ROM window.
- Manufacturer **5192** / `0x1448` (the Copperline manufacturer ID; see
  [](../zorro)), product **8**.
- Autoboot ROM present as of M2: `er_InitDiagVec` points at a DiagArea
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

## Register map

See `guest/copperhf/copperhf_board.h` for the full, authoritative table
(offsets, widths, access, and the exact doorbell/completion/ACK protocol
description). In summary:

| Register | Offset | Width | Access | Meaning |
|---|---|---|---|---|
| `CHF_MAGIC` | 0x4000 | 32 | RO | `"CPHF"` (0x43504846) |
| `CHF_VERSION` | 0x4004 | 16 | RO | register-protocol version (2 as of M4) |
| `CHF_UNITS` | 0x4006 | 16 | RO | unit slot count (7) |
| `CHF_UNIT_PRESENT` | 0x4008 | 16 | RO | bit *n* set = unit *n* configured (a slot stays present after its media is ejected/hot-detached -- see `CHF_UNIT_MEDIA`) |
| `CHF_UNIT_RDONLY` | 0x400A | 16 | RO | bit *n* set = unit *n* read-only (always 0 through M4 -- see the header's own comment) |
| `CHF_UNIT_SELECT` | 0x400C | 16 | RW | selects the unit `CHF_CHANGE_COUNT`/`CHF_UNIT_BLOCKS` report on |
| `CHF_CHANGE_COUNT` | 0x400E | 16 | RO | disk-change counter of the selected unit |
| `CHF_UNIT_BLOCKS` | 0x4010 | 32 | RO | total 512-byte blocks of the selected unit |
| `CHF_CHANGED_MASK` | 0x4014 | 16 | RO | M4: bit *n* set = unit *n*'s media changed (eject, hot attach/detach) and the guest has not yet acked it |
| `CHF_CHANGED_ACK` | 0x4016 | 16 | WO | M4: write a mask; clears those `CHF_CHANGED_MASK` bits |
| `CHF_UNIT_MEDIA` | 0x4018 | 16 | RO | M4: bit *n* set = unit *n* currently has media (distinct from `CHF_UNIT_PRESENT`, "slot configured") |
| `CHF_DOORBELL` | 0x4020 | 32 | WO | guest pointer to an IOStdReq; enqueues it (executed synchronously through M4, asynchronously on a worker thread as of M5 -- see "Asynchronous I/O (M5)" below; the guest-visible protocol is unchanged) |
| `CHF_COMPLETE_GET` | 0x4028 | 32 | RO | oldest completed request pointer, 0 if empty (idempotent -- does not pop) |
| `CHF_COMPLETE_ACK` | 0x402C | 16 | WO | any write pops the oldest completion |
| `CHF_IRQ_STATUS` | 0x4030 | 16 | RO | bit 0 = completion queue non-empty; bit 1 (M4) = `CHF_CHANGED_MASK` non-zero |
| `CHF_IRQ_ENABLE` | 0x4032 | 16 | RW | bit 0 = enable INT2 while `CHF_IRQ_STATUS` is non-zero, any bit (reset: 0) |

`io_Unit` on a request is the raw copperhf unit **number** (0..6), not a
guest `Unit` pointer -- this device has no per-unit `Unit` structures on
the guest side.

A unit's boot-time `[copperhf]` config attach bumps neither
`CHF_CHANGE_COUNT` nor `CHF_CHANGED_MASK`: a unit configured before the
guest ever booted has never changed from the guest's point of view, and
every M1-M3 guest build predates the changed-mask protocol entirely and
never acks it -- flagging one at boot would latch `CHF_IRQ_STATUS` bit 1
(and, once the guest enables `CHF_IRQ_ENABLE`, INT2 itself) permanently
set. Only a *runtime* change -- the guest's own `TD_EJECT`, or a hot
attach/detach through the control protocol
(`copperhf.attach`/`copperhf.eject`, [](../debugger/control)) -- bumps
either register.

### Commands

| Command | Value | M4 semantics |
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

M2 adds a boot ROM containing a working `copperhf.device` exec-device
stub, built from:

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
32-bit write. Through M4 the host-side protocol executed the request
synchronously (before `CHF_DOORBELL`'s write instruction even retired), but
the guest was never told that -- it always waited for the INT2 completion
drain, exactly as it would against genuinely asynchronous hardware. M5 (see
below) makes that host side actually asynchronous, on a worker thread; the
guest-visible protocol -- this section included -- is unchanged.

## Asynchronous I/O (M5)

As of M5, `CHF_DOORBELL` no longer executes a request inline. The board
splits each request into a doorbell-time half and a drain-time half:

- **Doorbell time** (`CopperhfBoard::dispatch_request`, on the emulation
  thread): read the `IOStdReq` header back out of guest memory, validate
  the unit/range against board-cached state (`present`/`media`/
  `total_sectors`, kept in sync with the worker's own unit table by the
  quiesce rule below), copy `CMD_WRITE`/`TD_WRITE64`-style write payloads
  *out* of guest memory into an owned buffer, and hand the job to the
  worker over a bounded channel (`WORKER_QUEUE_CAPACITY`, 64 requests). A
  request with no file I/O and no unit-table dependency (`TD_MOTOR`,
  `TD_GETGEOMETRY`, `TD_CHANGENUM`/`CHANGESTATE`/`PROTSTATUS`, `TD_SEEK64`,
  `CMD_CLEAR`, anything unrecognized) still takes a slot in the same
  in-order queue, answered later from board-cached state rather than the
  worker. Nothing at doorbell time ever writes a guest-visible result or
  pushes a completion.
- **Worker thread**: owns the real backing files (`units:
  Arc<Mutex<[Option<ScsiDisk>; NUM_UNITS]>>`) and does the actual sector
  read/write/SCSI-CDB work off the emulation thread, one single-threaded
  FIFO worker per board, replying on its own result channel in the same
  order jobs were sent.
- **Drain time** (`tick`, on the emulation thread, called every scheduler
  tick): pop the oldest in-flight request and block on the worker's result
  channel until *that* request's result has arrived, then apply every
  guest-visible effect -- `io_Error`/`io_Actual`, read data copied back
  into guest memory, the pointer pushed onto `CHF_COMPLETE_GET`'s queue,
  `TD_EJECT`'s media-mask/change-counter bookkeeping, and INT2 -- and
  repeat until nothing already queued as of this tick remains in flight.

### The determinism model

The core invariant (spelled out in full in `src/copperhf.rs`'s own module
doc, which this section summarizes): every guest-visible effect of a
request lands at an emulated time that is a pure function of emulated
time, never of how fast the worker thread's file I/O happened to run on
this particular invocation. This holds because a request's due time is, by
construction, "the first `tick()` call after its own doorbell write" --
`tick` always blocks on the worker until every request already in flight
*when tick runs* has actually delivered its result, so a slow disk costs
host wall-clock time inside that block, not a shift in which emulated tick
the completion becomes visible at. `CopperhfBoard::next_event_cck` reports
an in-flight request as due immediately (offset 0) rather than letting the
sparse-wake scheduler skip ahead, which is what makes that blocking-tick
guarantee actually reachable every time. All requests -- I/O and
board-answered alike -- ride one FIFO pipeline end to end (one
doorbell-ordered queue on the board, matched positionally against the
worker's own strictly-ordered output), so completions surface in doorbell
order exactly as M1-M4. `TD_EJECT` is the one command whose *decision*
(`io_Length != 0`) is made at doorbell time, since that only reads a
guest-supplied field that cannot change out from under it, but whose
*execution* (dropping the worker's file handle) is deferred to the
worker's own queue position, so it can never race file I/O already queued
against the same unit.

The doorbell-to-worker channel is bounded (backpressure, not an unbounded
queue): if the guest floods doorbells faster than the worker drains them,
the doorbell write itself blocks the emulation thread briefly. This stays
deterministic-safe because the worker thread never calls back into the
emulation thread and never waits on anything but its own job queue and its
own file I/O -- it cannot need the emulation thread to make progress, so
the emulation thread blocking on it cannot deadlock.

The browser build has no threads (`std::thread::spawn` fails at runtime on
`wasm32-unknown-unknown`), so there the "worker" runs each job inline at
dispatch time and buffers its result for the drain -- same FIFO pipeline,
same call sites, zero concurrency, determinism trivially intact.

### Quiesce-on-save

The worker thread owns the real file handles, so a save state (or a
runtime unit mutation) must never race the worker's own view of the unit
table. `CopperhfBoard::quiesce` blocks until every in-flight request has
surfaced -- the same drain loop `tick` runs, just repeated until the
pipeline is empty -- and is a no-op when nothing is in flight.
`Bus::copperhf_quiesce` calls it on whichever `[copperhf]` board is
configured; `Emulator::save_state`/`save_state_bytes` call that
unconditionally before serializing (both methods take `&mut self` as of
M5, to allow it), and the CCP `copperhf.attach`/`copperhf.eject` handlers
(`src/control/exec.rs`) call it before touching a unit, matching
`attach_unit`/`hot_attach_unit`/`eject_unit`'s own precondition that the
pipeline is already empty. A save state is therefore always captured with
an empty copperhf pipeline, so resuming it reproduces an uninterrupted
run's history byte-for-byte -- the same save-state contract every other
board on this project holds to (`AGENTS.md`'s "Save states"), just with an
explicit drain step to get there instead of nothing-in-flight-by
construction. The state format's version 71 (`src/savestate.rs`) reflects
this: `CopperhfBoard` gained a cached per-unit `total_sectors` field
alongside its existing per-unit state, always serialized quiesced.

`tests/copperhf_m5.rs` pins both properties end to end against a real AROS
boot that drives dense doorbell traffic (the M3 RDSK/PART mounter walk
plus the OFS Startup-Sequence's bootmark write): repeated boots of the
identical scenario must produce byte-identical mid-boot save states and
final screenshots, and a state saved mid-boot (deliberately inside the
busy mounter/Startup-Sequence I/O window) must resume byte-identical to an
uninterrupted run.

`Open` fails with `IOERR_OPENFAIL` unless the requested unit is below
`CHF_UNITS` and its `CHF_UNIT_PRESENT` bit is set; on success it sets
`io_Unit` to the raw unit number, `io_Device` to the device base, and
`io_Error` to 0. `Close` decrements the open count and never expunges --
this is a ROM-resident device, so `Expunge` unconditionally refuses
(returns 0) regardless of open count. `AbortIO` always reports
`IOERR_NOCMD`: by the time a client could call it, the synchronously
executed request has either already completed or is a doorbell-write away
from doing so, so there is never anything left to actually cancel.

The stub is V34-clean: no V36+ exec/expansion calls anywhere on this path,
68000-only instructions, word-aligned structures throughout.

## Milestone status

- **M1** (committed): board, register protocol, `[copperhf]` config,
  synchronous I/O. No boot ROM.
- **M2** (committed): boot ROM with a working device stub, as described
  above. A program that already knows the device's name can
  `OpenDevice("copperhf.device", unit, ...)` and drive it.
- **M3** (this page): the boot ROM's mounter (`guest/copperhf/mounter.c`):
  a polled-I/O RDSK/PART walk building a `DeviceNode` +
  `FileSysStartupMsg` + `DosEnvec` per partition, added via `AddBootNode`
  (V36+) or the hand-built `eb_MountList` fallback (V34), plus FSHD/LSEG
  loading into `FileSystem.resource` -- so attached units autoboot and
  mount like `[ide]`/`[scsi]`/`[lide]` units do.
- **M4** (this page): TD64/NSD/`HD_SCSICMD` command coverage and disk-change
  handling, as described above. Hot attach/detach is also exposed over the
  control protocol -- `copperhf.attach`/`copperhf.eject`
  ([](../debugger/control)) -- for scripts and agents to swap a unit's media
  at runtime the same way `TD_EJECT` does from the guest side.
- **M5** (this page): asynchronous I/O on a worker thread, as described in
  "Asynchronous I/O (M5)" above -- deterministic completion timing, a
  bounded doorbell-to-worker channel, and quiesce-on-save/quiesce-on-hot-
  mutate. The guest-visible protocol is unchanged from M4 (see the
  `BeginIO`/INT2 note above).
- **M6** (this page): the integration matrix, split across two test files
  (`tests/README.md` has the full asset/gate rundown):
  - `tests/copperhf_m6.rs` -- default-CI, bundled AROS, no local assets: a
    hand-built RDB whose FSHD/LSEG chain carries a self-checking synthetic
    fixture binary (`guest/copperhf-test/lsegfix`) proves
    `guest/copperhf/mounter.c`'s FSHD/LSEG loader end to end (FileSystem.
    resource entry, seglist walk, a called-and-verified relocation
    self-test, and the mounted DeviceNode's patched fields) without
    needing any licensed filesystem binary. This is the milestone's
    stand-in for the FFS-from-LSEG axis in default CI. Building it did
    exactly the job it exists to do: its first runs caught three real,
    previously-unexercised `mounter.c` bugs (forward-referencing
    relocations always failed because hunks were allocated lazily; an
    even-length partition name left the FileSysStartupMsg odd-aligned and
    address-faulted the 68000; `ADNF_STARTPROC`/`ConfigDev` were passed
    for non-bootable partitions against the autodoc's documented
    behaviour), all fixed at the site in `mounter.c`.
  - `tests/copperhf_kickstarts.rs` -- `#[ignore]`d, real Kickstart 1.3/
    3.1/3.2 ROMs: {RDB, RDB-less} x plain-OFS autoboot for each ROM (3.1/
    3.2 verified structurally via the same bootmark trick as `tests/
    copperhf_mounter.rs`; 1.3 has no ROM-resident `Echo`, so it is
    verified by golden screenshot of the resulting CLI prompt instead --
    genuinely exercising the V34 `AddDosNode`/`eb_MountList` fallback
    path for the first time, which had never run before this milestone
    since no earlier copperhf test booted Kickstart 1.3 from a copperhf
    unit at all); an FFS-from-LSEG axis against Kickstart 3.1 using a real
    `FastFileSystem` binary tagged DOS\3 (FFS+INTL is not ROM-resident on
    3.1, unlike DOS\0/DOS\1, so this is the one dostype that actually
    forces the LSEG path to run against real filesystem code); and a
    deliberately narrow-scoped PFS3-DS axis that only proves a >4 GiB
    LSEG-attached unit doesn't crash or hang the boot, not a full
    format-and-mount (see that test's own header comment and `tests/
    README.md` for the reasoning and the manual smoke-test alternative).
    This machine has only `KICK31.ROM` locally, so the 3.1 OFS axes are
    verified passing here; every other axis in this file is reviewed but
    **locally unverified** -- only its skip-cleanly path has been run.

## See also

- `guest/copperhf/copperhf_board.h` -- the authoritative register map.
- `guest/copperhf/README.md` -- building the boot ROM.
- [](../guide/configuration) -- the `[copperhf]` config section.
- [](../zorro) -- the Copperline manufacturer ID and product numbering.

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
| `CHF_VERSION` | 0x4004 | 16 | RO | register-protocol version (1) |
| `CHF_UNITS` | 0x4006 | 16 | RO | unit slot count (7) |
| `CHF_UNIT_PRESENT` | 0x4008 | 16 | RO | bit *n* set = unit *n* attached |
| `CHF_UNIT_RDONLY` | 0x400A | 16 | RO | bit *n* set = unit *n* read-only (always 0 in M1/M2 -- see the header's own comment) |
| `CHF_UNIT_SELECT` | 0x400C | 16 | RW | selects the unit `CHF_CHANGE_COUNT`/`CHF_UNIT_BLOCKS` report on |
| `CHF_CHANGE_COUNT` | 0x400E | 16 | RO | disk-change counter of the selected unit |
| `CHF_UNIT_BLOCKS` | 0x4010 | 32 | RO | total 512-byte blocks of the selected unit |
| `CHF_DOORBELL` | 0x4020 | 32 | WO | guest pointer to an IOStdReq; enqueues and (through M4) executes it synchronously |
| `CHF_COMPLETE_GET` | 0x4028 | 32 | RO | oldest completed request pointer, 0 if empty (idempotent -- does not pop) |
| `CHF_COMPLETE_ACK` | 0x402C | 16 | WO | any write pops the oldest completion |
| `CHF_IRQ_STATUS` | 0x4030 | 16 | RO | bit 0 = completion queue non-empty |
| `CHF_IRQ_ENABLE` | 0x4032 | 16 | RW | bit 0 = enable INT2 while `CHF_IRQ_STATUS` bit 0 is set (reset: 0) |

`io_Unit` on a request is the raw copperhf unit **number** (0..6), not a
guest `Unit` pointer -- this device has no per-unit `Unit` structures on
the guest side. Commands implemented through M2 (unchanged from M1):
`CMD_READ`, `CMD_WRITE`, `CMD_UPDATE`, `CMD_CLEAR`, `TD_MOTOR`,
`TD_FORMAT` (treated as `CMD_WRITE`), `TD_GETGEOMETRY`. Any other command
sets `io_Error = IOERR_NOCMD`.

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
32-bit write. The M1/M2 host-side protocol still executes the request
synchronously (before `CHF_DOORBELL`'s write instruction even retires), but
the guest is never told that -- it always waits for the INT2 completion
drain, exactly as it would against genuinely asynchronous hardware. This is
deliberate: it keeps the guest-visible contract stable across the M5
milestone that makes the host side actually asynchronous.

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
- **M4** (planned): TD64/NSD/`HD_SCSICMD` command coverage, disk-change
  handling.
- **M5** (planned): asynchronous I/O on a worker thread; the guest-visible
  protocol above does not change under it (see the `BeginIO`/INT2 note).

## See also

- `guest/copperhf/copperhf_board.h` -- the authoritative register map.
- `guest/copperhf/README.md` -- building the boot ROM.
- [](../guide/configuration) -- the `[copperhf]` config section.
- [](../zorro) -- the Copperline manufacturer ID and product numbering.

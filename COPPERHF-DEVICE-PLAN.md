# copperhf.device — implementation plan

Companion to the design note at
`~/src/project-ideas/active/copperhf-device-design-note.md`. That note is the
*what*; this file maps it onto Copperline's actual source and sequences the
work. Branch: `copperhf-device`.

## Guiding decision: in-tree Rust device, not a WASM plugin

The design note frames copperhf as "a plugin", but its two prerequisites
(§7 — guest memory access, INT2 raising) already exist for **in-tree**
`ZorroDevice` implementations, and the WASM plugin ABI has no file-I/O
imports and no off-thread story. So copperhf is built like `filesys.rs` /
`a2091.rs`: a new `BoardDevice` variant implementing
`src/zorro_device.rs::ZorroDevice`. The MIRAGE-relevant capabilities are
exercised at that layer, which is where MIRAGE will live too.

Consequences of the existing infrastructure (both are fine for us):

- **Guest memory** is only reachable inside device calls on the emulation
  thread (`DeviceHost::dma_read/dma_write`, bounds-checked, unmapped →
  fail with `IOERR_BADADDRESS`). So the host side buffers: on doorbell,
  read the IORequest fields and (for writes) copy the guest buffer out;
  on completion drain (inside `tick`), copy read data into the guest
  buffer and write back `io_Actual`/`io_Error`.
- **Interrupts** are level-sensitive lines polled by
  `Bus::tick_timed_devices` (`src/bus.rs:6042`). There is no cross-thread
  INTREQ poke and none is needed: a worker thread pushes completions onto
  an `mpsc` queue; `tick()` drains it and `int2_line()` goes high while
  completions are pending — the exact pattern the bridge NIC backend uses
  (`src/net/bridge/mod.rs`).

## What is reused as-is

| Need | Existing code |
|---|---|
| Image open, size checks, gzip/dir backing, host disks | `src/harddrive.rs::HardDriveImage::open` |
| RDB detection | `harddrive.rs` RDSK scan over first 16 sectors |
| Virtual RDB for RDB-less images | `harddrive.rs` synthesis: 16×32 geometry, dostype from block 0, one virtual cylinder, `build_rdsk_block`/`build_part_block`, write-protected `rdb_overlay` |
| Config descriptor | `config::DriveImage` + the `RawDrive` parser (path / name / bootpri / filesystem), shared today by `[ide]`, `[scsi]`, `[lide]` |
| Autoconfig board + DiagArea | `src/zorro.rs::BoardSpec` with `diag_vec` (A4091/lide/services are worked examples) |
| Boot ROM build | `guest/` convention: `entry.s` DiagArea + C, `toolchain.mk` (dockerized amiga-gcc), reloc/bss check, `objcopy -O binary` → committed `assets/`, `include_bytes!` |
| Async completion pattern | worker thread + `mpsc`, drained in `tick()` (bridge NIC, NAT TCP) |
| Attach/detach orchestration | `src/emulator.rs::build_machine` per-controller attach blocks |
| Headless test harness | `tests/probe_golden.rs`, AROS ROM boots with no licensed assets |

Per §9 of the design note, **the current shared layer is the spec** for
RDB-less behaviour: no FSHD/LSEG synthesis, no convert-to-RDB. Those are
deferred shared-layer follow-ups (see "Deferred"), so copperhf launches
byte-identical to IDE/SCSI on the same image.

## Register block (Zorro II, 64 KiB window)

Manufacturer 5030 (Copperline's id, see `zorro.rs::copperline_id`), new
product code. Layout (window-relative, word-wide like the services board):

```
0x0000  ROM (DiagArea at diag_vec offset), read-only
DOORBELL     (u32, write) guest pointer to struct IORequest → enqueue
COMPLETE_GET (u32, read)  next completed IORequest pointer, 0 = empty
                          (reading pops; ack is implicit)
UNITS        (u16, read)  number of configured units
UNIT_SELECT / UNIT_FLAGS  per-unit present / read-only / change counter
```

Completion queue lives host-side; the stub only ever sees one pointer per
INT2 service pass (loop until 0). A shared `guest/copperhf/copperhf_board.h`
defines offsets for both the 68k and Rust sides (existing convention).
Exact layout is finalised in M1.

## Milestones

Each milestone lands green (`cargo test`, clippy, fmt) and is
independently committable.

### M1 — Board, config, host-side device (sync I/O)

- `[copperhf]` config: `unit0..unit6` of the existing `RawDrive`/
  `DriveImage` shape; `enabled` implied by any unit. Validation mirrors
  `[ide]`. Docs: `docs/guide/configuration.md` + `copperline.example.toml`.
- `src/copperhf.rs`: `CopperhfBoard` implementing `ZorroDevice`; units are
  `Vec<Option<HardDriveImage>>`; new `BoardDevice::Copperhf` variant
  (append-only enum); `BoardSpec::copperhf()` in `zorro.rs`; wiring in
  `emulator.rs::build_machine`.
- Doorbell executes the request **synchronously** at first (like
  `filesys.rs` does) — the completion queue and INT2 exist from day one,
  only the I/O is inline. CMD_READ/WRITE/UPDATE/CLEAR + TD_GETGEOMETRY.
- Save state: serialize unit config + change counters + in-flight-empty
  invariant (quiesce on save), reusing `HardDriveImageState`.
- Rust unit tests: doorbell decode, bounds-checked DMA failures →
  `IOERR_BADADDRESS`, request/completion round-trip against a temp HDF.

### M2 — Boot ROM: device stub

- `guest/copperhf/`: `entry.s` (DiagArea, cloned from
  `guest/services/entry.s` — it documents the `-mpcrel` and
  `DAC_CONFIGTIME` traps) + C for the exec device:
  Open/Close/Expunge stubs, `BeginIO` (write pointer to DOORBELL, clear
  `IOF_QUICK`), `AbortIO` (best-effort), INT2 server draining
  COMPLETE_GET → `ReplyMsg`.
- Constraints from §6 of the note: 68000-only code, word-aligned
  structures. V36+ calls are fine *when gated*: runtime-check
  `lib_Version` and fall back to a V34-safe path on older systems (the
  `AddBootNode` → `AddDosNode` idiom generalises — never depend on a
  V36+ call being present, always carry the 1.3 fallback). Build via
  `toolchain.mk`; committed artifact in `assets/copperhf/`.
- Verify with a headless AROS boot: OpenDevice from a mounted shell,
  raw `CMD_READ` of block 0 (a small guest test program under
  `guest/`, like `videocd-test`).

### M3 — Boot ROM: mounter

- 68k RDSK→PART walk → `DosEnvec`s; FSHD/LSEG load into
  FileSystem.resource (creating it if absent); `AddBootNode` when
  `expansion.library` ≥ V36, `AddDosNode` + `eb_MountList` on V34.
  Written fresh with lide's open ROM as behavioural reference (licence
  check per note §9 before borrowing code).
- Because the host layer already wraps RDB-less images in a virtual RDB,
  the mounter has exactly one input shape — no bare-partition path.
- Structured as its own object file with a narrow interface (device name +
  IO entry points) so MIRAGE's ROM can link the same module later.
- Gate: AROS m68k autoboots to a Workbench/shell prompt from both an RDB
  image and an RDB-less image, verified by golden screenshot.

### M4 — Command coverage

- TD64 + NSD (`NSCMD_DEVICEQUERY` advertising both) + `HD_SCSICMD`
  (READ/WRITE 6/10/12/16, Inquiry, Read Capacity 10/16, TUR, Mode Sense
  stubs, Request Sense) — reuse/borrow from `src/scsi.rs::ScsiDisk` CDB
  handling where it fits.
- 32-bit CMD_READ/WRITE beyond 4 GiB → `IOERR_BADADDRESS` (no wrap).
- `TD_CHANGENUM/CHANGESTATE/PROTSTATUS/ADDCHANGEINT/REMCHANGEINT/EJECT`,
  hot attach/detach through CCP/UI surfaced as a disk change.

### M5 — Asynchronous I/O

- Worker thread owning the file handles; doorbell copies write-data out of
  guest RAM and enqueues; completions drain in `tick()`, copy read-data
  in, set `io_Actual`/`io_Error`, assert INT2. Bounded queue;
  `is_idle()`/`next_event_cck()` tuned so the sparse-wake scheduler still
  works. Save state quiesces the worker.
- This is the milestone that proves the MIRAGE `CAP_DMA` shape.

### M6 — Tests, CI, docs

- Integration matrix (design note §6): AROS m68k in default CI
  (`tests/`, no licensed assets); 1.3/3.1/3.2 runs behind `--ignored`
  like the existing ROM-needing tests. Axes: {RDB, RDB-less} ×
  {OFS, FFS-from-LSEG, PFS3-DS >4 GiB}.
- `docs/guide/configuration.md` (config), new `docs/internals/copperhf.md`
  (register block + protocol), note in `docs/guide/headless.md` if any
  flag surface is added.

## Deferred (shared-layer follow-ups, not copperhf-specific)

- FSHD/LSEG synthesis for RDB-less images with a configured filesystem
  handler (design note §4) — belongs in `harddrive.rs` so IDE/SCSI/LIDE
  get it too.
- Convert-to-RDB-on-virtual-write notice/operation — likewise shared.
- Per-image read-only flag in `DriveImage` (doesn't exist today for any
  controller; copperhf's PROTSTATUS reports read-only only for host disks
  until added).
- Whole-cylinder-size warning already exists as a hard error in the shared
  layer (non-multiple of 256 KiB refuses to mount); relaxing to warn+truncate
  is a shared-layer decision.

## Open items to resolve during M1

- Final register layout and completion-ack semantics (read-pops vs
  explicit ack register).
- Zorro II vs III window (II is enough for a 64 KiB register/ROM window
  and works on every machine; nothing needs the Z3 space).
- Unit count cap (7 to match SCSI naming, or 4 to match config ergonomics).

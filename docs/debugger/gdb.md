# Remote GDB

Copperline can run as a headless GDB remote target:

```sh
./target/release/copperline --config copperline.example.toml --noaudio --gdb :2345
```

Port-only forms (`2345` or `:2345`) bind to `127.0.0.1`. Use an explicit
address such as `0.0.0.0:2345` only on a trusted network: the remote
protocol can read and write guest RAM and can resume the emulated machine.

Connect from GDB with the 68k architecture selected:

```gdb
(gdb) set architecture m68k
(gdb) target remote :2345
```

The target starts paused at reset. The stub implements the normal all-stop
remote packets for register access, RAM reads/writes, hardware-style PC
breakpoints, memory watchpoints, single-step, continue, Ctrl-C interrupt,
and GDB reverse execution (`reverse-step` / `reverse-continue`).

## CPU and Memory

GDB sees the core 68000 register set as `d0`-`d7`, `a0`-`a6`, `sp`, `ps`,
and `pc`. Register writes go through Copperline's CPU wrapper so SR stack
banking and interrupt state stay coherent.

Generic GDB memory packets are intentionally conservative:

- Reads use a side-effect-free CPU-visible RAM/ROM view, including the boot
  ROM overlay.
- Writes modify RAM-backed regions only: chip RAM, trapdoor slow RAM, and
  configured RAM expansion boards.
- Writes to ROM, overlay ROM, custom chips, CIA, RTC, IDE, SCSI, CD, and
  other device windows are ignored by `M` packets.

Use `monitor write-reg` for deliberate custom-chip writes.

## Custom Chips

The stub is exercised against real clients: `m68k-amigaos-gdb` (the
bebbo amiga-gcc toolchain) connects, disassembles live ROM, steps,
breaks, and reverse-steps as expected. A stock multi-arch host `gdb`
also accepts the target description, but without an executable loaded
it guesses endianness from the host -- issue `set endian big` before
`target remote` (or just load your program's ELF with `file`, which the
Amiga toolchains' gdb does implicitly).

Use GDB's `monitor` command for Amiga-specific state:

```gdb
(gdb) monitor status
(gdb) monitor beam
(gdb) monitor custom
(gdb) monitor reg DMACON
(gdb) monitor reg DFF100
(gdb) monitor write-reg COLOR00 00f
```

Custom-register inspection is side-effect-free. It reads Copperline's
internal Agnus/Denise/Paula/blitter latches rather than executing a real CPU
read from `$DFFxxx`, so it will not acknowledge interrupts, clear latches, or
advance collision/audio state. `write-reg` is different: it routes a word
write through the normal custom-register write path and therefore has real
hardware effects.

Register names match the debugger window (`DMACON`, `BPLCON0`, `COLOR00`,
`AUD0VOL`, and so on). Numeric offsets (`96`) and full custom addresses
(`DFF096`) are also accepted.

## Beam traps

`monitor beam-trap VPOS [HPOS]` (decimal, like `monitor beam`'s output)
toggles a beam trap: `continue` then stops when the Agnus beam reaches that
position, at exact colour-clock granularity, even while the CPU sits in
`STOP`. `HPOS` omitted means the start of the line, and a trap re-fires
every frame until removed. `monitor clear-beam-traps` removes them all.

```gdb
(gdb) monitor beam-trap 100
(gdb) monitor beam-trap 100 40
(gdb) monitor clear-beam-traps
```

## Copper breakpoints

`monitor copper-break ADDR` (hex) toggles a Copper breakpoint: `continue`
stops when the Copper's PC arrives at that list address, before the
instruction there executes. `monitor clear-copper-breaks` removes them all.

```gdb
(gdb) monitor copper-break c01000
(gdb) monitor clear-copper-breaks
```

## Copper

The Copper list can be dumped from the live list pointer, the current Copper
PC, or an explicit chip-RAM address:

```gdb
(gdb) monitor copper
(gdb) monitor copper pc 20
(gdb) monitor copper 00c01000 80
```

Counts are hexadecimal, matching GDB's packet syntax and Copperline's other
debugger address inputs.

## Reverse Debugging

`--gdb` arms the same snapshot-ring reverse debugger used by the window and
headless reverse watchpoint. GDB commands map as follows:

| GDB command | Copperline operation |
|---|---|
| `reverse-step` | reconstruct the previous instruction boundary |
| `reverse-continue` | run backward to the previous GDB PC breakpoint |
| `monitor last-writer ADDR` | find the last instruction that changed the watched word |

Reverse history uses `COPPERLINE_DBG_RR_BUDGET_MB` and
`COPPERLINE_DBG_RR_INTERVAL`, with the same tradeoff as the other frontends:
more memory and more frequent snapshots make reverse operations faster.

For byte-identical replay, keep the usual determinism requirements from
[](reverse): set `COPPERLINE_RTC_FIXED_SECS` when guest RTC reads matter, and
avoid externally mutating hard-drive/CD images during a debug session.

## Source-Level Debugging of Amiga Programs

Copperline answers GDB's `qOffsets` query with the load addresses of the
current process's segment list (the hunks `LoadSeg()` scattered through
RAM): `TextSeg=` is the first hunk and `DataSeg=` the second, when
present. With an amiga-gcc toolchain (bebbo's `m68k-amigaos-gdb`), that
means source-level debugging of a program running inside the emulator
mostly just works:

1. Build with debug info: `m68k-amigaos-gcc -g -O0 prog.c -o prog`.
2. Start Copperline with `--gdb`, boot Workbench, and start `prog`
   (easiest from a CLI so the process has a `cli_Module` seglist).
3. `m68k-amigaos-gdb prog` then `target remote localhost:2345`. GDB asks
   `qOffsets`, relocates the program's sections to the hunk addresses,
   and `break main` / `next` / `print` work on source lines.

If the program was not yet running when GDB attached, re-run `qOffsets`
by reattaching, or relocate manually: `monitor segments` prints every
hunk address, and `add-symbol-file prog.elf ADDR` (first hunk) loads the
symbols at the right place. The `SEGMENTS` console command prints the
same map, pre-formatted with that hint. When no process seglist is
walkable (ROM code, task rather than process, OS not up yet), the
`qOffsets` reply is empty and GDB falls back to link-time addresses --
harmless for ROM-level sessions.

## Monitor Commands

| Command | Effect |
|---|---|
| `help` | list monitor commands |
| `status` | CPU PC/SR, frame, beam, instruction position, reverse status |
| `beam` | beam/frame/colour-clock position |
| `custom` | compact custom-chip state dump |
| `stepover` | step over a BSR/JSR/TRAP call (single step otherwise) |
| `finish` | run until the current subroutine returns to its caller |
| `reg NAME\|OFFSET` | side-effect-free custom-register latch read |
| `write-reg NAME\|OFFSET VALUE` | real custom-register word write |
| `watch-reg NAME\|OFFSET` | stop on CPU or Copper writes to the custom register |
| `unwatch-reg NAME\|OFFSET` | remove one custom-register watch |
| `clear-reg-watches` | remove all custom-register watches |
| `copper [auto\|pc\|ADDR] [COUNT]` | disassemble Copper instructions |
| `last-writer ADDR` | reverse-search the last write to a word |
| `segments` | the current process's loaded hunks (LoadSeg addresses) |

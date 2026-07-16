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
program load events (`qXfer:libraries:read`), and GDB reverse execution
(`reverse-step` / `reverse-continue`). Detaching leaves the machine
paused and the stub listening, so a later `target remote` picks up the
same session (and re-runs `qOffsets`); GDB's `kill` ends the emulator.

If `monitor` (or anything else) answers `"monitor" command not supported
by this target.`, GDB is not actually connected: that is GDB's built-in
dummy target speaking after a failed `target remote`, and `-batch`
scripts carry on past the connection error. A common cause is running
GDB inside a container (e.g. a cross-toolchain Docker image): the
container's `localhost` is not the host's, so it never reaches
Copperline's `127.0.0.1` listener. Run GDB on the host, or bind an
address the container can reach (`--gdb 0.0.0.0:2345`, trusted networks
only per the warning above) and connect to the host's address.

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

What symbol detail you get depends on the file you feed GDB, not on the
stub:

- An AmigaOS hunk executable built with `-g` (bebbo's amiga-gcc) carries
  a hunk symbol table: `file prog` gives function-level symbols --
  `break main`, named disassembly -- but no DWARF, so source lines,
  `next`, and `print` on variables are unavailable. Reading hunk files
  at all requires a GDB whose BFD includes the `amiga` target (check
  with `m68k-amigaos-objdump -i`); some builds of the same toolchain
  lack it and report "file format not recognized".
- Full source-level debugging needs an ELF file with DWARF kept
  alongside the hunk binary you actually run: build with an m68k ELF
  toolchain and convert the executable with `elf2hunk` (the flow the
  amiga-debug VS Code extension uses), then load the ELF's symbols with
  `add-symbol-file prog.elf ADDR` or through the library list below.

Either way the symbols must land at the addresses `LoadSeg()` chose.
Copperline answers GDB's `qOffsets` query with the load addresses of the
current process's segment list: `TextSeg=` is the first hunk and
`DataSeg=` the second, when present. GDB asks `qOffsets` once at attach,
so this relocates a program that is already running: start `prog` from a
CLI inside the emulator (so the process has a `cli_Module` seglist),
then `m68k-amigaos-gdb prog` + `target remote localhost:2345` places
`prog`'s symbols at the loaded hunk addresses. When no process seglist
is walkable (ROM code, task rather than process, OS not up yet), the
`qOffsets` reply is empty and GDB falls back to link-time addresses --
harmless for ROM-level sessions.

### Program Load Events

For a program AmigaDOS loads *after* GDB is connected, the stub tracks
the scheduled process's segment list and reports the load at the moment
`LoadSeg()`'s result is installed -- after relocation, before the
program's first instruction.

The workflow that works with every client, `-batch` scripts included:

```gdb
(gdb) target remote :2345
(gdb) monitor loadseg-break
(gdb) continue
loadseg: hello first hunk $018FE8 (monitor segments / add-symbol-file FILE 0x18FE8)
Program received signal SIGTRAP, Trace/breakpoint trap.
(gdb) disconnect
(gdb) file hello
(gdb) target remote :2345
(gdb) break main
(gdb) continue
```

`monitor loadseg-break` makes `continue` stop, with a console hint
naming the program and its first hunk, whenever a new program is
loaded. Detaching does not end the emulator: the machine stays paused
and the stub listens for the next connection, so reattaching with the
program's file loaded re-runs `qOffsets` against the now-current
process and relocates the symbols with no manual addresses (GDB's
`kill` command ends the server). Alternatively stay attached and use
`add-symbol-file FILE ADDR` with the printed hunk address; `monitor
segments` prints the full hunk map, and the `SEGMENTS` console command
prints the same map pre-formatted with that hint.

Clients that request GDB's target library lists get automatic load
events instead: fetching `qXfer:libraries:read` once arms them, and
each new program then reports a `library:` stop event -- GDB re-reads
the stub's library list (one `<library>` per tracked program, a
`<segment>` per hunk, named by the command's basename), relocates a
matching file found via `set solib-search-path`, binds pending
breakpoints, and resumes on its own. Be aware that stock bare-metal
m68k GDB builds -- `m68k-amigaos-gdb` 13.x and a multi-arch host `gdb`
alike -- never request library lists, so they never see these events;
use the `monitor loadseg-break` flow with them. Programs already
running when the list is first fetched are reported in it without a
stop; `monitor loadseg-list` prints the same table.

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
| `beam-trap VPOS [HPOS]` | toggle a stop when the beam reaches that position |
| `clear-beam-traps` | remove all beam traps |
| `copper-break ADDR` | toggle a stop when the Copper PC reaches ADDR |
| `clear-copper-breaks` | remove all Copper breakpoints |
| `last-writer ADDR` | reverse-search the last write to a word |
| `segments` | the current process's loaded hunks (LoadSeg addresses) |
| `loadseg-break` | toggle a visible stop whenever a new program is loaded |
| `loadseg-list` | tracked program loads with their hunk addresses |

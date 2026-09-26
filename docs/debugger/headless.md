# Headless debugger environment reference

Copperline includes a headless debugger (`src/debugger.rs`) driven by
`COPPERLINE_DBG_*` environment variables. It works in windowed sessions as
well as windowless `--screenshot-after` and `--dump-frames` runs. Like every
`COPPERLINE_*` variable, these are read once at start-up.

Reports go through the standard `log` crate at info level. Set `RUST_LOG=info`
(or `RUST_LOG=debug`) to see them:

```sh
RUST_LOG=info \
COPPERLINE_DBG_BREAK=C033C2 \
COPPERLINE_DBG_DUMP=C09580:4 \
COPPERLINE_DBG_SHOT=/tmp/hit \
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out.png
```

Addresses are hexadecimal, with or without a `0x` or `$` prefix (`MEMW` and
`FC` accept only `0x`).

## Timeline transparency

The headless debugger is a pure observer. Its hooks read machine state through
side-effect-free queries and consume no CPU or chip-bus cycles, so enabling any
combination of these variables leaves the emulated timeline identical to an
uninstrumented run: the same instructions retire at the same colour clocks,
memory contents match, and rendered frames match byte for byte.

`tests/debugger_transparency.rs` checks this. It boots the bundled ROM on two
machines side by side, one with every programmatic debugging hook armed
(breakpoints, watchpoints, instruction traces, and the internal CPU/bus
observers) and one without, and compares the machine states after every frame.

Two exceptions apply:

- When JIT is enabled (`[cpu] jit` / `--jit`), enabling any debug hook
  reverts the CPU to the precise per-instruction loop and logs a warning at
  start-up. Compare debugged and undebugged runs with JIT disabled.
- Settings that change the machine (such as `COPPERLINE_IRQ_LATENCY_CCK`, a
  different CPU model, or injected input scripts) alter its behaviour by design.
  So does `COPPERLINE_DBG_EXTCCK`, which sets the cost in hundredths of a colour
  clock of a CPU word access to ROM or fast RAM (default 200); it is compiled
  only into builds with the `internal-diagnostics` feature, and normal builds
  ignore it.

## Breakpoint and watchpoint variables

The debugger is armed when at least one of `BREAK`, `WATCH`, `MMIO`, `TRACE`,
`CATCH`, `CATCHALERT`, `COPPER`, `RAMDUMP`, or `LISTCHECK` is set; `DUMP` and
`SHOT` only add to the reports those produce. Each report (a hit) is a `DBG`
line with the emulated time, frame, beam position, SR, and PC, followed by the
registers, the display state, and any `DUMP` regions. Hits count against
`COPPERLINE_DBG_MAXHITS`.

`COPPERLINE_DBG_BREAK=PC[,PC...]`
: Program counter breakpoints. Each hit logs a report; the run continues.

`COPPERLINE_DBG_WATCH=ADDR[:LEN][,...]`
: Memory watchpoints (`LEN` in decimal bytes, default 2). The watched words are
  compared after every CPU instruction, and each change is reported with its
  old and new value and the PC of the instruction that just ran. A change made
  by DMA (Copper, blitter, disk) while that instruction ran is reported against
  it too.

`COPPERLINE_DBG_MMIO=ADDR[:LEN[:CLASS]][,...]`
: CPU access watches (MMIO watches). Logs every CPU data access to the byte range
  -- instruction fetches excluded -- with its direction, address, size, value, PC,
  frame, beam position, colour clock, and emulated time. `LEN` is a byte count
  (decimal, or hex with `$`/`0x`; default 2), `CLASS` is `read`, `write`, or
  `access` (default). Unlike `COPPERLINE_DBG_WATCH`, which compares memory words
  and so cannot see a device register, the watch is taken at the CPU's bus access,
  so it reports Akiko, CIA, Gayle, Zorro board, and custom register traffic.
  Accesses made by exception processing (the stack frame, the vector fetch) carry
  the last instruction retired before the exception. Each line counts as a hit
  against `COPPERLINE_DBG_MAXHITS`, and the `AFTER`/`UNTIL` window applies to
  the access time:

  ```text
  COPPERLINE_DBG_MMIO="B8001D:1:write,B80020:2:write"
  DBG MMIO write $B8001D.B = $03 (pc $E593B0, f141 v51 h33, cck 10029801) t=2.827769
  ```

`COPPERLINE_DBG_CD=1`
: Logs the CD drive's command trace (CD32 Akiko): one line per step of each
  command's life -- `executed`, `first_sector` (a read, play, or TOC dump's first
  delivered unit), and `completed` -- with the decoded command, its sector range,
  requested speed, reply status, each step as milliseconds after the host issued
  it, the units delivered, the outcome, and the emulated time and colour clock of
  the step. The stamps are emulated time, so two runs (two ROMs, two
  configurations) compare line for line. Not bound by the debugger's hit budget or
  time window:

  ```text
  DBG CD completed #4 toc x1 st $00: +5.00ms accept, +5.99ms exec, +9.81ms reply, +5968.97ms first, 81 units, +6282.87ms end t=9.114314 cck=32327513
  ```

  The same records are the debugger window's [CD tab](window.md#debugger-cd-tab),
  the console's `CDTRACE`, and the control protocol's `cd.trace` and `event.cd`.

`COPPERLINE_DBG_MEMW=ADDR`
: CPU-only write watchpoint on a single word. Logs the writing instruction's PC,
  the value written, the emulated time, and the frame.

`COPPERLINE_DBG_FC=ADDR`
: Logs every change to the word at `ADDR` with the emulated time, the PC of the
  instruction that just ran, and a running change count. Useful for measuring
  frame counters and polling loops.

`COPPERLINE_DBG_DUMP=ADDR:WORDS[,...]`
: Memory regions to hex-dump with every report (`WORDS` in decimal, default 2).

`COPPERLINE_DBG_TRACE=1`
: Logs every executed instruction, disassembled, while the debugger is active
  (see `AFTER`/`UNTIL`), up to 200,000 lines.

`COPPERLINE_DBG_TRACE_FULL=1`
: Like `TRACE` (which it implies), but each line is a fixed-width all-hex record
  of `D0`-`D7`, `A0`-`A7`, and the CCR, for diffing against a reference 68000
  trace.

`COPPERLINE_DBG_TRACE_LO=ADDR` / `COPPERLINE_DBG_TRACE_HI=ADDR`
: Restricts the trace to instructions in the address range `[LO, HI]`.

`COPPERLINE_DBG_CATCH=SPEC[,SPEC...]`
: Exception catchpoints: decimal vector numbers, or `irq N`, `trap N`, and
  `vec N` (e.g. `COPPERLINE_DBG_CATCH="3,4,irq 3,trap 0"`).

`COPPERLINE_DBG_CATCHALERT=1`
: Breaks at `exec.library/Alert()` once ExecBase is valid, and decodes the
  alert code (Guru Meditation).

`COPPERLINE_DBG_IRQ=1`
: Logs serviced interrupt levels and pending interrupt request bits.

`COPPERLINE_DBG_CIA=1`
: Logs CIA-A and CIA-B interrupt requests (with the ICR bits), CIA-B TOD alarms,
  and `INTENA`/`INTREQ` writes that touch the EXTER bit (or, for `INTENA`, the
  master enable).

`COPPERLINE_DBG_DSKLEN=1`
: Logs every `DSKLEN` write (disk DMA arming) with `DSKPT`, direction, and word
  count, and each DSKBLK completion interrupt.

`COPPERLINE_DBG_SPREN=1`
: Reports each `DMACON` write that clears the sprite DMA enable bit, with the
  writing PC.

`COPPERLINE_DBG_BLIT=LO[:HI]`
: Logs each blit started between `LO` and `HI` emulated seconds (`HI` defaults
  to the end of the run) with its control words, D pointer and modulo, size,
  and mode.

`COPPERLINE_DBG_RAMDUMP=ADDR:LEN:FILE`
: One-shot dump of `LEN` bytes (hex) from `ADDR` to `FILE` the first time the
  debugger activates.

`COPPERLINE_DBG_COPPER=auto[:COUNT] | ADDR[:COUNT]`
: Disassembles `COUNT` Copper instructions (decimal, default 256) once, when the
  debugger first activates. `auto` (or `1`) starts at the live `COP1LC`.

`COPPERLINE_DBG_LISTCHECK=HEAD[,HEAD...]`
: Walks the Exec `List` headers at these addresses after every instruction and
  reports the first instruction after which a node is linked twice or a list is
  unterminated.

`COPPERLINE_DBG_EXPORT_PLANES=1`
: Exports per-line fetched bitplane data and composite index images, as PGM
  files, for frames in the active window. `COPPERLINE_DBG_EXPORT_PLANES_DIR=DIR`
  sets the output directory (default: the system temporary directory).

`COPPERLINE_DBG_FRAMESTATE=1`
: Logs display configuration, palette, DMA state, and Denise sprite shadow/hardware registers.
  `COPPERLINE_DBG_FRAMESTATE_FULLPAL=1` extends palette logs to all 256 AGA entries.

`COPPERLINE_DBG_AFTER=SECS` / `COPPERLINE_DBG_UNTIL=SECS`
: Restricts the debugger to a window of emulated time. `MEMW`, `FC`, `IRQ`,
  `SPREN`, `EXPORT_PLANES`, and `FRAMESTATE` honour the same window; `CIA`,
  `DSKLEN`, and `CD` do not.

`COPPERLINE_DBG_MAXHITS=N`
: Stops reporting after `N` hits (default: 200).

`COPPERLINE_DBG_SHOT=PREFIX`
: Saves a PNG of the current frame with each report (`PREFIX-0000.png`,
  `PREFIX-0001.png`, ...).

The reverse-debugging variables (`COPPERLINE_DBG_RWATCH`, `COPPERLINE_DBG_RR`,
`COPPERLINE_DBG_RR_BUDGET_MB`, `COPPERLINE_DBG_RR_INTERVAL`) are described in
[](reverse.md).

## Subsystem diagnostic variables

These log subsystem internals, mostly for emulator development. The table lists
the most useful ones; the source has more (search for `COPPERLINE_DIAG_`).

| Variable | Description |
|---|---|
| `COPPERLINE_DIAG_SLOTMAP` | Dumps the per-colour-clock chip-bus owner map for one frame's lines; `COPPERLINE_DIAG_SLOTMAP_AT=SECS` picks the frame and `COPPERLINE_DIAG_SLOTMAP_RANGE=V0:V1` the lines |
| `COPPERLINE_DIAG_BLT_SLOTS` | Detailed blitter pipeline slot and bus ownership trace |
| `COPPERLINE_DIAG_IPL` | Per-frame split of CPU cycles between main code and interrupt handlers |
| `COPPERLINE_DIAG_PCSAMPLE` | Logs the PC and SR every 50 frames, to locate CPU hotspots |
| `COPPERLINE_DIAG_PCHIST` | Records recent PC execution history |
| `COPPERLINE_DIAG_COPLEN` | Measures Copper list length |
| `COPPERLINE_DIAG_COP_WRITES` | Logs exact landing colour-clock cycle for every Copper MOVE |
| `COPPERLINE_DIAG_CPU_BUS` | Logs CPU chip-bus request, grant, and cycle wait states |
| `COPPERLINE_DIAG_CPU_READS` | Logs CPU custom-register reads with bus slot and return value |
| `COPPERLINE_DIAG_CPU_SYNC` | CPU internal cycle trace at synchronization points |
| `COPPERLINE_DIAG_CPU_WRITES` | Logs CPU custom-register writes with bus slot and beam coordinates |
| `COPPERLINE_DIAG_DISPLAY` | Display register change log |
| `COPPERLINE_DIAG_CAPROW` | Per-line bitplane capture state at DDF start |
| `COPPERLINE_DIAG_PALETTE_ROW` | Logs beam-timed COLOR writes for selected scanlines |
| `COPPERLINE_DIAG_PALSTORE` | Logs COLOR and BPLCON3 writes at register store application |
| `COPPERLINE_DIAG_HAM_PIXELS` | Samples DMA playfield HAM pixels on a specified beam line |
| `COPPERLINE_DIAG_MANUAL_BPL_PIXELS` | Samples CPU/Copper BPLDAT replay pixels on a beam line |
| `COPPERLINE_DIAG_FRAME_PIXELS` | Samples final framebuffer pixels after rendering pipeline |
| `COPPERLINE_DIAG_SPRITES` | Sprite DMA fetch and render log |
| `COPPERLINE_DIAG_SPRCAP` | Logs captured sprite DMA lines |
| `COPPERLINE_DIAG_MANUAL_SPRITES` | Logs manually replayed sprite intervals and register writes |
| `COPPERLINE_DIAG_SPRITE_PIXELS` | Samples non-transparent sprite pixels on a beam line |
| `COPPERLINE_DIAG_BLITREGS` | Logs the full blitter register set at blit starts |
| `COPPERLINE_TRACE_BLITTER=PATH` | Writes a JSON-lines trace of blit starts, completions, and `DMACONR` polls to `PATH` |
| `COPPERLINE_DIAG_POLLSTATS` | Reports most-read CIA and custom registers on screenshots/dumps |
| `COPPERLINE_DIAG_DISK` | Disk DMA state transitions and DSKLEN writes |
| `COPPERLINE_DIAG_FLUXBRIDGE` | Detailed physical floppy drive head stepping and MFM sector metrics |
| `COPPERLINE_DIAG_AUDIO_NOTES` | Logs Paula channel note on/off transitions |
| `COPPERLINE_DIAG_CRASH` | CPU empty-RAM execution and low-memory write context |
| `COPPERLINE_DIAG_GAYLE` / `COPPERLINE_DIAG_CDTV` | Gayle IDE and CDTV controller traffic |
| `COPPERLINE_DIAG_A2091` | A2091 SCSI DMAC and WD33C93 register access trace |
| `COPPERLINE_DIAG_A4091` | A4091 NCR53C710 SCRIPTS instruction trace |
| `COPPERLINE_DIAG_CURSOR` | Host cursor mapping diagnostics |
| `COPPERLINE_DUMP_BLITMEM=LO:HI:START:END` | Dumps chip RAM `START`-`END` (hex) on each `BLTSIZE` write between `LO` and `HI` emulated seconds, into `COPPERLINE_DUMP_BLITMEM_DIR` (default: a `copperline-blitdump` folder in the system temporary directory) |
| `COPPERLINE_DUMP_BUS_ACCOUNTING` | Per-frame chip-bus slot accounting summary |
| `COPPERLINE_SHOT_RAW=1` | Writes screenshots and frame dumps as the unscaled 716x570 woven framebuffer instead of the presented image |

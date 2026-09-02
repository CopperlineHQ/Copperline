# Headless debugger environment reference

Copperline includes a headless debugger (`src/debugger.rs`) driven by
`COPPERLINE_DBG_*` environment variables. It operates during normal execution
as well as windowless `--screenshot-after` and `--dump-frames` runs.

Log output is emitted through the standard `log` crate. Set `RUST_LOG=info`
or `RUST_LOG=debug` to view reports:

```sh
RUST_LOG=info \
COPPERLINE_DBG_BREAK=C033C2 \
COPPERLINE_DBG_DUMP=C09580:4 \
COPPERLINE_DBG_SHOT=/tmp/hit \
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out.png
```

Addresses are specified in hexadecimal (with or without `0x` or `$` prefixes).

## Timeline transparency

The headless debugger operates as a non-intrusive observer. Its hooks inspect
machine state via side-effect-free queries without consuming CPU or chip-bus
cycles, ensuring that enabling any combination of debugging environment
variables leaves the emulated timeline identical to an uninstrumented run.
The exact same instructions retire at the same colour clocks, memory states
remain identical, and rendered frames match byte-for-byte.

This guarantee is verified by `tests/debugger_transparency.rs`, which boots
the bundled ROM simultaneously with and without all programmatic debugging hooks
active (including breakpoints, watchpoints, instruction traces, and internal
CPU/bus observers), comparing machine states after every frame.

Two exceptions apply:

- When JIT is enabled (`[cpu] jit` / `--jit`), enabling any debug hook
  automatically reverts CPU execution to cycle-accurate step mode and logs a
  warning at startup. Compare debugged and undebugged runs with JIT disabled.
- Hardware configuration overrides (such as `COPPERLINE_IRQ_LATENCY_CCK`,
  `COPPERLINE_DBG_EXTCCK`, changing the CPU model, or injecting input scripts)
  alter underlying machine behavior by design.

## Breakpoint and watchpoint variables

`COPPERLINE_DBG_BREAK=PC[,PC...]`
: Program counter breakpoints. On each hit, logs emulated timestamp, frame number,
  beam position (`v=`, `h=`), registers, and any configured memory dumps.

`COPPERLINE_DBG_WATCH=ADDR[:LEN][,...]`
: Memory watchpoints (length in bytes, default 2). Logs memory modifications
  from CPU, Copper, or Blitter DMA.

`COPPERLINE_DBG_MEMW=ADDR`
: CPU-only write watchpoint on a single word. Logs the writing instruction PC,
  post-write value, and emulated timestamp.

`COPPERLINE_DBG_FC=ADDR`
: Logs every change to the word at `ADDR` with emulated time and update counter.
  Useful for analyzing frame counters and polling loops.

`COPPERLINE_DBG_DUMP=ADDR:WORDS[,...]`
: Memory regions to hex-dump when breakpoint or watchpoint reports fire.

`COPPERLINE_DBG_TRACE=1`
: Emits disassembled per-instruction execution trace during the active debugger window.

`COPPERLINE_DBG_TRACE_FULL=1`
: Fixed-width all-hex register dumps per instruction for differential trace comparison.

`COPPERLINE_DBG_TRACE_LO=ADDR` / `COPPERLINE_DBG_TRACE_HI=ADDR`
: Restricts execution trace output to instructions within the address range `[LO, HI]`.

`COPPERLINE_DBG_CATCH=SPEC[,SPEC...]`
: Exception vector catchpoints (e.g., `COPPERLINE_DBG_CATCH="3,4,irq 3"`).

`COPPERLINE_DBG_CATCHALERT=1`
: Intercepts `exec.library/Alert()` calls and decodes the alert code (Guru Meditation).

`COPPERLINE_DBG_IRQ=1`
: Logs serviced interrupt levels and pending interrupt request bits.

`COPPERLINE_DBG_CIA=1`
: Logs INTENA and INTREQ writes touching the EXTER bit or master enable.

`COPPERLINE_DBG_DSKLEN=1`
: Logs every DSKLEN write (disk DMA arming) and DSKBLK interrupt completion.

`COPPERLINE_DBG_SPREN=1`
: Logs when a DMACON write clears the sprite-DMA enable bit, including the writing PC.

`COPPERLINE_DBG_BLIT=LO:HI`
: Logs Blitter operations started between `LO` and `HI` emulated seconds.

`COPPERLINE_DBG_RAMDUMP=ADDR:LEN:FILE`
: One-shot memory dump written to `FILE` the first time the debugger activates.

`COPPERLINE_DBG_COPPER=auto | ADDR[:COUNT]`
: Dumps disassembled Copper list on first debugger activation (`auto` reads `COP1LC`).

`COPPERLINE_DBG_LISTCHECK=HEAD[,HEAD...]`
: Walks Exec `List` linkage headers after every instruction to detect cycles or corruption.

`COPPERLINE_DBG_EXPORT_PLANES=1`
: Exports per-line fetched bitplane data and composite index images for frames in the active window.
  Use `COPPERLINE_DBG_EXPORT_PLANES_DIR=DIR` to set the output directory.

`COPPERLINE_DBG_FRAMESTATE=1`
: Logs display configuration, palette, DMA state, and Denise sprite shadow/hardware registers.
  `COPPERLINE_DBG_FRAMESTATE_FULLPAL=1` extends palette logs to all 256 AGA entries.

`COPPERLINE_DBG_AFTER=SECS` / `COPPERLINE_DBG_UNTIL=SECS`
: Restricts debugger evaluation to a specific emulated time window.

`COPPERLINE_DBG_MAXHITS=N`
: Limits maximum logged report hits (default: 200).

`COPPERLINE_DBG_SHOT=PREFIX`
: Saves a PNG screenshot on each breakpoint hit (`PREFIX-0000.png`, etc.).

## Subsystem diagnostic variables

| Variable | Description |
|---|---|
| `COPPERLINE_DIAG_SLOTMAP` | Dumps per-colour-clock chip-bus allocation map for a frame |
| `COPPERLINE_DIAG_BLT_SLOTS` | Detailed Blitter pipeline slot and bus ownership trace |
| `COPPERLINE_DIAG_IPL` | CPU cycle consumption breakdown per interrupt level |
| `COPPERLINE_DIAG_PCSAMPLE` | Sampled PC histogram every 50 frames to locate CPU hotspots |
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
| `COPPERLINE_DIAG_BLITREGS` | Logs full Blitter register set at blit starts |
| `COPPERLINE_TRACE_BLITTER` | Generates JSON trace of Blitter starts, polls, and IRQ latches |
| `COPPERLINE_DIAG_POLLSTATS` | Reports most-read CIA and custom registers on screenshots/dumps |
| `COPPERLINE_DIAG_DISK` | Disk DMA state transitions and DSKLEN writes |
| `COPPERLINE_DIAG_FLUXBRIDGE` | Detailed physical floppy drive head stepping and MFM sector metrics |
| `COPPERLINE_DIAG_AUDIO_NOTES` | Logs Paula channel note on/off transitions |
| `COPPERLINE_DIAG_CRASH` | CPU empty-RAM execution and low-memory write context |
| `COPPERLINE_DIAG_GAYLE` / `CDTV` | Gayle IDE and CDTV controller traffic |
| `COPPERLINE_DIAG_A2091` | A2091 SCSI DMAC and WD33C93 register access trace |
| `COPPERLINE_DIAG_A4091` | A4091 NCR53C710 SCRIPTS instruction trace |
| `COPPERLINE_DIAG_CURSOR` | Host cursor mapping diagnostics |
| `COPPERLINE_DUMP_BLITMEM` | Dumps chip RAM on BLTSIZE writes |
| `COPPERLINE_DUMP_BUS_ACCOUNTING` | Per-frame chip-bus slot accounting summary |
| `COPPERLINE_SHOT_RAW=1` | Exports unscaled 716x570 native raster framebuffer dumps |

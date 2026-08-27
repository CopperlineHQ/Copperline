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

`COPPERLINE_DBG_BLIT=LO:HI`
: Logs Blitter operations started between `LO` and `HI` emulated seconds.

`COPPERLINE_DBG_COPPER=auto | ADDR[:COUNT]`
: Dumps disassembled Copper list on first debugger activation (`auto` reads `COP1LC`).

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
| `COPPERLINE_DIAG_COP_WRITES` | Logs exact landing colour-clock cycle for every Copper MOVE |
| `COPPERLINE_DIAG_CPU_BUS` | Logs CPU chip-bus request, grant, and cycle wait states |
| `COPPERLINE_DIAG_FLUXBRIDGE` | Detailed physical floppy drive head stepping and MFM sector metrics |
| `COPPERLINE_DIAG_AUDIO_NOTES` | Logs Paula channel note on/off transitions |
| `COPPERLINE_DIAG_A2091` | A2091 SCSI DMAC and WD33C93 register access trace |
| `COPPERLINE_DIAG_A4091` | A4091 NCR53C710 SCRIPTS instruction trace |
| `COPPERLINE_SHOT_RAW=1` | Exports unscaled 716x570 native raster framebuffer dumps |

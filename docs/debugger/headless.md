# The headless debugger

A second, scriptable debugger (`src/debugger.rs`) is driven entirely
by `COPPERLINE_DBG_*` environment variables and works in any run, including
windowless `--screenshot-after` / `--dump-frames` captures. It is the main
tool for timing and compatibility investigations: because the core is
deterministic, a failing run can be replayed with progressively more
instrumentation and every replay hits the same cycle.

Output goes through the `log` crate at info level, so set `RUST_LOG=info`
(or `debug`) to see it.

```sh
RUST_LOG=info \
COPPERLINE_DBG_BREAK=C033C2 \
COPPERLINE_DBG_DUMP=C09580:4 \
COPPERLINE_DBG_SHOT=/tmp/hit \
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out.png
```

All addresses are hexadecimal, with or without a `0x` prefix. Like every
`COPPERLINE_*` knob, the variables are snapshotted once at startup and
cannot change at runtime (see [](../internals/architecture)).

## Variables

`COPPERLINE_DBG_BREAK=PC[,PC...]`
: PC breakpoints. Each hit logs a `DBG BREAK` report: emulated time, frame,
  beam position (`v=`/`h=`), SR, PC, the full register file, any
  `DBG_DUMP` memory regions, and a screenshot if `DBG_SHOT` is set.

`COPPERLINE_DBG_WATCH=ADDR[:LEN][,...]`
: Memory watchpoints (LEN in bytes, default 2). Logs when a watched word
  changes, whoever wrote it:
  `DBG WATCH 0x00c09580 0012->0013 by pc=0x00c03374`.

`COPPERLINE_DBG_MEMW=ADDR`
: CPU-write watchpoint on one word: whenever a CPU write covers the
  watched word, logs the writer PC, the post-write value, and the emulated
  time and frame. Narrower than `WATCH` (CPU writes only), but attributes
  the write from inside the CPU's own decode, so chip-RAM mirror aliases
  are noted too. Bounded by `AFTER`/`UNTIL`.

`COPPERLINE_DBG_FC=ADDR`
: Log every change of the 16-bit word at ADDR -- whoever caused it -- with
  emulated time and a running change count. Useful for pacing questions
  ("how often does this frame counter tick").

`COPPERLINE_DBG_DUMP=ADDR:WORDS[,...]`
: Memory regions to hex-dump with every break/watch report
  (`mem 0x00c09580: 0000 0001 0002 0003`).

`COPPERLINE_DBG_TRACE=1`
: Disassembled per-instruction trace while the debugger window (AFTER/UNTIL)
  is active, with key registers on each line. Capped at 200,000 lines per
  run as a flood guard.

`COPPERLINE_DBG_TRACE_FULL=1`
: Like `TRACE`, but each line is a fixed-width, all-hex record of the entire
  register file (`D0`-`D7`/`A0`-`A7`) and the CCR, prefixed `ft`. Intended for
  diffing Copperline's instruction stream against a reference 68000 (e.g.
  vAmiga) to isolate a mis-emulated instruction. Implies `TRACE`.

`COPPERLINE_DBG_TRACE_LO=ADDR` / `COPPERLINE_DBG_TRACE_HI=ADDR`
: Restrict the trace to instructions whose PC is in `[LO, HI]`. This isolates a
  single routine (e.g. a depacker loop) and, by excluding interrupt handlers,
  yields a contiguous deterministic stream that lines up across emulators.

`COPPERLINE_DBG_CATCH=SPEC[,SPEC...]`
: Exception catchpoints. `SPEC` can be a decimal vector number, `vec N`,
  `irq N`, or `trap N`; for example, `COPPERLINE_DBG_CATCH="3,4,irq 3"`
  reports address errors, illegal instructions, and VERTB interrupt entries.

`COPPERLINE_DBG_CATCHALERT=1`
: Resolve `ExecBase` once AmigaOS is valid and report when execution reaches
  exec.library `Alert()`. Each hit includes the D7 alert code decoded with the
  same Guru table used by the interactive debugger.

`COPPERLINE_DBG_IRQ=1`
: Log every serviced interrupt -- level plus the enabled pending source
  bits -- with emulated time. Bounded by `AFTER`/`UNTIL`.

`COPPERLINE_DBG_CIA=1`
: Log INTENA/INTREQ writes that touch the EXTER bit or the master enable,
  for tracing CIA-interrupt enable/acknowledge traffic.

`COPPERLINE_DBG_DSKLEN=1`
: Log every DSKLEN write (disk DMA arming: word count, direction, DSKPT,
  beam position) and the matching DSKBLK completion, closing each
  arm-to-completion interval to correlate disk activity with scene
  timing.

`COPPERLINE_DBG_SPREN=1`
: When a DMACON write clears the sprite-DMA-enable bit, log the
  instruction that did it (previous PC) and the full register file.

`COPPERLINE_DBG_BLIT=LO:HI`
: Log each blit started between LO and HI emulated seconds: control
  words, D pointer/modulo, size, and mode flags, with the starting beam
  position. For finding which blit produces a given rendered region.

`COPPERLINE_DBG_RAMDUMP=ADDR:LEN:FILE`
: One-shot memory dump the first time the debugger activates: LEN bytes
  from hex address ADDR are written to FILE, read through the CPU's own
  memory decode so chip-RAM mirrors resolve. Combined with AFTER, this
  captures bitplane or sample data exactly as displayed at a moment in
  time for offline analysis.

`COPPERLINE_DBG_COPPER=auto | ADDR[:COUNT]`
: One-shot Copper-list disassembly the first time the debugger activates.
  `auto` reads the live COP1LC; an explicit address disassembles from
  there. COUNT defaults to 256 instructions (`auto:64` works too).

`COPPERLINE_DBG_AFTER=SECS` / `COPPERLINE_DBG_UNTIL=SECS`
: Activity window in emulated seconds. Outside the window the debugger is
  inert, which keeps traces focused and runs fast: combined with
  determinism, you can binary-search a failure in time.

`COPPERLINE_DBG_MAXHITS=N`
: Stop reporting after N hits (default 200).

`COPPERLINE_DBG_SHOT=PREFIX`
: Save a PNG of the last completed frame on every hit, as
  `PREFIX-0000.png`, `PREFIX-0001.png`, ...

`COPPERLINE_DBG_EXPORT_PLANES=1`
: Export each bitplane and a composite colour-index image for every frame
  rendered inside the `AFTER`/`UNTIL` window, using the exact per-line
  plane words the renderer fetched -- a ground-truth view of each plane.
  `COPPERLINE_DBG_EXPORT_PLANES_DIR=DIR` sets the output directory.

`COPPERLINE_DBG_FRAMESTATE=1`
: Log the per-frame display state (one summary line per rendered frame)
  inside the `AFTER`/`UNTIL` window.

## Diagnostic knobs

Beyond the debugger, many subsystems have start-up diagnostic switches.
They are read through `src/envcfg.rs`; grep its call sites for the
authoritative list. The most useful ones:

| Variable | What it logs / does |
|---|---|
| `COPPERLINE_DIAG_SLOTMAP` | Per-colour-clock chip-bus owner map for a frame (`R`efresh, `B`itplane, `S`prite, `D`isk, `A`udio, `C`opper, b`L`itter, c`P`u, `.` idle); `COPPERLINE_DIAG_SLOTMAP_AT=SECS` picks the frame and `COPPERLINE_DIAG_SLOTMAP_RANGE=START:END` picks printed beam rows |
| `COPPERLINE_DIAG_BLT_SLOTS` | Blitter slot trace: one stderr line per blitter pipeline cycle (`BLTP frame vpos hpos TICK phase bus=0/1`), plus per-cck owner lines while a blit is in flight and `START`/`END` markers. Formatted for side-by-side diffing against a vAmiga build instrumented with the matching `VAMIGA_BLT_PROBE` hooks |
| `COPPERLINE_DIAG_IPL` | CPU cycles spent per interrupt level |
| `COPPERLINE_DIAG_PCSAMPLE` | Top-50 executed-PC histogram every 50 frames |
| `COPPERLINE_DIAG_PCHIST` | Full PC history (with `COPPERLINE_DIAG_PCHIST_START=SECS`) |
| `COPPERLINE_DIAG_COPLEN` | Copper list length (optionally at a given emulated time) |
| `COPPERLINE_DIAG_COP_WRITES` | Every Copper MOVE's landing colour clock (beam position, register, value), for cross-emulator write-landing comparison against vAmiga's `VAMIGA_COP_PROBE` trace |
| `COPPERLINE_DIAG_CPU_BUS` | CPU chip-bus access request/grant/end slots for fetch, chip/slow RAM, and custom space; optional `COPPERLINE_DIAG_CPU_BUS_ADDR=start:end[,start:end...]` filters by CPU-visible addresses including custom registers such as `0xdff01e` |
| `COPPERLINE_DIAG_CPU_READS` | CPU custom-register reads' granted chip-bus slot and returned value, plus the post-flush beam position; honors the same optional `COPPERLINE_DIAG_CPU_BUS_ADDR` filter as the bus trace |
| `COPPERLINE_DIAG_CPU_SYNC` | CPU-internal cycle trace at pre-access sync points and instruction-boundary catch-up; optional `COPPERLINE_DIAG_CPU_SYNC_PC=pc[,start:end...]` filters by instruction PC |
| `COPPERLINE_DIAG_CPU_WRITES` | Every CPU custom-register write's granted chip-bus slot and effect beam position (register, value), the CPU-side companion of `COPPERLINE_DIAG_COP_WRITES` for comparison against vAmiga's `VAMIGA_CPU_PROBE` trace |
| `COPPERLINE_DIAG_DISPLAY` | Display-register change log |
| `COPPERLINE_DIAG_CAPROW` | `=all`, `=V`, or `=START:END`: per-line bitplane capture state at DDF start, including DMACON, current and DDF-anchor BPLCON0, FMODE/DIW/DDF, effective fetch window, unit/period/quantum, words/row, modulos, and all BPLxPTs -- separates wrong-pointer from wrong-decode display bugs |
| `COPPERLINE_DIAG_PALETTE_ROW` | `=all`, `=V`, or `=START:END`: log beam-timed COLOR writes for selected beam lines, including source, framebuffer x, palette entry, LOCT, value, and BPLCON3; the setting is cached after first use |
| `COPPERLINE_DIAG_HAM_PIXELS` | `=BEAMY,X0,X1[,STEP]`: sample DMA playfield HAM pixels on one beam line, including framebuffer/native x, selected bitplane index, active/fetched state, HAM hold colour before/after, output latch, plane count, fetched width, BPLCON1 delays, DIW/DDF, and display window; pairs with `COPPERLINE_DBG_AFTER` / `COPPERLINE_DBG_UNTIL` and is cached after first use |
| `COPPERLINE_DIAG_MANUAL_BPL_PIXELS` | `=BEAMY,X0,X1[,STEP]`: sample CPU/Copper BPLDAT replay pixels on one beam line, including source x/native bit, selected index, HAM seed/output state, output latch, BPLCON0/BPLCON1, and display window; cached after first use |
| `COPPERLINE_DIAG_FRAME_PIXELS` | `=BEAMY,X0,X1[,STEP]`: sample final framebuffer pixels after playfield, manual BPLDAT replay, sprites, and final blanking so post-decode overwrites can be isolated; cached after first use |
| `COPPERLINE_DIAG_SPRITES` | Sprite DMA fetch/render log |
| `COPPERLINE_DIAG_SPRCAP` | `=BEAMY` or `=all`: log every captured sprite DMA line (frame, channel, hstart, attach, FMODE width, data words) on one beam line or all of them; also logs SPRxPT writes and active stream retargets |
| `COPPERLINE_DIAG_MANUAL_SPRITES` | `=BEAMY` or `=all`: log manually replayed sprite intervals, sprite register writes with CPU/Copper source, BPLCON3/BPLCON4/FMODE/COLOR timing, sprite pointer alignment, and held wide-sprite words |
| `COPPERLINE_DIAG_SPRITE_PIXELS` | `=BEAMY[,STEP]`: sample non-transparent sprite pixels on one beam line, including sprite or attached-pair index, palette entry, sprite RGB, final framebuffer RGB, playfield mask, priority/display gates, DIW, BPLCON2, BPLCON3, and BPLCON4; STEP defaults to 32 framebuffer pixels |
| `COPPERLINE_EXP_NO_SPRITE_RENDER` | With `--features internal-diagnostics`, skip sprite rendering in full-frame output while leaving playfield/manual-BPL rendering active; useful for isolating sprite-owned pixels in screenshots |
| `COPPERLINE_DIAG_BLITREGS` | `=START:END` (emulated seconds): log the full blitter register set at every blit start (classic BLTSIZE and ECS BLTSIZH); pairs with `COPPERLINE_DUMP_BLITMEM` snapshots for offline blit verification |
| `COPPERLINE_TRACE_BLITTER` | Path to a JSONL trace of blitter starts, forced finishes, DMACONR polls, and completion IRQ latches; start records include minterm/control registers, DMA/display context, FMODE, and all eight bitplane pointers |
| `COPPERLINE_DIAG_DISK` | Disk DMA state changes (DSKLEN writes) |
| `COPPERLINE_DIAG_AUDIO_NOTES` | Paula channel note on/off events |
| `COPPERLINE_DIAG_CRASH` | CPU empty-RAM execution and low-memory blitter write context |
| `COPPERLINE_DIAG_GAYLE` / `COPPERLINE_DIAG_CDTV` | Gayle IDE / CDTV controller traffic |
| `COPPERLINE_DIAG_A2091` | A2091 SCSI board register traffic (DMAC + WD33C93 accesses; the trace that brings up boot-ROM issues) |
| `COPPERLINE_DIAG_CURSOR` | On every mouse-button press, log the raw host cursor position, the window's scale factor and inner size, the texture supersample factor, the `window_pos_to_pixel` result, and which region (status bar / display / none) the click resolved to; for diagnosing mouse capture on DPI scale changes or mixed-scale monitors |
| `COPPERLINE_DUMP_BLITMEM=START:END:LO:HI` | Dump chip RAM `[LO,HI)` on every BLTSIZE write between START and END emulated seconds; output goes to `$TMPDIR/copperline-blitdump` unless `COPPERLINE_DUMP_BLITMEM_DIR` is set |
| `COPPERLINE_DUMP_BUS_ACCOUNTING` | Per-frame chip-bus slot accounting |
| `COPPERLINE_DUMP_RENDER_META[_VERBOSE]` | Renderer event/fetch metadata |

Timing-model knobs that pair well with the debugger:

- `COPPERLINE_IRQ_LATENCY_CCK=N` -- override the modelled Paula INTREQ-to-
  IPL-pin pipe (default 5 colour clocks; `0` also disables the 68000
  boundary-sampling delay, delivering interrupts immediately).
- `COPPERLINE_DBG_AFTER=SECS` / `COPPERLINE_DBG_UNTIL=SECS` -- bound
  debugger and renderer diagnostics to an emulated-time window. Renderer
  diagnostics parse these bounds once when their diagnostic option is first
  used.
- `COPPERLINE_HCENTER=0` -- disable presentation recentring when debugging
  display alignment.
- `COPPERLINE_SHOT_RAW=1` -- save screenshots and frame dumps as the raw
  716x570 woven framebuffer instead of the TV PNG aperture (692x540 for
  standard PAL fields) or full-overscan presentation scale. Per-scanline
  forensics (which exact framebuffer row carries an artifact) need the raw
  field.
- `COPPERLINE_OVERSCAN=full|tv` -- override the configured overscan mask.
- `COPPERLINE_PIXEL_ASPECT=tv|square` -- override `[display] pixel_aspect`
  for one run: `tv` is the 4:3 CRT presentation, `square` maps one host
  row per woven scanline (a standard PAL screen becomes an exact 2x2 of
  its bitmap in screenshots and the window).
- `COPPERLINE_DEINTERLACE=0` -- disable the motion-adaptive deinterlacer.
- `COPPERLINE_PHOSPHOR=0.0..0.95` -- CRT phosphor persistence for one run
  (overrides `[display] phosphor`).
- `COPPERLINE_THREADED_RENDER=0` -- force the synchronous renderer instead
  of the default render worker when bisecting presentation or capture
  issues.
- `COPPERLINE_REAL_PACING_BUDGET=cycles|instructions` and
  `COPPERLINE_REAL_CPU_CPI=N` -- pacing-budget overrides (see
  [](../internals/timing)).
- `COPPERLINE_AUDIO_PROFILE=1` / `COPPERLINE_REAL_PACING_PROFILE=1` --
  one-line-per-second performance counters (see [](../internals/peripherals)).

Behavior-changing A/B switches such as `COPPERLINE_NO_*`,
`COPPERLINE_EXP_*`, `COPPERLINE_DISK_SPEED_DIV`, and
`COPPERLINE_DBG_EXTCCK` (external-access cost in hundredths of a colour
clock, default 200 = 2.00 cck) are compiled only with the
`internal-diagnostics` feature. Normal builds ignore them so release runs
stay hardware-derived and reproducible.

For finding registers a guest probes that Copperline does not decode,
see the `[debug] log_unmapped` config key in
[Configuration](../guide/configuration.md) -- it is a config setting
rather than an environment variable because it pairs with a specific
machine setup.

## A worked example

A frame-pacing investigation is a template for using these tools
together:

1. Reproduce headlessly: `--screenshot-after` at a known-bad timestamp.
2. Find the guest's frame pacing: `COPPERLINE_DIAG_PCSAMPLE` to locate
   the hot loop, then `COPPERLINE_DBG_BREAK` on the loop head with
   `COPPERLINE_DBG_DUMP` of its counters.
3. Narrow in time with `COPPERLINE_DBG_AFTER`/`UNTIL`, watch the
   interesting word with `COPPERLINE_DBG_WATCH`.
4. Check the bus: `COPPERLINE_DIAG_SLOTMAP_AT` to see who owned every
   colour clock of the suspect frame, with `COPPERLINE_DIAG_SLOTMAP_RANGE`
   when only specific beam rows matter.
5. Compare against real hardware with the `timing-test/` disk when the
   question is "is this operation too fast/slow".

For interactive sessions, the same instruction trace is available at
runtime without environment variables: the [console](console)'s
`TRACE START [PATH]` / `TRACE STOP`.

## CPU and AmigaOS probe notes

CPU-visible behavior is best pinned with a tiny executed ROM program through
`M68kMachine` and its `CpuBus`, rather than by directly mutating `Bus` state.
Useful assertions include the final `PC`, `SR`, `A7`, exception stack frame,
and Paula/CIA interrupt latches. Keep the external address mask explicit:
68000, 68010, and 68EC020 use 24 bits; 68020, 68030, 68040, and 68060 use
32 bits.

The following addresses were observed while probing one Kickstart 2.05 ROM.
The ExecBase field offsets are ABI layout; the absolute ROM and chip-RAM
addresses are image/run-specific landmarks, useful for reproducing old traces
but not emulator behavior to special-case:

| Field or routine | Observed location |
| --- | --- |
| `ThisTask` | ExecBase `+$114` |
| `ResModules` | ExecBase `+$12C` |
| `LibList` head | ExecBase `+$17A` |
| five soft-interrupt list heads | ExecBase `+$1B2..+$1F2` |
| `OpenLibrary` (LVO -552) | ROM `$F819AE` |
| `InitCode` (LVO -72) | ROM `$F80F4C` |
| `InitResident` (LVO -102) | ROM `$F80F86` |
| `graphics.library` resident header | ROM `$FA8C28` |
| `graphics.library` base in that run | chip RAM around `$2A50` |
| `expansion.library` base in that run | chip RAM around `$A44` |

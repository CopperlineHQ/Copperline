# Debugging workflows

The other chapters document each tool on its own; this one shows how
they combine. Every walkthrough below is a real investigation shape --
the same ones Copperline's own regressions were hunted with -- written
against the standard tool set: the [debugger window](window), the
[console](console), the frame analyzer, [reverse execution](reverse),
[headless knobs](headless), and the [GDB stub](gdb).

All of these start the same way: get the machine *near* the problem
cheaply, then investigate precisely. Save states are the lever --
snapshot once just before the scene (`--save-state-after SECS out.state`
headlessly, or from the menu), then every later experiment starts from
`--load-state` in seconds instead of re-emulating minutes.

## A sprite vanishes

The logo disappears mid-demo, or an in-game object flickers out.
Sprites fail for a handful of reasons: sprite DMA got turned off, the
sprite pointers were repointed somewhere wrong, the control words
disarmed it, or the display window moved off it.

1. Pause just before the vanish (reverse-step back if you overshot:
   `RFRAME` in the console rewinds a whole frame).
2. Debugger **Video** tab: the sprite viewer decodes all eight channels
   -- SPRxPOS/CTL positions, DMA line counts, and a thumbnail of what
   each channel fetched this frame. A sprite that is armed but empty
   fetched nothing: suspect DMA. A sprite with data but a wrong
   position: suspect the copper list that positions it.
3. If DMA is the suspect: console `RWATCH DMACON` (or the Break tab's
   register watch) and run. When it fires, the console names the writer
   -- CPU instruction, Copper, or blitter -- with its address. A game
   clearing `SPREN` deliberately looks exactly like this.
4. If the pointers are the suspect: `RWATCH SPR0PTH` catches every
   repoint, and the **Copper** tab shows the list that does it each
   frame.
5. Layer isolation confirms any theory instantly: the Video tab's
   plane/sprite masks re-render the paused frame with channels removed,
   so "is that object sprite 4 or a playfield trick?" is one click.

## The picture is torn or the copper list is trashed

A horizontal band of garbage, colours changing at the wrong line, or a
display that falls apart after some event.

1. Open the **Frame Analyzer** and enable the picture underlay (`U`):
   the captured DMA heatmap draws over the actual frame, so a band of
   garbage lines up visually with whatever DMA was (or was not)
   happening there.
2. Hover the analyzer at the first bad line: it decodes the custom
   register writes near the beam position. A `COLORxx` write that
   arrives a line late, or a missing `BPLxPTH` refresh, is visible
   directly.
3. Set a beam trap at the first bad line (`BTRAP 145` in the console,
   or the analyzer's To-slot). The machine halts with the beam exactly
   there; the **Copper** tab now shows what the Copper is executing at
   that moment, live.
4. `CSTEP` single-steps Copper instructions from there. A WAIT with a
   wrong comparator, or a list that ran past its terminating WAIT into
   garbage, shows up within a few steps. `CBREAK ADDR` breaks when the
   Copper reaches a specific instruction on any frame.
5. If the list itself is corrupt in memory: `WRITER ADDR` reverse-scans
   execution to find the last instruction that wrote the corrupted
   word. That answers "who trashed my copper list" in one command --
   typically a blitter destination running long, which the attribution
   names as such.

## Something writes where it should not

Memory corruption generally: a variable, a bitmap, or OS structure gets
stomped.

1. `WATCH ADDR` sets an attributed watchpoint. Any CPU, blitter, or
   disk-DMA write there halts the machine and names the writer. Filter
   to one source with `WATCH ADDR BLITTER` when the CPU legitimately
   touches the address constantly.
2. If the corruption already happened: `WRITER ADDR` uses the reverse
   engine to find the most recent writer without re-running anything by
   hand.
3. For "when did this go bad" rather than "who": binary-search with
   save states -- load, `MEM ADDR`, and the deterministic core
   guarantees the same answer on every replay.

## Crash triage: guru meditation

The machine gurus, or worse, freezes silently.

1. Arm `CATCHALERT`. When exec's `Alert()` runs, the machine halts with
   D7 holding the alert code -- before the flashing box is drawn, with
   the faulting context still warm.
2. `GURU` decodes it: deadend flag, subsystem, cause -- and for CPU
   traps the vector name (`address error`, `illegal instruction`).
3. `STACK` and `HISTORY` show how it got there: the call-stack scan and
   the disassembled ring of recently retired PCs. `RSTEP` walks
   backwards from the alert into the faulting code with full state.
4. A **double fault** (bus/address error during exception processing)
   cannot guru -- the CPU halts. Copperline surfaces it on screen, in
   the console, and on the Break tab automatically; `HISTORY` is the
   main tool from there.
5. For OS-level context: `TASKS` shows what was scheduled, `CATCHTASK
   NAME` stops when a suspect process next gets the CPU, and `SEGMENTS`
   maps a process's loaded hunks so addresses in `HISTORY` can be
   attributed to a program rather than "somewhere in RAM".

## Find the lives counter (memory hunting)

The trainer-making workflow, also the fastest way to locate any game
variable you can influence.

1. `HUNT START` snapshots all writable RAM (chip, slow, and Zorro
   boards). Word width is the default; `HUNT START B` hunts bytes.
2. You have three lives: `HUNT EQ 3`. Thousands of candidates.
3. Lose a life, pause, `HUNT EQ 2`. The intersection is usually a
   handful of addresses; two or three rounds isolate one. `HUNT SAME` /
   `HUNT DIFF` filter against the previous snapshot when you cannot
   name the value ("it changed", "it did not change").
4. `HUNT LIST` shows the survivors with live values.
5. From there: `WATCH ADDR` answers "what code decrements this"
   (the death routine), and `POKE ADDR 9` tests the theory.

## Source-level debugging of your own program

For programs you build yourself, the GDB stub relocates symbols to the
addresses `LoadSeg()` chose -- automatically at attach for a program
already running (`qOffsets`), and via `monitor loadseg-break` plus a
reattach for one the guest loads later. How much detail you get depends
on the symbol file: a `-g` hunk executable carries function-level
symbols, while source lines, `next`, and `print` need an
ELF-with-DWARF sibling of the binary. The walkthrough lives in the
[GDB chapter](gdb), "Source-Level Debugging of Amiga Programs".

## Making it reproducible

Any of the above is dramatically easier when the failure replays
identically every run:

- **Record the session**: `--record-input session.clscript`
  (`Cmd+Shift+R` live) captures input on emulated time; `--script
  session.clscript` replays it byte-identically.
- **Headless repro**: once scripted, `--screenshot-after` /
  `--dump-frames` plus the `COPPERLINE_DBG_*` knobs ([headless
  chapter](headless)) turn the bug into a shell command -- the form a
  regression test wants.
- **Trace the suspect window**: console `TRACE START PATH` writes every
  retired PC with beam positions to a file; diff two traces (one good
  run, one bad) to find the first divergence.

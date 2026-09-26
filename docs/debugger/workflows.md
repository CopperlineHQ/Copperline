# Debugging workflows

This chapter shows common debugging workflows that combine the [debugger window](window),
[console](console), Frame Analyzer, [reverse execution](reverse), [headless options](headless),
[GDB remote stub](gdb), and the [A/B divergence finder](diverge).

## Diagnosing sprite rendering issues

If an on-screen sprite disappears, flickers, or displays incorrect graphics:

1. **Pause before the failure point:** Step backward if necessary (`RFRAME` in the
   console steps back one frame).
2. **Inspect the Video tab:** It decodes each sprite's position (`SPRxPOS`,
   `SPRxCTL`) and DMA line count, with a thumbnail of the fetched graphics.
   - If a sprite is armed but fetches zero DMA lines, check whether sprite DMA is disabled in `DMACON`.
   - If the sprite contains valid graphics data but renders at the wrong coordinates, inspect
     the Copper list positioning instructions.
3. **Trace register writes:** In the console, run `RWATCH DMACON` to find when and
   where sprite DMA was changed.
4. **Isolate layers:** On the **Video** tab, toggle individual sprites or bitplanes
   to see which one draws a given part of the screen.

## Investigating Copper list corruption and visual artifacts

If the display shows raster splits on the wrong scanlines or corrupted palettes:

1. **Open the Frame Analyzer:** Press `U` to draw the rendered picture beneath
   the chip-bus slot heatmap.
2. **Inspect scanline writes:** Select a slot on the affected scanline to decode the
   custom register writes (`COLORxx`, `BPLxPTH`) made near that beam position.
3. **Set a beam trap:** Set a trap at the problem scanline (e.g. `BTRAP 145` in the
   console, or select the slot and use **Run to beam** in the Frame Analyzer).
   Execution halts when the beam reaches that line.
4. **Single-step the Copper:** Switch to the **Copper** tab and use **CStep** (`C`) to
   execute Copper instructions one at a time across `WAIT` boundaries.
5. **Find a memory change:** `WRITER ADDR` replays retained snapshots and moves back
   to the last observed change of that word. Its PC identifies the CPU step
   around the change; use `WATCH ADDR CPU` or `WATCH ADDR BLITTER` to distinguish
   the writer on a forward run.

## Identifying memory corruption

When tracking down overwritten data buffers or corrupted OS structures:

1. **Set a memory watchpoint:** In the console, run `WATCH ADDR` (or `WATCH ADDR BLITTER`
   to catch only blitter writes).
2. **Reverse lookup:** If memory has already been corrupted, run `WRITER ADDR` to replay
   the snapshot ring and find the instruction responsible for the write.
3. **Bisection with save states:** Use `--save-state-after` and `--load-state` to narrow
   down when the corruption happened.

## Diagnosing Guru Meditation crashes and unhandled exceptions

1. **Catch system alerts:** In the console, enter `CATCHALERT`. Emulation halts
   when `exec.library/Alert()` is called, before the alert screen is drawn.
2. **Decode alert codes:** Run `GURU` to translate the alert code in register `D7` into a
   description.
3. **Inspect the call stack:** Use `STACK` and `HISTORY` in the console to inspect recent
   subroutine calls and retired program counters. Step backward with `RSTEP` to inspect
   the state before the crash.
4. **Inspect Exec tasks:** Run `TASKS` to view the ready and waiting task queues, or
   `TASK <name>` to inspect a task's stack pointers and signal allocations.

## Locating in-game variables (Memory search / Trainer workflow)

1. **Start a search:** In the console, enter `HUNT START` (or `HUNT START B` for a byte search).
2. **Filter by value:** If searching for a lives counter starting at 3, run `HUNT EQ 3`.
3. **Update and narrow:** Change the in-game value (e.g. lose a life) and run `HUNT EQ 2`.
4. **Review candidates:** Run `HUNT LIST` to view the matching memory addresses.
5. **Set watchpoints or modify:** Attach a watchpoint (`WATCH ADDR`) or change the value
   (`POKE ADDR 9` writes a word; use `POKE.B ADDR 9` after a byte search).

## Logic analyzer waveform capture

To investigate fine-grained DMA and bus arbitration timing:

```text
WAVE START glitch.vcd beam=100 2f
```

This arms a capture that triggers at scanline 100 and records two frames of
chip-bus activity. Open the resulting `.vcd` file in GTKWave to inspect the exact
cycle-by-cycle interleaving of the CPU, Copper, blitter, and DMA channels.

## Bisecting an emulator regression between two builds

When a title that worked in one build misbehaves in another, find the first
emulated state that differs before reading any code:

1. **Pin the scene:** Save a state just before the problem with the build
   that still works (`--save-state-after SECS before.clstate`) and record
   the input that reaches it (`--record-input inputs.clscript`), so both
   sides start from the same snapshot and see the same input.
2. **Run both builds in lockstep:**

   ```sh
   copperline-ctl diverge --a ./good/copperline --b ./bad/copperline \
       --until 130 --memory all --screenshots /tmp/shots -- \
       --factory --config game.toml --noaudio \
       --load-state before.clstate --script inputs.clscript
   ```

   The report names the first differing frame, then the first instruction
   inside it at which the registers, the colour clock or RAM differ, with
   the beam position and, for memory, the first differing byte. A
   **DMA-only** verdict (the picture differs while the CPU state never
   does) points at the chipset's fetch, Copper, blitter or output path
   rather than the CPU core; a **timing** verdict (same instruction,
   different colour clock) points at bus arbitration or instruction
   timing.
3. **Inspect the point on either side:** Open the same state in the
   [debugger window](window) or over the [control protocol](control),
   `run_until {"frame": F}` to the reported frame, then `step` to the
   reported instruction and use `custom.writer`, `frame.slots` or the
   Frame Analyzer to see what the two builds did differently there.
4. **Bisect the change:** Repeat with intermediate builds (`git bisect
   run` around the command; exit status 1 means diverged) until the
   commit that introduced the difference is found. The same command with
   `--config-a`/`--config-b` instead of two binaries checks whether a
   configuration knob changes behaviour it should not.

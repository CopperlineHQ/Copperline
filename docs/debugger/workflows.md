# Debugging workflows

This chapter illustrates common debugging workflows combining the [debugger window](window),
[console](console), Frame Analyzer, [reverse execution](reverse), [headless options](headless),
and [GDB remote stub](gdb).

## Diagnosing sprite rendering issues

If an on-screen sprite disappears, flickers, or displays incorrect graphics:

1. **Pause before the failure point:** Reverse-step if necessary (`RFRAME` in the console
   steps backward by one frame).
2. **Inspect the Video tab:** The sprite viewer decodes positions (`SPRxPOS`, `SPRxCTL`),
   DMA line counts, and fetched graphics.
   - If a sprite is armed but fetches zero DMA lines, check whether sprite DMA is disabled in `DMACON`.
   - If the sprite contains valid graphics data but renders at the wrong coordinates, inspect
     the Copper list positioning instructions.
3. **Trace register writes:** In the console, execute `RWATCH DMACON` to verify when and
   where sprite DMA was modified.
4. **Isolate layers:** In the **Video** tab, toggle individual sprite channels or bitplane
   layers to verify which subsystem is drawing specific screen elements.

## Investigating Copper list corruption and visual artifacts

If the display exhibits raster splits at incorrect scanlines or corrupted palettes:

1. **Open the Frame Analyzer:** Press `U` to enable the rendered video underlay beneath
   the chip-bus slot heatmap.
2. **Inspect scanline writes:** Hover over the affected scanline to decode custom register
   writes (`COLORxx`, `BPLxPTH`) executed near that beam position.
3. **Set a beam trap:** Set a trap at the problem scanline (e.g. `BTRAP 145` in the console
   or **To slot** in the Frame Analyzer). Execution halts when the raster beam reaches that line.
4. **Single-step the Copper:** Switch to the **Copper** tab and use `CStep` (`C`) to execute
   Copper instructions sequentially across `WAIT` boundaries.
5. **Find a memory change:** `WRITER ADDR` replays retained snapshots and moves back
   to the last observed change of that word. Its PC identifies the CPU step
   around the change; use `WATCH ADDR CPU` or `WATCH ADDR BLITTER` to distinguish
   the writer on a forward run.

## Identifying memory corruption

When tracking down overwritten data buffers or corrupted OS structures:

1. **Set a memory watchpoint:** In the console, execute `WATCH ADDR` (or `WATCH ADDR BLITTER`
   if isolating Blitter writes).
2. **Reverse lookup:** If memory has already been corrupted, execute `WRITER ADDR` to query
   the snapshot ring and find the instruction responsible for the write.
3. **Bisection with save states:** Use `--save-state-after` and `--load-state` to narrow down
   the exact timeframe when corruption occurred.

## Diagnosing Guru Meditation crashes and unhandled exceptions

1. **Catch system alerts:** In the console, enter `CATCHALERT`. Emulation halts immediately
   when `exec.library/Alert()` is called before the alert screen renders.
2. **Decode alert codes:** Run `GURU` to translate the alert code in register `D7` into a
   descriptive error message.
3. **Inspect the call stack:** Use `STACK` and `HISTORY` in the console to inspect recent
   subroutine calls and retired program counters. Step backward using `RSTEP` to inspect
   state prior to the crash.
4. **Inspect Exec tasks:** Run `TASKS` to view scheduled and waiting task queues, or `TASK <name>`
   to inspect task stack pointers and signal allocations.

## Locating in-game variables (Memory search / Trainer workflow)

1. **Initialize search:** In the console, enter `HUNT START` (or `HUNT START B` for byte search).
2. **Filter by value:** If searching for a lives counter starting at 3, run `HUNT EQ 3`.
3. **Update and narrow:** Change the in-game value (e.g. lose a life) and run `HUNT EQ 2`.
4. **Review candidates:** Run `HUNT LIST` to view matching memory addresses.
5. **Set watchpoints or modify:** Attach a watchpoint (`WATCH ADDR`) or modify the value (`POKE ADDR 9`).

## Logic analyzer waveform capture

When investigating fine-grained DMA and bus arbitration timing issues:

```text
WAVE START glitch.vcd beam=100 2f
```

This arms a capture triggering at scanline 100 and records two frames of chip-bus
activity. The resulting `.vcd` file can be opened in GTKWave to inspect exact
cycle-by-cycle interleaving between CPU, Copper, Blitter, and DMA channels.

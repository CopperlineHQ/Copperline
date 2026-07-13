# The debugger window

Press `Cmd+B` on macOS or `Alt+B` on Linux/Windows (or pick **Debugger**
from the status-bar menu) to pause the machine and open the debugger
tool window alongside the emulated display. Closing it restores the pause
state from before it opened. The debugger, frame analyzer, and the
command-line [console](console) are independent tool windows, so all three
can stay open while you compare CPU/chipset state with the captured bus
trace.
Everything the debugger shows comes from
side-effect-free peeks -- inspecting memory or registers never disturbs the
emulated machine -- and stepping drives the same cycle-exact core as normal
execution.
While the machine runs, tool windows repaint at 20 Hz rather than every
emulated frame: repainting shares the emulation thread, and a per-frame
repaint costs enough to starve the audio output. Interaction (stepping,
clicks, breakpoint stops) always repaints immediately.

```{figure} ../images/ui-preview-debugger.png
:alt: The debugger window on the CPU tab
:width: 90%

The CPU tab: register file, live disassembly, and the transport controls.
```

## Tabs

**CPU** shows the PC and SR (with decoded supervisor/IPL/CCR flags), the
D0-D7/A0-A7 register file, a `recent` line with the last few retired PCs
(the console's `HISTORY` command shows the full ring, disassembled), and
a live 68000 disassembly that follows the PC, with the current
instruction highlighted. Type a hex address in the
`$` box and press Enter to *pin* the disassembly elsewhere; empty the box
and press Enter to follow the PC again.

**Chipset** decodes the live custom-chip state bit by bit: the beam
position and frame counter, DMACON / INTENA / INTREQ with bit names,
Copper state (COP1LC/COP2LC/COPPC), the display window and fetch registers
(BPLCONx, DIWSTRT/STOP, DDFSTRT/STOP, modulos), the bitplane and sprite
pointers, and the full palette.

**Copper** is a debugger for the Copper itself. The header shows
COP1LC/COP2LC, the live Copper PC, and the execution state (running,
waiting -- with the decoded beam position it waits for -- or stopped). The
listing disassembles MOVE/WAIT/SKIP around the live PC (following it as
the Copper executes; a stopped Copper shows the head of the COP1 list),
with the current instruction highlighted and WAIT/SKIP positions resolved
to the same decimal `v`/`h` beam coordinates the Chipset tab and Frame
Analyzer use. Two controls sit above the listing:

- **CBreak +/-** toggles a *Copper breakpoint* at the hex address in the
  `$` box. The machine stops when the Copper's PC arrives at that address
  -- before the instruction there executes -- whether it got there by
  sequential fetch, a COPJMP, a CPU strobe write, or the vertical-blank
  restart. Breakpointed lines are marked `*`, and the Break tab lists them.
- **CStep** (`C`) runs until the Copper retires one instruction: a MOVE
  applied (or discarded by SKIP), or a WAIT/SKIP/COPJMP starting. Stepping
  onto a WAIT parks the Copper; the next CStep runs through the wait to
  the following instruction, so you can walk a raster effect
  instruction by instruction. The machine pauses at the next CPU
  instruction boundary, so the Copper can occasionally be a fetch further
  along -- the highlight always shows exactly where it stopped.

**Video** shows the display pipeline visually and hosts *layer
isolation*. The header decodes BPLCON0 (depth, resolution, HAM/DPF) and
the display/sprite DMA enables. Two toggle rows hide or show individual
bitplanes (1-8) and sprites (0-7) in the presented picture -- the video
analogue of the Audio tab's mutes, for answering "which layer is drawing
*that*" by elimination. Isolation is an output-only filter: collisions,
sprite/playfield priority, and everything the program can observe still
use the true data, so hiding layers can never perturb the emulation (a
sprite behind a hidden plane therefore stays occluded). Toggles re-render
the current frame immediately while paused, through the same
side-effect-free snapshot render the Frame Analyzer underlay uses.
Each sprite row decodes SPRxPOS/SPRxCTL (vstart-vstop, hstart, attach,
armed) with the number of DMA lines the sprite fetched this frame --
zero lines with plausible-looking pointers is the classic
copper-driven-sprite symptom -- and a thumbnail rendered from those
captured DMA words. Below, the palette grid shows all 32 entries (or the
full 256 on AGA) as swatches.

**Audio** decodes Paula's four audio channels. A header line shows DMACON
(master DMAEN and the per-channel AUDx enable bits) and ADKCON (the audio
attach bits for volume/period modulation). Each channel then shows its DMA
state-machine state (Off, Manual, ManualHold, StartPending, Running), whether
its DMA is enabled, its interrupt-pending flag, the CPU latches
(AUDxLC/LEN/PER/VOL), and the live playback state: the current pointer, words
remaining, the period accumulator, the output phase, and the sample currently
on the output. A `pending:` line appears when the channel is holding a
deferred DMA-disable (AUDxEN cleared mid-word), a deferred loop reload, a
manual AUDxDAT write, an outstanding DMA request, or a fetched-ahead word.
Step frames (**Frame**) to watch the state machine advance -- the Running
line is highlighted while a channel is actively streaming samples.

Each channel row also has a **Mute** button on the left and an oscilloscope on
the right; a fifth row at the bottom does the same for the **CD-DA** audio
stream (CDTV/CD32). The oscilloscope traces the channel's output level (the DAC
sample scaled by AUDxVOL), so both the waveform and its loudness are visible;
the CD scope traces the mixed CD stereo level. Clicking **Mute** silences that
channel (or the CD stream) in the host output while leaving its trace drawn
(greyed) so you can still see what it would play. Mutes are developer aids:
they change only the audio you hear, never the emulated Paula state, and are
not part of a save state.

**Memory** is a hex/ASCII dump, 256 bytes per page. Type a hex address in
the `$` box and press Enter to jump there; the `<` and `>` buttons page by
256 bytes, the mouse wheel and cursor keys scroll by 16-byte rows, and
PageUp/PageDown by whole pages. Four buttons sit above the dump:

- **Find** searches CPU-visible memory for the `$` box's hex byte pattern
  (`4E75`, or spaced pairs like `C0 FF EE`), starting past the previous
  hit and wrapping the 24-bit space once. The view jumps to the match.
- **Save...** writes the `$` box's `ADDR LEN` (hex) region to a file via
  a save dialog -- the GUI counterpart of `COPPERLINE_DBG_RAMDUMP`.
- **Writer?** reports the last instruction that wrote the word at the
  `$` box address: `$C09580: 0012->0034 by pc $C033C2 (frame 1234)`,
  replayed from the reverse-debug snapshot ring (so it sees every bus
  master, and costs a short replay). The result also lands on the Break
  tab until the next stop.
- **Bits** toggles a 1-bit-per-pixel bitplane view of the same memory:
  set bits are drawn light, one row per stride. Type a decimal stride in
  the `$` box when toggling to set the row width (default 40 bytes = a
  320-pixel plane), then wheel/cursor keys scroll by rows and `<`/`>`
  page by screenfuls. Point it at a bitplane pointer from the Chipset
  tab to eyeball graphics data directly -- misaligned strides show as
  the familiar diagonal shear.

**IO Map** is a browsable map of the whole custom bank ($DFF000-$DFF1FE):
every word offset with its hardware name and live value (write-only
registers show their last-written latch, `----` marks offsets with no
latch at all). Arrows and the mouse wheel move the selection, PageUp/Down
change page, and the `$` box jumps to an offset (`96`) or address
(`DFF096`). The pane below decodes the selected register's bits by name
-- DMACON's enables, INTENA/INTREQ sources, BPLCON0's mode flags and
plane count, ADKCON, CLXCON, BEAMCON0, FMODE, and the playfield
scroll/priority fields.

**Break** manages breakpoints and watchpoints (next section) and shows the
reason for the last stop.

```{figure} ../images/ui-preview-debugger-break.png
:alt: The Break tab
:width: 90%

The Break tab with a PC breakpoint, a memory watchpoint, and a
chipset-register watch armed.
```

## Breakpoints and watchpoints

On the Break tab, type an address into the `$` box and toggle any of:

- **Break** -- a PC breakpoint. The machine stops *before* the instruction
  at that address executes.
- **Watch** -- a memory word watchpoint. The machine stops when the word
  changes, whichever bus master wrote it, and the stop names the true
  writer: CPU stops report the writing instruction's PC, while blitter
  and disk-DMA writes are flagged at their write sites and report the
  source with the beam position (`Watch $060000: 0000->BEEF (blitter
  write, v44 h100)`). The console's `WATCH ADDR CPU|BLITTER|DISK` form
  additionally filters a watch to one writer -- "stop only when the
  blitter touches this" -- with other writers just moving the baseline.
  The current value is shown live in the list.
- **Reg** -- a chipset-register write watch. `96` and `DFF096` both mean
  DMACON. The machine stops on *every* write, CPU or Copper, and reports
  the writer and beam position.
- **Beam** -- a beam trap: the machine stops when the Agnus beam reaches a
  position. Unlike the other boxes it takes *decimal* `VPOS [HPOS]`
  (matching the `v=`/`h=` coordinates the Chipset tab and Frame Analyzer
  display); `HPOS` omitted means the start of the line. The check rides
  the beam advance itself, so it fires at exact colour-clock granularity
  and even while the CPU sits in `STOP`. A persistent beam trap re-fires
  every frame, which makes it a raster-line single-step: resume, and the
  machine stops at the same beam position of the next frame.
- **Catch** -- an exception catchpoint: the machine stops when the CPU
  enters the vector, at the handler's entry. The box takes `irq N`
  (interrupt level 1-7, e.g. `irq 3` for the vertical blank), `trap N`
  (TRAP #0-15), or `vec N` (any raw vector number: `vec 2` bus error,
  `vec 4` illegal instruction, `vec 8` privilege violation). Every entry
  path is seen -- traps, faults, and interrupts alike -- so `irq` catches
  fire on the exact dispatch, before the handler's first instruction.

**Clear all** removes everything. Breakpoints and watchpoints stay armed
when the window is closed: a hit pauses the machine, reopens the debugger
on the Break tab with the reason highlighted, and shows it as an on-screen
message. A breakpoint also shows as a `*` next to its line in the CPU
tab's disassembly.

### Conditional and counted breakpoints

The Break box accepts more than a bare address:

```
ADDR [LHS OP RHS] [IGN N]
```

- **Condition** `LHS OP RHS` -- the breakpoint only stops when it holds.
  Operands are registers (`D0`-`D7`, `A0`-`A7`, `PC`, `SR`), a memory word
  `M<hex>` (e.g. `MC00002`), or a bare hex immediate. A register name wins
  over hex, so write an immediate that looks like a register with a leading
  zero (`0D0`). Operators are `EQ NE LT GT LE GE` and `AND` (a bit test:
  true when `LHS & RHS` is non-zero).
- **Ignore count** `IGN N` -- skip the first `N` (hex) qualifying hits, then
  stop on the next one. The Break list shows the running `ign hits/N`.

Examples: `C033C2 D0 EQ 5` stops at `$C033C2` only when `D0` is 5;
`40 MC00002 AND 4000 IGN A` stops at `$40` once the word at `$C00002` has
bit `$4000` set, after ten earlier qualifying passes.

## Transport controls

| Control | Key | Effect |
|---|---|---|
| Run / Pause | `R` | Resume or pause the machine |
| Step | `S` | Execute exactly one instruction (into calls) |
| Step Over | `O` | Run a BSR/JSR/TRAP callee to completion, stopping after the call |
| Step Out | `U` | Run until the current subroutine returns to its caller |
| Frame | `F` | Run to the next video frame and re-render the display |
| Line | `L` | Run to the start of the next scanline (a one-shot beam trap) |
| Run to `$` | -- | Run until the PC reaches the address in the box |
| &lt; Frame | -- | Step one video frame *backward* |
| &lt; Step | -- | Step one instruction *backward* (see [](reverse)) |
| &lt; Run | -- | Run *backward* to the previous breakpoint hit |

The `R`/`S`/`O`/`U`/`F`/`L` keys work whenever the box is unfocused (while it
is focused they are text input). **Run to $**, **Step Over**, **Step Out**,
and **Line** are bounded by an instruction budget so a never-returning call
or never-reached address cannot wedge the UI; if the budget runs out, the
debugger stays paused. Step Out detects the return by the stack pointer
rising past its value at entry, so nested calls and interrupt handlers do
not end it early. If the CPU is sitting in a `STOP`, stepping fast-forwards
device time to the interrupt that wakes it, exactly as the live core would.

## Editing memory and registers

While paused you can patch state live from the `$` box and the **Poke** /
**Set Reg** button (the second transport row, on the Memory and CPU tabs):

- On the **Memory** tab, type `ADDR VALUE` (two hex words) and click
  **Poke** to write a 16-bit word. ROM and device windows are left
  untouched, exactly like the GDB memory-write path.
- On the **CPU** tab, type `REG VALUE` (e.g. `D0 1234`, `PC F80000`; `SP`
  aliases `A7`) and click **Set Reg**.

**Frame** is the tool for raster work: combined with the Chipset and Copper
tabs it lets you single-step a Copper effect one frame at a time and watch
the register state the beam will replay.

## Frame Analyzer pane

Pick **Frame Analyzer...** from the status-bar menu to pause the machine and
open the chip-bus frame analyzer in a separate tool window, leaving the
normal emulated display visible in the main window. It can remain open next
to the debugger window. The analyzer shows the whole captured Agnus beam
frame, not just the TV-presented display. The trace includes vertical and
horizontal overscan, blanking, and the visible display window.

```{figure} ../images/ui-preview-frame-analyzer.png
:alt: The Frame Analyzer with the picture underlay enabled
:width: 90%

The Frame Analyzer: chip-bus owner heatmap over the picture underlay, the
selected-scanline strip, and per-owner colour-clock counters.
```

The main heatmap is indexed by beam position: X is `hpos` colour clocks and Y
is `vpos` lines. Each cell records the chip-bus owner for that colour clock:
refresh, bitplane, sprite, disk, audio, Copper, blitter, CPU, or idle. The
white outline marks the framebuffer display area that Copperline captured for
presentation; the orange box is the programmed display window (DIW) and the
cyan verticals are the bitplane fetch bounds (DDFSTRT/DDFSTOP), decoded from
the frame-start registers (mid-frame changes appear as write markers).
Register-write markers show CPU, Copper, and interrupt-time custom-register
writes at their beam positions -- hover a slot to inspect them: the writes
within roughly one heatmap pixel of the pointer are decoded (writer,
register, value, and exact beam position) under the heatmap, and the same
readout follows the selected slot when the pointer is elsewhere.

Click or drag across the heatmap to select a beam slot. The cursor keys nudge
the selector one colour clock or line at a time. The lower strip expands that
selected scanline, so horizontal DMA contention in overscan is easier to
inspect. The right-hand counters summarize total colour clocks per owner, the
percentage of busy-blitter time that the blitter actually received, and which
owners consumed cycles while the blitter was waiting.

The pane has the same transport rhythm as the debugger:

| Control | Key | Effect |
|---|---|---|
| Run / Pause | `R` | Resume or pause while continuing to collect frame traces |
| Frame | `F` | Run exactly one frame and show the completed trace |
| To slot | `T` | Run until the beam reaches the selected slot |
| Picture underlay | `U` | Draw the rendered frame beneath the heatmap |
| Beam scrub | `B` | Show the picture only up to the selected slot |

Opening the pane starts a partial trace immediately; pressing **Frame**
captures a clean full frame. Closing it restores the run/pause state selected
inside the pane and disables the tracing hot path.

While the analyzer is open, every blit started in the traced frame is
recorded (control words, size, channel pointers, and the beam positions
where it started and finished). The console's `BLITS` command lists them,
and selecting a slot inside a blit's beam span names it on the
selected-slot line -- click the brown blitter run that is eating your
frame and see which blit it is.

Click a slot (or nudge the selection with the cursor keys) and press
**To slot** to run the machine until the beam reaches exactly that colour
clock -- a one-shot beam trap, reported like any other debugger stop. It
answers "what is the machine doing when the beam is *here*" directly from
the picture: select the glitch, run to it, and inspect the CPU/Copper
state at that moment.

Ticking **Picture underlay** draws the traced frame's rendered picture under
the heatmap, dimmed so the DMA colours stay readable: the picture shows
through idle slots and owned slots blend their owner colour over it. That
correlates bus activity spatially with the picture -- a bitplane fetch block
sits over the graphics it fetches, a Copper colour split lands on the raster
line where the picture changes. The underlay is re-rendered from the trace's
frame in beam coordinates (none of the presentation recentring or TV masking
is applied), so it lines up with the white display box exactly, and the extra
render reads a snapshot only -- it never perturbs the emulation.

**Beam scrub** turns the underlay into a beam-time scrubber: only what the
CRT had drawn by the time the beam reached the selected slot shows at
normal underlay brightness; everything past it ghosts at low brightness.
(Enabling it with the selection still at or before the display window's
top-left corner -- where nothing has been drawn yet -- would ghost the
whole picture, so the selection snaps to the end of the frame instead
and scrubbing backward peels the picture away.)
Because Copperline's renderer replays register writes at their recorded
beam positions, every pixel before the cursor is exactly what the display
carried at that instant -- drag the selection (or hold a cursor key) and
you watch the Copper build the frame line by line: palette splits snap in
at their WAIT lines, sprites appear when their DMA lines pass, screen
splits land where the pointers were rewritten. Combined with **To slot**,
"scrub to the artefact, then run the machine to that exact colour clock"
turns a what-is-the-beam-doing question into two clicks.

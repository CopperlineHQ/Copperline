# The Console

Pick **Console...** from the status-bar menu (or press
{kbd}`Cmd+K` / {kbd}`Alt+K`) to open the debugger console: a GDB-flavoured
command line in its own tool window. Like the debugger and Frame Analyzer
it is a separate host window, so all three can be visible at once --
console on one side driving execution, the debugger's disassembly and the
analyzer's beam view updating beside it.

```{figure} ../images/ui-preview-console.png
:alt: The debugger console
:width: 90%

A console session: a breakpoint set, hit, and inspected.
```

Opening the console pauses the machine (`RUN` resumes it); closing it
restores the previous run state. The prompt takes any printable text;
{kbd}`Enter` executes, {kbd}`Backspace` edits, {kbd}`Up` / {kbd}`Down`
walk the command history, and {kbd}`PageUp` / {kbd}`PageDown` or the
mouse wheel scroll the output. {kbd}`Cmd+V` (macOS) or {kbd}`Ctrl+V`
pastes the host clipboard -- a multi-line paste executes each complete
line in order and leaves the trailing fragment in the prompt, so a
saved command script can be replayed with one paste. Commands are
case-insensitive; addresses and values are hex (a leading `$` is fine),
beam positions (VPOS, HPOS) are decimal, matching the coordinates every
other debugger surface displays.

The console drives the same machinery as the debugger window and the GDB
stub, so everything set here shows there and vice versa: a `BREAK` lands
in the Break tab's list, a `BTRAP` fires with the same exact-colour-clock
semantics, and stops report in the console when it caused the run.

## Commands

Execution:

| Command | Effect |
|---|---|
| `RUN` | Resume the machine (also `GO`, `C`) |
| `PAUSE` | Pause and report where the machine is |
| `STEP [N]`, `S` | Execute N instructions (default 1) |
| `OVER`, `N` | Step over a BSR/JSR/TRAP call |
| `OUT` | Run until the current subroutine returns |
| `FRAME`, `F` | Run one video frame |
| `LINE` | Run to the start of the next scanline |
| `CSTEP` | Run until the Copper retires one instruction |
| `RUNTO ADDR` | Run until the PC reaches ADDR |
| `TOSLOT V [H]` | Run until the beam reaches the position |
| `RSTEP [N]` | Step backward (reverse debugging) |
| `RFRAME` | Step one frame backward |
| `RRUN` | Run backward to the previous breakpoint |

Every forward command ends by printing the stop reason (if a breakpoint,
watchpoint, trap, or catchpoint fired) and a status line: the PC with its
disassembled instruction, SR, beam position, and frame.

Stops (each toggles: repeat the command to remove):

| Command | Effect |
|---|---|
| `BREAK ADDR [COND] [IGN N]`, `B` | PC breakpoint, with the Break tab's condition grammar |
| `WATCH ADDR [CPU\|BLITTER\|DISK]`, `W` | Memory word watchpoint; the optional filter stops only on that writer |
| `RWATCH NAME\|OFF` | Custom-register write watch (`RWATCH DMACON`) |
| `BTRAP V [H]` | Beam trap (decimal position) |
| `CBREAK ADDR` | Copper breakpoint |
| `CATCH IRQ N \| TRAP N \| VEC N` | Exception catchpoint |
| `BREAKS` | List everything armed |
| `CLEARBREAKS` | Remove everything |

Inspection and modification:

| Command | Effect |
|---|---|
| `STATUS` | PC/SR/beam/frame summary |
| `REGS`, `R` | The register file |
| `MEM ADDR [BYTES]`, `M` | Hex/ASCII dump (default 64 bytes) |
| `DIS [ADDR] [N]`, `D` | Disassemble (default: at the PC) |
| `COPPER [PC\|ADDR] [N]` | Copper list around the live Copper PC |
| `CUSTOM` | Key custom registers |
| `BLITS` | Blits started in the traced frame (needs the Frame Analyzer open): control words, size, pointers, beam start/end |
| `FIND HEXBYTES [START]` | Search CPU-visible memory |
| `WRITER ADDR` | Last instruction that wrote ADDR (reverse history) |
| `HISTORY [N]`, `H` | The most recent retired PCs, disassembled (recorded while a debug window is open) |
| `STACK`, `BT` | Heuristic call-stack walk: stack longwords that look like return addresses after a JSR/BSR |
| `POKE ADDR VAL` | Write a memory word |
| `SETREG REG VAL` | Set a CPU register (`SETREG D0 1234`) |
| `TRACE START [PATH]` | Start a runtime instruction trace: one disassembled line per retired instruction with its beam position, no env var or restart needed (capped at a million lines) |
| `TRACE STOP` / `TRACE` | Stop the trace / report its progress |
| `HELP`, `CLEAR`, `CLOSE` | Console housekeeping |

Memory hunting (a trainer-style delta search over all writable RAM --
chip, slow, and Zorro RAM boards):

| Command | Effect |
|---|---|
| `HUNT START [B\|W]` | Snapshot RAM and begin a byte- or word-wide (default) hunt |
| `HUNT EQ\|NE\|LT\|GT VALUE` | Keep candidates whose *current* value compares to VALUE (hex) |
| `HUNT SAME` / `HUNT DIFF` | Keep candidates unchanged / changed since the last filter |
| `HUNT LIST [N]` | Show surviving candidates with live values |
| `HUNT OFF` | Forget the hunt |

The classic workflow: `HUNT START`, `HUNT EQ 3` while you have three
lives, lose one, `HUNT EQ 2` -- the survivor is your lives counter,
ready for `WATCH` (who decrements it?) or `POKE`.

AmigaOS introspection (read-only walks of exec's lists, safe at any
time -- if the OS is not up yet the command says so instead of printing
garbage):

| Command | Effect |
|---|---|
| `TASKS` | The scheduled task (`>`), then the ready and waiting lists, with priority, state, and name |
| `LIBS` | Opened libraries with versions (`graphics.library v40.10`) |
| `DEVS` | Devices with versions |
| `RESOURCES`, `PORTS` | The resource and message-port lists |
| `SEGMENTS` | The current process's loaded hunks (its CLI command's segment list when there is one), with the `add-symbol-file` line a source-level GDB session needs |
| `CATCHTASK NAME` | Stop when exec schedules a task whose name contains NAME (case-insensitive); `CATCHTASK` alone clears it |
| `CATCHALERT` | Break at exec's `Alert()` entry: fires on every guru/alert with D7 holding the code |
| `GURU [CODE]` | Decode an alert code (default: the current D7): deadend flag, subsystem, cause, CPU-trap alerts |

`CATCHTASK` is the tool for "wake me when my process actually runs": it
baselines on the currently scheduled task and fires on the next
reschedule to a matching one, reporting the task's name and address.

`CATCHALERT` plus `GURU` is the crash workflow: arm the catch, and when
the machine stops in `Alert()`, `GURU` translates D7 into words
(`DEADEND exec.library, no memory`). A CPU **double fault** -- a bus or
address error during exception processing, the condition even the OS
cannot report -- is always surfaced: the machine pauses with a
"CPU halted: double fault" message on screen, in the console, and on
the Break tab.

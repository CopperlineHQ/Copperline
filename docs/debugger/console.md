# The Console

Pick **Console...** from the status-bar menu (or press
`Cmd+K` / `Alt+K`) to open the debugger console: a GDB-flavoured
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
`Enter` executes, `Backspace` edits, `Up` / `Down`
walk the command history, and `PageUp` / `PageDown` or the
mouse wheel scroll the output. `Cmd+V` (macOS) or `Ctrl+V`
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
| `RUN` | Resume the machine (also `GO`, `CONTINUE`, `C`) |
| `PAUSE` | Pause and report where the machine is |
| `STEP [N]`, `S` | Execute N instructions (default 1) |
| `OVER`, `N` | Step over a BSR/JSR/TRAP call (also `NEXT`) |
| `OUT` | Run until the current subroutine returns (also `FINISH`) |
| `FRAME`, `F` | Run one video frame |
| `LINE` | Run to the start of the next scanline |
| `CSTEP` | Run until the Copper retires one instruction |
| `RUNTO ADDR` | Run until the PC reaches ADDR |
| `TOSLOT V [H]` | Run until the beam reaches the position |
| `RSTEP [N]`, `RS` | Step backward (reverse debugging) |
| `RFRAME` | Step one frame backward |
| `RRUN`, `RC` | Run backward to the previous breakpoint |

Every forward command ends by printing the stop reason (if a breakpoint,
watchpoint, trap, or catchpoint fired) and a status line: the PC with its
disassembled instruction, SR, beam position, and frame.

Stops (each toggles: repeat the command to remove):

| Command | Effect |
|---|---|
| `BREAK ADDR [COND] [IGN N]`, `B` | PC breakpoint, with the Break tab's condition grammar |
| `WATCH ADDR [CLASS] [PC=ADDR]`, `W` | Memory word watchpoint. `CLASS` narrows it to one accessor: `CPU`, `BLITTER`, `DISK`, `COPPER`, or a DMA channel (`BPL1`..`BPL8`, `SPR0`..`SPR7`, `AUD0`..`AUD3`) -- the DMA channels catch *reads* too, which a value compare cannot see. `PC=ADDR` stops only when that instruction made the access, and cannot be combined with a DMA class: only the CPU has an instruction behind an access |
| `RWATCH NAME\|OFF`, `RW` | Custom-register write watch (`RWATCH DMACON`) |
| `BTRAP V [H]` | Beam trap (decimal position) |
| `CBREAK ADDR` | Copper breakpoint |
| `CATCH IRQ N \| TRAP N \| VEC N` | Exception catchpoint |
| `BREAKS`, `INFO` | List everything armed |
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
| `WAVE START [PATH] [TRIGGER] [DURATION] [SIGNALS]` | Arm a trigger-based VCD "logic analyser" capture of chipset signals for GTKWave; the arguments are order-free and all optional (see [](waveform.md)) |
| `WAVE STOP` / `WAVE` | Finish the capture early / report its progress (`WAVEFORM` is an alias) |
| `HELP` (`?`), `CLEAR`, `CLOSE` (`QUIT`, `EXIT`) | Console housekeeping |

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
| `TASK [ADDR\|NAME]` | One task in full; no argument dumps `ExecBase->ThisTask` |
| `EXECBASE`, `EXEC` | ExecBase's own state: the scheduler counters and nesting counts, then what exec recorded about the machine |
| `MEMLIST`, `AVAIL` | Exec's memory list: free, largest chunk, and attributes per region |
| `LIBS`, `LIBRARIES` | Opened libraries with versions (`graphics.library v40.10`) |
| `DEVS`, `DEVICES` | Devices with versions |
| `RESOURCES`, `PORTS` | The resource and message-port lists |
| `SEGMENTS` | The current process's loaded hunks (its CLI command's segment list when there is one), with the `add-symbol-file` line a source-level GDB session needs |
| `CATCHTASK NAME` | Stop when exec schedules a task whose name contains NAME (case-insensitive); `CATCHTASK` alone clears it |
| `CATCHALERT` | Break at exec's `Alert()` entry: fires on every guru/alert with D7 holding the code |
| `GURU [CODE]` | Decode an alert code (default: the current D7): deadend flag, subsystem, cause, CPU-trap alerts |

`TASKS` prints `ThisTask` on the `>` line with that task's own state, so on
an idle machine it reads `wait` and appears again in the waiting list below.
Exec leaves `ThisTask` naming the task it dispatched last, so read the `>`
line as "last dispatched", not "running".

`EXECBASE` answers "what is the OS doing right now": `IdleCount` and
`DispCount` say whether exec is dispatching at all, `SysFlags` shows the
scheduler's pending attention bits, and `IDNestCnt`/`TDNestCnt` decode
into plain English -- `-1` means enabled, anything else is live
`Disable()`/`Forbid()` nesting, which is the usual reason a machine
"hangs" with the display still running. `AttnFlags` is the CPU and FPU
exec detected at boot, and the rest is what exec measured of the machine
(memory bounds, VBlank/PSU frequency, E-clock, its own supervisor stack)
plus the last alert code, decoded.

```text
> execbase
ExecBase $C00B00  exec.library v40.10  SoftVer 63
sched  IdleCount 34  DispCount 145  Quantum 4  Elapsed 2
sched  SysFlags $0000 (none)  AttnResched $0000
sched  IDNestCnt -1 (interrupts enabled)
sched  TDNestCnt -1 (task switching enabled)
cpu    AttnFlags $0000 (none)
task   ThisTask $C03580  exec.library
```

`TASK` dumps one `struct Task`: node type and priority, `tc_Flags`,
signal masks, trap and exception vectors, and stack bounds with the
bytes in use. For the *running* task the stack pointer is taken live
from the CPU (the user stack pointer, so a snapshot taken inside an
interrupt still measures the task's own stack) rather than from the
stale `tc_SPReg`. A `NT_PROCESS` continues into the DOS half: CLI
number and command name, `pr_StackSize`, `IoErr()`, the directory
locks, and the loaded hunks.

```text
> task input
task $C07192  input.device  (task)  pri 20  state wait
  flags  $00 (none)  IDNestCnt -1  TDNestCnt 0
  sigs   alloc $C000FFFF  wait $C0000000  recvd $00000000
  sigs   except $00000000  trap alloc $8000 able $0000
  vecs   trap $F83558/$000000  except $F83558/$000000
  vecs   switch $000000  launch $000000  userdata $000000
  stack  $C071F0-$C081F0 (4096 bytes)  sp $C0819E (SPReg), 82 used
```

The argument is an address (`TASK $C07192`) or a case-insensitive
substring of a task name; an ambiguous name lists the candidates instead
of guessing. `MEMLIST` walks the memory list exec allocates from, per
region: `mh_Free`, the largest free chunk and the chunk count (walked
from `mh_First`, so fragmentation is visible), and the `MEMF_*`
attributes.

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

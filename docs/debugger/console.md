# Debugger console

The debugger console provides an interactive command line interface in a
dedicated tool window (`Cmd+K` on macOS, `Alt+K` on Linux/Windows, or via
the status bar menu).

```{figure} ../images/ui-preview-console.png
:alt: The debugger console
:width: 90%

Debugger console interface with active breakpoint output.
```

Opening the console pauses emulation (`RUN` resumes execution). Closing the
console restores the previous execution state.

Input navigation:
- `Enter`: Execute command.
- `Up` / `Down`: Navigate command history.
- `PageUp` / `PageDown` or mouse wheel: Scroll console output buffer.
- `Cmd+V` (macOS) or `Ctrl+V` (Linux/Windows): Paste clipboard contents. Multi-line
  pastes execute each complete line sequentially.

Commands are case-insensitive. Addresses and data values use hexadecimal notation
(optional `$` or `0x` prefix). Raster beam coordinates (VPOS, HPOS) use decimal notation.

## Command reference

### Execution control

| Command | Description |
|---|---|
| `RUN` (or `GO`, `CONTINUE`, `C`) | Resume execution |
| `PAUSE` | Halt execution and print current CPU state |
| `STEP [N]` (or `S`) | Single-step `N` CPU instructions (default 1) |
| `OVER` (or `NEXT`, `N`) | Step over `BSR`, `JSR`, or `TRAP` subroutine calls |
| `OUT` (or `FINISH`) | Run until current subroutine returns (`RTS`/`RTE`/`RTR`) |
| `FRAME` (or `F`) | Advance execution by one video frame |
| `LINE` | Advance execution to start of next scanline |
| `CSTEP` | Advance execution by one Copper instruction |
| `RUNTO ADDR` | Run until PC reaches `ADDR` |
| `TOSLOT V [H]` | Run until beam reaches raster coordinates (VPOS, HPOS) |
| `RSTEP [N]` (or `RS`) | Reverse step `N` CPU instructions |
| `RFRAME` | Step one video frame backward |
| `RRUN` (or `RC`) | Run backward to previous breakpoint or watchpoint |

### Breakpoints and watchpoints

| Command | Description |
|---|---|
| `BREAK ADDR [COND] [IGN N]` (or `B`) | Set PC breakpoint (toggle) with optional condition and ignore count |
| `WATCH ADDR [CLASS] [PC=ADDR]` (or `W`) | Set memory watchpoint. `CLASS` can be `CPU`, `BLITTER`, `DISK`, `COPPER`, or DMA channels (`BPL1`..`BPL8`, `SPR0`..`SPR7`, `AUD0`..`AUD3`) |
| `RWATCH NAME\|OFFSET` (or `RW`) | Set custom register write watchpoint (e.g. `RWATCH DMACON`) |
| `BTRAP V [H]` | Set raster beam trap at decimal coordinates (VPOS, HPOS) |
| `CBREAK ADDR` | Set Copper instruction breakpoint at hex address |
| `CATCH IRQ N \| TRAP N \| VEC N` | Catch CPU exception vectors |
| `BREAKS` (or `INFO`) | List all active breakpoints and traps |
| `CLEARBREAKS` | Remove all active breakpoints and traps |

### Memory and state inspection

| Command | Description |
|---|---|
| `STATUS` | Print current PC, SR, beam position, and frame count |
| `REGS` (or `R`) | Display 68000 register file (`D0`-`D7`, `A0`-`A7`, `SR`, `PC`) |
| `MEM ADDR [BYTES]` (or `M`) | Hex/ASCII memory dump (default 64 bytes) |
| `DIS [ADDR] [N]` (or `D`) | Disassemble `N` instructions starting at `ADDR` (default: PC) |
| `COPPER [PC\|ADDR] [N]` | Disassemble Copper list |
| `CUSTOM` | Display custom chipset register summary |
| `BLITS` | List all blits started in the traced frame (with control words, size, pointers, and start/end beam positions; requires Frame Analyzer) |
| `FIND HEXBYTES [START]` | Search CPU-visible memory (RAM and ROM) for byte sequence |
| `WRITER ADDR` | Query last instruction that modified memory at `ADDR` |
| `HISTORY [N]` (or `H`) | Display recent instruction history |
| `STACK` (or `BT`) | Heuristic stack trace of recent return addresses |
| `POKE ADDR VAL` | Write word value to memory |
| `SETREG REG VAL` | Set CPU register value (e.g. `SETREG D0 1234`) |
| `TRACE START [PATH]` | Begin continuous instruction disassembly logging |
| `TRACE STOP` | Stop instruction trace logging |
| `WAVE START [ARGS]` | Arm VCD logic analyzer capture (see [](waveform.md)) |
| `WAVE STOP` | Stop VCD capture |
| `HELP [CMD]` (or `?`) | Display command summary or detailed command syntax |

### Memory delta search (Trainer / Value hunter)

| Command | Description |
|---|---|
| `HUNT START [B\|W]` | Snapshot memory and begin byte or word search (default: word) |
| `HUNT EQ\|NE\|LT\|GT VAL` | Filter candidate addresses by current value comparison |
| `HUNT SAME` / `HUNT DIFF` | Filter candidate addresses by unchanged / changed values |
| `HUNT LIST [N]` | Display surviving address candidates |
| `HUNT OFF` | Reset memory search state |

### AmigaOS and Exec introspection

| Command | Description |
|---|---|
| `TASKS` | List active, ready, and waiting Exec tasks |
| `TASK [ADDR\|NAME]` | Inspect detailed `Task` or `Process` structure |
| `EXECBASE` (or `EXEC`) | Display `ExecBase` scheduler counters and `Disable()`/`Forbid()` nesting |
| `MEMLIST` (or `AVAIL`) | Display free memory headers and memory fragmentation |
| `LIBS` (or `LIBRARIES`) | List active library bases and versions |
| `DEVS` (or `DEVICES`) | List active device drivers |
| `RESOURCES`, `PORTS` | List Exec resources and public message ports |
| `SEGMENTS` | Display loaded hunk segments for current CLI process |
| `CATCHTASK NAME` | Break when Exec schedules a task matching `NAME` |
| `CATCHALERT` | Break on `exec.library/Alert()` (Guru Meditation) calls |
| `GURU [CODE]` | Decode Guru alert numbers into human-readable descriptions |

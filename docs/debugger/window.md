# The debugger window

Press `Cmd+B` on macOS or `Alt+B` on Linux/Windows (or select **Debugger** from
the status bar menu) to pause emulation and open the debugger tool window.
Closing the window restores the previous execution state.

The debugger, Frame Analyzer, and [Console](console) operate in separate host
windows, allowing them to remain open simultaneously while inspecting CPU,
custom chipset, and bus activity. Emulation and register queries are non-intrusive
and do not alter hardware state.

```{figure} ../images/ui-preview-debugger.png
:alt: The debugger window on the CPU tab
:width: 90%

Debugger window: register file, live disassembly, and transport controls.
```

## Tabs

### CPU
Displays the 68000 register file (`D0`-`D7`, `A0`-`A7`), status register (`SR` with
decoded flags), program counter (`PC`), and live disassembly centered on the current
instruction. Enter a hexadecimal address in the address input box to inspect code
elsewhere in memory; clear the box to return to the active PC.

### Chipset
Decodes custom chipset registers in real time: raster beam position, frame counter,
`DMACON`, `INTENA`, `INTREQ`, Copper pointers (`COP1LC`, `COP2LC`, `COPPC`), display
window controls (`BPLCONx`, `DIWSTRT`, `DIWSTOP`, `DDFSTRT`, `DDFSTOP`), bitplane
and sprite pointers, and color palette entries.

### Copper
Dedicated Copper list inspector and disassembler. Shows `COP1LC`, `COP2LC`, active
Copper PC, and execution state (running, waiting, or halted).
- **CBreak +/-:** Toggles a Copper breakpoint at the specified hex address.
- **CStep (`C`):** Steps forward by one Copper instruction (advances through `WAIT`
  instructions to the subsequent instruction).

### Video
Displays the active display pipeline configuration and provides bitplane and
sprite layer isolation toggles:
- Toggle individual bitplanes (1-8) or sprites (0-7) to isolate visual elements
  without altering collision detection or emulation state.
- Decodes sprite registers (`SPRxPOS`, `SPRxCTL`), armed status, and DMA line counts.
- Displays full 32-color (OCS/ECS) or 256-color (AGA) palette grids.

### Audio
Decodes Paula audio channels (0-3) and expansion sound devices (CD-DA, MT-32,
Coppersynth, Toccata, MHI). Displays channel DMA state machine status, period,
volume, active buffer pointers, and real-time audio waveform scopes. Channels
can be muted individually.

### Memory
Hexadecimal and ASCII memory dump viewer (256 bytes per page).
- **Find:** Searches memory for specified byte sequences.
- **Save...:** Dumps address ranges to a file.
- **Writer?:** Queries the reverse execution snapshot ring to identify the instruction
  that last wrote to the specified address.
- **Bits:** Displays raw 1-bit-per-pixel bitplane visualizations with configurable
  stride.

### IO Map
Interactive memory map of custom chipset registers (`$DFF000` - `$DFF1FE`).
Selecting a register decodes its individual bitfields (e.g. `DMACON`, `INTENA`,
`BPLCON0`, `ADKCON`).

### Break
Manages active breakpoints, memory watchpoints, and custom register write traps.

```{figure} ../images/ui-preview-debugger-break.png
:alt: The Break tab
:width: 90%

Active PC breakpoints, memory watchpoints, and custom register traps.
```

### Wave
Interface for arming and configuring VCD logic analyzer waveform exports (see [](waveform.md)).

## Breakpoints, watchpoints, and traps

From the **Break** tab, enter a target address or identifier:

- **Break:** PC breakpoint. Execution halts before the instruction executes.
- **Watch:** Memory watchpoint. Halts when memory at the specified address is modified
  by CPU, Blitter, or DMA channels.
- **Reg:** Custom register write trap (e.g. `DMACON` or `96`). Halts whenever CPU
  or Copper writes to the register.
- **Beam:** Raster beam trap. Halts when the beam reaches the specified decimal `VPOS`
  (and optional `HPOS`).
- **Catch:** Exception vector trap (e.g. `irq 3`, `trap 0`, `vec 2`).

### Conditional and counted breakpoints

The breakpoint address field accepts conditional expressions and ignore counts:

```text
ADDR [LHS OP RHS] [IGN N]
```

- **Operands:** Registers (`D0`-`D7`, `A0`-`A7`, `PC`, `SR`), memory words (`M<hex>`, e.g. `MC00002`),
  or hex constants.
- **Operators:** `EQ`, `NE`, `LT`, `GT`, `LE`, `GE`, `AND` (bitwise test).
- **Ignore count (`IGN N`):** Skips the first `N` qualifying hits.

Examples:
- `C033C2 D0 EQ 5`: Breaks at `$C033C2` only when `D0` equals 5.
- `40 MC00002 AND 4000 IGN A`: Breaks at `$40` when bit `$4000` of word `$C00002` is set,
  after skipping 10 occurrences.

## Transport controls

| Button | Key | Action |
|---|---|---|
| **Run / Pause** | `R` | Resume or pause emulation |
| **Step** | `S` | Single-step one instruction |
| **Step Over** | `O` | Step over subroutine call (`BSR`/`JSR`/`TRAP`) |
| **Step Out** | `U` | Run until current subroutine returns |
| **Frame** | `F` | Advance emulation by one video frame |
| **Line** | `L` | Advance emulation to start of next scanline |
| **`< Frame`** | -- | Step backward one video frame |
| **`< Step`** | -- | Step backward one instruction (see [](reverse.md)) |
| **`< Run`** | -- | Run backward to preceding breakpoint |

(frame-analyzer-pane)=
## Frame Analyzer

Open the Frame Analyzer via the status bar menu to inspect chip-bus slot allocations
and memory access patterns.

```{figure} ../images/ui-preview-frame-analyzer.png
:alt: The Frame Analyzer
:width: 90%

Frame Analyzer: chip-bus owner heatmap overlaid on rendered frame.
```

### Beam tab
Displays a 2D heatmap indexed by raster beam coordinates (`X` = colour clock HPOS,
`Y` = scanline VPOS). Each cell indicates which subsystem owned the chip bus during
that colour clock cycle (CPU, Copper, Blitter, Bitplane, Sprite, Audio, Disk, Refresh, Idle).

- **Picture underlay (`U`):** Overlays the rendered video frame beneath the bus heatmap.
- **Beam scrub (`B`):** Progressively displays the frame up to the selected raster position.
- **To slot (`T`):** Advances execution until the beam reaches the selected colour clock.
- **CPU wait (`W`):** Switches the heatmap, scanline strip, legend and counters
  column to the CPU's side of the arbitration. Every colour clock the CPU asked
  for the chip bus and was denied is painted in the colour of what held it:
  bitplane, sprite, disk, audio, refresh, Copper, the blitter with BLTPRI clear
  (the "nice" hold before the slowdown counter yields), a hotter red for the
  blitter with BLTPRI set (its warm-up fence included, where the slot itself is
  idle), and grey for the 020+ chip port's own turnaround. Everything else is
  dimmed so the stolen cycles read against the DMA pattern that took them. The
  counters column shows the waited clocks as a share of the CPU's chip-bus
  time, the breakdown by denier and by access kind (read, which includes
  opcode prefetches; fetch, for immediate and extension words read outside
  the prefetch queue; write; custom register), and the instructions that
  waited longest ("Top stalled PCs": per
  instruction on the precise CPU loop, per batch under `[cpu] jit`). The
  selected-slot line names the denier whenever the selected slot was a CPU
  wait, in either view.
- **Stall gutter:** the narrow strip right of the heatmap is drawn in both
  views: one bar per line, as long as the share of that line's colour clocks
  the CPU spent waiting, in the colour of the line's dominant denier -- a
  profile of where the frame chokes the CPU.

```{figure} ../images/ui-preview-frame-analyzer-cpu-wait.png
:alt: The Frame Analyzer's CPU wait view
:width: 90%

Frame Analyzer Beam tab in the CPU wait view: denied slots lit by denier, the
stall gutter, and the wait breakdown with the top stalled PCs.
```

The console's `CPUWAIT` command prints the same summary for the traced frame,
and a [profile capture](profiling) exports it per frame.

Profile captures over the control protocol ([](profiling)) share bus tracing
with the Frame Analyzer: closing the pane does not interrupt an active capture,
and stopping a capture keeps an open pane recording.

(frame-analyzer-memory-tab)=
### Memory heatmap tab

```{figure} ../images/ui-preview-frame-analyzer-memory.png
:alt: The Frame Analyzer Memory tab
:width: 90%

Frame Analyzer Memory tab displaying address space activity.
```

The Memory tab displays a 256x256 grid representing memory activity across configured
RAM banks (Chip, Slow, Fast, Motherboard, Zorro II/III). Cells reflect recent read/write
activity by subsystem and fade over 32 frames.

The first four debug resources registered by the guest via the
[uaelib trap](../guide/run.md#uaelib-trap) appear as memory window presets
alongside hardware banks (the Resources tab lists all registered items).
Hovering or pinning a cell in the heatmap displays the name of any registered
resource mapped at that address.

### Resources tab

```{figure} ../images/ui-preview-frame-analyzer-resources.png
:alt: The Frame Analyzer Resources tab
:width: 90%

Frame Analyzer Resources tab previewing a registered bitmap.
```

The Resources tab inspects memory structures registered by guest software via
uaelib trap helpers (`debug_register_bitmap`, `debug_register_palette`,
`debug_register_copperlist`):

- **Bitmap**: Decodes planar or interleaved bitmaps up to 8 bitplanes using the
  first registered palette resource (or the active Denise palette if none is
  registered). Masked planes and HAM modes are rendered as indexed pixels;
  invalid geometry dimensions are clamped safely.
- **Palette**: Displays a swatch grid of 12-bit palette entries as rendered by
  Denise.
- **Copper list**: Disassembles initial instructions from the registered copper
  list address.

Selecting an entry decodes its contents dynamically from guest memory on every
repaint, allowing live visual inspection as the program executes. **Save...**
exports a selected bitmap or palette as PNG through that same decoder.

The same registry is accessible over the control protocol (`debug.resources`,
`debug.resource.export`)
and via the console's `DBGRES` command.

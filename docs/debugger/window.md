# The Debug workspace

Press `Cmd+B` on macOS or `Alt+B` on Linux/Windows (or select **Debugger** from
the status bar menu) to enter Debug layout in the main window. Opening the
first inspector pauses emulation; **Run** resumes it.

The Amiga display sits beside the debugger, Frame Analyzer, and [Console](console).
Drag the divider to give either side more space. The display retains its scaling,
CRT effects, and RTG support. Select an inspector from the tab strip; each retains
its state while another is visible. Opening another inspector preserves the
current run/pause state. Inspection reads do not acknowledge hardware registers
or consume emulated bus cycles. Stepping, register edits, and memory writes
change the machine as requested.

The **Play / Debug** switch in the title bar changes which layout the window
shows. **Play** restores the previous window size and hides the inspectors
without closing them: their selections, captures, and command history remain
available when you return to Debug, and anything they are capturing keeps
recording meanwhile. Switching layouts keeps the current run/pause state.
Closing the main window exits Copperline.

Click the display to give the Amiga keyboard and mouse input. `Cmd+G` / `Alt+G`
returns input to the debugger. While the debugger owns input, typing and
clipboard shortcuts operate its fields and never reach the Amiga. While the
Amiga owns input, ordinary keys, including `Esc`, go to the guest.

Host shortcuts also work while the debugger owns input. In a text field,
macOS `Cmd+A` and `Cmd+Z` retain their Select All and Undo/Redo behaviour.

(shared-inspector-window)=
## Inspectors

The GPU-rendered inspector UI is included in every desktop build. Monospace
readouts use Hack with a slashed zero to distinguish `0` from `O`. The font
is bundled; no system installation is needed.

```{figure} ../images/ui-preview-debugger-egui.png
:alt: Debugger with resizable register, disassembly, and memory panes
:width: 100%

Inspector-only preview of the CPU tab, rendered from the deterministic test machine.
```

On the CPU tab, drag the dividers to resize the register, disassembly, and memory
panes.
Text can be selected and copied, and the address/command field supports normal
text editing and paste. Register **Edit** buttons prepare a command in that
field; **Set Reg** applies it. Enter in the field pins the disassembly address
(an empty field follows PC), jumps to a memory address, or selects an IO register,
according to the active tab.

The CPU memory pane has its own address and page controls. The Memory tab
has a Goto address box, Find, Save, Writer, Bits, Poke, and in-place editing
of the dump; the other tabs retain their layer toggles, audio mutes,
breakpoints, and waveform controls. Scrollbars expose
content that does not fit the window. Transport keyboard shortcuts work while
not editing text.

Each inspector's tab reports what it is doing, so an inspector working in the
background can be told from one that was never opened:

| Tab | Meaning |
|---|---|
| Dimmed, no dot | Not open. Click it to open that inspector. |
| Filled, with a dot and a close box | Open. Its state and its capture are there whether or not it is the inspector on screen. |
| Blue | Open and on screen. |

The dot is filled while that inspector's capture is armed on the machine in
front of you, and hollow while it is merely open: showing the state it last
collected rather than collecting more. An inspector goes hollow when something
else takes its capture away, such as a profile run over the
[control protocol](control.md) finishing with the arming it adopted from the
pane.

The close box on a tab closes that inspector, as does the controller's back
button for the selected one; the remaining inspectors stay available in the
shared workspace, and the machine keeps running. Closing an inspector releases
what it was capturing and restores the execution state from before it was
opened; an explicit Run/Pause choice takes precedence. Closing the last one
returns to Play. `Esc` leaves a text field first. Outside a text field it
returns to Play, retaining the inspectors.

Select **Frame Analyzer** above the debugger tabs to inspect its **Beam**,
**Blits**, **Memory**, and **Resources** views. **Capture frame** records a
frame, and **Run** collects live frames. Switching between the debugger and
analyzer preserves their selections, capture data, and current run/pause state.
The analyzer stays armed while its view is hidden. Closing it releases captures
it owns; captures started through the control protocol continue independently.

```{figure} ../images/ui-preview-analyzer-egui.png
:alt: Frame Analyzer in the shared egui window with beam raster and bus counters
:width: 100%

Inspector-only preview of the Beam view, rendered from the analyzer test machine.
```

Click or drag the beam raster or scanline strip to select a slot. Picture,
beam scrub, CPU waits, and run-to-beam retain their normal controls and
shortcuts. The Memory view offers address presets and cell picking; Blits
shows source/result previews with previous/next selection; Resources offers
paging, previews, and **Save resource**. Text readouts can be selected and copied.
Click a PC in **Most stalled PCs** to open CPU disassembly there.
Below the selected beam slot, **Inspect memory**
opens the Memory tab; a **Copper instruction** link pins the Copper listing at
that instruction. The heat map's pinned cell also links to memory. **Follow
Copper** returns the listing to the live Copper. These links inspect the current
machine at the recorded address; they do not restore historical memory or run
the guest, and the analyzer's capture and selection remain available.

Select **Console** at the top, or use `Cmd+K` / `Alt+K`, for the same
[command interpreter](console) and history in this window. Output is selectable;
the command field supports editing and clipboard paste. Enter or **Execute**
runs the entered commands. Shift+Enter adds a line, and pasted commands remain
editable until submitted. Up/Down browse history. Console output continues to
arrive while another inspector is selected.

The Debug window size, display divider, CPU pane sizes, and debugger and analyzer
tabs are saved when returning to Play, closing an inspector, or exiting Copperline.
They are restored on reopening or relaunching. Debug keeps the expanded window
on its monitor. Returning to Play restores its previous position when that monitor
is available; switching layouts preserves fullscreen. Preferences live in `inspector-layout.toml` in the
[host data directory](../guide/ui.md#where-files-go). They contain layout choices
only, not command text, captures, or machine state. Opening a specific inspector
always selects the one requested.

Live inspector snapshots and layout updates are limited to 20 Hz; input and
stepping update immediately. The Amiga display retains its normal presentation
cadence while the inspectors reuse their last layout between updates.

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
can be muted individually. Each source has a fixed-height row with its scope
beside its details, so pending DMA and interrupt flags cannot move other
channels. Long detail lines scroll horizontally inside their row.

```{figure} ../images/ui-preview-debugger-audio-egui.png
:alt: Audio inspector with fixed channel rows and waveform scopes beside the channel details.
:width: 100%

Audio scopes remain aligned as channel status changes.
```

### Memory
Hexadecimal and ASCII memory dump viewer (256 bytes per page).
- **Goto:** The page's base address. Drag the value, or click it and type a
  hex address. The address/command field also jumps: type an address there
  and press Enter. **Previous page** / **Next page**, `PageUp` / `PageDown`,
  and the cursor keys (one row) move through memory.
- **Find:** Searches memory for specified byte sequences.
- **Save...:** Dumps address ranges to a file.
- **Writer?:** Queries the reverse execution snapshot ring to identify the instruction
  that last wrote to the specified address.
- **Bits:** Displays raw 1-bit-per-pixel bitplane visualizations with configurable
  stride.
- **Poke:** Writes the word in the address/command field (`ADDR VALUE`).

#### Editing memory in place

Click a byte in the hex column to select it, then type hex digits: the
first digit replaces the high nibble, the second completes the byte and
moves the selection to the next one. In the ASCII column a typed printable
character replaces the byte. Edited bytes are shown in blue until they are
written. While a byte is selected:

- Arrow keys move the selection; at the edge of the page the view scrolls
  to follow it. `PageUp` / `PageDown` page the view with the selection.
- `Backspace` forgets a half-typed digit, or steps back one byte.
- `Enter` writes every edited byte. So does leaving the dump: clicking a
  button, another tab, or the address box, or focusing any text field.
- `Esc` discards the edits and clears the selection (it does not leave
  Debug while a byte is selected).

Typed characters never reach the transport shortcuts, so `C`, `F`, `R`,
and `S` are hex digits or text while editing. The outcome (bytes written,
or the address that refused) appears beside the tab's controls. Bytes in
ROM, the overlay ROM, and device windows are drawn grey and cannot be
selected; the message names the address.

Edits are plain CPU-visible RAM writes with the semantics of the console's
`POKE` and the control protocol's `mem.write`: the data changes, no bus
cycles are charged, no interrupt or DMA state moves, and the Break tab's
memory watchpoints are rebaselined so the edit itself does not stop the
machine. As with `mem.write`, a write is not part of the reverse-execution
journal, so replaying backwards across it can diverge.

### IO Map

The selected register includes access direction, chipset availability, and a
summary from the checked-in [custom-register Markdown catalogue](../reference/custom-registers/index.md).
The console, control protocol, DAP Chipset scope, and VS Code Custom Registers
tree consume the same generated table.
Interactive memory map of custom chipset registers (`$DFF000` - `$DFF1FE`).
Selecting a register decodes its individual bitfields (e.g. `DMACON`, `INTENA`,
`BPLCON0`, `ADKCON`).

### Break
Manages active breakpoints, memory watchpoints, and custom register write traps.

```{figure} ../images/ui-preview-debugger-break-egui.png
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

A CPU parked in `STOP` (an idle Workbench waiting for a disk, a program
waiting for its interrupt) executes nothing until an interrupt arrives, so
**Step**, **Step Over**, **Step Out** and **Run to** carry it to the
interrupt that wakes it. A single **Step** there runs the machine on to
that interrupt and retires one instruction -- the first of its handler,
where control actually goes -- so the PC lands inside the handler instead
of standing still. The CPU tab shows *CPU stopped* while it is parked.
When no interrupt can reach the CPU -- the SR mask is 7, or nothing is
enabled in `INTENA` -- a step gives up after two video frames and leaves
the machine stopped where the hardware itself is stuck, and says so on the
display.

(frame-analyzer-pane)=
## Frame Analyzer

Select **Frame Analyzer** in the Debug workspace to inspect chip-bus slot allocations
and memory access patterns.

```{figure} ../images/ui-preview-analyzer-egui.png
:alt: The Frame Analyzer
:width: 90%

Frame Analyzer: chip-bus ownership and per-slot inspection.
```

### Beam tab
Displays a 2D heatmap indexed by raster beam coordinates (`X` = colour clock HPOS,
`Y` = scanline VPOS). Each cell indicates which subsystem owned the chip bus during
that colour clock cycle (CPU, Copper, Blitter, Bitplane, Sprite, Audio, Disk, Refresh, Idle).
Pointing at a cell shows its full slot record below the raster; clicking pins
the same readout. An interlaced display alternates a long field and a short
field one line shorter, so the raster is laid out against the long field: the
diagram keeps its size and the cell under the pointer stays put as the fields
alternate. A short field has no last line, and a position over it reads the
line before. It includes the custom register, address, transfer data and
width, owner subtype, CPU-visible IPL, and decoded hardware events.
Copper MOVE execution slots are cross-shaped markers coloured by destination
register class (blitter, audio, display/bitplane, sprite, palette, or control).
Their readout includes the Copper instruction address; the reciprocal
`copper.list {"trace": true}` entry links that instruction back to this beam
slot.

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
  instruction on the precise CPU loop, per batch under `[cpu] jit`). A ROM PC
  is shown with its live LVO or resident name, such as `[exec] AllocMem+$12`,
  after AmigaOS has initialised the relevant Exec lists. The
  selected-slot line names the denier whenever the selected slot was a CPU
  wait, in either view.
- **Stall gutter:** the narrow strip left of the heatmap is drawn in both
  views: one bar per line, as long as the share of that line's colour clocks
  the CPU spent waiting, in the colour of the line's dominant denier -- a
  profile of where the frame chokes the CPU.

The console's `CPUWAIT` command prints the same summary for the traced frame,
and a [profile capture](profiling) exports it per frame.

Profile captures over the control protocol ([](profiling)) share bus tracing
with the Frame Analyzer: closing the pane does not interrupt an active capture,
and stopping a capture keeps an open pane recording.

### Blits tab

The Blits tab lists every recorded blit with its start/end frame and beam
position, ascending/descending, fill or line mode, enabled channels, transfer
geometry, BLTDPT, and colour clocks used versus stalled. A blit that crosses a
frame boundary retains one stable identity and is finalised in both frame
records.

Selecting a row shows the first active source channel and the computed
result/D channel side by side. These previews use the exact captured DMA words,
including shifts, first/last-word masks, modulos, and latched BLTxDAT inputs;
the detail line shows the simplified minterm expression and whether plane
count came from the registered destination bitmap or BPLCON0. Cursor Up/Down
changes the selected blit. The same renderer is available as `blit.render`.

(frame-analyzer-memory-tab)=
### Memory heatmap tab

```{figure} ../images/ui-preview-analyzer-memory-egui.png
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

```{figure} ../images/ui-preview-analyzer-resources-egui.png
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

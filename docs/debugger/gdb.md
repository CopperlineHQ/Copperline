# Remote GDB debugging

Copperline includes a built-in GDB remote debugging stub:

```sh
./target/release/copperline --config copperline.example.toml --noaudio --gdb :2345
```

Port-only syntax (`2345` or `:2345`) binds to `127.0.0.1`. To bind to all
interfaces on a trusted local network, specify `0.0.0.0:2345`.

## Headless and windowed modes

`--gdb` runs headless: the stub takes control of the machine, halts at reset,
advances unthrottled, and cannot be combined with an interactive window or
scheduled capture flags.

`--gdb-gui ADDR` attaches the GDB remote stub to an interactive windowed
session:

```sh
./target/release/copperline --config copperline.example.toml --gdb-gui :2345
```

- The emulated machine remains interactive in the window and runs at real-time
  speed. Execution advances in real time on `continue` unless unthrottled via
  `monitor warp on`, which engages a warp hold for the GDB client. `monitor
  warp off` releases that hold only: the machine re-paces once no other holder
  (a control client's `warp.set`, the guest's `warpmode()`) remains, and the
  console line names who still holds it. Disconnecting releases the GDB hold
  the same way; the window's warp shortcut ends every hold at once.
- Attaching a debugger pauses the machine. Detaching (`detach`, connection loss,
  or GDB `kill`) leaves the window open and listening for new connections
  (`kill` detaches rather than terminating the process so that VS Code "Stop
  Debugging" does not close the window).
- Breakpoints and watchpoints are shared with the internal debugger and trigger
  during the windowed frame loop. Breakpoints set within the UI remain independent,
  and detaching GDB removes only points set by the remote client.
- When execution halts during an active GDB `continue`, the stop event is sent
  to the client. With `--control-gui` attached to the same window, a
  control-protocol resume outstanding at the same time gets its stop reply
  too, and a stop the control client causes (its `pause`, a `run_until`
  target) completes the GDB `continue` with a plain `T05` and a console line
  naming the reason. Local debugger breakpoints open the internal debugger
  window only when neither client had a resume pending; a plain pause from
  the window just pauses (and completes any outstanding resume).
- Anything that repositions the machine -- reverse execution
  (`reverse-step`, `reverse-continue`), resuming or stepping at an address
  (`continue ADDR`, `jump`), a write to `pc` -- is refused while a
  control-protocol resume is outstanding: pause first. Plain `continue`,
  `stepi`, memory and other register writes stay allowed.
- `--run` break-at-entry functions identically to headless `--gdb`.
- `--gdb-gui` cannot be combined with `--gdb`, `--control`, or
  `--benchmark-until`. It can share the window with `--control-gui`: a GDB
  frontend for source-level debugging and a control-protocol client for
  observation and control attach to one session (see
  [Control Protocol](control.md)).

Standard GDB frontends work with both modes: VS Code cppdbg configurations
(`"MIMode": "gdb"`, `"miDebuggerServerAddress": "localhost:2345"`,
`"miDebuggerPath"` pointing to `m68k-amigaos-gdb`) or Native Debug's `target remote`
setup, with `--gdb-gui` keeping the Amiga display live alongside the IDE.
For IDE debugging without a GDB at all, with source lines read straight from
the hunk executable, see the [Debug Adapter Protocol](dap.md) server
(`copperline-ctl --dap`).

## Connecting from GDB

Start a 68k-aware GDB (such as `m68k-amigaos-gdb` or multiarch `gdb`) and connect:

```gdb
(gdb) set architecture m68k
(gdb) set endian big
(gdb) target remote :2345
```

The target starts halted at reset. The stub supports:
- Register reading and writing (`d0`-`d7`, `a0`-`a5`, `fp`, `sp`, `ps`, `pc`)
- Memory read and write operations
- Breakpoints plus write (`Z2`), read (`Z3`), and access (`Z4`) watchpoints
- Single-stepping and continuation
- Ctrl-C interrupt handling
- Reverse execution (`reverse-step`, `reverse-continue`)
- Program relocation querying (`qOffsets`) and dynamic library tracking (`qXfer:libraries:read`)

## Amiga-specific monitor commands

GDB's `monitor` command provides access to Amiga custom chipset state, raster positions,
Copper disassembly, and Exec structures:

```gdb
(gdb) monitor status           # Summary: PC, SR, frame, beam position, reverse debug status
(gdb) monitor beam             # Current raster beam position (VPOS, HPOS) and colour clock
(gdb) monitor custom           # Custom chipset state dump
(gdb) monitor reg DMACON       # Read custom register without side effects
(gdb) monitor write-reg COLOR00 00F # Write custom register
(gdb) monitor copper           # Disassemble Copper instructions
(gdb) monitor beam-trap 100 40 # Break when beam reaches VPOS 100, HPOS 40
(gdb) monitor copper-break C01000 # Break when Copper PC reaches address
(gdb) monitor segments         # List loaded hunk segments for current process
(gdb) monitor who F81234       # Name a live ROM/LVO address and offset
(gdb) monitor tasks            # List Exec ready, waiting, and active tasks
(gdb) monitor memlist          # List Exec memory allocations
(gdb) monitor return-to-program # Run until PC leaves $F80000-$FFFFFF
```

`monitor who` reads the running guest's Exec library and device vectors, so it
follows `SetFunction()` patches and does not depend on the Kickstart version.
Addresses not reached through a public vector are identified by their
containing ROM resident module when possible.

In windowed mode (`--gdb-gui`), `monitor warp on|off|status` controls the GDB
client's warp hold, running unthrottled with audio muted until that hold is
released or the client disconnects (another holder keeps the machine
warping; `status` names every holder).

## Source-level debugging and program loading

### Launching with `--run`

When using `--run` alongside `--gdb`, Copperline automatically halts at the
entry point of the loaded Amiga executable before the first instruction executes:

```sh
copperline --run build/hello --gdb :2345
```

In GDB:

```gdb
(gdb) target remote :2345
(gdb) continue
# Halts at LoadSeg completion, reporting hunk base address
(gdb) add-symbol-file build/hello.elf 0x018FE8
(gdb) break main
(gdb) continue
```

### Automatic library and segment relocation

If your GDB client supports `qXfer:libraries:read`, Copperline reports new program
loads dynamically. When using `m68k-amigaos-gdb` with an existing process:

1. Launch your program from the Amiga shell.
2. In GDB, run `target remote :2345`.
3. GDB queries `qOffsets`, automatically aligning symbols with the loaded hunk addresses.

## Reverse debugging with GDB

The GDB stub integrates with Copperline's snapshot ring buffer:

| GDB command | Action |
|---|---|
| `reverse-step` | Reconstructs and steps backward by one instruction |
| `reverse-continue` | Executes backward until a preceding GDB breakpoint is reached |
| `monitor last-writer ADDR` | Finds the last instruction that modified memory at `ADDR` |

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
- Single-stepping and continuation; forced PC writes discard stale instruction prefetch
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


## Bartman extension backend

Install the public Copperline fork of
[Bartman's Amiga C/C++ extension](https://github.com/LinuxJedi/vscode-amiga-debug/tree/copperline-backend)
using the pinned-revision VSIX instructions in
[Bartman with Copperline](vscode-bartman.md). That guide includes a complete
project setup and illustrated profiler walkthrough; it does not depend on
an upstream merge or release. The fork adds these VS Code settings:

```json
{
  "amiga.emulator": "copperline",
  "amiga.copperline-path": "/path/to/copperline"
}
```

Use the extension's usual launch configuration (`program` is the ELF path
without its extension, beside a matching `.exe` hunk executable). The backend
keeps the extension's patched `m68k-amiga-elf-gdb`, maps its model and memory
presets to Copperline CLI arguments, and seeds the guest RTC from the host
clock. Use the bundled AROS ROM (omit `kickstart`) or Kickstart 1.3 or
newer for `--run`; detached launches require 2.0+ or AROS. The standard Marketplace extension does not contain this
backend; keep the fork installed as described in the setup guide.

The equivalent manual command is:

```sh
copperline --factory --model A500 --chip 512K --slow 512K \
  --run build/demo.exe --gdb-gui :2345 --gdb-dialect bartman
```

`--gdb-dialect bartman` works with `--gdb` and `--gdb-gui`. Standard is the
default; the switch explicitly selects the patched GDB's wire contract:

- `qOffsets` lists every hunk base separated by semicolons. With `--run`, the
  initial stop query finishes loading the executable before replying, so
  GDB caches registers and relocates symbols from the same machine state.
  Windowed Bartman launches wait paused for the first GDB connection, so
  slow debugger startup cannot miss the executable's load event.
  The protocol smoke test below accepts `--gui --attach-delay 5` to exercise
  this delayed-attachment path.
- A software breakpoint at `$FFFFFFFF` is a one-shot return from ROM.
- Stops use `S0A` for address error, `S04` for illegal instruction, and `S05`
  otherwise. Exception catches are installed after the run target loads.
  Signal-bearing continue/step requests resume the CPU after the exception;
  they do not inject a second exception into the whole-machine target.
- Registers are D0-D7, A0-A7, SR, PC, each represented by a 32-bit word.
- `qAttached` returns `1`; `k` detaches; `monitor reset` restores the saved program-entry state after a `--run`
  attachment, preserving symbol addresses; without an entry snapshot it resets
  the machine.
- Guest debug text and monitor messages have a `DBG: ` prefix.

```gdb
(gdb) monitor profile 2 "build/demo.unwind" "/tmp/demo.profile"
```

This runs a bounded capture of 1-100 frames, sends live `PRF: n/N` console
packets, and writes the mixed-endian binary consumed by the extension's
`ProfileFile` reader. Use an empty unwind path (`""`) for leaf-only sampling.
The writer finishes the current partial frame before snapshotting replay
memory, then captures complete frames. It publishes the output only on
success and restores its instrumentation on failure. Both transports
suspend their session-owned exception catches and memory watches during
capture, restoring them and their watch baselines afterward.

The legacy binary has a fixed PAL 227×313 DMA grid and carries chip and slow
RAM only. NTSC and programmable geometry use Copperline's native
[profile captures](profiling.md). The upstream `.uss` editor's UAE launch
path is separate; use [USS import](../guide/winuae-state.md) for those states.


To exercise attach ordering, exact `main` and ROM-exit stops, a two-frame
capture, restart, and illegal-instruction/address-error signals using the
real patched GDB:

```sh
python3 tools/check-bartman-gdb.py \
  --gdb /path/to/vscode-amiga-debug/bin/darwin/opt/bin/m68k-amiga-elf-gdb \
  --program build/demo.exe --elf build/demo.elf --out /tmp/gdb-check
```

An optional `--unwind FILE` uses the extension's compact table; `--gui`
checks the windowed transport, including an exact breakpoint stop when
automatic launch warp ends at the same boundary. The output directory contains emulator/GDB
logs and the binary profile, suitable for `tools/check-bartman-profile.cjs`.

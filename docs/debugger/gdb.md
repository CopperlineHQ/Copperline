# Remote GDB debugging

Copperline includes a built-in GDB remote debugging stub:

```sh
./target/release/copperline --config copperline.example.toml --noaudio --gdb :2345
```

Port-only syntax (`2345` or `:2345`) binds to `127.0.0.1`. To bind to all
interfaces on a trusted local network, specify `0.0.0.0:2345`.

## Headless and windowed modes

`--gdb` runs headless: the stub owns the machine, starts it halted at
reset, advances it unpaced, and cannot be combined with a window or the
scheduled capture flags.

`--gdb-gui ADDR` attaches the same stub to the normal interactive
window instead, the way `--control-gui` attaches the control server:

```sh
./target/release/copperline --config copperline.example.toml --gdb-gui :2345
```

- The machine stays visible and paced to real time; `continue` runs at
  Amiga speed unless the client turns pacing off with `monitor warp on`
  (`monitor warp off` re-paces, and a disconnect releases the client's
  warp hold automatically).
- Attaching pauses the machine. Detaching (`detach`, a dropped
  connection, or GDB's `kill`) leaves the window running and the stub
  listening for the next client -- `kill` deliberately detaches instead
  of ending the session, because the window belongs to the user and
  VS Code sends `kill` on Stop Debugging.
- Breakpoints, watchpoints, and register watches install into the same
  machine break stores the debugger window uses, so they hit inside the
  windowed frame loop. Points the window's own debugger set are never
  touched, and a detach removes exactly what the client installed.
- A stop while the client's `continue` is outstanding answers the
  client; any other debug stop (a GUI breakpoint, a watchpoint) opens
  the local debugger window as usual, and a plain pause from the window
  just pauses.
- `--run` break-at-entry works exactly as with `--gdb` (below).
- `--gdb-gui` cannot be combined with `--gdb`, `--control`,
  `--control-gui`, or `--benchmark-until`.

Stock GDB frontends debug either mode: VS Code's cppdbg configuration
(`"MIMode": "gdb"`, `"miDebuggerServerAddress": "localhost:2345"`,
`"miDebuggerPath"` pointing at `m68k-amigaos-gdb`) or Native Debug's
`target remote` setup, with `--gdb-gui` keeping the Amiga screen
interactive beside the editor.

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
- Breakpoints and watchpoints
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
(gdb) monitor tasks            # List Exec ready, waiting, and active tasks
(gdb) monitor memlist          # List Exec memory allocations
```

A windowed session (`--gdb-gui`) additionally answers
`monitor warp on|off|status`, toggling the App's warp hold: the machine
runs unpaced (audio muted) until the client turns it back off or
disconnects.

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

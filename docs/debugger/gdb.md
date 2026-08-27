# Remote GDB debugging

Copperline includes a built-in GDB remote debugging stub:

```sh
./target/release/copperline --config copperline.example.toml --noaudio --gdb :2345
```

Port-only syntax (`2345` or `:2345`) binds to `127.0.0.1`. To bind to all
interfaces on a trusted local network, specify `0.0.0.0:2345`.

## Connecting from GDB

Start a 68k-aware GDB (such as `m68k-amigaos-gdb` or multiarch `gdb`) and connect:

```gdb
(gdb) set architecture m68k
(gdb) set endian big
(gdb) target remote :2345
```

The target starts halted at reset. The stub supports:
- Register reading and writing (`d0`-`d7`, `a0`-`a7`, `sr`, `pc`)
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

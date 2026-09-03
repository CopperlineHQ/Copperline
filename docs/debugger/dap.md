# Debug Adapter Protocol (IDE debugging)

`copperline-ctl --dap` is a [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
server: VS Code, nvim-dap and any other DAP client can debug a program
running in Copperline with source-level breakpoints and stepping, the
register file, locals and globals, memory, the custom chipset, reverse
execution and a Debug Console, without a GDB in between.

The adapter is a client of the [control protocol](control.md), like the
MCP mode. It launches an emulator with `--run PROG` (windowed by default,
so the Amiga's screen stays live beside the editor) or attaches to one
started with `--control-info`, and turns each DAP request into control
protocol calls. Source lines, symbols, variables and call frames come from
the program's own debug information, relocated by the hunk addresses the
guest reports when it loads the program.

```sh
copperline-ctl --dap                 # on stdin/stdout, how IDEs spawn adapters
copperline-ctl --dap-listen :4711    # over TCP (a "debugServer" connection)
copperline-ctl --dap --info /tmp/ccp.json   # launch/attach both use this session
```

## VS Code

Install the extension in `tools/vscode-copperline` (no build step:
`npx @vscode/vsce package` there and `code --install-extension` the
`.vsix`, or symlink the directory into `~/.vscode/extensions/`). It
contributes the `copperline` debug type and runs `copperline-ctl --dap`
from your PATH (setting `copperline.ctlPath`). A launch configuration:

```json
{
  "type": "copperline",
  "request": "launch",
  "name": "Run hello in Copperline",
  "program": "${workspaceFolder}/hello",
  "model": "A1200",
  "fast": "8M",
  "entryPoint": "main",
  "stopOnEntry": true
}
```

F5 opens a Copperline window, warp-boots to the program, and stops at its
entry point with the source open; breakpoints, Step Over/Into/Out, Step
Back, the Variables view, hovers, the Disassembly view, the memory viewer
and the Debug Console all work from there. Stop closes the window.

## Other clients

nvim-dap, with the adapter on stdio:

```lua
local dap = require("dap")
dap.adapters.copperline = { type = "executable", command = "copperline-ctl", args = { "--dap" } }
dap.configurations.c = {
  { type = "copperline", request = "launch", name = "Run in Copperline",
    program = "${workspaceFolder}/hello", stopOnEntry = true, entryPoint = "main" },
}
```

Any client that connects to a TCP debug server can use `--dap-listen ADDR`
instead. Zed loads debug adapters through extensions only; none exists yet.

## Launch and attach

`launch` starts `copperline --control-gui :0 --control-info FILE --run
PROGRAM` (with `--control` and `--noaudio` when `headless` is true) and
connects to it. The emulator boots the bundled AROS ROM, or whatever the
configuration says. Arguments:

| Argument | Meaning |
|---|---|
| `program` | The hunk executable on the host (required). Its directory is mounted in the guest. |
| `args` | Command-line arguments for the program (a string or an array). |
| `copperline` | The emulator binary. Default: `COPPERLINE_BIN`, then a `copperline` next to `copperline-ctl`, then PATH. |
| `config` | A TOML configuration file (`--config`). Default: the launcher's saved default. |
| `factory` | Ignore the saved default configuration (`--factory`). |
| `model`, `chipset`, `cpu`, `chip`, `fast`, `slow` | The matching `copperline` flags. |
| `rtcTime` | `--rtc-time`: the guest clock's seed. Default: the launch time, pinned, so reverse execution replays exactly (a guest reading the host clock would diverge; see [Reverse debugging](reverse.md)). |
| `extraArgs` | Further emulator flags, as an array. |
| `headless` | No window (`--control`). |
| `noAudio` | `--noaudio` for a windowed session. |
| `stopOnEntry` | Stop at the entry point once the program is loaded (default true). |
| `entryPoint` | The symbol to stop at instead of the first instruction of the first hunk: `main` or `entry` for C programs whose first hunk begins with a startup stub. |
| `symbolFile` | An ELF with DWARF the program was converted from (elf2hunk); `program.elf` is tried automatically. |
| `sourceMap` | `{"/build/prefix": "/host/prefix"}` for sources recorded under another path. |
| `cwd`, `timeoutMs` | The emulator's working directory, and how long to wait for its control endpoint (default 60 s). |

`attach` connects to a running emulator through `controlInfo` (the
`--control-info` file) or `address` + `token`, and takes `program`,
`symbolFile`, `sourceMap`, `entryPoint` and `stopOnEntry` the same way.
When the program is already running, its segments are matched against the
file and the symbols relocate at once; otherwise a `loadseg` break waits for
the guest to load it.

Behind a launch, the control server arms a one-shot `loadseg` stop for the
program before the first frame runs (the same break-at-entry `--gdb` has), so
the boot cannot outrun the adapter. At that stop the adapter reads the
program's segments with `segments.list`, relocates its debug information,
binds the breakpoints the client set while the program was not loaded yet,
and runs to the entry point.

## Debug information

The adapter reads the executable itself and, optionally, an ELF sibling.
What each toolchain provides:

| Toolchain | What the adapter gets |
|---|---|
| vasm `-Fhunkexe -linedebug` | Source lines from the `LINE` debug hunks, symbols from the symbol hunks. Breakpoints and stepping by assembly source line. |
| bebbo amiga-gcc 6.5 (`m68k-amigaos-gcc -g -O0`) | DWARF from the trailing debug hunk: lines, functions, parameters and locals, globals, struct/array/pointer/enum types, call-frame information for the call stack. |
| bebbo amiga-gcc 13/15/16 (`-g`) | These toolchains' linkers drop the DWARF sections from hunk output: symbols only (function breakpoints, symbolised disassembly, a scanned call stack). Build with the 6.5 toolchain for source-level debugging. |
| Bartman's `m68k-amiga-elf` + elf2hunk | The hunk file's symbols plus the ELF's DWARF through `symbolFile` (or `program.elf`); ELF sections map onto hunks in section order, as elf2hunk allocates them. |
| Anything stripped | Disassembly, registers, memory, chipset. |

Locals need `-O0`: the adapter evaluates the single-operation DWARF
locations (`DW_OP_fbreg`, `DW_OP_breg`, `DW_OP_reg`, `DW_OP_addr`,
`DW_OP_call_frame_cfa`) and shows anything else as unsupported rather than
guessing. Values render by type: integers, characters, booleans, floats,
pointers (with the string behind a `char *`), arrays and structs expand in
the Variables view. Without DWARF the Globals scope lists the data and BSS
symbols as longs.

The console line printed at launch says what was found: `19 hunk symbols;
DWARF from the executable's debug hunk: 3 function(s), 43 line row(s), 5
global(s), call-frame info`.

## What maps to what

| DAP | Copperline |
|---|---|
| Source, function and instruction breakpoints | `break.add {"kind": "pc"}`; a source line without code binds to the next line that has some, like GDB. Conditions are one comparison the machine evaluates itself (`d0 == 5`, `[$DFF006] != 0`, `a0 >= d1`, `sr & $2000`; a memory operand compares the 16-bit word at the address); hit conditions are ignore counts. |
| Data breakpoints | `break.add {"kind": "watch"}` on the words of the variable (writes only, eight words at most). |
| Exception breakpoints | `break.add {"kind": "catch"}`: bus error, address error, illegal instruction, zero divide, CHK/TRAPV, privilege violation, line-A, line-F. |
| Continue / Pause | `continue` / `pause`. |
| Step Over / Into / Out | `step_over` / `step` / `step_out`, repeated until the source line changes (statement granularity) or once (instruction granularity). Stepping into code without lines runs it to its return. A line that outgrows 64 single steps (a loop) gets temporary breakpoints on the function's other lines and the return address instead. |
| Step Back / Reverse Continue | `reverse_step` / `reverse_continue` from the snapshot ring (see [Reverse debugging](reverse.md)); Step Back also repeats until the line changes. The adapter takes a snapshot (`reverse_anchor`) at every stop a run ends in, so stepping back replays from there: the boot volume `--run` stages is a host directory mount, whose traffic a replay from an older snapshot could not reproduce. |
| Call stack | Call-frame information when the DWARF has it, else a scan of the stack for return addresses that follow a `JSR`/`BSR`. Frames beyond the innermost are looked up at the call site. |
| Scopes | Registers (D0-D7, A0-A7, PC, SR with its flags), Locals, Globals, Chipset (every custom register and the beam position). |
| Set variable | `regs.set` for the innermost frame's registers; `mem.write` for base-type variables and members. |
| Evaluate / hover / watch | Registers, variables and symbols by name, numbers (`$DFF000`, `0x1234`, `%1010`), `+ - * /`, `[expr]` / `[expr].w` / `[expr].b` memory reads, `d0.w`. In the Debug Console, `!method {json}` sends a raw control-protocol request and prints the reply (`!status`, `!beam.get`, `!capture.screenshot {"path": "/tmp/s.png"}`). |
| Memory view | `mem.read` / `mem.write` (base64). |
| Disassembly view | `disasm`, with symbols and source lines; backwards disassembly anchors at the nearest function or symbol start. |
| Jump to cursor | `regs.set {"reg": "pc"}`. |
| Debug Console output | Serial output (which is where `KPrintF` goes) as `stdout`; uaelib function 86 (`debug_log`) text and the adapter's own notes as `console`. |
| Modules / loaded sources | The program with its first hunk's address, and the source files its debug information names. |

A stop from a breakpoint the debugger window set, or from a `catch`, `reg
watch`, beam trap or Copper breakpoint set through `!break.add`, is reported
as a breakpoint with the machine's description. In a windowed session a
pause or resume made from the window is noticed within a second and
reflected in the IDE.

## Limitations

- Locals are shown for simple location expressions and one level of
  struct/array nesting; optimised code, location lists and DWARF
  expressions render as unsupported.
- Data breakpoints trigger on writes (Copperline's memory watches); read
  watchpoints are not offered.
- One thread (the CPU) is presented; Exec tasks are not separate threads.
- The `sourceMap` is a prefix replacement; when a recorded path does not
  exist on the host, the adapter also tries the path's tail under the
  program's directory.
- Restart re-launches the emulator with the same arguments.

# Copperline Amiga Debug for VS Code

Debug Amiga programs running in the [Copperline](https://copperline.dev)
emulator from VS Code: source-level breakpoints and stepping, the
register file, locals and globals, memory, the custom chipset, reverse
stepping, and the Debug Console over the emulator's control protocol.

The extension is a thin shell: it tells VS Code to run `copperline-ctl
--dap`, the Debug Adapter Protocol server built into Copperline's control
client. Everything else is in that adapter; see the Copperline
documentation, *Debug Adapter Protocol* chapter (`docs/debugger/dap.md`).

## Requirements

- Copperline 0.19 or later, with `copperline-ctl` on your PATH, or the
  `copperline.ctlExecutable` setting pointing at the executable file.
  `copperline.emulatorExecutable` names the `copperline` executable file
  to launch when it is not next to `copperline-ctl` (a source build's
  `target/release/copperline`, say).
- A program built with debug information: vasm `-linedebug`, amiga-gcc
  6.5 `-g`, or an ELF + `elf2hunk` toolchain (`symbolFile`). Without any,
  you still get hunk symbols, disassembly and registers.

## Getting started

Add a launch configuration (the *Copperline: launch a program* snippet):

```json
{
  "type": "copperline",
  "request": "launch",
  "name": "Run hello in Copperline",
  "program": "${workspaceFolder}/hello",
  "stopOnEntry": true
}
```

Press F5: Copperline opens a window, warp-boots to the program, stops at
its entry point, and the IDE shows its source. Add `"model"`, `"fast"`,
`"config"` and friends to pick the machine. `"memoryFill": "0xDEAD"`
fills cold-start RAM with a diagnostic pattern, `"fpu": true` fits an FPU,
`"stack": 32768` sets the guest CLI stack, `"ntsc": true` selects NTSC,
`"detach": true` closes the boot CLI after starting the program, and
`"emulatorLog": true` mirrors the emulator log into the Debug Console.
`"headless": true` runs without a window. Data breakpoints support read,
write, and read/write access; fitted FPU registers appear in the Registers
scope.

When stopped in a loaded program, use the graph button (**Profile**) for one
emulated frame or the pulse button (**Profile (Multi)**) to choose a frame
count. Copperline records source-mapped instruction call stacks and opens the
resulting `.cpuprofile` in VS Code. Profiles separate each function's CPU work
from a `[Bus wait]` child showing chip-DMA contention; register snapshots and
IRQ level/vector metadata remain in the capture directory returned by the
adapter.

To attach to an emulator you started yourself:

```sh
copperline --control-gui :0 --control-info /tmp/ccp.json --run hello
```

```json
{
  "type": "copperline",
  "request": "attach",
  "name": "Attach to Copperline",
  "controlInfo": "/tmp/ccp.json",
  "program": "${workspaceFolder}/hello"
}
```

## Building the extension

There is no build step. To package it:

```sh
cd tools/vscode-copperline
npx @vscode/vsce package
code --install-extension copperline-debug-*.vsix
```

For development, symlink this directory into `~/.vscode/extensions/`.

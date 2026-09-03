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

- Copperline 0.19 or later, with `copperline-ctl` on your PATH (or set
  `copperline.ctlPath`).
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
`"config"` and friends to pick the machine; `"headless": true` runs
without a window.

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

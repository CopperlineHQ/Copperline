---
abstract: |
  Copperline is a cycle-driven Commodore Amiga emulator (OCS, ECS, and
  AGA) written in Rust. This document covers using the emulator,
  configuring machines from the A500 to the A4000 and CD32, describing
  Zorro expansion boards, the browser (WebAssembly) build, the
  interactive and headless debuggers, and the
  internal architecture: the per-colour-clock chip-bus timing model, the
  chipset modules, and the beam-event-replay video pipeline.
---

# Copperline

Copperline is a cycle-driven Commodore Amiga emulator (OCS, ECS, and AGA)
written in Rust. Cycle-driven means the whole machine -- the 68000-family
CPU, Agnus, Denise, Paula, the CIAs, the floppy subsystem, and the chip bus
that ties them together -- advances on one colour-clock timeline: the chip
bus is arbitrated per colour clock, the Copper and blitter are scheduled
per DMA slot with the hardware bus sequences, and the Paula IPL pipe plus
68000 interrupt sampling are modelled. That timing model is what lets
it run the current cycle-sensitive OCS and AGA regression set, as well as
Kickstart, Workbench, games, and CDTV/CD32 titles.

The project home is [copperline.dev](https://copperline.dev/); the source
lives on [GitHub](https://github.com/CopperlineHQ/Copperline).

```{figure} images/state-of-the-art.png
:alt: Spaceballs' State of the Art running in Copperline
:width: 85%

Spaceballs' *State of the Art* (1992), a cycle-exact OCS stress test,
running in Copperline.
```

## Where to start

- [](guide/getting-started) -- build the emulator and boot your first
  machine.
- [](guide/configuration) -- the `copperline.toml` reference: machine
  profiles (A500 through the A4000 and CD32), CPU, memory, chipset,
  floppy, IDE/SCSI, host-directory mounts, serial/MIDI, and CD options.
- [](guide/ui) -- the window, status bar, keyboard shortcuts, menus, and
  gamepad calibration.
- [](guide/headless) -- scripted, deterministic runs: screenshots, frame
  dumps, scripted input, and WAV capture.
- [](guide/browser) -- the same core compiled to WebAssembly, hosted at
  [copperline.dev/try](https://copperline.dev/try/): how it works and how
  to embed it.
- [](zorro) -- describing additional Zorro II/III expansion boards in
  TOML metadata files.
- [](debugger/window), [](debugger/headless), and [](debugger/gdb) -- the
  interactive debugger window, chip-bus frame analyzer, environment-driven
  headless debugger, and remote GDB frontend.
- [](debugger/control) -- the JSON-RPC control protocol for driving the
  emulator from scripts, CI, and AI agents (`--control`, `copperline-ctl`).
- [](internals/architecture) -- how the emulator works inside, for
  contributors.

## Design principles

Two rules shape every change in Copperline:

1. **Hardware first.** There are no branches keyed to game, demo, ROM, or
   file names. Compatibility problems are fixed by modelling the underlying
   chip behaviour; software titles appear only as regression examples.
2. **Determinism.** The emulated core is deterministic and independent of
   the host: a headless unthrottled run and a real-time windowed run produce
   the same emulated result when given the same inputs and media. That is
   what makes headless captures reproducible, and it is also what lets the
   debugger step backwards.

---
abstract: |
  Copperline is an Amiga emulator (OCS, ECS, and AGA) written in Rust.
  This documentation covers running and configuring the emulator, setting up
  machines from the A500 to the A4000 and CD32, using expansion boards,
  running headless test sessions, and exploring internal architecture.
---

# Copperline

Copperline is a cycle-driven Commodore Amiga emulator (OCS, ECS, and AGA)
written in Rust. The emulator advances the CPU (68000 through 68060), Agnus,
Denise, Paula, CIAs, floppy subsystem, and chip bus on a unified colour-clock
timeline. Bus arbitration occurs per colour clock, with Copper and blitter DMA
scheduled according to hardware slot sequences.

The project home is [copperline.dev](https://copperline.dev/); the source code
is hosted on [GitHub](https://github.com/CopperlineHQ/Copperline).

```{figure} images/state-of-the-art.png
:alt: Spaceballs' State of the Art running in Copperline
:width: 85%

Spaceballs' *State of the Art* (1992) running in Copperline.
```

## Documentation overview

- [](guide/getting-started) -- Installation, build instructions, and initial setup.
- [](guide/configuration) -- Complete `copperline.toml` reference for machine
  profiles, memory, storage, expansion boards, and input.
- [](guide/ui) -- Window controls, status bar, keyboard shortcuts, and gamepad mapping.
- [](guide/whdload) -- Direct launching and library management for WHDLoad packages.
- [](guide/run) -- Rapid testing of cross-compiled Amiga executables.
- [](guide/fluxbridge) -- Connecting physical floppy drives via Greaseweazle hardware.
- [](guide/host-disks) -- Attaching physical host drives and storage cards directly.
- [](guide/mt32) -- Built-in Roland MT-32 emulation and front-panel display.
- [](guide/coppersynth) -- Built-in General MIDI SoundFont synthesizer.
- [](guide/modem) -- Hayes-compatible AT modem emulation over TCP.
- [](guide/import-uae) -- Converting WinUAE, Amiberry, and FS-UAE configurations.
- [](guide/headless) -- Scripted, non-interactive execution for automated testing and CI.
- [](guide/browser) -- WebAssembly build and web integration details.
- [](guide/publishing) -- Bundling standalone player packages for specific games.
- [](zorro.md) -- Expansion bus specification and custom Zorro II/III plugin definitions.
- [](debugger/window), [](debugger/headless), and [](debugger/gdb) -- Interactive,
  command-line, headless, and GDB debugging tools.
- [](debugger/control) -- JSON-RPC control protocol (`copperline-ctl`) for automation.
- [](internals/architecture) -- Emulator internals and subsystem architecture.

## Core design principles

1. **Hardware-accurate modeling.** Behavior is implemented according to chip
   specifications rather than application-specific hacks or game title detection.
2. **Determinism.** The emulation core executes deterministically with respect to
   emulated clock cycles and input events. Given identical media and inputs,
   headless runs and interactive sessions produce identical results.

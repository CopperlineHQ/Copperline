---
abstract: |
  Copperline is an Amiga emulator (OCS, ECS, and AGA) written in Rust.
  This documentation covers running and configuring the emulator, setting up
  machines from the A500 to the A4000 and CD32, using expansion boards,
  running headless test sessions, and understanding the emulator's architecture.
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

- [](guide/getting-started) -- Installation, building from source, and first boot.
- [](guide/configuration) -- The `copperline.toml` reference for machine
  profiles, memory, storage, expansion boards, and input.
- [](guide/ui) -- Window controls, status bar, keyboard shortcuts, and gamepad mapping.
- [](guide/whdload) -- Direct launching and library management for WHDLoad packages.
- [](guide/run) -- Booting straight into cross-compiled Amiga executables.
- [](guide/fluxbridge) -- Using physical floppy drives through a Greaseweazle.
- [](guide/host-disks) -- Giving the machine a real disk or storage card of the host's.
- [](guide/mt32) -- Built-in Roland MT-32 emulation and front panel.
- [](guide/coppersynth) -- Built-in General MIDI SoundFont synthesizer.
- [](guide/modem) -- Hayes-compatible AT modem emulation over TCP.
- [](guide/import-uae) -- Converting WinUAE, Amiberry, and FS-UAE configurations.
- [](guide/winuae-state) -- Importing supported WinUAE save states and profiling resumed frames.
- [](guide/netplay) -- Rollback netplay for two to four players across desktop instances or browsers.
- [](guide/headless) -- Scripted, non-interactive execution for automated testing and CI.
- [](guide/browser) -- WebAssembly build and web integration details.
- [](guide/libretro) -- The libretro core for RetroArch and other libretro frontends.
- [](guide/publishing) -- Bundling standalone player packages for specific games.
- [](zorro.md) -- Writing data-driven and WebAssembly Zorro II/III expansion board plugins.
- [Custom chip registers](reference/custom-registers/index.md) -- One reference
  page per custom chip register (in the HTML manual and the debugger's help, not
  the PDF).
- [](debugger/window), [](debugger/console), [](debugger/headless), and
  [](debugger/gdb) -- The Debug workspace, the debugger console, the headless
  debugger, and the GDB remote stub.
- [](debugger/reverse), [](debugger/waveform), and [](debugger/profiling) --
  Reverse execution, VCD chipset waveform export, and per-frame profiling and
  guest code coverage.
- [](debugger/vscode) and [](debugger/vscode-bartman) -- VS Code setup, source debugging, and illustrated CPU/DMA and graphics profiling using the installable Bartman fork.
- [](debugger/control) -- JSON-RPC control protocol (`copperline-ctl`) for automation, with an MCP server mode for coding agents.
- [](debugger/dap) -- DAP launch, attach, stepping, and debug-information reference.
- [](debugger/diverge) -- A/B divergence finder: two builds or two configs in lockstep, narrowed to the first differing frame and instruction.
- [](debugger/workflows) -- Worked debugging workflows that combine these tools.
- [](internals/architecture) -- Source layout, frame loop, and host boundary.
- [](internals/timing), [](internals/chipset), [](internals/video),
  [](internals/audio), and [](internals/cpu) -- The timing model and the
  chipset, video, audio, and CPU implementations.
- [](internals/peripherals), [](internals/toccata), [](internals/mhi),
  [](internals/copperhf), [](internals/zz9k), [](internals/picasso2), and
  [](internals/graffity) -- Expansion hardware models and board protocols.
- [](internals/netplay) and [](internals/savestate) -- Netplay architecture
  and the save-state format.
- [](internals/reference-docs) -- The in-repository reference documents behind
  the timing models.

## Core design principles

1. **Hardware-accurate modelling.** Behaviour is implemented according to chip
   specifications rather than application-specific hacks or game title detection.
2. **Determinism.** Given the same configuration, media, clock seed, and timed
   inputs, the core produces the same results in headless and interactive runs.
   Live host services, such as networking and physical drives, have separate
   [replay limits](internals/architecture.md#determinism-and-the-host-boundary).

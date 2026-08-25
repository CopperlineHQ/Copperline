![Copperline](assets/brand/copperline-logo.png)

Website: [copperline.dev](https://copperline.dev/) |
Chat: [Discord](https://discord.gg/HDTjt3tYAC) |
Support: [Patreon](https://www.patreon.com/cw/Copperline)

An Amiga emulator written in Rust, built around the pure-Rust
[m68k](https://crates.io/crates/m68k) CPU core, with a
[pixels](https://crates.io/crates/pixels) + [winit](https://crates.io/crates/winit)
window for video and stdout for serial. It started life with the modest
goal of booting [DiagROM](https://www.diagrom.com/) far enough to show a
menu; it now boots Kickstart and runs timing-sensitive OCS and AGA software
from the regression set at real speed.

It covers OCS, ECS, and AGA (independent Agnus/Denise revisions,
programmable blanking, machine profiles from the A500 to the A4000,
CDTV, and CD32, with Gayle and A4000 IDE, A2091/A4091/A3000 SCSI, and the
AGA display path: 8 bitplanes,
256-entry palette, HAM8, FMODE wide fetch; remaining gaps are recorded in
the internals docs). Cycle-driven means the whole machine advances on one
colour-clock timeline: the chip bus is arbitrated per colour clock, the
Copper and blitter are scheduled per DMA slot with the hardware bus
sequences, and 68000 interrupt-recognition latency is modelled. 68000 cycle
counts are validated against the TomHarte SingleStepTests, and chip-bus
timing against a test disk cross-checked on real hardware
(`timing-test/`).

## Background

It began as a two-part experiment: could I build an Amiga emulator that is
easy to drive from the command line, and could I give it the tooling to
feed debugging data and screenshots back to an AI agent automatically, so
issues could be investigated from a timestamp or a snapshot? It grew into a
larger emulator whose development was AI-driven, guided by books, my own
knowledge of Amiga internals, and test disks generated to compare timings
against real hardware.

## Features

- **Cycle-driven timing core.** The chip bus is arbitrated per colour clock
  between refresh, display/sprite/disk/audio DMA, the Copper, the blitter,
  and CPU accesses; the Copper and blitter are scheduled per DMA slot with
  the hardware per-word bus sequences, and 68000 interrupt-recognition
  latency is modelled. Real-hardware reference numbers come from the
  cross-emulator disk in `timing-test/`.
- **OCS, ECS, and AGA**, with independent Agnus/Denise revisions and machine
  profiles from the A500 to the A4000, plus CDTV and CD32. Boots the bundled
  AROS ROM out of the box, as well as Kickstart 1.3 / 2.05 / 3.1 and DiagROM
  v2.0, and runs the current timing-sensitive OCS and AGA regression set at
  real speed.
- **Configurable CPU** (68000 / 68010 / 68EC020 / 68020 / 68030 / 68040 / 68060) and clock,
  with an optional 68881/68882 FPU (default-on for the 68040/68060) and the
  68030/68040 MMUs. The 68020 integer timing model selects the cache or
  uncached execution totals from the MC68020 User's Manual per instruction.
- **Peripherals**: a bit-timed keyboard (6500/1 MCU), mouse, USB gamepad
  (via the pure-Rust `gilrs`, no SDL2), 4-channel Paula audio, floppy
  (ADF / ADZ / ZIP / DMS, read-only IPF and SCP, or a real 3.5" drive over
  a Greaseweazle via the pure-Rust FluxBridge library,
  `docs/guide/fluxbridge.md`), Gayle and A4000 IDE, SCSI (A2091,
  A4091, or the A3000's onboard Super DMAC), CDTV/CD32 CD, the CD32 Full
  Motion Video module with pure-Rust MPEG-1 video decoding, A2065 Ethernet,
  a bundled host-backed `bsdsocket.library` (socket networking for guest
  applications with no guest TCP/IP stack to boot),
  Z3660 and Picasso II/II+ RTG cards (high-colour Picasso96 screens), the serial
  port bridged to host stdout/TCP/PTY/MIDI, a parallel-port printer capture and
  audio sampler, host directories served live as AmigaDOS volumes, and Zorro
  boards loadable as WASM plugins.
- **Tooling**: an in-window debugger that can step backwards, an
  interactive chip-bus frame analyzer, a trigger-based VCD waveform export
  of the chipset signals for GTKWave (`docs/debugger/waveform.md`), remote
  GDB support, a JSON-RPC control protocol for scripts and AI agents
  (`docs/debugger/control.md`, with the `copperline-ctl` client and bounded
  frame/serial/interrupt/media event streams),
  deterministic save states, input recording/replay, and
  headless screenshot/frame-dump capture -- the deterministic core makes
  every replay byte-identical.
- **A browser build**: the same core compiled to WebAssembly with a
  canvas/Web Audio frontend, hosted at
  [copperline.dev/try](https://copperline.dev/try/) -- boots the bundled
  AROS ROM, takes your own Kickstart and disk images, and runs entirely
  client-side. Exact-repeat frames skip both Rust rendering and browser
  uploads, and the live stat line splits core, render, upload, and monitor
  submission cost. See `docs/guide/browser.md` for how it works and how to
  embed it.

## Requirements

- Rust 1.93+ (stable). Tested with Rust 1.96.
- Fedora build dependencies:
  `sudo dnf install alsa-lib-devel systemd-devel gcc`.
- No SDL2 dependency. Developed on **macOS**; CI builds and runs the test
  suite on macOS, Linux, and Windows.
- **Linux requires a Vulkan driver.** The display is presented with wgpu via
  the Vulkan backend; the OpenGL fallback is not usable (see "Linux: Vulkan
  required" below). Any GPU from roughly Intel Skylake / 2015 onward has a
  hardware Vulkan driver. Older hardware (or a headless/VM host) can use the
  software lavapipe ICD: `vulkan-swrast` on Arch, `mesa-vulkan-drivers` on
  Debian/Ubuntu/Fedora. macOS (Metal) and Windows (DX12) are unaffected.

## Install (macOS, Homebrew)

```sh
brew tap copperlinehq/copperline https://github.com/CopperlineHQ/Copperline
brew install copperline
```

This builds from source on your machine, so the binary is not subject to
macOS Gatekeeper quarantine -- there is no Security & Privacy override to
click through. Use `brew install --HEAD copperline` to build the latest
`main` instead of the most recent tagged release. Then run `copperline` from
the terminal.

## Install (Linux)

```sh
flatpak install flathub dev.copperline.Copperline   # any distribution
```

Or grab the single-file `Copperline-*.AppImage` from the
[releases page](https://github.com/CopperlineHQ/Copperline/releases),
`chmod +x` it and run. Both bundle the AROS boot ROM. Packaging sources are in
`packaging/`.

### Linux: Vulkan required

On Linux the display is presented through wgpu's **Vulkan** backend. The
OpenGL fallback is deliberately disabled: wgpu creates its EGL instance
without a display handle, so it silently selects Mesa's "surfaceless"
platform, which cannot be paired with an on-screen window. The symptom on a
Vulkan-less machine is the window flashing open and then exiting with:

```
ERROR copperline::video::window] pixels init failed: No suitable `wgpu::Adapter` found.
```

If you hit this, install a Vulkan driver:

- Hardware Vulkan (recommended): any GPU from roughly Intel Skylake / 2015
  onward ships one. Update your `mesa`/GPU driver package.
- Software fallback (older hardware, headless, or a VM) -- the lavapipe ICD:
  - Arch: `sudo pacman -S vulkan-swrast`
  - Debian/Ubuntu: `sudo apt install mesa-vulkan-drivers`
  - Fedora: `sudo dnf install mesa-vulkan-drivers`

Copperline does all rendering on the CPU and only asks the GPU to blit one
framebuffer per frame, so software Vulkan (lavapipe) is perfectly adequate.
The Flatpak runtime already includes lavapipe, so the Flatpak works without
any extra package. Setting `WGPU_BACKEND` overrides the backend selection if
you need to force a specific one for debugging.

## Build and run

```sh
cargo build --release
./target/release/copperline
```

The binary looks for `./copperline.toml`; if it isn't present, a bare
invocation opens the machine-configuration screen, starting from the built-in
defaults (an A500 Rev 6A: 68000 at ~7.09 MHz, ECS 8372A Agnus with OCS Denise,
PAL, real speed, and the bundled [AROS](https://www.aros.org/) ROM with
512 KiB chip plus 512 KiB trapdoor slow RAM). Any argument skips the screen
and boots directly: your own ROM as a positional argument, or a config file:

```sh
./target/release/copperline path/to/kickstart.rom
./target/release/copperline --config path/to/copperline.toml
```

Essential shortcuts use `Cmd` on macOS and `Alt` on Linux/Windows:
`Cmd+Q` / `Alt+Q` closes the window, `Esc` passes through to the Amiga (or
closes an open menu/window), `Cmd+S` / `Alt+S` saves a screenshot, and
`Cmd+B` / `Alt+B` opens the debugger. `Cmd+J` / `Alt+J` (or the status-bar
icon) toggles joystick input between gamepad-only and keyboard emulation.
The status bar, pop-up menu, tool windows (debugger and frame analyzer),
overlay panels, save/load state, input recording, and the full shortcut list
are documented in the
[user guide](docs/guide/ui.md).

## Configuration

Copy `copperline.example.toml` to `copperline.toml` and edit; every field is
optional and missing fields use documented defaults.

```toml
rom = "kickstart205.rom"

[cpu]
model = "68000"       # 68000, 68010, 68EC020, 68020, 68030, 68040, 68060
# fpu = true          # fit a 68881/68882 (default-on for the 68040)
# clock_mhz = 14.0    # defaults to the model's stock speed

[memory]
chip = "512K"         # OCS 512K, ECS/AGA up to 2M
fast = "0"            # Zorro II autoconfig fast RAM, up to 8M
slow = "512K"         # A500 trapdoor RAM at $C00000, up to 512K
# init = "random"     # deterministic garbage fill; or fixed "pattern:0x5555"

[chipset]
revision = "OCS"      # OCS, ECS, or AGA (picks the Agnus/Denise revisions)
video = "PAL"         # PAL or NTSC

[floppy]
# drives = 2           # wired mechanisms, 1-4; default is DF0 only

[floppy.df0]
path = "AmigaTestKit.adf"   # DD ADF / ADZ / DMS / IPF / SCP; omit for no disk
```

The full reference -- every key, machine profiles, Zorro boards, CD/HDD
images, validation rules, and audio options -- is in the
[configuration guide](docs/guide/configuration.md).

## Audio output

Real-time audio goes through [cpal](https://crates.io/crates/cpal), so the
same code drives CoreAudio, WASAPI and ALSA. By default it uses the system
default output; `--list-audio-devices` prints the alternatives and
`--audio-device NAME` (or `[audio] output_device`) selects one by
case-insensitive substring, falling back to the default if it disappears. The
device is also selectable in the configuration screen and switchable live from
the in-window menu (or `Cmd+Shift+A` / `Alt+Shift+A`), which additionally offers
"Disabled" to turn sound off entirely (equivalent to `--noaudio`).

Two host-side shaping options leave the emulated audio untouched:
`--audio-channel-mode mono` averages the left/right output into both channels,
and `--audio-stereo-separation 0-100` narrows the Amiga's hardware left/right
panning (100 = full, 0 = mono). Both are also `[audio]` keys and configuration-
screen fields.

## MIDI

Copperline can bridge Paula's serial port to the host's MIDI system, so an
Amiga sequencer or tracker plays real synths -- or is itself played from a
host MIDI keyboard -- over the emulated serial line. It is built in by default;
`cargo build --no-default-features` compiles it out, and such a build pulls no
MIDI code and links no MIDI framework.

The backend is selected at compile time and talks to each platform's native
API directly, with no wrapper crate: **CoreMIDI** on macOS, the **ALSA
sequencer** on Linux, and **WinMM** on Windows. Outgoing bytes are scheduled
so each one is delivered at the host instant it left the emulated wire,
keeping the guest's MIDI timing intact rather than collapsing it to whenever a
frame's worth of bytes is flushed.

List the host endpoints, then select them by name (a case-insensitive
substring is enough); `--midi-out`/`--midi-in` imply `--serial midi`:

```sh
./target/release/copperline --list-midi
./target/release/copperline --midi-out "FluidSynth" --midi-in "Keystation"
```

Devices can also be chosen in the launcher's **I/O Ports** tab, swapped live
from the in-window **MIDI In / MIDI Out** menu, or set in the config file:

```toml
[serial]
mode = "midi"            # off, stdout, midi, tcp, tcp-connect, or pty
midi_out = "FluidSynth"  # host destination; substring match
midi_in = "Keystation"   # host source
```

The ALSA development headers the Linux backend links against are already a
build requirement (see Requirements); macOS and Windows need nothing beyond
the OS.

## Parallel port

Copperline models the Centronics parallel port on CIA-A (data and `PC` strobe
on port B at `$BFE101`, printer `/ACK` back through CIA-A `FLAG`, the
BUSY/POUT/SEL status lines on CIA-B port A) and can attach one peripheral,
chosen by `[parallel] device`. With no `[parallel]` section the connector is
unplugged: the status lines float high on their pull-ups, so guest software
waits for a printer exactly as on a real machine with an empty port.

A **printer** captures the guest's raw byte stream to a file, preserving the
printer-language bytes verbatim for a compatible converter or spooler:

```toml
[parallel]
device = "printer"
output = "printer.raw"
```

A **sampler** is an 8-bit audio digitizer on the data lines -- the emulated
equivalent of a classic parallel-port sampler cartridge, driving AudioMaster,
ProTracker, OctaMED, TurboSound and the like. It captures from a host audio
input (via cpal, like live audio output) and can switch input device and gain
live:

```toml
[parallel]
device = "sampler"
sampler_input = "MacBook Air Microphone"  # omit for the system default
sampler_gain = 6.0                        # preamp gain in dB (0 = unity)
```

See the configuration guide for the full set of options, the equivalent
`--parallel` / `--sampler-*` flags, and the signal/interrupt behaviour.

## Documentation

User and developer documentation -- getting started, the UI and shortcuts,
headless capture, save states, input recording, the debugger frontends
(window, headless, remote GDB, and the JSON-RPC control protocol), the
configuration reference, and the
internals (timing model, chipset, CPU, video pipeline) -- is published at
[copperline.dev](https://copperline.dev/) and lives under `docs/` as a
[MyST](https://mystmd.org/) project you can also build locally:

```sh
npm install -g mystmd
cd docs && myst build --html      # static site in docs/_build/html
```

See `docs/README.md` for conventions and PDF output.

## Packaging

Copperline is distributed from source. On macOS this repository doubles as a
Homebrew tap (`Formula/`); on Linux it builds as a Flatpak for Flathub
(`packaging/flatpak/`) and as a portable AppImage (`packaging/appimage/`). It
also supports a `portable.txt` marker beside the executable (or downloaded
AppImage) to keep save-state slots and host preferences within the application
folder. It is not on crates.io: `Cargo.toml` sets `publish = false` because
Copperline is distributed as an application rather than a library. Release
steps for every channel are in
[`RELEASE.md`](RELEASE.md).

## What gets emulated

| Subsystem | Notes |
| --- | --- |
| M68K CPU | Via the published pure-Rust m68k crate; model selectable through 68060, accurate 68000 cycle counts, datasheet-based 68020 integer timing, 020+ caches, 6888x FPU, 68030/68040 MMUs. |
| Chip RAM | mem_map'd; reset starts with ROM overlaid at $0 until CIA-A releases /OVL. |
| Fast RAM | Optional Zorro II autoconfig RAM at $00200000 and Zorro III autoconfig RAM (`[memory] z3`); runs at the CPU clock. |
| Slow RAM | Optional A500 trapdoor/fake-fast RAM at $00C00000; arbitrated on the chip bus through Agnus like chip RAM. |
| ROM | Kickstart at $F80000 (512 KiB); optional extended ROM for CD32 ($E00000) and CDTV ($F00000). |
| CD32 FMV | Optional Commodore Full Motion Video cartridge: Zorro II autoconfig ROM/RAM, CL450 MPEG-1 video, L64111 MPEG Layer II audio, and analogue-keyed presentation. |
| Battery RTC | Oki MSM6242 or Ricoh RP5C01 at $DC0000 (`rtc_chip`; Ricoh is the A3000/A4000 default), read-only wall clock -- guest writes drive latch/bank/control state and the RP5C01's battery RAM, which persists to a WinUAE/Amiberry-compatible `.nvram` file (`battmem`). A `rtc_time` seed ticks in emulated time for reproducible runs; `rtc_frozen` pins it. |
| CIA-A / CIA-B | I/O ports, /OVL, timers, TOD, keyboard SDR/ICR, disk control/status lines, CIA-B FLAG disk index pulses, and the Centronics parallel port (data/strobe/ACK on CIA-A, BUSY/POUT/SEL on CIA-B). |
| Paula serial | SERDAT through a one-word transmit buffer and timed shift register, out to stdout, a TCP port, a pseudo-terminal, or -- with the default `midi` feature -- bridged to host MIDI in/out; SERDATR reports TBE/TSRE/RBF, and serial receive is fed from the selected input. |
| Paula audio | 4-channel DMA/sample playback, stereo mix, LED filter. |
| Paula DMACON / INTENA / INTREQ | IRQ bits are stored and delivered through manual M68K autovectors with modelled 68000 interrupt-recognition latency; audio and disk DMA raise completion IRQs. |
| Floppy / ADF / DMS / IPF / SCP | DF0-DF3 standard DD ADF read/write, read-only ADZ/DMS, UAE extended ADF, read-only IPF (decoded natively, no capsimg) and SCP flux import, track-timed disk DMA, CIA drive lines, index FLAG, DSKLEN/DSKBYTR/DSKSYNC/DSKDAT, per-drive multi-disk playlists with a swap key, and real 3.5" drives over a Greaseweazle through the pure-Rust FluxBridge library. |
| Hard disks | Gayle IDE (A600/A1200) and A4000 motherboard IDE; SCSI via the A2091 (Zorro II DMAC + WD33C93A), A4091 (Zorro III 53C710 with SCRIPTS), or A3000 Super DMAC; RDB HDFs, bare partition hardfiles, gzip-compressed hardfiles (HDZ), and host-directory volumes. |
| Host filesystem | `[[filesys]]` mounts serve host directories live as AmigaDOS volumes (read/write, `.uaem` attribute sidecars, Latin-1 name mapping). |
| WHDLoad | `--whdload game.lha` boots a WHDLoad-installed package directly: LHA extraction, slave-header machine derivation, a synthesized boot volume around the real WHDLoad, content-identified `Devs:Kickstarts/` staging, persistent saves (`docs/guide/whdload.md`). |
| Warp launch | `--run prog` boots straight into an Amiga executable from the host: a synthesized boot volume plus the program's directory mounted live, warp catch-up until the guest loads it, gdb stop-at-entry with `--gdb`, and a `loadseg` debugger break kind (`docs/guide/run.md`). |
| Expansion | Zorro II/III autoconfig chain, TOML-described RAM boards, WASM plugin boards (registers/interrupts/DMA in a sandboxed module), A2065 Ethernet (Am7990 LANCE) with loopback, userspace NAT, and direct host-adapter bridge backends, and the bundled HostSocket board (`bsdsocket.library` backed by a host-side TCP/IP stack over the same backends). |
| Agnus VPOSR / VHPOSR | Beam counters advanced per colour clock; PAL and NTSC timing (including NTSC long/short lines). |
| Agnus Copper | Beam-scheduled OCS Copper with COP1/COP2 jumps, WAIT, SKIP, DMAEN/COPEN gating, and chip-bus grants. |
| Agnus blitter | Scheduled per-slot engine: normal/line/fill modes, hardware per-word channel bus sequences (including the area-fill idle C slot), BBUSY/BZERO, BLTPRI "nasty" vs CPU starvation-yield arbitration, blit-done IRQ. |
| Denise BPLCON / COLORxx | Stored and replayed by beam position. |
| Bitplane renderer | OCS lo-res or hi-res; reads chip RAM via BPLxPT; honours modulos and beam-timed BPLCON1 scroll; EHB, HAM, dual playfield, and CLXDAT collisions. Completed frames render on a worker thread by default; `COPPERLINE_THREADED_RENDER=0` forces synchronous rendering. |
| Display window | winit 0.30 + pixels 0.17 surface; 716x285 framebuffer presented in the live window at 4:3 (716x537) plus a 44-pixel status bar with power/disk controls. TV-mode PNG screenshots and frame dumps present PAL and NTSC fields on the same 716x540 4:3 glass. |
| Keyboard / mouse / gamepad | Host keyboard/mouse mapped to Amiga input paths; key down and key up events go through CIA-A SDR/ICR with acknowledge + KDAT handshake backpressure and keyboard-MCU pacing; mouse deltas feed JOY0DAT; `Cmd+G` on macOS or `Alt+G` on Linux/Windows toggles host mouse capture; a USB gamepad (gilrs) or keyboard joystick emulation drives the port-2 digital joystick (JOY1DAT directions, /FIR1 fire, POT1Y button 2); Ctrl+Ami+Ami resets. |
| OCS sprites | 8 DMA/manual 16-pixel sprites, attached sprites, composited over bitplanes with playfield priority. |
| Chip bus arbitration | Per-colour-clock OCS slot ownership for refresh, display DMA, sprites, disk, audio, Copper, blitter, and CPU chip/custom accesses, with CPU wait states. |
| ECS | ECS Agnus revisions (8372A/8375) and ECS Denise (8373): up to 2M chip RAM, DIWHIGH, BEAMCON0, SuperHires, ECS blitter (BLTSIZV/BLTSIZH), programmable geometry. |
| AGA | Alice/Lisa: 8 bitplanes, 256-entry 24-bit palette (plus the genlock T bit) with BANK/LOCT, HAM8, FMODE wide bitplane/sprite fetch, 35 ns BPLCON3 SPRES output, BPLCON4, CLXCON2; A1200/CD32 profiles. Remaining gaps are recorded in the internals docs. |

The detailed architecture (source layout, the bus, the replay renderer) and
timing model live in the [internals docs](docs/internals/architecture.md).

## Tests

`cargo test` runs the unit suite, which needs no external assets. The
integration tests under `tests/` are marked `#[ignore]` because they run the
emulator against local Kickstart ROM and disk images that are not part of
the repository; they skip cleanly when the assets are absent. See
[`tests/README.md`](tests/README.md) for the asset list and lookup
directory, and the [headless guide](docs/guide/headless.md) for the vAmigaTS
compatibility runs. `timing-test/` is a bootable disk that measures CPU and
chip-bus timings against the CIA E-clock for cross-emulator comparison.

## Known quirks

- Lo-res content is pixel-doubled horizontally inside the framebuffer; the
  window presents the field at a TV-like 4:3 aspect plus the status bar.
- The CPU may halt with an `EXCEPTION` on an unimplemented feature (an exotic
  custom register or CIA edge case). This is non-fatal: the window stays
  alive showing the last framebuffer, and the debugger can inspect the
  halted state.

## Community

The project Discord is where development, bug hunting, and general Amiga
talk happen: [discord.gg/HDTjt3tYAC](https://discord.gg/HDTjt3tYAC). Bug
reports and feature requests are best filed as GitHub issues so they do not
scroll away; [`CONTRIBUTING.md`](CONTRIBUTING.md) covers what a useful
report looks like and the hardware-first rule that patches follow.

If you would like to support development, [`FUNDING.md`](FUNDING.md) lists
the options -- [Patreon](https://www.patreon.com/cw/Copperline) for ongoing
support of Copperline, or GitHub Sponsors, Ko-fi, and PayPal for one-off
contributions. Copperline stays GPL and freely available regardless;
funding buys development time, real hardware to measure against, and the
Apple and Windows code-signing subscriptions needed to ship binaries that
open without a Gatekeeper or SmartScreen warning.

## Credits

- The [AROS Research Operating System](https://www.aros.org/), bundled in
  [`assets/aros`](assets/aros) as the default boot ROM. AROS is an
  open-source re-implementation of the AmigaOS API, distributed under the
  AROS Public License; the ROM images here are unmodified from the official
  m68k nightly build (see [assets/aros/README.md](assets/aros/README.md)).
- [DiagROM](https://www.diagrom.com/) by John "Chucky" Hertell,
  licensed for free use.
- The MIT-licensed [m68k](https://crates.io/crates/m68k) CPU core.
- The public-domain `font8x8` glyphs by Daniel Hepper / Marcel Sondaar
  for the on-screen overlay font.
- The Amiga Hardware Reference Manual for register-level documentation.
- The [DESiRE](https://demozoo.org/groups/1077/) demo group, whose practice of
  releasing the source code to some of their demos has been invaluable for
  debugging Copperline's hardware modelling against real-world code.

## License

Copperline is free software, released under the GNU General Public
License version 3 or (at your option) any later version. See
[LICENSE](LICENSE) for the full text. Its
[m68k](https://crates.io/crates/m68k) dependency is MIT licensed.

## Trademarks

Amiga and Commodore are trademarks of their respective owners. Copperline
is an independent, unofficial project and is not affiliated with, sponsored
by, or endorsed by any trademark holder.

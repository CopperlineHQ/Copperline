![Copperline](assets/brand/copperline-logo.png)

Website: [copperline.dev](https://copperline.dev/) |
Online demo: [copperline.dev/try](https://copperline.dev/try/) |
Chat: [Discord](https://discord.gg/HDTjt3tYAC) |
Support: [Patreon](https://www.patreon.com/cw/Copperline)

Copperline is a cycle-driven Commodore Amiga emulator (OCS, ECS, and AGA) written in Rust. It models the Amiga custom chipset, 680x0 CPU, memory subsystems, and common expansion hardware on a unified clock timeline.

It boots out of the box with the bundled open-source AROS Kickstart replacement, and supports official Kickstart ROMs (1.x through 3.2, plus the CDTV and CD32 extended ROMs), DiagROM, and standard disk, hardfile, and CD media.

The [manual](https://copperline.dev/docs/),
[troubleshooting and known limits](docs/guide/troubleshooting.md), and
[changelog](CHANGELOG.md) cover setup and upgrades. Match the manual's version
label to **About**, which includes the revision for an untagged source build;
`copperline --version` prints only the package version. Each release carries
its own PDF manual.

## Features

- **Chipset and machine models**: OCS, ECS, and AGA support with profiles from the A1000 and A500 through the A4000 (including A500+, A2000, A600, A1200, A3000), plus CDTV and CD32.
- **Configurable CPU and memory**: Motorola 68000 through 68060 (including EC variants), optional FPU and MMU support, and configurable Chip, Fast, Slow, motherboard, accelerator, and Zorro III RAM.
- **Accelerated execution**: Optional 68020+ JIT, either at a fixed, reproducible clock or in a `fastest` mode that gives the CPU whatever host time the real-time chipset leaves. See [CPU configuration](docs/guide/configuration.md).
- **Storage and media**: Floppy disk images (DD and HD ADF, ADZ, DMS, IPF with deterministic weak-bit revolutions, SCP, and ZIP-packed ADF), physical floppy drives via Greaseweazle (FluxBridge), IDE (Gayle, A4000, and lide boards), SCSI (A2091, A3000, A4091), virtual hard disks (`copperhf.device`), CHD and VHD hardfiles, PCMCIA CF/SRAM cards, SF2000 SD, CD-ROM (with CD32 FMV, CD32 NVRAM, and CDTV bookmark RAM), SCSI tape drives backed by host files, physical host disks, and live host directory mounts.
- **Audio and video**: 4-channel Paula audio, Toccata 16-bit sound board, MHI MPEG audio decoder board, RTG graphics cards (Picasso II/II+, Graffity, Z3660), NewTek Video Toaster 4000 and Video Flyer emulation, Smart Framing with Autocrop and integer scaling (including native RTG integer scaling), AVI/MP4/GIF capture, host MIDI in/out bridging, and built-in Roland MT-32 and General MIDI (Coppersynth) synthesis.
- **Expansion and networking**: Zorro II/III autoconfig, A2065 Ethernet, host-backed `bsdsocket.library` (HostSocket), ZZ9000 SDK crypto acceleration (`zz9k`), Hayes AT modem and host serial bridging, opt-in clipboard sharing, and sandboxed WebAssembly expansion plugins.
- **Debugging**: Unified [Debug workspace](docs/debugger/window.md) (Debugger, Frame Analyzer, and Console beside the display in one desktop window, opened with `Cmd+B` / `Alt+B`), reverse stepping, live Kickstart/AROS symbols, frame and [instruction profiling](docs/debugger/profiling.md), VCD waveforms, disk-fault injection, A/B divergence finding, and source debugging through GDB or DAP. Both the native VS Code extension and the [Copperline fork of Bartman's extension](docs/debugger/vscode-bartman.md) are supported.
- **Automation and replay**: [Save states](docs/guide/ui.md#save-states), [WinUAE state import](docs/guide/winuae-state.md), headless input scripts and captures, and a JSON-RPC control protocol. `copperline-ctl` also provides DAP (`--dap`) and MCP (`--mcp`) servers. GDB and TCP DAP listen on loopback unless `--allow-remote-debugging` is supplied; control tokens are handed off through an info file and kept out of logs.
- **Freezer cartridge**: Action Replay-style cartridge support with bundled HRTMon (`--cartridge hrtmon`), allowing running software to be frozen into the monitor via the menu, a hotkey, headless `--freeze-after`, or the control protocol.
- **Direct launching**: Boot directly into WHDLoad game packages (`--whdload`) or host-built Amiga executables (`--run`, including bare Kickstart 1.3), with a WinUAE-compatible `uaelib` trap allowing guest code to control warp speed, log debug messages, and register debug resources.
- **WebAssembly and publishing**: Run directly in modern web browsers at [copperline.dev/try](https://copperline.dev/try/), or bundle standalone native and single-file web players with the [publishing kit](docs/guide/publishing.md).
- **Libretro core**: [Run Amiga and CD32 software in RetroArch](docs/guide/libretro.md), with embedded AROS ROMs, WHDLoad, disk playlists, save states, and RetroArch rollback netplay.
- **Terminal display**: Interactive sessions inside Kitty-protocol terminals (`--terminal`), including over SSH.

## Installation

### macOS (Homebrew)

```sh
brew tap copperlinehq/copperline https://github.com/CopperlineHQ/Copperline
brew trust copperlinehq/copperline
brew install copperline
```

Homebrew 7 and later load formulae only from taps you have trusted, which is
what `brew trust` records; on an older Homebrew, leave that line out.

To build from the latest development commit instead:

```sh
brew install --HEAD copperline
```

Pre-built disk images (`.dmg`) are also available on the [releases page](https://github.com/CopperlineHQ/Copperline/releases).

### Linux (AppImage)

Standalone AppImage binaries are available on the [releases page](https://github.com/CopperlineHQ/Copperline/releases):

```sh
chmod +x Copperline-*.AppImage
./Copperline-*.AppImage
```

*Note:* On Linux, presentation requires a Vulkan driver. Most modern GPUs support Vulkan natively. For virtual machines or older hardware, install the Mesa software Vulkan driver (`mesa-vulkan-drivers` on Debian/Ubuntu/Fedora, or `vulkan-swrast` on Arch).

### Windows

Download `Copperline-X.Y.Z-win-x64-setup.exe` or
`Copperline-X.Y.Z-win-arm64-setup.exe` from the
[releases page](https://github.com/CopperlineHQ/Copperline/releases), matching
your PC's architecture. The installer installs for just you (no
administrator rights) or for all users in Program Files, and adds a Start
menu shortcut. Run a newer installer to update; uninstall through Windows
Settings. Your settings and saves are preserved.

Portable ZIPs remain available for both architectures; extract the whole
archive and run `copperline.exe`. See the [Windows installation
guide](docs/guide/getting-started.md#installing-on-windows).

### Building from source

```sh
cargo build --release
```

Run the resulting `target/release/copperline` binary. `--release` is a Cargo
build option; unoptimized debug builds are too slow for real-time emulation.

Dependencies:

- Rust 1.95+ (tested on stable)
- Windows ARM64 source builds: LLVM clang on `PATH` for Internet netplay's crypto backend. The Windows packaging script also finds Visual Studio's bundled Clang tools.
- Fedora build dependencies: `sudo dnf install alsa-lib-devel systemd-devel gcc`
- Debian/Ubuntu build dependencies: `sudo apt install libasound2-dev libudev-dev pkg-config gcc`

## Quick Start

Run Copperline from the terminal:

```sh
./target/release/copperline
```

Running with no arguments and no `copperline.toml` (in the working directory or beside the executable) opens the interactive configuration launcher (defaulting to an A500 profile with the bundled AROS ROM).

An installed build is run as `copperline`; the `./target/release/` prefix applies to a build from source. A machine booted on the bundled AROS ROM with no disk inserted shows the AROS logo on a black screen with the message "Waiting for bootable media" above a row of boot-media icons.

To boot directly into a Kickstart ROM, configuration file, or floppy image:

```sh
./target/release/copperline path/to/kickstart.rom
./target/release/copperline path/to/copperline.toml
./target/release/copperline --model A1200 --fast 8M KICK31.ROM game.adf
./target/release/copperline disk1.adf disk2.adf
./target/release/copperline game.lha
```

Positional files are classified like window drops: floppy images boot in DF0,
multiple floppies form its swap playlist, and WHDLoad packages boot directly.
CD images require a CD32/CDTV configuration. Desktop packages offer **Open with**;
see [Opening files](docs/guide/getting-started.md#opening-files) for registration
and supported formats.

Long options also accept `=` syntax (such as `--model=A1200` or
`--screenshot-after=10 out.png`), and window sizing and placement can be
controlled with `--window-scale`, `--maximized`, `--full-screen`, `--monitor`,
and `--window-position`. See [command-line overrides](docs/guide/configuration.md#command-line-overrides)
for the complete flag reference.

### Essential keyboard shortcuts

- **Quit**: `Cmd+Q` (macOS) / `Alt+Q` (Linux/Windows)
- **Debugger**: `Cmd+B` (macOS) / `Alt+B` (Linux/Windows)
- **Screenshot**: `Cmd+S` (macOS) / `Alt+S` (Linux/Windows) saves the displayed picture with its current effects.
- **Native screenshot (1:1)**: `Cmd+Option+S` (macOS) / `Ctrl+Alt+S` (Linux/Windows) saves the original playfield pixels.
- **Mouse capture**: `Cmd+G` (macOS) / `Alt+G` (Linux/Windows)
- **Joystick toggle**: `Cmd+J` (macOS) / `Alt+J` (Linux/Windows) switches between physical gamepad and keyboard arrows.

See the [UI guide](docs/guide/ui.md) for the full menu, shortcut, mouse
sensitivity, and status-bar reference.

## Configuration

Copperline uses TOML configuration files. You can copy `copperline.example.toml` to `copperline.toml` to customize machine settings:

```toml
rom = "kickstart31.rom"

[cpu]
model = "68020"

[memory]
chip = "2M"
fast = "8M"

[chipset]
revision = "AGA"
video = "PAL"

[floppy.df0]
path = "game.adf"
```

See the [configuration guide](docs/guide/configuration.md) for the complete reference of all configuration options, machine profiles, and storage settings.

## Documentation

The manual is published at [copperline.dev/docs](https://copperline.dev/docs/) and available under `docs/`:

- [Getting Started](docs/guide/getting-started.md) - Installation, building from source, and first boot
- [Configuration](docs/guide/configuration.md) - Configuration file reference, machine profiles, and CLI options
- [Troubleshooting](docs/guide/troubleshooting.md) - Version matching, compatibility, and known limits
- [User Interface](docs/guide/ui.md) - Windows, menus, recordings, shortcuts, and gamepad calibration
- [Terminal Display](docs/guide/terminal.md) - Running interactively inside Kitty-protocol terminals
- [WHDLoad Support](docs/guide/whdload.md) - Direct WHDLoad package loading and library management
- [Direct Executable Launching](docs/guide/run.md) - Running cross-compiled Amiga binaries
- [Floppy Hardware Bridge](docs/guide/fluxbridge.md) - Real floppy drives via Greaseweazle
- [Real Hard Disks](docs/guide/host-disks.md) - Attaching physical host disks and storage cards
- [MT-32](docs/guide/mt32.md), [Coppersynth](docs/guide/coppersynth.md), and [Modem](docs/guide/modem.md) - Built-in MIDI synthesizers and Hayes AT modem emulation
- [UAE Import](docs/guide/import-uae.md) and [WinUAE States](docs/guide/winuae-state.md) - Importing existing configurations and save states
- [Rollback Netplay](docs/guide/netplay.md) - Two- to four-player rollback sessions and spectators across desktop and browser, with automatic media sharing
- [Headless Mode](docs/guide/headless.md) - Scripted runs, captures, and automated verification
- [Browser](docs/guide/browser.md), [Libretro](docs/guide/libretro.md), and [Publishing](docs/guide/publishing.md) - WebAssembly frontend, RetroArch core, and standalone native/web player kits
- [Zorro Board Plugins](docs/zorro.md) - Data-driven and WebAssembly expansion boards
- [Debugging](docs/debugger/window.md) - Debug workspace, console, headless debugger, reverse stepping, waveforms, profiling, and GDB
- [VS Code](docs/debugger/vscode.md) and [Bartman with Copperline](docs/debugger/vscode-bartman.md) - IDE source debugging and visual profiling
- [Debug Adapter Protocol](docs/debugger/dap.md) and [Control Protocol](docs/debugger/control.md) - IDE DAP (`--dap`), JSON-RPC automation (`copperline-ctl`), and MCP (`--mcp`)
- [Internals](docs/internals/architecture.md) - Architecture, chipset, timing, peripherals, and save-state format

To build the documentation locally:

```sh
npm install -g mystmd
cd docs
python3 ../tools/prepare-docs.py
myst build --html
```

`docs/README.md` covers the PDF build and the full set of checks CI runs.

## Testing

Every PR runs a curated 68020/030/040/060 instruction corpus and asset-free
MMU/native-JIT regressions against the resolved `m68k` dependency. See
[CPU conformance](crates/cputest-runner/README.md) for local commands and
coverage limits; scheduled runs add full generated instruction sets.

Run the unit suite and fast integration tests:

```sh
cargo test
```

Run the optimized native suite, including probe goldens and save-state checks:

```sh
cargo test --profile ci --locked
```

Slow tests are reported as ignored in debug builds. Select asset-backed tests
explicitly once their required files are installed, for example:

```sh
cargo test --release --test image_regression kickstart_boot_screen -- --ignored
```

Selected image regressions fail if their assets are missing.

Every PR also runs the [AROS performance gate](docs/internals/jit-performance.md#ci-aros-performance-gate):
release builds of the candidate and a tracked baseline run on the same host,
covering A500/A1200 core execution and rendering. A median slowdown over 10%
fails the check; raw samples and build provenance are retained.

`cargo test --test fuzz_regressions` replays retained parser inputs without
nightly or external media. [Fuzzing instructions](fuzz/README.md) cover the
nine libFuzzer targets, brief PR smoke runs, and daily sustained campaigns.

See [`tests/README.md`](tests/README.md) for details on asset paths and test configuration.

## Community and Support

- Chat with developers and users on [Discord](https://discord.gg/HDTjt3tYAC).
- Report bugs and request features via [GitHub Issues](https://github.com/CopperlineHQ/Copperline/issues).
- Contributing guidelines are detailed in [CONTRIBUTING.md](CONTRIBUTING.md).
- Support ongoing development on [Patreon](https://www.patreon.com/cw/Copperline) (see [FUNDING.md](FUNDING.md)).

## Credits

See [CREDITS.md](CREDITS.md) for contributor and third-party software credits.

- [AROS Research Operating System](https://www.aros.org/) (bundled boot ROM)
- [DiagROM](https://www.diagrom.com/) by John "Chucky" Hertell
- [HRTMon](https://github.com/wepl/hrtmon) by Alain Malek, Bert Jahn and contributors (bundled freezer-cartridge monitor, GPL-2.0-or-later)
- [m68k](https://crates.io/crates/m68k) CPU core
- Toni Wilen (WinUAE developer), whose [blitter hardware observations](https://eab.abime.net/showpost.php?p=1426048) helped guide the One ROM A500 timing experiments.

## License

Copperline is free software released under the GNU General Public License version 3 or later. See [LICENSE](LICENSE) for details.

## Trademarks

Amiga and Commodore are trademarks of their respective owners. Copperline is an independent, unofficial project and is not affiliated with or endorsed by any trademark holder.

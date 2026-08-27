# Getting started

Copperline can be run as a native desktop application or in the browser
at [copperline.dev/try](https://copperline.dev/try/). This chapter covers
system requirements, installation, building from source, and initial setup.

## System requirements

- **Rust:** 1.93 or newer (tested with Rust 1.96).
- **Supported operating systems:** macOS, Linux, and Windows.
- **Graphics backend:** Metal on macOS, Direct3D 12 on Windows, and Vulkan on Linux
  (see [](#vulkan-is-required-on-linux)).
- **Linux build dependencies (Fedora):** `sudo dnf install alsa-lib-devel systemd-devel gcc`.
- **Boot ROM:** Copperline includes the open-source [AROS](http://www.aros.org/)
  Kickstart replacement and boots it by default. It also supports official
  Commodore Kickstart ROMs (1.3, 2.05, 3.1, plus CDTV and CD32 extended ROMs)
  and [DiagROM](https://www.diagrom.com/). Standard Kickstart ROM images must
  be 512 KiB.

## Installing on macOS (Homebrew)

To install using Homebrew:

```sh
brew tap copperlinehq/copperline https://github.com/CopperlineHQ/Copperline
brew install copperline
```

To build directly from the latest development commit:

```sh
brew install --HEAD copperline
```

Pre-built macOS application bundles (`Copperline-X.Y.Z-macos-universal.dmg`) are
also available on the [releases page](https://github.com/CopperlineHQ/Copperline/releases).
Mount the disk image and copy `Copperline.app` to `/Applications`. If macOS
quarantine blocks initial launch, right-click the app and choose **Open**, or run:

```sh
xattr -dr com.apple.quarantine /Applications/Copperline.app
```

## Installing on Linux

### Flatpak

Flatpak packages include all runtime dependencies and work across distributions:

```sh
flatpak install flathub dev.copperline.Copperline
flatpak run dev.copperline.Copperline
```

### AppImage

Standalone AppImage binaries are provided on the
[releases page](https://github.com/CopperlineHQ/Copperline/releases):

```sh
chmod +x Copperline-*.AppImage
./Copperline-*.AppImage
```

(vulkan-is-required-on-linux)=
### Vulkan is required on Linux

On Linux, presentation uses the Vulkan backend via `wgpu`. If no Vulkan adapter
is found, the application exits at launch.

Modern GPUs generally provide hardware Vulkan support through Mesa drivers.
For virtual machines or older hardware, install the software Vulkan driver (lavapipe):

- **Arch Linux:** `sudo pacman -S vulkan-swrast`
- **Debian / Ubuntu:** `sudo apt install mesa-vulkan-drivers`
- **Fedora:** `sudo dnf install mesa-vulkan-drivers`

The Flatpak build bundles lavapipe by default.

## Building from source

```sh
cargo build --release
```

```{warning}
Always run Copperline with `--release`. Unoptimized debug builds are not fast
enough for real-time emulation.
```

To run the test suite:

```sh
cargo test                          # Unit tests (no external assets required)
cargo test --release -- --ignored   # Integration tests (requires local test media)
```

## First boot

Run Copperline from the terminal:

```sh
./target/release/copperline
```

When started with no arguments and no `copperline.toml` in the current directory,
Copperline displays the interactive launcher screen where you can configure
machine models, memory, storage, and peripherals.

The default configuration is an Amiga 500 (Rev 6A) with an OCS/ECS chipset,
512 KiB chip RAM, 512 KiB slow RAM, and the bundled AROS Kickstart replacement.

To boot directly into a specific Kickstart ROM or configuration file:

```sh
./target/release/copperline path/to/kickstart.rom
./target/release/copperline --config path/to/copperline.toml
```

You can also specify machine parameters via command-line flags:

```sh
./target/release/copperline --model A1200 --fast 8M KICK31.ROM
```

See [](configuration#command-line-overrides) for the full list of CLI flags.

```{figure} ../images/kick13-insert-disk.png
:alt: Kickstart 1.3 insert-disk screen
:width: 75%

Kickstart 1.3 waiting for a boot floppy.
```

To mount a floppy disk image on boot:

```toml
rom = "KICK13.ROM"

[floppy.df0]
path = "Game.adf"
```

Supported floppy formats include ADF, ADZ (gzip-compressed ADF), single-file ZIP,
DMS, extended ADF, IPF, and SCP. In interactive sessions, disk images can also
be inserted via drag-and-drop into the emulator window.

## Example configuration

A fully commented example configuration is available in `copperline.example.toml`
in the root of the repository. Copy it to `copperline.toml` or pass it via `--config`:

```sh
./target/release/copperline --config copperline.example.toml
```

## Logging and crash reports

Set `RUST_LOG=debug` or `RUST_LOG=trace` in the environment to enable detailed logging.

If an unhandled panic occurs, Copperline writes diagnostic output and a backtrace
to `copperline-crash.txt` (attempted next to the executable first, falling back to
the current working directory, and then to the system temporary directory).
Please include this file when reporting bugs.

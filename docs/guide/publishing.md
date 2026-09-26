# Standalone game player (Player kit)

Copperline can be built as a standalone game player: an executable made for a
single game, with no launcher and no debugging interface.

The distributable package contains the player executable, the game payload as
a separate sidecar file, the open-source AROS boot ROM pair, the open CD32 FMV
cartridge ROM, and license documentation.

## The game manifest

A single TOML file defines the player build. Everything in it is fixed at
compile time; `crates/copperline-player/game.example.toml` is a commented
template:

```toml
[game]
title = "Skyhawks"            # Window title and bundle name
id = "skyhawks"              # Per-user settings/saves directory name
version = "1.2"              # Optional release version (--version, bundle name)

[payload]                    # Exactly one payload kind
cd = "skyhawks.iso"          # CD image (ISO, CUE/BIN, CHD)
# adf = "skyhawks.adf"       # Floppy disk image for DF0
# run = { files = "amiga", executable = "Skyhawks", args = "" }
# sha256 = "..."             # Optional SHA-256 payload integrity check

[machine]
model = "CD32"               # A500, A500+, A600, A1200, A3000, A4000, A1000, CDTV, CD32
# chip = "2M"                # Optional memory overrides
# fast = "8M"
# slow = "512K"

[display]
shader = "crt"               # Default CRT shader: none, scanlines, mask, crt
bezel = "1084"               # Default bezel: off, 1084, classic
fullscreen = true            # Launch in fullscreen

[features]
save_states = false          # Offer quick save / quick load slots in the menu

[branding]
icon = "icon.png"            # Window/dock icon PNG, relative to the manifest
```

The `[display]` values are only defaults; the player's menu can change them,
and the user's choices persist. `id` must be a plain directory name (letters,
digits, `.`, `-` and `_`). The build fails on an unknown model, a missing
payload kind, or a payload named by a path rather than a bare file name.

### Supported payload types

The `[payload]` section requires exactly one of the following:

- **`cd`:** A CD image inserted in the machine's CD drive at power-on and read
  straight from the bundle. CD32 saves persist through the console's NVRAM in
  the per-game directory.
- **`run`:** A directory of game files beside the executable (`files`) and the
  Amiga program inside it (`executable`, with optional `args`), booted the way
  [`--run`](run.md) boots one. The files are copied into the per-game directory
  on first launch, so the game's own saves persist and survive updates.
- **`adf`:** A floppy image in `DF0:` at power-on. The player also runs from a
  per-user copy, so guest writes persist.

The optional `sha256` key pins the payload of a `cd` or `adf` build: the player
refuses to start if the sidecar file does not match. It is not supported for
`run` payloads. When a new release changes an `adf` or `run` payload, the
player replaces its per-user copy: an updated ADF starts afresh, while in a
`run` tree only the payload's own files are replaced and files the game
created are kept. Bump `version` for an update that keeps every file size and
timestamp, so the change is still noticed.

## Building and packaging

To build using Cargo:

```sh
COPPERLINE_GAME_MANIFEST=/path/to/game.toml \
  cargo build --release --manifest-path crates/copperline-player/Cargo.toml
```

Without `COPPERLINE_GAME_MANIFEST`, the example manifest is built, which only
checks that the crate compiles. The payload is never embedded in the binary.

To build and package a complete bundle for the host platform:

```sh
tools/publish --manifest /path/to/game.toml
```

`tools/publish` takes the payload from beside the manifest unless
`--payload PATH` names it (its file name must still match the manifest), writes
into `target/publish` unless `--out DIR` is given, and with `--skip-build`
reuses the player binary already built. Package outputs:

- **macOS:** An ad-hoc signed application bundle (`<title>.app`, with the
  payload and assets under `Contents/Resources/`) and
  `<title>-<version>-macos.zip`.
- **Linux and Windows:** A directory named after the game `id`, with the
  payload and assets next to the executable, archived as
  `<title>-<version>-linux.tar.gz` or `<title>-<version>-windows.zip`.
- **`--portable`:** Adds a `portable.txt` beside the executable so all user
  settings and save data stay inside the bundle directory.

The player is built without the control server and GDB stub, ignores every
`COPPERLINE_*` environment variable, and disables the debugging and
input-recording shortcuts.

## User experience

The game launches directly into the title screen.

Pressing `Cmd+E` (macOS) or `Alt+E` (Linux/Windows) or the controller Menu/Guide
button opens a simplified in-game menu:

- **Video Settings:** menu size, pixel aspect, scaling, autocrop, centring,
  CRT shader and its strength, screen tint, fullscreen, VSync and monitor
  bezel
- **Audio Settings:** output device (or Disabled) and the audio filter
- **Input Settings:** port devices, joystick input mode, autofire, gamepad
  calibration and input mapping
- **Pause** and **Reset**
- **Save State** quick save and quick load slots (if enabled in the manifest)
- **About...** and **Quit**

Menu changes are saved to `settings.toml` in the per-game directory. User
configuration, game saves, NVRAM, and save states are stored there
(`~/.config/<id>/` on Linux and macOS, `%APPDATA%\<id>\` on Windows), or in
the application directory in portable mode.

The player accepts `--fullscreen` or `--windowed` to override the saved
display mode for one launch, and `--version` to print the game title and
version.

## Automated bundle verification

The player binary supports headless screenshot capture for CI test validation:

```sh
"Skyhawks.app/Contents/MacOS/skyhawks" --screenshot-after 30 boot.png
```

`--screenshot-after SECS PATH` (repeatable) runs the game without a window,
silent and unthrottled, saves the PNG, and exits.

## Licensing requirements

Bundles generated by `tools/publish` include Copperline's GPL-3.0-or-later
license text, a README naming the source repository and the release tag the
player was built from, the third-party font notices, and the AROS license
(APL 1.1) and acknowledgement files beside the ROM images.

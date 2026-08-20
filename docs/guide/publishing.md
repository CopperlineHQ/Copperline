# Publishing a game (the player kit)

Copperline can be built as a dedicated, launcher-free player for one game:
a small binary that boots straight into the title, with the machine model
and RAM fixed at build time, no status bar, no debugger, and an in-game
menu carrying only what an end user of a shipped game changes -- CRT
shader, bezel, fullscreen, controller setup, and (if the publisher opts
in) save states. The distributable is a bundle: the player binary, the
game payload as a *sidecar file next to the binary*, the AROS boot ROM
pair, and the license documents.

The payload is deliberately not embedded in the binary. The player stays
pure GPLv3 Copperline code and the game remains a separate work distributed
alongside it (mere aggregation, the model commercial ScummVM releases use),
so publishing a commercial title needs no license exception -- just passing
on the GPL text and a source link, which the kit generates.

## The game manifest

One TOML file describes the product; `crates/copperline-player/build.rs`
bakes it into the binary as constants. The commented reference is
`crates/copperline-player/game.example.toml`:

```toml
[game]
title = "Skyhawks"           # window title
id = "skyhawks"              # per-user save/settings directory name
version = "1.2"              # optional; names the bundle

[payload]                    # exactly one payload kind
cd = "skyhawks.iso"          # CD32/CDTV disc image (iso/cue/chd), or
# adf = "skyhawks.adf"       # floppy in DF0, or
# run = { files = "amiga", executable = "Skyhawks", args = "" }
# sha256 = "..."             # optional integrity pin on the sidecar

[machine]
model = "CD32"               # any machine model: A500, A1200, CD32, ...
# chip = "2M"                # optional RAM overrides on the profile
# fast = "8M"

[display]                    # defaults only; users change them in the menu
shader = "crt"
bezel = "1084"
fullscreen = true

[features]
save_states = false          # offer quick save/load slots in the menu

[branding]
icon = "icon.png"            # window/dock icon, replaces Copperline's
```

Payload kinds:

- **cd**: the disc is in the machine's CD drive at power-on and boots the
  way a real CD32/CDTV disc does. Saves persist through the console's
  NVRAM, which lands in the per-game config directory.
- **run**: an ordinary Amiga executable with its data files, booted the
  way `--run` boots one ([Warp launch](run.md)), warp-booting to the program's load.
  On first launch the files tree is copied into the per-game config
  directory; guest writes land in the copy, so saves persist and survive
  updates (an updated payload re-copies its own members and leaves
  everything else alone).
- **adf**: the image is in DF0 at power-on, also from a per-user copy so
  the guest can write to it.

The optional `sha256` pins the payload: the player refuses to start when
the sidecar does not match, so a bundle cannot be quietly retargeted at a
different disc. Leave it out to let the payload be patched without
rebuilding the player. It applies to file payloads only.

## Building and bundling

```sh
COPPERLINE_GAME_MANIFEST=/path/to/game.toml \
  cargo build --release --manifest-path crates/copperline-player/Cargo.toml
```

or let `tools/publish` do the whole thing -- build, stage, license
documents, archive:

```sh
tools/publish --manifest /path/to/game.toml
```

On macOS that produces a signed (ad-hoc) `.app` plus a zip of it, with the
payload and AROS under `Contents/Resources/`; on Linux and Windows a
directory plus archive with them beside the executable. `--portable` adds
a `portable.txt`, so per-user data stays inside the bundle directory --
the right shape for itch.io-style zips. Because the machine is baked at
compile time, each platform's bundle is built on (or for) that platform;
the practical route for a multi-platform release is a CI matrix that runs
`tools/publish` per OS runner.

The player binary carries only the `frontend` and `cpu-jit` features of
the root crate: no control server, no GDB stub, no game-library HTTP
stack, no synthesizers or wasm boards. Every `COPPERLINE_*` environment
knob is sealed off at startup, so a shipped game exposes no debugging
surface at all.

## What the end user gets

The game opens straight into play (fullscreen if the manifest says so).
`Cmd+E` / `Alt+E` opens the trimmed menu -- Video, Audio, and Input
settings, Pause, Reset, optionally the save-state slots, About, and Quit
-- and a gamepad drives it too: the Select/Back or guide button opens the
menu (or a calibrated **Open menu** binding; see [the UI chapter](ui.md)), the d-pad
walks it, fire activates, and the second button backs out. Everything set
in the menu persists per game in `settings.toml` under the per-game config
directory (`~/.config/<id>/` and platform equivalents), alongside gamepad
calibration, keymaps, saves, and NVRAM.

## Verifying a bundle

The player keeps one hidden flag for exactly this: `--screenshot-after
SECS PATH` runs the machine headless and unthrottled, writes the
framebuffer at the emulated timestamp, and exits -- the same deterministic
verification the main binary offers ([Headless and scripted runs](headless.md)). A publisher's CI
can boot the assembled bundle and compare the screenshot against a known
good frame:

```sh
"Skyhawks.app/Contents/MacOS/skyhawks" --screenshot-after 30 boot.png
```

For everything richer -- scripted input, save states, the control
protocol -- test with the full Copperline against the same payload, and
ship with the player.

## Licensing obligations, in short

The bundle `tools/publish` assembles already contains what redistribution
requires: Copperline's GPL-3.0-or-later text with a corresponding-source
link (a tagged-release URL suffices for unmodified builds), and the AROS
ROM pair's APL 1.1 license and acknowledgements beside the ROM files. The
game payload itself stays the publisher's own work under their own terms.
A publisher who *modifies* the player or the emulator must publish those
modified sources; the manifest and payload are not modifications.

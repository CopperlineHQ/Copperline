# Standalone game player (Player kit)

Copperline can be built as a standalone game player: an executable made for a
single game, with no launcher and no debugging interface. The same game
manifest also packs a floppy title into a [single HTML file](#web-player-kit)
that plays in a browser.

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
The player crate is a member of the repository's Cargo workspace, so the
binary lands in the shared `target/release/` directory at the repository root.

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
  `<title>-<version>-macos.zip`. Like Copperline itself, the bundle needs
  macOS 14.2 or newer and declares that, so older systems refuse to open it
  instead of crashing at launch.
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

- **Video Settings:** menu size, pixel aspect, framing, scaling, autocrop,
  centring, CRT shader and its strength, screen tint, fullscreen, VSync,
  monitor bezel
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

(web-player-kit)=
## Web player (single HTML file)

`tools/publish-web` is the browser twin of `tools/publish`. It reads the same
game manifest and writes one `.html` file that holds everything: the
WebAssembly build of the emulator, its JavaScript, the AROS boot ROM pair, the
disk images and the license texts. The page makes no request beyond itself, so
it can be dropped on any static host, attached to a mail, or opened straight
from disk.

```sh
tools/publish-web --manifest /path/to/game.toml
```

This builds the browser bundle first (see
[Building the WebAssembly package locally](#building-the-web-bundle)
for the prerequisites: the `wasm32-unknown-unknown` target and the pinned
`wasm-bindgen` CLI; `wasm-opt` from binaryen is used when it is installed),
then writes `target/publish/<id>.html`. Python 3.11 or newer is needed.

| Option | Effect |
|---|---|
| `--payload PATH` | The manifest's `adf` payload (default: the file the manifest names, beside the manifest; the file name must match) |
| `--disk PATH` | A further disk of a multi-disk title; repeatable |
| `--out FILE` | Output file (default `target/publish/<id>.html`) |
| `--skip-build` | Reuse the bundle already in `--pkg` instead of building |
| `--pkg DIR` | The `wasm-bindgen` output directory (default `crates/copperline-web/pkg`) |
| `--profile NAME` | Cargo profile for the bundle build (default `release`) |
| `--drives N` | Floppy drives to fit, 1-4 (default 1) |
| `--video PAL\|NTSC` | Video standard (default: the model's own) |
| `--joystick keys\|cd32\|off` | Keyboard joystick mode the page starts in (default `keys`) |
| `--rom PATH`, `--ext-rom PATH` | Embed a Kickstart image you are licensed to redistribute instead of the AROS ROM |
| `--source-url URL` | Where the emulator's source is published, for the About box (default: the Copperline repository) |
| `--max-size SIZE` | Refuse to write a page larger than this (default `64M`) |

A page is about 4 MB before its disks; each 880 KiB floppy adds its compressed
size (base64 costs a third on top of that).

The packer warns about a disk the page's drives may not take the way you
expect: a high-density image (the machines it builds fit double-density
drives), a file that is not a standard 880 KiB ADF, and a DMS, IPF, SCP or
gzip/zip-packed image, which plays write-protected so nothing the title saves
to it is kept. A plain ADF or a UAE extended ADF is writable.

(web-kit-notices)=
### Source and license notices

Copperline is GPL-3.0-or-later, so a page you distribute must say where the
complete source of the emulator inside it is. The About box does: it names
the repository and the exact commit the page was packed from (and a tag, when
one points at that very commit), and carries the full license text. That
makes three things your side of the bargain:

- Pack from a commit that is published. A checkout with uncommitted changes
  still packs, with a warning, and the page then says it was built from that
  commit with local changes: commit and publish them before distributing it.
- If you changed Copperline, pass `--source-url` with the address of your own
  repository, which must hold the commit the page names.
- `--skip-build` trusts that the bundle in `--pkg` was built from the
  checked-out commit; rebuild it after changing commits.

The AROS ROM is a separate work under the AROS Public License 1.1. A page
that embeds it also carries its license, its acknowledgements and
`assets/aros/README.md`, which names the AROS repository and commit the ROM
was built from. With `--rom` no AROS is embedded and none of those are
claimed: the About box says the ROM was supplied by the publisher, and
whether you may redistribute it is yours to establish.

### What the manifest means in a browser

| Manifest | Web player |
|---|---|
| `[game] title` | Page title and start screen |
| `[game] id` | Names the browser storage the visitor's saves live in |
| `[game] version` | Shown in the About box |
| `[payload] adf` | The disk in `DF0:` at power-on |
| `[payload] sha256` | Checked when the page is packed |
| `[machine] model` | The machine profile, as `--model` |
| `[display] fullscreen` | Enter fullscreen with the Start click |
| `[features] save_states` | Adds Save state / Load state buttons |
| `[branding] icon` | The page's favicon (a plain placeholder without one, so the browser never asks the host for `/favicon.ico`) |

The browser build has no CD drive, no host-directory volume and no memory
overrides, so the packer refuses `cd` and `run` payloads, the CDTV and CD32
models, and `[machine] chip`/`fast`/`slow`. It packs any other model but warns
outside the A500 and A1200, the two the hosted browser build vets against the
AROS ROM. `[display] shader` and `bezel` are accepted and not drawn yet (the
packer says so): the page shows the plain picture.

With several disks, the first `--drives` of them start in `DF0:` onwards; the
page then offers a `DF0` list to change between the manifest's disk and those
that did not get a drive of their own.

### What the visitor gets

The page shows the title and a **Start** button; the click is what lets a
browser start sound. Below the picture are Pause, Reset, the keyboard joystick
mode, the disk list (multi-disk titles), Export disk, Import disk, Screenshot,
Volume, Fullscreen and About.

- **Keyboard:** goes to the Amiga while the picture has focus. In joystick
  mode `keys` the cursor keys steer and Right Ctrl, Right Alt or Left Ctrl
  fire (Left Alt is the second button); `cd32` adds the pad's extra buttons on
  C/X/D/S/Enter/Z/A; `off` leaves every key to the Amiga keyboard. Shift+Esc
  moves focus from the picture to the controls, which then take Tab and the
  arrow keys themselves; clicking the picture gives the keyboard back.
- **Mouse:** click the picture to capture the pointer, Esc to release it.
- **Gamepads:** the first drives port 2, a second port 1.
- **Touch screens:** an on-screen stick and two fire buttons, and the picture
  works as a trackpad (drag to move, tap to click).
- **Saves:** each disk runs from the visitor's own writable copy, as the native
  player's does. Changes are stored in the browser (IndexedDB, in a database
  named after the manifest's `id`) and return on the next visit. **Export
  disk** downloads the current image of `DF0:` and **Import disk** takes one
  back (to move saves between browsers); **Erase saves** in the About box
  returns to the pristine disks and restarts the title. A page repacked with a changed disk starts
  afresh.
- **Save states** (with `[features] save_states`): one quick slot, offered
  back only to the exact build that wrote it. A state is the whole machine,
  disks included, as everywhere else in Copperline: loading one puts every
  disk that was in a drive when it was saved back to how it was at that
  moment, and the stored copy follows at once. Anything the title wrote to
  such a disk after the state was saved is gone, and the page says which disk
  went back; a visitor who wants to keep it uses **Export disk** first. A
  disk that was not in a drive when the state was saved is left alone.
- **About:** names Copperline, the source repository and the commit the page
  was built from, and shows the notices described under
  [Source and license notices](#web-kit-notices), all carried
  inside the file.

### Hosting and browser support

The page needs `DecompressionStream`, `AudioWorklet` and CSS container units:
Chrome or Edge 105, Firefox 113, Safari 16.4, or newer. An older browser is
told so instead of being left on "loading...": one that parses the loader but
lacks a feature gets the loader's own message, and one too old to parse it at
all is caught by a small guard script written in the oldest syntax, which
takes any script error before the loader has run to mean that. Browser storage can be unavailable
(private windows, some `file://` origins); the title still plays, and the page
says saves cannot be kept.

No server configuration is needed: no MIME type for WebAssembly, no
cross-origin isolation headers. A host that sends a strict
`Content-Security-Policy` must allow what a self-unpacking page does: inline
scripts, `blob:` scripts, `data:` for the worklet fallback, and
`'wasm-unsafe-eval'`.

### Checking a packed page

```sh
node tools/check-web-kit.mjs target/publish/skyhawks.html
node tools/check-web-kit-browser.mjs target/publish/skyhawks.html --seconds 30
```

The first needs only Node. It runs the page's own scripts against a stand-in
for the page: the guard, the loader over the page's own embedded assets, and
the shell the loader starts. It then drives the shell's controls on a
synthetic clock: every About text opens, Start boots the machine, which after
20 emulated seconds (`--seconds N`) must show a picture with at least
`--min-colours` colours, and the save paths are exercised (an imported disk
is stored, a loaded state takes its disk back, Erase saves stays erased, a
Restart after a failure runs one frame loop). It has no layout, canvas or
audio device, and imports the modules from `data:` instead of `blob:` URLs.
The second covers those: it opens the file in a headless Chrome (`--chrome PATH` or
`$CHROME` when it is not in a usual place), clicks Start, and additionally
fails if the page requested anything but itself; give it an `http://` URL to
check a hosted copy, and `--screenshot OUT.png` to keep the picture.

### How the file is put together

Every asset is a `<script type="text/plain" data-cl-asset="NAME">` block whose
text is base64 of a zlib stream, with its unpacked size alongside; one
`<script type="application/json" id="cl-config">` block describes the title.
Module import specifiers are resolved against the page directory only, so the
packer refuses a module in a subdirectory that imports relative to itself
(`wasm-bindgen` snippets under `pkg/`, should the bundle ever grow any).
A small inline loader (`crates/copperline-web/www/kit-loader.js`) inflates the
blocks with `DecompressionStream`, removes them from the document, turns the
page's ES modules into Blob URLs (rewriting each relative import to the Blob
URL of the module it names), and starts the shell
(`kit-player.js`, laid out by `kit-shell.html`). The shell shares its input
mapping, audio pacing and pointer scaling modules with the hosted
[browser page](#using-the-hosted-page).

# Browser build and WebAssembly integration

Copperline compiles to WebAssembly with a canvas and Web Audio frontend. A hosted
instance is available at [copperline.dev/try](https://copperline.dev/try/).

This chapter covers using the browser build, its architecture, building it
locally, and embedding the emulator in your own web pages.

(using-the-hosted-page)=
## Using the hosted web emulator

The web version runs at [copperline.dev/try](https://copperline.dev/try/):

- **Machine models:** Choose between an Amiga 500 (68000, 512 KiB chip RAM, 512 KiB slow RAM)
  or an AGA Amiga 1200 (68EC020, 2 MiB chip RAM). Selecting a model while the system
  is running reboots the machine with the selected profile. URL query parameter: `?machine=A1200`.
- **Video standards:** Toggle between PAL (default) and NTSC. URL query parameter: `?video=NTSC`.
- **Boot ROMs:** The open-source AROS Kickstart replacement is fetched automatically at load.
  You can also load 512 KiB or 256 KiB Kickstart ROM files via the **Kickstart ROM** picker or
  drag-and-drop. URL query parameter: `?kick=<url>` (a same-origin path).
- **Floppy disk images:** Mount disk images in `DF0:` and `DF1:` (ADF, ADZ, DMS, IPF, SCP,
  or gzip/ZIP-packed). Images mount read-only unless **Open disks writable** is checked,
  which keeps guest writes in memory. **Blank DF0/DF1** inserts an empty formatted disk,
  and **Download DF0/DF1** exports the current image. URL query parameters:
  `?df0=<url>&df1=<url>`. Gzip images may expand to at most 128 MiB; netplay
  replacements have a smaller 16 MiB limit on both the file and its expanded contents.
- **Floppy speed:** 100, 200, 400 or 800 percent, or turbo. URL query parameter:
  `?fdspeed=400` (`0` or `turbo` for turbo).
- **Display options:**
  - **Monitor presentation:** Combine the CRT shader with the 1084 cabinet or the Classic
    bezel: **1084 (CRT + cabinet)** (default), **Classic (CRT + bezel)**, **CRT filter**,
    **1084 cabinet**, **Classic bezel**, or **Plain**.
  - **View (Overscan):** Crop to standard TV aperture or view full overscan border areas.
  - **Scaling:** **Smooth** (default) fits the picture to the display element using
    linear interpolation. **Integer** matches desktop `[display] scaling = "integer"`:
    the picture scales by whole device pixels per column and scan line using independent
    per-axis integer multipliers to approximate the 4:3 CRT aspect ratio (e.g. 4:5
    pixels for 200-line NTSC on 1080p, square multipliers for PAL).
  - **Autocrop:** Matches desktop `[display] autocrop`: dynamically crops the display
    to the active raster area containing fetched bitplane data rather than the fixed TV
    aperture. When paired with **Integer** scaling, integer multipliers recalculate
    against the cropped viewport. Both scaling and autocrop are automatically suspended
    when monitor bezels are enabled. Screenshots capture the standard presentation buffer
    geometry.
  - **Screen tint:** Monochrome simulation presets (Black & White, Green, Amber, Sepia).
  - **Deinterlacing:** Motion-adaptive field merging for interlaced display modes.
  - **Phosphor persistence:** Simulates CRT phosphor decay trails.

### Input methods

- **Mouse:** Click the canvas to engage browser Pointer Lock for relative mouse tracking.
  Press `Esc` to release. On touch devices, drag on the canvas to move the pointer.
- **Physical keyboard:** Maps host keyboard scancodes directly to Amiga raw keycodes.
- **On-screen keyboard:** Click **Keyboard** to toggle a virtual Amiga 600 keyboard layout
  with latching modifier keys (`Shift`, `Ctrl`, `Alt`, `Amiga`) for mobile and tablet devices.
- **Joystick emulation:** Cycle between off, **Keys** (arrow keys + Ctrl/Alt), **CD32**
  (adds C/X/D/S/Enter/Z/A), and, on touch screens, **Touch** (virtual on-screen D-pad and
  fire buttons). URL query parameter: `?joy=off|keys|cd32|touch`.
- **Gamepads:** Standard USB and Bluetooth gamepads are detected automatically via the
  browser Gamepad API. The first gamepad drives Amiga port 2 (the joystick port); a second
  one takes port 1 for two-player games, and the mouse returns to port 1 when it
  disconnects.

(browser-save-states)=
### Save states in the browser

The web build uses the same `.clstate` file format as the desktop version:

- **Save state / Load state:** Download or upload state files (`.clstate`). Transfers
  between browser and desktop require devices supported by both builds; the chunked
  format carries states across releases that only add or drop state fields. Desktop
  states that depend on host files or native-only devices are not portable to the
  browser.
- **Quick save / Quick load:** Stores the current session in browser local storage (IndexedDB)
  for instant resumption across page reloads.
- **Saved states panel:** Manage named state slots in browser storage.

### Rollback netplay

**Controls -> Netplay** connects two browsers using WebRTC. The host shares an
invitation link or QR code; the other player opens it and clicks **Join game**.
**Advanced** keeps manual offer/answer codes for pages without a room service.
The host sends ROMs, floppy images and machine settings over the encrypted peer
connection. The guest verifies them before startup and uses them only for the
session; its remembered ROM and local choices are preserved. Both pages start
fresh machines with matching ROMs, disks and hardware settings.
During play the host can use **Netplay -> Swap disk** or **Eject selected drive**;
both peers pause and apply the disk change together. The guest receives each
replacement automatically, including repeated swaps in DF0. Up to eight
spectators can watch through a separate spectator invitation.
Each player controls one Amiga port; late input is predicted and corrected by
rollback. See [browser netplay setup](netplay.md#browser-netplay) for the steps,
controller mapping, relay troubleshooting and restrictions.

Netplay requires WebRTC data channels as well as WebAssembly. It works on a
static site; room invitations use a separate signaling service, while manual
codes need no server. A desktop peer cannot join a browser session. Save
states, media changes, serial connections and pause are unavailable until
disconnect.

## Architecture

The browser implementation consists of the following components:

- **Core emulator crate (`copperline`):** Compiled to `wasm32-unknown-unknown` with default
  desktop dependencies disabled (`--no-default-features`).
- **Web wrapper crate (`crates/copperline-web`):** A lightweight `cdylib` crate exporting
  the `WebEmu` interface via `wasm-bindgen`.
- **Threading and compatibility:** Single-threaded execution without requiring
  `SharedArrayBuffer` or special server headers (`COOP`/`COEP`). Works on standard static
  web hosts (including GitHub Pages).
- **Video pipeline:** Framebuffers are rendered to an RGBA pixel buffer and presented to
  HTML5 `<canvas>` via `putImageData` or WebGL2 textures with custom CRT fragment shaders.
- **Audio pipeline:** Stereo 44.1 kHz float samples are transferred directly to an
  `AudioWorklet` processor for low-latency playback.
- **Netplay:** The Rust core owns the shared rollback timeline and bounded packet
  queues. `www/netplay.js` builds the netplay panel and the `RtcLink` peer
  connection on top of `www/netplay-rtc.js` (WebRTC and manual codes) and
  `www/netplay-room.js` (room invitations). `www/netplay-media.js` transfers and
  verifies the host setup on a reliable channel, `www/netplay-swap.js` coordinates
  host disk changes on confirmed frame boundaries, and `www/netplay-watch.js` serves
  and receives the spectator feed. `try.js` owns the session lifecycle and locks
  controls that change the machine.

## Building the WebAssembly package locally

### Prerequisites

Ensure the `wasm32-unknown-unknown` Rust target and the matching `wasm-bindgen-cli` version
are installed:

```sh
rustup target add wasm32-unknown-unknown
# Run from the repository root; the CLI must match the crate exactly.
bindgen_version=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/copperline-web/Cargo.toml)
cargo install wasm-bindgen-cli --version "$bindgen_version" --locked
```

### Compilation

```sh
cd crates/copperline-web
cargo build --release --target wasm32-unknown-unknown --locked
wasm-bindgen --target web --out-dir pkg \
  target/wasm32-unknown-unknown/release/copperline_web.wasm
```

The compiled JavaScript loader (`copperline_web.js`) and WebAssembly binary
(`copperline_web_bg.wasm`) are output to the `pkg/` directory.

From the repository root, `node tools/check-web-netplay.mjs` exercises this release
bundle with two emulators, packet loss/reordering, input changes and different
presentation settings, and `node tools/check-web-netplay-swaps.mjs` adds the
reliable disk-change channel. Run `npm test --prefix crates/copperline-web/www` for the
page controller tests. These checks need no display, network or external ROMs.
`tools/check-web-netplay-browser.mjs` and `tools/check-web-netplay-rooms.mjs`
drive the real page in a browser through Playwright; the second also needs a
room service.

## Embedding with the WebEmu API

The example below assumes that the page has loaded the ROM and floppy bytes,
created a 2D canvas context named `ctx`, and connected an `audioWorkletNode`
whose processor accepts interleaved stereo samples. Resize the canvas to the
presentation dimensions when they change. The complete page and audio processor
are in `crates/copperline-web/www/`.

```js
import init, { WebEmu } from './pkg/copperline_web.js';

const wasm = await init();

// Initialize emulator (Profile, Video standard, Floppy drive count)
const emu = new WebEmu('A1200', 'PAL', 2);

// Load Kickstart ROM and insert disks
emu.load_rom(romUint8Array, extRomUint8Array);
emu.insert_floppy(0, gameDiskBytes, 'game.adf');
emu.insert_floppy_writable(1, saveDiskBytes, 'save.adf');

// Main animation and audio loop
function renderLoop(timestampMs) {
  emu.run(timestampMs, 5); // Step emulator up to current time (max 5 frames)

  const rows = emu.present_rows();
  if (rows > 0) {
    const width = emu.present_width();
    const pixelView = new Uint8ClampedArray(
      wasm.memory.buffer,
      emu.present_ptr(),
      width * rows * 4
    );
    ctx.putImageData(new ImageData(pixelView, width, rows), 0, 0);
  }

  const audioSamples = emu.take_audio(); // Interleaved stereo Float32Array (44.1 kHz)
  if (audioSamples.length > 0) {
    audioWorkletNode.port.postMessage(audioSamples, [audioSamples.buffer]);
  }

  requestAnimationFrame(renderLoop);
}
requestAnimationFrame(renderLoop);
```

### Key `WebEmu` API methods

- `new WebEmu(model, video, drives)`: Instantiate emulator. Each argument is optional:
  `model` takes the desktop `--model` names (default A500), `video` is `"PAL"` or
  `"NTSC"` (default: the profile's), and `drives` fits 0-4 floppy drives.
- `load_rom(mainRom, extRom)`: Fit Kickstart ROM bytes (and an optional extended ROM)
  and cold-reset the machine. 256 KiB images are mirrored automatically.
- `run(nowMs, maxFrames)` / `run_hidden(nowMs, maxFrames)`: Step emulated time up to
  `performance.now()`, at most `maxFrames` frames per call; `run_hidden` skips rendering
  for a hidden page.
- `insert_floppy(driveIndex, diskBytes, label)`: Insert read-only floppy image.
- `insert_floppy_writable(driveIndex, diskBytes, label)`: Insert writable in-memory floppy image.
- `export_floppy(driveIndex)`: Export current in-memory floppy image as `Uint8Array`.
- `eject_floppy(driveIndex)`: Eject floppy image from drive.
- `key_event(code, pressed)`: Send W3C keyboard event code (e.g., `"KeyA"`, `"Digit1"`).
- `key_raw(rawCode, pressed)`: Send Amiga raw key scan code.
- `mouse_delta(dx, dy)`: Inject relative mouse motion.
- `mouse_button(button, pressed)`: Set mouse button state (`0` = Left, `1` = Middle, `2` = Right, matching `MouseEvent.button`).
- `set_joystick_port(port, up, down, left, right, fire, button2)`: Set joystick directional and fire button state (`port` 1 or 2).
- `set_cd32_buttons_port(port, play, rwd, ffw, green, yellow)`: Set CD32 pad extra button state (`port` 1 or 2; red/blue map to `fire`/`button2` via `set_joystick_port`).
- `set_port_device(port, device)`: Configure controller port device (`port` 1 or 2, e.g., `"mouse"`, `"joystick"`, `"cd32"`, `"analogue"`, `"none"`).
- `save_state()`: Export full machine state as `Uint8Array`.
- `load_state(stateBytes)`: Restore machine state from `Uint8Array`.
- `set_overscan(mode)`: `"tv"` (default) or `"full"` presentation overscan.
- `set_scaling(mode)`: `"smooth"` (default) or `"integer"`. Under `"integer"`, 60 Hz
  standard scans present captured apertures at native scanlines without resampling.
- `set_autocrop(on)`: Enable or disable autocrop.
- `present_content_rect()`: Returns the active content bounding box as `[x, y, width, height]`
  in presentation-buffer pixels, or an empty array if no frame has been drawn.
- `present_layout(availWidth, availHeight)`: Computes presentation placement for a viewport
  given device pixel dimensions: `[sx, sy, sw, sh, dx, dy, dw, dh, columns, lines]`,
  containing the source sub-rect, destination rect, and integer scale multipliers (`0, 0` for
  smooth scaling). Returns an empty array until the first frame is presented. Used by `try.js`
  when integer scaling or autocrop is enabled without a monitor bezel.

Other methods cover the rest of the page:

- Machine and pacing: `reset()` (cold reset), `resync_clock()` (after a pause),
  `emulated_seconds()`, `machine_model()`, `video_standard()`, `machine_summary()`, and
  the static `models()`, `video_standards()`, `floppy_formats()` and `build_info()`.
- Presentation: `set_tv_centre(h, v)`, `set_monitor_bezel(drawn)`,
  `set_deinterlace(on)`, `set_phosphor(fraction)` (0 to 0.95), `presentation_revision()`
  and `present_crt_lines()` (for a page-side CRT shader).
- Audio and drives: `set_volume_percent(n)`, `set_mono_audio(on)`,
  `set_floppy_sounds(on)`, `set_floppy_sounds_volume(n)`, `set_floppy_speed(percent)`
  (100/200/400/800, or 0 for turbo), `drive_connected(drive)`, `disk_name(drive)` and
  `floppy_write_protected(drive)`.
- Front panel: `power_led()`, `fdd_led()`, `fdd_track()`, `hdd_led()`, `cd_led()` and
  `caps_lock_led()`.
- Serial port: `serial_send(bytes)`, `serial_take()`, `serial_input_backlog()`,
  `serial_dtr()` and `serial_set_carrier(connected)`.

### Embedding netplay

Create and load a fresh `WebEmu` for each connection. After WebRTC opens, call
`start_netplay(player, session, delay, window, controller)` before the first
`run` or `run_hidden` call. `player` is 1 or 2; `session` is a shared 32-digit hex
ID; delay is an integer from 0 to 6, window from 1 to 12, and controller is
`"joystick"`, `"cd32"` or `"mouse"`. Both ports use that controller. A machine that has run
or loaded a save state is ineligible. A fitted RTC is seeded to 2000-01-01 UTC;
this does not add a clock to models without one. Failed startup restores the
machine and leaves it available for local use or another startup attempt.

`RtcLink` in `www/netplay.js` provides `offer(settings)`, `answer(offerCode)` and
`accept(answerCode)`, with `configureIce(iceServers, relayOnly)` for temporary
TURN credentials obtained from a trusted service. `report()` returns sanitized
connection diagnostics. Its `onOpen` callback is the point to construct the machine
and call `start_netplay`. The page sets `settings.media = "host-v1"` to add a
reliable setup channel; its `onOpen` awaits `transferMedia(hostSnapshot, progress)`
on the host, or `transferMedia(null, progress)` on the guest, which returns the
verified host snapshot. Embedders that omit this setting must supply matching
media themselves. Its `onClose` callback must stop the page's loops and
free the machine. Immediately after startup, call `run_hidden(now, 0)` and
`link.send(emu)` once to send the initial fingerprint. From then on, call
`link.receive(emu)` before each `run`/`run_hidden` and `link.send(emu)` after
it, including polls that advance zero frames: those still process handshakes,
corrections and retransmissions. The ordinary render and audio-drain APIs
still apply. On close, drain queued packets and poll once more before freeing
the machine, to surface a pending mismatch error.

For another transport, pass each complete received packet to
`netplay_receive(Uint8Array)`, and drain `netplay_take_packet()` until it returns
an empty array. Preserve packet boundaries; do not interpret packets as text.
The supported browser transport uses an unordered data channel with
`maxRetransmits: 0`; Copperline performs input retransmission itself.

Set `settings.swaps = "disk-v1"` and supply `swapCallbacks.machine` (returning
the current `WebEmu`) to enable the separate reliable disk channel. Optional
`status` and `changed` callbacks update the page. The host calls
`link.swaps.swap(drive, {bytes, name, writable})`, or passes `null` to eject.
Keep polling and running the emulator during this operation: Rust stops
forward execution at the negotiated boundary but must still process input and
acknowledgements. Do not call the underlying hold/stage/apply/resume methods
independently; both peers must complete the coordination protocol before
either resumes.

`netplay_status()` returns `[connected, frame, confirmed, acknowledged, rollbacks,
replayed, checked]`, with `connected` represented by 0 or 1. It returns an empty
array outside netplay. `netplay_release_input()` clears this peer's held input;
call it when the page loses focus. Existing key methods collect local input.
During netplay, the port-2 joystick/CD32 methods collect the local player's
controller even when that player owns Amiga port 1; port-1 calls are ignored.
This preserves the page's first-gamepad mapping.

`WebEmu.netplay_packet_layout()` returns `[protocol, maxBytes, headerBytes,
inputBytes]` for glue compatibility checks. Floppy sound enablement and level
must match before startup; their setters fail during a session. Output volume
and mono/stereo presentation remain local.

Machine/media/state operations, including controller fitting and floppy speed,
fail while a session exists. Serial input is ignored, and so is mouse input
unless the session's controller is `"mouse"`. A protocol error stays latched.
Free the instance and start fresh after any disconnect or error; there is no
operation to resume it locally.

Spectating uses the same shape. A host that will admit spectators calls
`netplay_enable_spectators()` straight after `start_netplay`, before the first
run, so the confirmed history is kept from frame zero; `netplay_identity()`
returns the fingerprint a spectator must reproduce, and each spectator reads
the history through `spectator_feed_open()`, `spectator_feed_take(id, maxBytes)`
and `spectator_feed_close(id)`. A browser host refuses new spectators once it
retains more than 64 MiB of history. A spectator page loads the host's media
into a fresh `WebEmu`, calls `start_spectating(controller)`, sends
`spectate_identity()` to the host, and feeds the host's stream to
`spectate_receive(bytes)`; `spectate_status()` returns `[spectating, frame,
behind, checked, diskChanges]`. `www/netplay-watch.js` implements both sides
over a WebRTC data channel.

### HTML element hooks in `try.js`

When using the bundled `try.js` harness, standard UI elements can be connected by ID:

- `#machine`: `<select>` element for machine model selection.
- `#video`: `<select>` element for PAL / NTSC switching.
- `#floppy-speed`: `<select>` for floppy drive speed multiplier (`100`, `200`, `400`, `800`, `0` for turbo).
- `#monitor`: `<select>` for CRT shader and bezel style.
- `#overscan`: `<select>` for TV aperture vs. full overscan view.
- `#scaling`: `<select>` for smooth vs. integer scaling (`smooth` / `integer`).
- `#autocrop`: `<input type="checkbox">` for the autocrop presentation. A shell can ship
  both with the `hidden` attribute; the glue un-hides them on a bundle that supports them.
- `#df0list` / `#kicklist`: `<select>` elements populated from remote disk/ROM manifests.
- `#pause`, `#screenshot`, `#keyboard`: Action button triggers.

(browser-page-config)=
### Page configuration file (`copperline.json`)

You can provide default settings via a `copperline.json` file next to the page (the glue
fetches `./copperline.json`):

```json
{
  "machine": "A1200",
  "video": "PAL",
  "df0": "adf/game.adf",
  "autoboot": true,
  "floppy_speed": 400,
  "monitor": "1084",
  "scaling": "integer",
  "autocrop": true,
  "background_run": true
}
```

Every key is optional. The file accepts `machine`, `video`, `kick` (a same-origin ROM
path), `df0`, `df1`, `floppy_speed` (100/200/400/800, or 0 for turbo), `floppy_sounds`,
`mono_audio`, `overscan` (`tv`/`full`), `tint` (`none`/`bw`/`green`/`amber`/`sepia`),
`monitor` (`1084`/`classic`/`crt`/`cabinet`/`bezel`/`plain`), `scaling`, `autocrop`,
`deinterlace`, `phosphor` (0 to 0.95), `joy` (`off`/`keys`/`cd32`/`touch`),
`background_run`, `serial_url`, `serial_raw` and `autoboot` (power on once everything
has loaded). The link parameters (`?df0=`, `?df1=`, `?kick=`, `?machine=`, `?video=`,
`?joy=`, `?fdspeed=`) override the file. Display choices (`overscan`, `tint`, `monitor`,
`scaling`, `autocrop`, `deinterlace`, `phosphor`) and `background_run` are starting
points for first-time visitors: a visitor's own remembered choice wins.

## Serial port over WebSockets

The hosted page can connect the Amiga serial port to a `ws://` or `wss://`
gateway, such as a telnet BBS behind a WebSocket bridge. Enter the gateway URL and
click **Connect**; `serial_url` and `serial_raw` in [`copperline.json`](#browser-page-config)
preset the URL box and the raw checkbox.

- In the default telnet mode, the page answers telnet negotiation and waits to dial
  until the guest's terminal has raised DTR and kept the line quiet for three emulated
  seconds, so boot-time debug output never reaches the far end. The socket's state
  drives the guest's carrier-detect line, and a terminal that drops DTR hangs up; the
  page dials again when the terminal returns.
- In raw mode, bytes pass between the guest and the socket unchanged, with no DTR gate.

(benchmarking-the-core-as-wasm)=
## Headless WebAssembly benchmarking

From the repository root, build the frontend-free WASI benchmark:

```sh
rustup target add wasm32-wasip1
cargo build --release --locked --target wasm32-wasip1 \
  --no-default-features --features bench-bin --bin copperline-bench
wasmtime run --dir . target/wasm32-wasip1/release/copperline-bench.wasm \
  --config test.toml --seconds 30
```

Keep the config and its media under the directory exposed by `--dir`.
`copperline-bench` has its own small parser, separate from the desktop
binary's: `--config PATH`, `--rom PATH`, `--ext PATH`, `--df0 PATH` and
`--seconds N` (default 30). Add `--render` to include framebuffer rendering
and post-processing.

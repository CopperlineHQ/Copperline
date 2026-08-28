# Browser build and WebAssembly integration

Copperline compiles to WebAssembly with a canvas and Web Audio frontend. A hosted
instance is available at [copperline.dev/try](https://copperline.dev/try/).

This chapter covers using the browser build, architecture details, building locally,
and embedding the emulator into your own web applications.

(using-the-hosted-page)=
## Using the hosted web emulator

The web version runs at [copperline.dev/try](https://copperline.dev/try/):

- **Machine models:** Choose between an Amiga 500 (68000, 512 KiB chip RAM, 512 KiB slow RAM)
  or an AGA Amiga 1200 (68EC020, 2 MiB chip RAM). Selecting a model while the system
  is running reboots the machine with the selected profile. URL query parameter: `?machine=A1200`.
- **Video standards:** Toggle between PAL (default) and NTSC. URL query parameter: `?video=NTSC`.
- **Boot ROMs:** The open-source AROS Kickstart replacement is fetched automatically at load.
  You can also load standard 512 KiB Kickstart ROM files via the **Kickstart ROM** picker or
  drag-and-drop.
- **Floppy disk images:** Mount disk images in `DF0:` and `DF1:` (ADF, ADZ, DMS, IPF, SCP, or ZIP).
  By default, images mount read-only. Check **Open disks writable** to enable in-memory
  modifications. Use **Blank DF0/DF1** to create an empty formatted disk, and **Download DF0/DF1**
  to export modified disk images. URL query parameters: `?df0=<url>&df1=<url>`.
- **Display options:**
  - **Monitor presentation:** Select CRT shader and bezel frames (**1084**, **Classic**,
    **CRT filter**, or **Plain**).
  - **View (Overscan):** Crop to standard TV aperture or view full overscan border areas.
  - **Screen tint:** Monochrome simulation presets (Black & White, Green, Amber, Sepia).
  - **Deinterlacing:** Motion-adaptive field merging for interlaced display modes.
  - **Phosphor persistence:** Simulates CRT phosphor decay trails.

### Input methods

- **Mouse:** Click the canvas to engage browser Pointer Lock for relative mouse tracking.
  Press `Esc` to release. On touch devices, drag on the canvas to move the pointer.
- **Physical keyboard:** Maps host keyboard scancodes directly to Amiga raw keycodes.
- **On-screen keyboard:** Click **Keyboard** to toggle a virtual Amiga 600 keyboard layout
  with latching modifier keys (`Shift`, `Ctrl`, `Alt`, `Amiga`) for mobile and tablet devices.
- **Joystick emulation:** Cycle between **Keys** (arrow keys + Ctrl/Alt), **CD32**
  (adds C/X/D/S/Enter/Z/A), and **Touch** (virtual on-screen D-pad and fire buttons).
  URL query parameter: `?joy=keys`.
- **Gamepads:** Standard USB and Bluetooth gamepads are detected automatically via the
  browser Gamepad API. Controller 1 maps to Amiga Port 2 (standard joystick port).

(browser-save-states)=
### Save states in the browser

The web build uses the same `.clstate` file format as the desktop version:

- **Save state / Load state:** Download or upload state files (`.clstate`) for desktop interoperability.
- **Quick save / Quick load:** Stores the current session in browser local storage (IndexedDB)
  for instant resumption across page reloads.
- **Saved states panel:** Manage named state slots in browser storage.

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

## Building the WebAssembly package locally

### Prerequisites

Ensure the `wasm32-unknown-unknown` Rust target and the matching `wasm-bindgen-cli` version
are installed:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
```

### Compilation

```sh
cd crates/copperline-web
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg \
  target/wasm32-unknown-unknown/release/copperline_web.wasm
```

The compiled JavaScript loader (`copperline_web.js`) and WebAssembly binary
(`copperline_web_bg.wasm`) are output to the `pkg/` directory.

## Embedding with the WebEmu API

To embed Copperline in a custom web application:

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

- `new WebEmu(model, video, drives)`: Instantiate emulator.
- `load_rom(mainRom, extRom)`: Load Kickstart ROM bytes and reset CPU.
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

### HTML element hooks in `try.js`

When using the bundled `try.js` harness, standard UI elements can be connected by ID:

- `#machine`: `<select>` element for machine model selection.
- `#video`: `<select>` element for PAL / NTSC switching.
- `#floppy-speed`: `<select>` for floppy drive speed multiplier (`100`, `200`, `400`, `800`, `0` for turbo).
- `#monitor`: `<select>` for CRT shader and bezel style.
- `#overscan`: `<select>` for TV aperture vs. full overscan view.
- `#df0list` / `#kicklist`: `<select>` elements populated from remote disk/ROM manifests.
- `#pause`, `#screenshot`, `#keyboard`: Action button triggers.

(browser-page-config)=
### Page configuration file (`config.json`)

You can provide default settings via a `config.json` file in the web root:

```json
{
  "machine": "A1200",
  "video": "PAL",
  "df0": "adf/game.adf",
  "autoboot": true,
  "floppy_speed": 400,
  "monitor": "1084",
  "background_run": true
}
```

## Serial port over WebSockets

The browser build can route Amiga serial communication to remote WebSocket servers:

- Set `serial_url: "wss://bbs.example.com:8443/"` in configuration or via query parameter `?serial=wss://...`.
- In standard mode, the browser manages AT modem commands and dials the WebSocket host on connect.
- In raw mode (`serial_raw: true` or `?serial_raw=1`), bytes sent by the guest are forwarded directly
  to the WebSocket connection.

(benchmarking-the-core-as-wasm)=
## Headless WebAssembly benchmarking

To benchmark WebAssembly performance using Wasmtime:

```sh
cargo build --release --target wasm32-wasip1 --bin copperline-bench --features "bench-bin"
wasmtime run --dir . target/wasm32-wasip1/release/copperline-bench.wasm -- --config test.toml --benchmark-until 30
```

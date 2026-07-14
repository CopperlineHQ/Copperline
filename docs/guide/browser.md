# The browser build

Copperline runs in a browser: the same deterministic core, compiled to
WebAssembly with a thin canvas/Web Audio frontend instead of the desktop
window. A hosted build lives on the website at
[copperline.dev](https://copperline.dev/) under `/try`; this page explains
how to use that page, how it is put together, how to build and run it
locally, and how to embed the emulator in your own page.

## Using the hosted page

[copperline.dev/try](https://copperline.dev/try/) boots a default A500. The
page fetches the open-source AROS ROM while it loads, so the boot button
works with no files of your own; the **Kickstart ROM** and **DF0 disk**
pickers load local images instead. Both work before or after boot: a
pre-boot choice is stashed and applied when the machine starts (the boot
button relabels to show which ROM it will use), and a post-boot pick swaps
the disk live. Disk images are recognised by content -- ADF, ADZ, DMS, IPF,
and SCP, plain or gzip/zip packed -- and are always write-protected, since
the browser has no filesystem to write changes back to. On iOS the pickers
offer every file rather than filtering by extension, because the system
document picker greys out extensions it does not recognise, which would
lock out `.adf` and friends.

A disk can also come from a link: `/try/?df0=<url>` fetches the image while
the emulator loads and inserts it at boot, so a bootable demo is one
shareable URL, and the **DF0 from URL** button does the same for a pasted
address, inserting live when the machine is already running. The fetch
happens in the visitor's browser and nothing is proxied, so the image's
host must allow cross-origin GETs (same-origin always works; archive.org
does too). Only http(s) URLs are accepted, capped at 64 MiB (SCP flux
dumps run tens of MB). ROMs stay local-only on purpose: Kickstart images
are copyrighted, and a `?kick=` parameter would only exist to share them.

Controls:

- **Mouse**: with the pointer unlocked, the cursor drives the Amiga pointer
  through position deltas (Workbench-friendly); clicking the canvas
  requests pointer lock for relative motion (games), and Esc releases it.
- **Keyboard**: physical keys map to Amiga raw keycodes with the same table
  as the desktop frontend.
- **Joystick**: the toggle cycles off -> keys (-> touch on touch screens).
  Keys is the desktop frontend's FS-UAE-compatible mapping -- cursor keys
  for directions, Right Ctrl / Right Alt for fire, CD32 extras on
  C/X/D/S/Enter/Z/A -- and while enabled those keys are captured from the
  Amiga keyboard, like the desktop toggle.
- **Touch**: the canvas works like a trackpad, because the Amiga pointer
  only takes relative motion and an absolute finger position cannot map to
  it. One finger drags the pointer, a quick tap left-clicks, holding still
  for a moment picks the button up for a drag (icons, windows), and a
  second finger holds the right button, so Intuition menus work: hold,
  steer with the first finger, lift to select. With the joystick toggle in
  touch mode the canvas is a pad instead: a floating eight-way stick on
  the left half and a fire button on the right.

Audio starts with the boot click, but a browser autoplay policy can keep
the AudioContext suspended anyway; the boot never waits for it. The
emulator runs silent and the next click or key press unlocks the sound.

## How it is put together

The crate is split by cargo features so the core carries no desktop
dependencies:

- **`frontend`** (default) -- the winit/pixels window, launcher and UI, cpal
  audio output, gamepads, file dialogs, and clipboard. With the feature off,
  the library is the portable headless core plus the pure presentation
  helpers (`video::present_common`), which is the surface every alternative
  frontend builds against.
- **`wasm-boards`** (default) -- the wasmtime host for
  [functional Zorro board plugins](../zorro.md). Wasmtime's JIT cannot be
  compiled *to* wasm32, so browser builds turn it off; plugin boards are a
  desktop-only feature.
- **`bench-bin`** -- the headless `copperline-bench` benchmark binary (see
  [](#benchmarking-the-core-as-wasm)).

`cargo check --no-default-features` is the portability invariant: the core
must always compile without the desktop stack (CI enforces this, along with
a `wasm32-unknown-unknown` check of the web crate).

The browser frontend itself is `crates/copperline-web`, a small standalone
`cdylib` crate (deliberately not a workspace member, so building it never
touches the root lockfile). It wraps the core in a `WebEmu` class exported
through wasm-bindgen; the page's JavaScript drives everything from
`requestAnimationFrame`:

- **Video**: the core's rendered frame is post-processed and deinterlaced by
  the same code the desktop uses, then blitted to a `<canvas>` with
  `putImageData` -- the internal framebuffer is RGBA in memory order, so no
  conversion happens. Standard PAL screens are presented as the captured TV
  aperture, a 668x540 crop with the standard window exactly centred between
  symmetric overscan margins, so the canvas carries none of the bezel-mask
  black columns of the full framebuffer; non-standard frames (true
  overscan, NTSC, programmable scans) keep the full 716-pixel width, as on
  the desktop (see [the presentation internals](../internals/video.md)).
  There is no wgpu in the build, which keeps the wasm
  around 1.4 MiB (about 0.6 MiB over the wire).
- **Audio**: Paula's 44.1 kHz stereo mix is drained once per animation frame
  and posted to an `AudioWorklet` as transferred `Float32Array` chunks. The
  build is single threaded -- no SharedArrayBuffer, so no COOP/COEP headers
  are needed and any static host (GitHub Pages included) can serve it.
- **Pacing**: each animation frame steps the core up to the wall clock, with
  the audio queue as the master clock -- when the worklet reports more than
  ~150 ms buffered, stepping pauses for a tick. Deficits past 100 ms (a
  backgrounded tab, a GC pause) are forgiven rather than fast-forwarded,
  mirroring the native pacer's re-anchor behaviour.
- **Input**: `KeyboardEvent.code` strings map to Amiga raw keycodes with the
  same table as the desktop frontend (winit's `KeyCode` names *are* the W3C
  code strings); the mouse uses Pointer Lock for relative motion, with a
  cursor-following fallback when unlocked. Touch support (the trackpad
  mouse and the on-screen joystick described above) is page glue in
  `try.js`, built entirely on the exported mouse and joystick calls -- the
  wasm bundle is touch-agnostic.

The guest sees a stock machine: ROMs arrive as bytes
(`Emulator::reload_rom`), floppies as bytes
(`FloppyController::insert_disk_image_bytes`, which sniffs the same image
formats by content as the desktop file paths), and disks are always
write-protected because the browser has no filesystem to write changes back
to.

## Building it locally

Requirements: the `wasm32-unknown-unknown` target and a `wasm-bindgen` CLI
that exactly matches the version pinned in `crates/copperline-web/Cargo.toml`
(the CLI and the crate must never drift apart):

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked

cd crates/copperline-web
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg \
  target/wasm32-unknown-unknown/release/copperline_web.wasm
```

`pkg/` then holds `copperline_web.js` (the ES module loader) and
`copperline_web_bg.wasm`. To run the hosted page against a local build, copy
those two files into the website's `try/pkg/` directory and serve the site
with any static server (`python3 -m http.server`); the page fetches the AROS
ROMs from `try/aros/` (copies of `assets/aros/`). AudioWorklet requires a
secure context, which `localhost` satisfies.

Releases publish automatically: the `wasm-demo.yml` workflow rebuilds the
bundle on every `v*` tag and pushes it to the website repository, together
with `crates/copperline-web/www/try.js` and `www/audio-worklet.js` -- the
page glue lives in this repository precisely so it can never drift from the
`WebEmu` API it drives.

## Embedding: the WebEmu API

The exported surface is small; a minimal page is a canvas plus this:

```js
import init, { WebEmu } from './pkg/copperline_web.js';

const wasm = await init();
const emu = new WebEmu();          // default A500 machine, placeholder ROM
emu.load_rom(romBytes, extBytes);  // Kickstart or AROS bytes; cold reset
emu.insert_floppy(0, adfBytes, 'game.adf');

function tick(nowMs) {
  emu.run(nowMs, 5);               // step to the wall clock, max 5 frames
  const rows = emu.present_rows();
  if (rows > 0) {
    const view = new Uint8ClampedArray(
      wasm.memory.buffer, emu.present_ptr(), emu.present_width() * rows * 4);
    ctx.putImageData(new ImageData(view, emu.present_width(), rows), 0, 0);
  }
  const audio = emu.take_audio();  // interleaved stereo f32 at 44.1 kHz
  if (audio.length) worklet.port.postMessage(audio, [audio.buffer]);
  requestAnimationFrame(tick);
}
```

Input goes through `key_event(event.code, pressed)` (returns whether the key
mapped, for `preventDefault`), `mouse_delta(dx, dy)` and
`mouse_button(button, pressed)`; `set_joystick_port2(...)` and
`set_cd32_buttons_port2(...)` drive a port-2 pad, which the hosted page
feeds from the desktop frontend's FS-UAE-compatible keyboard mapping
(cursor keys, Right Ctrl / Right Alt fire, CD32 extras on C/X/D/S/Enter/Z/A).
`reset()` power-cycles, `eject_floppy(n)`
and `set_volume_percent(p)` do what they say, and `emulated_seconds()`
exposes the guest clock for diagnostics. The presentation pointer is only
valid until the next `run` call -- rebuild the typed-array view every frame,
because wasm memory can grow. The presentation *size* is dynamic too:
`present_width()` and `present_rows()` change when the guest switches
between a standard PAL screen (presented as the captured TV aperture crop)
and anything else (presented as the full framebuffer), so size the canvas
from both every frame rather than assuming fixed dimensions.

`www/try.js` and `www/audio-worklet.js` are the reference implementation of
all of the above, including the audio drift control.

(benchmarking-the-core-as-wasm)=
## Benchmarking the core as wasm

Whether a machine holds real speed in a browser is a measurable question.
The `copperline-bench` binary builds for `wasm32-wasip1` (where `std` time
and file I/O work natively) and runs under Node's WASI, whose V8 is the same
engine Chrome uses:

```sh
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1 \
  --no-default-features --features bench-bin --bin copperline-bench

node tools/wasi-bench.mjs \
  target/wasm32-wasip1/release/copperline-bench.wasm \
  --rom /work/assets/aros/aros-amiga-m68k-rom.bin \
  --ext /work/assets/aros/aros-amiga-m68k-ext.bin \
  --seconds 30 --render
```

`--render` includes the full per-frame presentation pipeline (render,
post-process, deinterlace), which is what an interactive frontend pays; the
report shows the realtime factor and the frame-time distribution against the
20 ms PAL budget. The same binary builds natively for a
direct wasm-versus-native comparison on identical workloads -- the render
checksums match between the two, which is the determinism contract doing its
job. As a reference point, on an Apple-Silicon laptop the wasm build ran the
default AROS machine at 6.4x realtime and a Copper/blitter-heavy OCS demo at
2.7x, roughly 1.3-1.5x slower than native.

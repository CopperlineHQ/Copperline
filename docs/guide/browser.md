# The browser build

Copperline runs in a browser: the same deterministic core, compiled to
WebAssembly with a thin canvas/Web Audio frontend instead of the desktop
window. A hosted build lives on the website at
[copperline.dev](https://copperline.dev/) under `/try`; this page explains
how to use that page, how it is put together, how to build and run it
locally, and how to embed the emulator in your own page.

## Using the hosted page

[copperline.dev/try](https://copperline.dev/try/) boots a default A500. The
**Machine** select below the screen switches to an AGA A1200 (68EC020,
2 MiB chip RAM, like the desktop's `--model A1200`); both models boot the
AROS ROM or a loaded Kickstart. Changing it before boot just changes what
the boot button builds, and changing it while a machine runs rebuilds the
machine and powers it up again -- the model is the board itself, not a
knob on it -- keeping the chosen ROM and the inserted disk. A link can
preset the model with `?machine=A1200`, and a
[save state](#browser-save-states) carries its own machine, so loading
one switches the select to whatever the state brings back. The
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

Files can also be dragged onto the page: a `.rom` file loads (or, before
boot, queues) a Kickstart exactly like the ROM picker, a `.clstate` file
restores a [save state](#browser-save-states), and anything else inserts
into DF0 like the disk picker -- dropped before boot it queues and inserts
when the machine starts. The same 64 MiB cap as URL fetches applies.

A disk can also come from a link: `/try/?df0=<url>` fetches the image while
the emulator loads and inserts it at boot, so a bootable demo is one
shareable URL, and the **DF0 from URL** button does the same for a pasted
address, inserting live when the machine is already running. The fetch
happens in the visitor's browser and nothing is proxied, so the image's
host must allow cross-origin GETs (same-origin always works; archive.org
does too). Only http(s) URLs are accepted, capped at 64 MiB (SCP flux
dumps run tens of MB).

A Kickstart can come from a link too, but only from the page's own
origin: `?kick=<path>` fetches the ROM and queues or fits it exactly like
the picker (the boot button relabels; a running machine is power-cycled).
The same-origin restriction is the copyright gate: Kickstart images are
copyrighted, and a cross-origin `?kick=` would only exist to share them.
A same-origin path can never load a ROM the serving site does not already
host, so the hosted page stays exactly as ROM-free as its server -- while
a self-hosted copy that serves its owner's ROM files next to the page (a
Docker deployment with a mounted volume, an intranet install) can boot
them by URL: `?kick=files/kick13.rom`. ROM fetches are capped at 4 MiB
and the image is validated like a picked file. A page shell may also
offer a **Kickstart from URL** button (id `kickurl`), which prompts for
a same-origin address, or a **Kickstart list** select (id `kicklist`)
that fills itself with the ROMs the site serves next to the page (see
the page-shell hooks below); the hosted page has no ROMs to point them
at and omits both.

Controls:

- **Mouse**: with the pointer unlocked, the cursor drives the Amiga pointer
  through position deltas (Workbench-friendly); clicking the canvas
  requests pointer lock for relative motion (games), and Esc releases it.
- **Keyboard**: physical keys map to Amiga raw keycodes with the same table
  as the desktop frontend.
- **Joystick**: the toggle cycles off -> keys -> cd32 (-> touch on touch
  screens). Keys is a two-button stick, the desktop frontend's
  FS-UAE-compatible mapping -- cursor keys for directions, Right Ctrl /
  Right Alt or Left Ctrl for fire, Left Alt for the second button (the
  left-hand fire keys pair with the right-hand arrows, and compact
  keyboards often lack the right-side modifiers). Cd32 adds the pad
  extras on C/X/D/S/Enter/Z/A. While a mode is enabled its mapped keys
  are captured from the Amiga keyboard, like the desktop toggle -- the
  two-mode split exists so a typing-heavy guest (a BBS terminal) keeps
  Enter and the letters on the keyboard in keys mode, and only a CD32
  title captures them. A link can preset the mode with `?joy=keys` (or
  `off`/`cd32`/`touch`), so a game URL starts with the joystick already
  on; `touch` falls back to `keys` on screens without touch.
- **Gamepads**: a USB or Bluetooth controller needs no toggle -- the page
  polls the Gamepad API every frame and whatever is plugged in drives a
  port. The first pad takes port 2 (where a game looks for its joystick),
  a second pad takes port 1, which is two-player. Claiming port 1 also
  displaces the mouse, exactly as plugging a stick into that socket does
  on real hardware; unplugging the pad plugs the mouse back in. Sticks
  and d-pads both steer. The face buttons follow the CD32 pad, a superset
  of a two-button stick: A fires (red), B is button 2 (blue), X and Y are
  green and yellow, the shoulders are rewind and forward, Start is play --
  a plain joystick guest only ever sees fire and button 2. Browsers hide
  gamepads until the page has seen a real interaction, so the first
  button press after loading may be the one that makes a pad appear.
- **Touch**: the canvas works like a trackpad, because the Amiga pointer
  only takes relative motion and an absolute finger position cannot map to
  it. One finger drags the pointer, a quick tap left-clicks, holding still
  for a moment picks the button up for a drag (icons, windows), and a
  second finger holds the right button, so Intuition menus work: hold,
  steer with the first finger, lift to select. With the joystick toggle in
  touch mode the canvas is a pad instead: a floating eight-way stick on
  the left half and a fire button on the right.

The **Fullscreen** button takes over the whole monitor, keeping the
display's 4:3 shape: the picture becomes the largest 4:3 box that fits
and is letterboxed against the monitor's own aspect ratio, so an
ultrawide gets pillarbox bars instead of a stretched screen. The
letterbox is applied by the page glue itself, not the page's stylesheet,
so it holds on any shell that embeds the emulator. While fullscreen,
small Joystick, Pause and Exit buttons sit in the top-right corner. On
iPhones, where Safari has no element fullscreen, the button pins the
shell over the page instead -- Safari's chrome stays, the page furniture
goes, and the same letterbox applies.

Once a machine boots, a status strip appears below the screen with the
same front-panel readouts as the desktop [status bar](ui.md): the PWR and
FDD LEDs (plus HDD/CD on machines fitted with those drives), the floppy
track counter, and the name of the disk in each connected drive.

What the page has to say -- a screenshot copied, a state saved, a disk
inserted, something refused -- appears as a caption across the bottom of
the screen for a few seconds, the browser's version of the desktop's
on-screen display. It is over the screen rather than under it so it reads
in fullscreen too. Before boot the same messages go to the status line in
the middle of the boot overlay, which is where a shell's own
`#load-status` element lives; the caption takes over when that line is
hidden, which is the whole time a machine is running.

Audio starts with the boot click, but a browser autoplay policy can keep
the AudioContext suspended anyway; the boot never waits for it. The
emulator runs silent and the next click or key press unlocks the sound.

(browser-save-states)=
### Save states

The page carries the desktop's [save states](ui.md#save-states), which
snapshot the whole emulated machine -- RAM, ROM, chipset, CPU, and the
inserted floppy images themselves -- in the same `.clstate` format, so a
state moves between a browser and a desktop build in either direction.
Four buttons, which insert themselves below the canvas on a shell that
does not place them:

- **Save state** downloads the snapshot as a `.clstate` file. This is the
  form that survives everything and can be shared or carried to a desktop
  build.
- **Load state...** picks a `.clstate` file and restores it; dropping the
  file on the page does the same.
- **Quick save** keeps the snapshot in the browser itself (IndexedDB),
  under a single slot, which is what resuming a game usually wants: one
  click out, one click back in, and it survives page reloads and browser
  restarts.
- **Quick load** restores that slot. It is enabled only when the browser
  holds a quick state, and its tooltip says when the state was taken,
  what was in DF0, and how far the machine had run.

Loading works from a cold page: with no machine booted, a load boots one
and restores over it, so a visitor returning to a game lands straight back
in it. A state carries its own ROM and disks and replaces the whole
machine, so nothing needs to be re-picked first -- and no boot ROM is
needed at all, not even AROS. A page whose ROM download failed, or a
self-hosted shell that serves none, can still restore a state; the
machine that comes out of it is complete. A blob that is not a readable
state of this build's format version is refused with the running machine
untouched -- including a state from an older Copperline whose format
version has moved on. If that refusal follows a boot the load itself
asked for, the page returns to its pre-boot screen rather than leaving a
machine running that nothing was restored into.

There are no keyboard shortcuts for these (the desktop's
Cmd/Alt+Shift+S and +L): every key on the page belongs to the guest, so a
host shortcut would shadow an Amiga key.

Host-side settings are not part of a machine and do not travel in a state:
the page re-applies its own volume, drive-sound, floppy-speed and
controller choices over whatever is restored.

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
  the desktop, and a programmable super-hi-res scan carries its double
  (1432-pixel, 35 ns pitch) canvas straight to the browser canvas (see
  [the presentation internals](../internals/video.md)).
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
// ...or pick a machine model: new WebEmu('A1200')
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

The constructor's optional argument picks the machine profile by name,
exactly as the desktop's `--model` flag does ("A500", "A1200", ...);
omitted, it builds the default A500, so pages written against the
model-less constructor keep booting what they always did. The static
`WebEmu.models()` lists the vetted profiles a page can offer
unconditionally (currently `A500` and `A1200` -- both boot AROS or a
plain Kickstart with nothing but a floppy; other names the desktop flag
takes are accepted too, but CDTV/CD32 want pieces a browser page cannot
supply), and its absence on an older bundle is the feature test.
`machine_model()` returns the running machine's profile name
(`undefined` for a shape no profile describes, such as a state saved
from a custom desktop config) and follows `load_state`, so a page can
re-point its machine select at what a state brought back;
`machine_summary()` is a one-line description of the machine -- profile,
CPU, chipset, RAM, ROM fingerprint -- for bug reports and diagnostics.

Input goes through `key_event(event.code, pressed)` (returns whether the key
mapped, for `preventDefault`), `mouse_delta(dx, dy)` and
`mouse_button(button, pressed)`. Mouse motion is pooled and fed to the
hardware counters at a physically plausible rate (at most 100 counts per
emulated frame): browsers coalesce pointer events, and a fast flick
delivered as one huge delta would wrap the 8-bit JOYxDAT counters and
read back as motion in the wrong direction. `set_joystick_port(port, ...)`
and `set_cd32_buttons_port(port, ...)` drive a joystick or CD32 pad in
either port (`1` or `2`) -- two ports is two players, and the hosted page
feeds them from the keyboard mapping and from the Gamepad API.
`set_port_device(port, name)` plugs a device into a port (`"mouse"`,
`"joystick"`, `"cd32"`, `"analogue"`, `"none"`); a page whose gamepad
disappears restores the mouse with `set_port_device(1, "mouse")` rather
than leaving a stuck stick where the pointer used to be. The older
`set_joystick_port2(...)` / `set_cd32_buttons_port2(...)` still work and
now forward to the port-taking calls. `reset()` power-cycles,
`resync_clock()` forgets the pacer's wall-clock anchor so a page resuming
from a pause does not sprint through the frames the pause "owed",
`eject_floppy(n)`
and `set_volume_percent(p)` do what they say, and `emulated_seconds()`
exposes the guest clock for diagnostics.

`save_state()` returns the whole emulated machine as a `Uint8Array` in the
desktop's `.clstate` format, and `load_state(bytes)` restores one --
the browser side of [save states](#browser-save-states). Where the bytes
live is the page's choice (a download, IndexedDB, a fetch); the core only
deals in the blob. Both are frame-boundary operations, which any
JS-facing call is by construction, and a blob that does not parse throws
with the running machine untouched. A load re-anchors the pacer and
repaints the restored screen immediately, so a paused page shows where it
resumes; host-side settings (volume, drive sounds, floppy speed, port
devices) are not part of the machine, so a page that keeps its own should
re-apply them afterwards. `set_floppy_sounds(on)` and
`set_floppy_sounds_volume(p)` control the synthesized drive sounds (on and
100 by default, like the desktop's `[audio] floppy_sounds` knobs).
`set_mono_audio(on)` averages the left and right channels into both
outputs, like the desktop's `[audio] channel_mode = "mono"`; off by
default, leaving Paula's hardware stereo panning.
`set_floppy_speed(percent)` / `floppy_speed()` set and read the emulated
drive speed -- 100/200/400/800 percent, or 0 for turbo -- like the
desktop's `[floppy] speed` option (see
[Configuration](configuration.md)); changes apply to the live machine.
Front-panel status getters mirror the desktop status bar's LED block and
are cheap enough to poll every frame: `power_led()` and `fdd_led()` return
booleans, `hdd_led()` and `cd_led()` return `undefined` on machines
without the drive (hide the LED), `fdd_track()` returns the cylinder under
the selected drive's head or `undefined` when no drive is selected (latch
the last value so a counter does not flicker), and `drive_connected(n)` /
`disk_name(n)` describe DF0-DF3 -- a `disk_name` of `undefined` means the
drive is empty. `serial_send(bytes)`,
`serial_take()`, `serial_input_backlog()` and `serial_dtr()` bridge
Paula's serial port to whatever byte stream the page likes (see
[the serial bridge section](#browser-serial-bridge)). The presentation pointer is only
valid until the next `run` call -- rebuild the typed-array view every frame,
because wasm memory can grow. The presentation *size* is dynamic too:
`present_width()` and `present_rows()` change when the guest switches
between a standard PAL screen (presented as the captured TV aperture crop)
and anything else (presented as the full framebuffer), so size the canvas
from both every frame rather than assuming fixed dimensions.

`www/try.js` and `www/audio-worklet.js` are the reference implementation of
all of the above, including the audio drift control.

### Optional page-shell hooks

`try.js` drives any page shell that provides its element ids; beyond the
required canvas and control bar, a shell can opt into extras by adding
elements, and pages without them are untouched:

- `#df0url` / `#kickurl` (buttons): prompt for a disk / same-origin ROM
  URL, as described above.
- `#floppy-sounds` (checkbox): toggles the synthesized floppy drive
  sounds -- motor hum, head-step clicks, read hiss -- live and at boot, so
  a shell can also default them off by shipping the box unchecked.
- `#mono-audio` (checkbox): mixes the left and right channels into both
  speakers (the desktop's `[audio] channel_mode = "mono"`), live and at
  boot, so a shell can default to mono by shipping the box checked.
  Without the element the output stays stereo unless the
  [configuration file](#browser-page-config) sets `mono_audio`.
- `#machine` (a `<select>`): hosts the machine model control, letting the
  page place and style it. Like `#floppy-speed` it is always on: without
  the element a labelled select inserts itself below the canvas shell
  (carrying the same `machine` id, so page scripts can drive it either
  way). The
  glue fills an empty select from `WebEmu.models()` (a shell may also ship
  its own options, whose values must be model names), a
  `data-default="A1200"` attribute presets the choice, and the control
  hides itself on a wasm bundle too old to take a model. Changing it
  rebuilds a running machine as described above; `?machine=` in the URL
  overrides the initial choice (model names match the way the core parses
  them, so `?machine=a1200` works).
- `#floppy-speed` (a `<select>` with option values `100`, `200`, `400`,
  `800`, and `0` for turbo): hosts the floppy drive speed control, letting
  the page place and style it. Unlike the other hooks this one is always
  on: without the element the page gets a self-inserted, labelled speed
  select directly below the canvas shell (the status strip's pattern), so
  the option is reachable on any shell. Changes apply live and at boot;
  `?fdspeed=` in the URL (`100`..`800`, `0`, or `turbo`) overrides the
  initial choice, so a game link can ship fast loading regardless of what
  the control shows by default.
- `#df0list` (a `<select>`): fills itself with the disk images the site
  serves next to the page and inserts the picked one into DF0 (queued
  when picked before boot, live after). The folder is the select's
  `data-src` attribute (default `adf/`), and the list comes from
  `<folder>/index.json` -- a JSON array of file names, or of
  `{name, url}` objects with URLs resolved against the folder. Without a
  manifest, a server directory listing of the folder (nginx `autoindex`,
  Apache, `python -m http.server`) is scraped for disk-image links
  instead. If the folder yields nothing, the select hides itself.
- `#kicklist` (a `<select>`): the same list pattern for Kickstart ROMs.
  The folder is the select's `data-src` attribute (default `kick/`), with
  the same manifest-or-directory-listing contract as `#df0list`: a
  manifest lists whatever the site chooses, while a scraped directory
  listing is filtered to raw `.rom`/`.bin` images (a list pick feeds the
  ROM loader directly, which takes uncompressed 256/512 KiB images). A
  picked ROM is fitted
  like the picker: queued before boot (the boot button relabels), and a
  running machine is power-cycled. Picks go through the same-origin
  copyright gate described above, and the list enforces it up front -- a
  cross-origin folder or manifest entry is hidden rather than offered and
  refused pick by pick. The hosted page's server carries no ROMs, so the
  select never appears there; a self-hosted shell that serves its owner's
  ROMs next to the page gets a one-click ROM chooser.
- `#pause` and `#screenshot` (buttons): pause/resume the machine, and
  copy the current screen to the clipboard. Like `#floppy-speed` these
  are always on -- without the elements the two buttons insert
  themselves below the canvas shell, so both are reachable on any shell.
  Pause stops the emulated clock (not just the page): the frame loop
  stops stepping, audio is suspended, and resuming resyncs the pacer so
  the guest carries on from where it was rather than racing to catch up;
  the button relabels itself Resume, and the fullscreen overlay carries
  a copy of it. Screenshot writes a PNG of the canvas -- exactly what
  the screen shows -- to the clipboard, falling back to downloading the
  file when the browser has no clipboard image support or refuses the
  write (an unfocused document, an insecure origin); the caption over the
  screen says which happened, since a clipboard copy has nothing else to
  show for itself.
- `#savestate`, `#loadstate`, `#quicksave`, `#quickload` (buttons): the
  [save-state controls](#browser-save-states) -- download a state, pick a
  state file, and the browser-resident quick slot. Always on like
  `#pause`: without the elements they insert themselves below the canvas
  shell. `#loadstate` is a plain button wherever the shell puts it; the
  file picker behind it is built by the glue, so no `<input type="file">`
  is needed in the shell.
- `#ledbar` (a container): hosts the front-panel status strip (LEDs,
  track counter, disk names), letting the page own its placement and
  outer styling. Without it the strip inserts itself directly below the
  canvas shell. Either way it fills in once a machine boots.
- `data-default="keys"` on the `#joy` toggle: the joystick mode the page
  starts in -- `off`, `keys`, `cd32`, or `touch` (the config file's
  `joy` and then `?joy=` in the URL override it).
- `#serial-url`, `#serial-connect`, `#serial-status`, `#serial-raw`: the
  serial/BBS bridge, described in
  [the serial bridge section](#browser-serial-bridge).

(browser-page-config)=
### The page configuration file

A site can set its defaults in one hand-editable file instead of editing
the shell: `copperline.json`, served next to the page. Every key is
optional, a missing or invalid file means no defaults, link parameters
(`?df0=`, `?kick=`, `?machine=`, `?joy=`, `?fdspeed=`) override the file
per URL, and anything the visitor changes by hand wins as usual:

```json
{
  "machine": "A1200",
  "kick": "roms/kick31.rom",
  "df0": "adf/demo.adf",
  "floppy_sounds": false,
  "mono_audio": true,
  "floppy_speed": 800,
  "joy": "keys",
  "serial_url": "wss://bbs.example.com:8443/",
  "serial_raw": false,
  "autoboot": true
}
```

`machine` picks the machine model, like `?machine=`; `kick` follows the
same-origin rule as `?kick=` (the file can only name a ROM the site
already serves); `df0` is any URL the visitor's browser may fetch, like
`?df0=`. `floppy_sounds`, `mono_audio`, and
`floppy_speed` reach the machine whether or not the shell has their
controls -- the speed select inserts itself, and a configured
`floppy_sounds` or `mono_audio` is applied at boot even with no checkbox
to show it. `serial_url` and `serial_raw` preset the
serial bridge's inputs and therefore need those elements: a shell
without them has no connect button to dial with either. `joy` picks the
starting joystick mode. `autoboot: true` powers the machine on by itself once the
emulator, the ROM, and any configured disk have loaded -- the whole
recipe for a page dedicated to one demo or a BBS: name the disk, set
`autoboot`, and a visitor lands in the running machine. Browsers keep
audio suspended until the first real click or keypress; the page unlocks
it on that gesture.

(browser-serial-bridge)=
## The serial port: dialling a BBS from a browser

The wasm build exposes Paula's serial port as a byte channel, so a page can
bridge the emulated Amiga to a network service -- the classic use being a
telnet BBS, with a terminal program running on the guest. Three calls:

- `serial_send(bytes)` queues received bytes for the guest's UART. The
  queue is unbounded and consumed at the emulated baud rate, so pace large
  transfers with `serial_input_backlog()` (stop reading the socket while
  it is large) instead of pushing megabytes at once.
- `serial_take()` drains everything the guest transmitted since the last
  call; call it once per animation frame like `take_audio`. The buffer
  behind it is bounded (oldest bytes drop if a page never drains), and it
  also carries boot-ROM/OS debug output, which a page may simply log.
- `serial_input_backlog()` reports the bytes `serial_send` has queued that
  the UART has not yet consumed -- the flow-control signal.
- `serial_dtr()` reports whether the guest is asserting the serial port's
  DTR line (CIA-B PA7 driven low). A terminal raises DTR when it opens the
  port -- serial.device does it on OpenDevice, hardware-level terminals
  set the CIA bit themselves -- and drops it on exit and at reset, so this
  is the "a terminal is actually listening" signal, exactly what a real
  modem keys off.

Browsers cannot open raw TCP, so the page's transport is a WebSocket to a
gateway that forwards to the real service --
[websockify](https://github.com/novnc/websockify) in front of a telnet
port is the standard shape, and the page must use `wss://` when it is
served over HTTPS. Telnet servers also negotiate options in-band (IAC
sequences) that a guest terminal program knows nothing about;
`www/serial-telnet.js` is a small NVT layer that answers the negotiation
(ECHO, suppress-go-ahead, binary mode for ZModem, and a terminal-type
reply of "ANSI"), unescapes inbound data, and escapes outbound data.

`try.js` wires all of this up when the page shell provides the elements:
an input `#serial-url` for the gateway URL, a button `#serial-connect`,
and optionally a status span `#serial-status` and a checkbox `#serial-raw`
that bypasses the telnet layer (for gateways to non-telnet byte services).
The hosted `/try` page omits them; a page embedding the emulator next to a
BBS adds four elements and inherits the whole flow. The guest side needs a
terminal program on a bootable disk (set to serial.device, 8N1 -- the
bridge carries whatever baud the guest picks), inserted like any other
floppy.

In telnet mode the connection follows the guest's DTR line the way a
modem follows its terminal. Clicking Connect before the terminal is up
would scroll the BBS greeting into a UART nobody is reading and forward
boot-ROM chatter to the BBS as phantom keypresses (a stray newline at a
login prompt walks straight into the new-user flow), so Connect defers
the dial until the guest's line has *settled*: DTR asserted and no guest
transmit, both held for a three-second guard period measured in emulated
time (so a throttled background tab cannot shrink it). The guard matters
because AROS raises DTR for a couple of seconds during early boot while
its kernel debug output streams to the serial port; that burst fails
both conditions, while a real terminal holds DTR silently and passes.
While deferred, the status line shows "waiting for the terminal" and the
button cancels. A connected session hangs up when the guest drops DTR
(terminal exit, reboot, power cycle) and re-arms the deferred dial, so
rebooting the terminal disk reconnects by itself. Visitors can therefore
click Connect at any point -- before booting, after booting, mid-session
before a reboot -- and the dial always lands on a listening terminal.
Raw mode is ungated for byte services and guest programs that never
drive the CIA-B DTR bit.

On the desktop build the equivalent is `[serial] mode = "tcp-connect"`
plus `connect = "host:port"` (or `--serial-connect host:port`), which
dials the service directly with no gateway in between; see
[the configuration chapter](configuration.md).

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

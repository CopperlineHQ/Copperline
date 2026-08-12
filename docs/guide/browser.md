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
one switches the select to whatever the state brings back. The **Video**
select is the same idea for the standard: PAL (the default) or NTSC (the
desktop's `[chipset] video` key) -- the standard is the Agnus crystal,
so changing it rebuilds a running machine exactly like the model select,
and `?video=NTSC` presets it per link. The
page fetches the open-source AROS ROM while it loads, so the boot button
works with no files of your own; the **Kickstart ROM** and **DF0 disk**
pickers load local images instead. Both work before or after boot: a
pre-boot choice is stashed and applied when the machine starts (the boot
button relabels to show which ROM it will use), and a post-boot pick swaps
the disk live. Disk images are recognised by content -- ADF, ADZ, DMS, IPF,
and SCP, plain or gzip/zip packed -- and are always write-protected, since
the browser has no filesystem to write changes back to. A file picker
cannot sniff content, though: it filters by extension and hides whatever
the filter leaves out, which is how an `.ipf` stayed greyed out in a
bundle that decodes IPF perfectly well. So the glue rewrites the disk
picker's filter from the running build's own format list instead of
leaving it as the page shell's hand-written HTML spelled it. A shell that
ships no filter at all still offers every file, and on iOS the pickers
filter nothing either, because the system document picker greys out
extensions it does not recognise, which would lock out `.adf` and
friends.

A Kickstart that fits is also *remembered*: the image goes into the
browser's own storage (IndexedDB, never uploaded anywhere), and the next
visit boots it with no picker round trip -- the boot button simply reads
"Boot Kickstart" again. An explicit choice always wins over the memory
(the picker, a drop, `?kick=`), and the
[saved-states panel](#browser-save-states) shows what is remembered with
a **Forget** button that puts the boot button back on AROS.

Five more controls shape what the glass shows without touching the
machine. **Monitor** is the desktop window's monitor presentation, on by
default: the CRT shader preset (bowed tube face, scanlines, aperture
grille, corner vignette -- the desktop's `[display] shader = "crt"`)
with the picture seated in one of the desktop's two bezel styles
(`[display] bezel`), rendered through WebGL2 at display resolution.
*1084* pairs the preset with the two-tone 1084 cabinet -- moulding, model
badge, logotype and power lamp -- and *Classic* with the plain rounded
frame Copperline drew before the 1084 arrived. *CRT filter*, *1084
cabinet* and *Classic bezel* select each half alone, and *Plain* is the
undecorated blit the page always had -- also what a browser without
WebGL2 falls back to (the select hides there). As on the desktop, a
drawn frame also widens what its tube shows: the whole captured raster,
border colour to the glass edges, so the opening's rounded corners crop
into overscan border the way a real tube's do, not into the picture.
And as on the desktop, the CRT pass suspends itself on programmable
scans, which have no 15 kHz line structure to draw; the bezel stays. The selected monitor fronts the page before anything boots
too -- the powered-off tube, dark glass in the moulded frame, rather
than a bare black rectangle -- and while a bezel mode is up the page
shell's own thin border around the canvas hides, since the moulded case
is the frame.
**View** is the desktop's `[display] overscan` knob: *TV* (the
default) masks the deep horizontal overscan like a CRT bezel and crops
standard screens (PAL and NTSC alike) to a TV aperture, *Full overscan*
presents the whole field Denise produced -- junk pixels, border tricks
and all.
**Screen** tints the picture like a monochrome monitor's phosphor --
black & white, green, amber, or sepia. Under the monitor path the tint is
applied in the shader to the picture alone, so the bezel's plastic never
turns phosphor-green (the desktop tints its buffer before the
presentation passes too); on the 2D fallback it is a CSS filter on the
canvas. **Deinterlace** opts interlaced (LACE) displays into the desktop's
motion-adaptive field merging; off, those fields use cheaper line doubling.
Progressive displays are identical either way. **Phosphor persistence**
opts into a 40% previous-frame decay trail, useful for field-rate flicker
effects but more expensive because every presented pixel is blended with
retained history. Both history effects default off in the browser for
maximum throughput.

All five are viewing
preferences rather than machine state, so the page remembers them in the
browser and restores them on the next visit; the machine and video
selects deliberately reset instead, because a shared `?df0=` link should
boot the same machine for everyone.

Screenshots capture the presentation buffer, not the canvas: the CRT
pass and the bezel never appear in them, exactly as on the desktop,
whose captures skip its presentation passes so they stay comparable
whatever the monitor setting. The screen tint is the one exception,
baked in as it always was.

While a machine runs (and is not paused), the page holds a screen wake
lock where the browser supports one, the way a video player does: a demo
or a long loading sequence is exactly the hands-off viewing that trips a
host's idle timeout. Pausing or stopping releases it.

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
  as the desktop frontend. The mapping is positional, and the browser does
  not report the host's layout, so the one Amiga key it cannot reach is
  `$2B` (beside Return on an ISO keyboard, `#`/`~` on a UK machine): a UK
  host board reports that position as the code the table reads as the
  number row's backslash. The on-screen keyboard below can type it.
- **On-screen keyboard**: the **Keyboard** button raises an A600 -- the one
  Amiga keyboard with no numeric keypad, so the whole machine fits a
  phone's width. All 78 keys of the ISO case are there, including both
  national keys, Help and the two Amiga keys, and they go to the keyboard
  MCU as raw keycodes. Keycaps carry UK legends by default with a UK/US
  switch on the keyboard itself, remembered per browser; the legends are
  all that changes, since the raw keycodes a machine sends are the same
  either way (what they *print* is the guest's keymap, which out of the
  box is `usa0` -- so a stock boot disagrees with the UK caps on about six
  keys). Qualifiers latch, because a touch screen has no spare finger:
  tapping Shift, Ctrl, Alt or Amiga holds it for the next keystroke,
  double-tapping locks it until tapped again, and holding it with a second
  finger behaves like the real key, so Ctrl+Amiga+Amiga reboots. Caps Lock
  latches the way the hardware does -- the keyboard MCU owns that lamp --
  and the cap lights with it. Unlike the physical keyboard, the on-screen
  keys are never captured by the joystick modes below: an on-screen Amiga
  keyboard always types. The keyboard carries its own dismiss button -- the
  small x in the cursor notch, above the UK/US switch -- so it can be put
  away in fullscreen without reaching for the page's toggle.
- **Device keyboard**: the **Device keys** button raises the phone or
  tablet's own keyboard instead, for when typing matters more than reaching
  every Amiga key -- a BBS session, a filename, a high-score name -- and
  swipe typing, predictions and a familiar layout are worth more than the
  drawn caps. What such a keyboard reports is typed text rather than key
  positions, so each character is translated back into the key that types
  it, using the same UK/US legends the drawn keyboard is set to (which is a
  statement about the guest's keymap, and out of the box that is `usa0`).
  Return, Backspace, Delete and Tab come through; predictive text and
  suggestions come through as the corrections they are, backspacing over
  what was staged. Everything with no character to send -- Ctrl, Alt, both
  Amiga keys, the function keys, Help, the cursor keys -- is unreachable
  this way and stays the drawn keyboard's job, which is why the two are a
  choice rather than a replacement. Only one is ever up: raising either puts
  the other away. The two buttons are independent switches, so either
  keyboard can be dismissed on its own. The drawn keyboard is remembered
  between visits and the device one is not, because a browser only raises a
  soft keyboard inside the tap that asked for it. The button appears on
  touch screens alone: a desktop already has the real thing.
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
small Joystick, Keys, Type, Pause and Exit buttons sit in the top-right
corner (Type, the device keyboard, on touch screens only).
Raising either keyboard there does not cover the picture: the letterbox
recomputes into the space above the keys, so the display shrinks and stays
whole. The drawn keyboard reports its own measured height for that; the
device keyboard is measured from how much of the viewport it covers, which
is what a browser offers instead of leaving room. On iPhones, where Safari has no element
fullscreen, the button pins the shell over the page instead -- Safari's
chrome stays, the page furniture goes, and the same letterbox applies.

Once a machine boots, a status strip appears below the screen with the
same front-panel readouts as the desktop [status bar](ui.md): the PWR and
FDD LEDs (plus HDD/CD on machines fitted with those drives), the floppy
track counter, and the name of the disk in each connected drive. The PWR
LED shows the desktop bar's two lit levels -- bright with the guest's
/LED line engaged, dimmed when released -- and is never dark, as a
running machine is always powered.

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

On an iPhone or iPad, leaving the browser for another app deactivates
the page's audio output at the OS level, and coming back does not
reliably reactivate it -- the pipeline can keep consuming samples with
nothing reaching the speaker. The page rebuilds its audio pipeline
whenever it returns to the foreground there, so the sound carries on;
if iOS demands a fresh gesture first, the next tap brings it back.

Hiding the tab normally puts the machine to sleep with the page, exactly
where it was until the tab returns. Ticking **Run in background** keeps
it running -- and audible -- the way a video tab keeps playing, so a long
render or a BBS transfer carries on while you read something else. The
choice is remembered per browser. Running hidden needs the sound to have
been unlocked (any click or key press does it): the audio pipeline is
what clocks the machine while the page cannot see it, so with audio still
suspended the machine sleeps as before.

A page that stays visible but is starved of animation frames -- an
unfocused window on a host bent on saving power (Windows "efficiency
mode" is the classic) -- keeps real time the same way, and needs no
option: the audio pipeline steps the machine between whatever animation
frames still arrive, so emulation and sound never slow down and only the
displayed frame rate drops to what the browser delivers. This too needs
unlocked audio; with the AudioContext still suspended there is no
fallback clock, and a starved page slows down as the browser dictates.

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
- **Saved states...** opens the panel over everything the browser
  remembers: the stored Kickstart (with its **Forget** button), the quick
  slot, and *named* states -- type a name and **Save new** keeps the
  running machine under it, as many as browser storage will hold, so one
  browser can park several games at once rather than fighting over the
  quick slot. Each row has **Load**, **Export** (downloads the stored
  state as the same `.clstate` file Save state writes, so a browser-kept
  state can still move to a desktop build), and **Delete**; saving under
  an existing name replaces it.

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
  conversion happens. Standard screens are presented as the captured TV
  aperture, a 668x540 canvas with the standard window exactly centred
  between symmetric overscan margins, so the canvas carries none of the
  bezel-mask black columns of the full framebuffer. PAL and NTSC share the
  one canvas shape -- both apertures fill the same 4:3 glass, so an NTSC
  scan's shorter 428-row crop is scaled onto the same 540 output rows.
  While the page draws a monitor bezel (`set_monitor_bezel`), the crop
  widens to the tube aperture: the whole rendered field, 668x570, an
  NTSC field's 470 rows scaled onto the same 570 -- the browser
  counterpart of the desktop's tube view.
  Non-standard frames (true overscan, programmable scans) keep the full
  716-pixel width, as on the desktop, and a programmable super-hi-res scan
  carries its double (1432-pixel, 35 ns pitch) canvas straight to the
  browser canvas (see
  [the presentation internals](../internals/video.md)). Border-only frames
  keep the previous frame's geometry, as on the desktop, so the canvas
  does not switch shape across the blanks a screen change produces.
  A frame whose render input exactly matches the previous one skips the
  render pipeline entirely (the desktop render cache's reuse detector),
  so a static screen costs no render work at all. The wasm wrapper exports
  a presentation revision that advances whenever a non-reused frame is
  copied into the presentation buffer; the page therefore also skips the
  typed-array view, canvas/WebGL texture upload, and monitor draw when the
  exact-reuse detector matches.
  There is no wgpu in the build, which keeps the wasm-opt'd wasm
  around 2.1 MiB (about 0.8 MiB over the wire).
- **Audio**: Paula's 44.1 kHz stereo mix is drained once per animation frame
  and posted to an `AudioWorklet` as transferred `Float32Array` chunks. The
  build is single threaded -- no SharedArrayBuffer, so no COOP/COEP headers
  are needed and any static host (GitHub Pages included) can serve it.
- **Pacing**: each animation frame steps the core up to the wall clock, with
  the audio queue as the master clock -- when the worklet reports more than
  ~150 ms buffered, stepping pauses until the queue drains back under
  ~90 ms (hysteresis, so a queue riding one threshold cannot flip the gate
  every report), and the pacer re-anchors while it waits, so the pause is
  forgiven rather than repaid as a burst that would drop frames from the
  output. Deficits past 100 ms (a backgrounded tab, a GC pause) are
  forgiven the same way, mirroring the native pacer's re-anchor behaviour.
  A hidden tab normally sleeps -- no animation frames, audio suspended --
  but with the run-in-background option on, the worklet's queue reports
  (messages, which background tabs still receive; only timers are
  throttled) clock the machine in rAF's place: the real-time audio
  pipeline never stops, so the tab keeps running the way a video tab
  keeps playing, skipping only the frame rendering nobody can see. The
  same clock backstops a visible page whose animation frames are being
  throttled (an unfocused window under a power-saving OS): a queue report
  that finds the last animation frame more than ~50 ms stale steps the
  machine itself, keeping emulation and audio real time while rAF is left
  to blit the newest frame at whatever rate the compositor manages.
  On a host too slow to afford a frame render per emulated frame, the
  pacer degrades the picture before it degrades the machine. It measures
  host cost per fixed 60 Hz pacing slice (normalising a late animation
  tick that catches up several slices), and only engages after sustained
  pressure near the slice budget. Alternate ticks then step with the
  render deferred, so emulation and audio hold real time and only the
  displayed rate halves. The stat line shows `render 1/2` while this is
  active; a sustained lower-cost interval disengages it without a
  one-off catch-up burst making the mode flap. Its
  `host core + render + upload + shader` figures split the average host
  milliseconds per emulated frame over the last reporting interval:
  `core` is machine advancement, `render` is the Rust framebuffer and
  presentation pipeline, `upload` is the browser-side canvas copy or WebGL
  texture submission, and `shader` is the CPU time submitting the selected
  monitor passes. WebGL commands are asynchronous, so `shader` is main-thread
  submission cost rather than a synchronous GPU-completion measurement.
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
with `crates/copperline-web/www/try.js`, `www/render-stride.js`, and
`www/audio-worklet.js` -- the
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

The constructor's first optional argument picks the machine profile by
name, exactly as the desktop's `--model` flag does ("A500", "A1200", ...);
omitted, it builds the default A500, so pages written against the
model-less constructor keep booting what they always did. The static
`WebEmu.models()` lists the vetted profiles a page can offer
unconditionally (currently `A500` and `A1200` -- both boot AROS or a
plain Kickstart with nothing but a floppy; other names the desktop flag
takes are accepted too, but CDTV/CD32 want pieces a browser page cannot
supply), and its absence on an older bundle is the feature test. The
second optional argument picks the video standard on top of the profile
(`new WebEmu('A500', 'NTSC')`), the desktop's `[chipset] video` key;
omitted, the profile keeps its own (PAL for every offered profile).
`WebEmu.video_standards()` lists the accepted names and is the matching
feature test. `machine_model()` returns the running machine's profile name
(`undefined` for a shape no profile describes, such as a state saved
from a custom desktop config) and follows `load_state`, so a page can
re-point its machine select at what a state brought back;
`video_standard()` does the same for the standard ("PAL"/"NTSC" -- the
fitted Agnus crystal, not the live BEAMCON0 bit ECS software can flip);
`machine_summary()` is a one-line description of the machine -- profile,
CPU, chipset, video standard, RAM, ROM fingerprint -- for bug reports
and diagnostics.

`insert_floppy(drive, bytes, name)` takes any format the core reads --
ADF/ADZ, extended ADF, DMS, IPF, SCP, plain or gzip/zip packed -- decided
by signature, so the name it is given is only a label. The static
`WebEmu.floppy_formats()` lists the extensions those formats conventionally
carry (`["adf", "adz", ...]`, no dots) for the one thing a page cannot
decide by content: a file input's `accept` filter, and any list it scrapes
by name. Building the filter from it is what stops a picker hiding an image
the build would happily read; its absence is the feature test on an older
bundle.

Input goes through `key_event(event.code, pressed)` (returns whether the key
mapped, for `preventDefault`), `mouse_delta(dx, dy)` and
`mouse_button(button, pressed)`. `key_raw(rawkey, pressed)` is the same
path one step lower down, taking an Amiga raw keycode instead of a
`KeyboardEvent.code`: it is what an on-screen keyboard wants, whose keys
already are Amiga keys, and it reaches the codes no positional `code`
expresses on every host layout. Mouse motion is pooled and fed to the
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
exposes the guest clock for diagnostics. `presentation_revision()` identifies
the current presentation-buffer generation without hashing it, while
`last_run_core_ms()` and `last_run_render_ms()` split the host cost of the
most recent paced call for page-side diagnostics.
`set_deinterlace(on)` switches motion-adaptive LACE field merging live
(`deinterlace_enabled()` reads it); the browser default is off, so LACE
fields are line-doubled and the weave history stays unallocated.
`set_phosphor(fraction)` selects CRT persistence from 0.0 (off, the browser
default) through 0.95, with `phosphor()` returning the quantised value.
When rendering is deferred, the next presentation ages the persistence by
all elapsed emulated fields, so returning from a hidden tab cannot revive an
old one-field trail.
Both setters re-present the held frame immediately and leave progressive,
zero-persistence output on the direct copy path.

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
`set_overscan(mode)` picks the presentation overscan, the desktop's
`[display] overscan` knob: `"tv"` (the default) masks the deep
horizontal overscan like a CRT bezel and presents standard screens
as the captured TV aperture, `"full"` presents the whole overscan field;
unknown names are ignored. The last completed frame is re-presented
under the new aperture immediately, so a paused page only has to blit.
`set_monitor_bezel(drawn)` tells the emulator a monitor front is drawn
around the picture, which widens the standard-scan crop from the TV
aperture to the tube aperture (the whole rendered field, the desktop's
tube view); full overscan and programmable scans are unaffected, and the
last completed frame is re-presented like `set_overscan`.
`set_tv_centre(h, v)` centres the TV picture on the glass, the desktop's
`[display] tv_h_centre` / `tv_v_centre` knobs (a monitor's front-panel
H-CENTER/V-CENTER controls): `h` in lo-res pixels (-16..16, positive
right), `v` in scan lines (-8..8, positive down), clamped to those
ranges. Glass the nudge exposes past the captured raster shows black; a
TV-aperture control, so full overscan ignores it. The last completed
frame is re-presented like `set_overscan`.
Front-panel status getters mirror the desktop status bar's LED block and
are cheap enough to poll every frame: `power_led()` and `fdd_led()` return
booleans, `caps_lock_led()` returns the keyboard MCU's own Caps Lock lamp
(read it rather than tracking key presses, or a state load leaves the two
disagreeing), `hdd_led()` and `cd_led()` return `undefined` on machines
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
between a standard screen (presented as the captured TV aperture crop)
and anything else (presented as the full framebuffer), so size the canvas
from both every frame rather than assuming fixed dimensions.
`present_crt_lines()` describes the same presentation for a page-side CRT
shader pass: the emulated field lines it shows (270 on the standard 50 Hz
TV aperture, 214 on a 60 Hz scan, 285 and 235 under the tube aperture of
a drawn bezel, half the presented rows in full overscan), and 0 when a
scanline effect has nothing honest to draw -- no frame yet, or a
programmable scan, where the desktop suspends its CRT preset too.

`www/try.js`, `www/render-stride.js`, and `www/audio-worklet.js` are the
reference implementation of
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
- `#background-run` (checkbox): keeps the machine running -- and audible
  -- while the tab is hidden, clocked by the audio pipeline the way a
  video tab keeps playing; unticked (the default), a hidden tab sleeps as
  it always has. Like `#machine` it is always on: without the element a
  labelled checkbox inserts itself below the canvas shell. The visitor's
  choice is remembered per browser, and the
  [configuration file](#browser-page-config)'s `background_run` is the
  starting point for first-time visitors. Running hidden needs unlocked
  audio: while the AudioContext is still suspended there is no clock, and
  the machine sleeps as before.
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
- `#video` (a `<select>`): hosts the PAL/NTSC video standard control,
  the machine select's pattern exactly -- always on (self-inserting
  without the element, carrying the `video` id), filled from
  `WebEmu.video_standards()` unless the shell ships its own options,
  preset by `data-default="NTSC"`, hidden on a wasm bundle too old to
  take a standard, and rebuilding a running machine on change with the
  ROM and disk carried over. `?video=` in the URL overrides the initial
  choice (names compare case-insensitively, so `?video=ntsc` works).
- `#overscan` (a `<select>` with option values `tv` and `full`): hosts
  the presentation overscan control (**View** on the hosted page), always
  on and self-inserting like `#video`. Changes apply live, including to a
  paused machine; the choice is a viewing preference, so the glue
  remembers it in the browser (localStorage) and restores it next visit.
  Hidden on a wasm bundle without `set_overscan`.
- `#tint` (a `<select>`): the screen tint (**Screen** on the hosted
  page) -- `none`, `bw`, `green`, `amber`, `sepia` -- applied in the
  monitor path's shaders to the picture alone (a CSS filter on the
  canvas under the 2D fallback) and baked into screenshots. Always on,
  self-inserting, and remembered in the browser like `#overscan`; it
  never touches the wasm, so it works with any bundle.
- `#deinterlace` and `#phosphor` (checkboxes): the **Deinterlace** and
  **Phosphor persistence** options described under
  [Using the hosted page](#using-the-hosted-page). Always on like
  `#floppy-speed`: without the elements two labelled checkboxes insert
  themselves below the canvas shell. Both are viewing preferences the
  glue remembers in the browser, and each hides itself on a wasm bundle
  without the matching call (`set_deinterlace` / `set_phosphor`) -- for
  a shell-hosted checkbox the input element is hidden, so a shell can
  drop the whole labelled row with a
  `.row:has(> input[hidden]) { display: none }` rule.
- `#monitor` (a `<select>` with option values `1084`, `classic`, `crt`,
  `cabinet`, `bezel`, and `plain`): the monitor presentation (**Monitor**
  on the hosted page), the desktop window's CRT shader preset and its two
  bezel styles rendered through WebGL2 -- `1084` and `classic` pair the
  preset with the 1084 cabinet or the Classic frame, `cabinet` and
  `bezel` are those frames alone. Defaults to `1084`, self-inserting,
  applied live including to a paused machine -- and to the powered-off
  monitor a page shows before boot -- and remembered in the browser like
  `#overscan`. While a bezel mode is up the glue makes the shell's
  border transparent (the moulded case is the frame), restoring it on
  the other modes; a shell that wants no part of that can simply not
  style a border. It hides itself -- and the page keeps its
  plain 2D blit -- in a browser without WebGL2.
- `#bezel-stickers` (a `<script type="application/json">` element): PNG
  stickers drawn onto the monitor front while a bezel mode is up, the
  desktop's `[display] bezel_stickers` for a hosting page -- community
  logos as die-cut decals on the plastic, with a soft drop shadow and the
  plastic's lighting. The element's content is a JSON array of up to 16
  entries with the same keys as the desktop folder's `stickers.toml`
  ([the configuration chapter](configuration.md)): `image` (a URL,
  resolved against the page -- typically a `stickers/` folder beside it),
  optional `x`/`y` (the sticker's centre as fractions of the canvas, as a
  pair), `width` (fraction of the canvas width; height follows the
  image's aspect), `rotate` (degrees clockwise) and `opacity`. Entries
  without `x`/`y` line up along the cabinet's top band in written order
  with a slight alternating tilt, exactly as the desktop lays a bare
  folder out. The decals are drawn on the canvas itself, so they follow
  the plastic through resizes and fullscreen, and never appear in
  screenshots, which capture the presentation buffer. Cross-origin images
  are requested with CORS and skipped (with a console note) when the host
  refuses. Without the element, or without WebGL2, no stickers are drawn.
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
  Apache, `python -m http.server`) is scraped for links whose extension
  the build reads (`WebEmu.floppy_formats()`, the same list the disk
  picker filters on). If the folder yields nothing, the select hides
  itself, as it does on a bundle too old to name its formats.
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
  a copy of it. Screenshot writes a PNG of the presentation buffer --
  the picture with the screen tint baked in, without the monitor
  effect's CRT pass or bezel, like the desktop's captures -- to the
  clipboard, falling back to downloading the
  file when the browser has no clipboard image support or refuses the
  write (an unfocused document, an insecure origin); the caption over the
  screen says which happened, since a clipboard copy has nothing else to
  show for itself.
- `#keyboard` (a button): raises and dismisses the on-screen A600 keyboard
  described under [Using the hosted page](#using-the-hosted-page). Always
  on like `#pause`:
  without the element the button inserts itself below the canvas shell,
  and the fullscreen overlay carries a copy of it. It hides itself on a
  wasm bundle without `key_raw`, since the on-screen keys are raw keycodes
  and there is no half of this that still works. Two notes for a shell
  hosting it: the keyboard strip is `position: fixed` so that it spans the
  viewport in the page and the screen in fullscreen alike, which means no
  ancestor of `#shell` may carry a `transform`, `filter` or `contain` (any
  of those would make the strip resolve against that box instead); and the
  glue publishes the strip's measured height as `--cl-kbd-h` on `<html>`
  (`0px` while it is closed), so a shell that wants to reserve room for it
  in its own layout can read it.
- `#devkeyboard` (a button): raises and dismisses the device's own keyboard,
  the other half of the keyboard choice above. Like `#pause` it inserts
  itself below the canvas shell when the element is absent, and the
  fullscreen overlay carries a copy of it labelled Type -- but only where
  there is a keyboard to raise. On a screen without touch, or a wasm bundle
  without `key_raw`, neither is built at all and a shell that provided the
  button has it hidden; a shell may also ship the button with the `hidden`
  attribute already set, so a desktop never shows it even for the moment
  before the glue loads, and the glue un-hides it on the touch screens it
  serves. It shares `--cl-kbd-h` with `#keyboard`,
  publishing how much of the viewport the device keyboard covers. The
  off-screen field it focuses is built by the glue and lives inside
  `#shell`, so no input element is needed in the shell.
- `#savestate`, `#loadstate`, `#quicksave`, `#quickload`, `#savedstates`
  (buttons): the [save-state controls](#browser-save-states) -- download
  a state, pick a state file, the browser-resident quick slot, and the
  saved-states panel (named states, the quick slot, and the remembered
  Kickstart with its Forget button; the panel itself is glue-built, the
  shell only places the button). Always on like `#pause`: without the
  elements they insert themselves below the canvas shell. `#loadstate`
  is a plain button wherever the shell puts it; the file picker behind
  it is built by the glue, so no `<input type="file">` is needed in the
  shell.
- `#ledbar` (a container): hosts the front-panel status strip (LEDs,
  track counter, disk names), letting the page own its placement and
  outer styling. Without it the strip inserts itself directly below the
  canvas shell. Either way it fills in once a machine boots.
- `#build-info` (a container): filled once the wasm module loads with the
  bundle's build identity -- the tag or branch and commit CI compiled it
  from (`v0.14.0 (abc123def)`, the commit linked to its GitHub page), or
  `dev build` for a bundle built outside CI -- so a page can show what is
  deployed. The element is untouched until the module resolves, so a
  shell can hide the empty state with `:empty`; without the element
  nothing is inserted. The same string is what the bug-report link files
  under its version field.
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
  "video": "NTSC",
  "kick": "roms/kick31.rom",
  "df0": "adf/demo.adf",
  "floppy_sounds": false,
  "mono_audio": true,
  "floppy_speed": 800,
  "overscan": "full",
  "tint": "green",
  "monitor": "plain",
  "joy": "keys",
  "background_run": true,
  "serial_url": "wss://bbs.example.com:8443/",
  "serial_raw": false,
  "autoboot": true
}
```

`machine` picks the machine model, like `?machine=`; `video` the PAL/NTSC
standard, like `?video=`; `kick` follows the
same-origin rule as `?kick=` (the file can only name a ROM the site
already serves); `df0` is any URL the visitor's browser may fetch, like
`?df0=`. `floppy_sounds`, `mono_audio`, and
`floppy_speed` reach the machine whether or not the shell has their
controls -- the speed select inserts itself, and a configured
`floppy_sounds` or `mono_audio` is applied at boot even with no checkbox
to show it. `overscan`, `tint`, and `monitor` (the CRT + bezel
presentation: `1084`, `classic`, `crt`, `cabinet`, `bezel`, or `plain`)
are starting points for first-time visitors only: all three are
per-browser viewing preferences the glue remembers, and a visitor's own
remembered choice wins over the file. `serial_url` and `serial_raw` preset the
serial bridge's inputs and therefore need those elements: a shell
without them has no connect button to dial with either. `joy` picks the
starting joystick mode. `background_run` starts first-time visitors with
the run-in-background box ticked (a per-browser preference the glue
remembers, so as with the viewing choices a visitor's own remembered
choice wins over the file). `autoboot: true` powers the machine on by itself once the
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
report shows the realtime factor, the frame-time distribution against the
20 ms PAL budget, how many final presentation buffers were unchanged, and
how many conservative input matches skipped rendering entirely. The same
binary builds natively for a
direct wasm-versus-native comparison on identical workloads -- the render
checksums match between the two, which is the determinism contract doing its
job. As a reference point, on an Apple-Silicon laptop the wasm build ran the
default AROS machine at 6.4x realtime and a Copper/blitter-heavy OCS demo at
2.7x, roughly 1.3-1.5x slower than native.

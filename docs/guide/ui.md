# The window, status bar, and menus

Copperline opens a single window: the emulated display presented at a
TV-like 4:3 aspect ratio, above a status bar with the machine's controls.
The window scales continuously when resized.

## Keyboard shortcuts

The app shortcut modifier is `Cmd` on macOS and `Alt` on Linux/Windows.

| macOS | Linux/Windows | Action |
|---|---|---|
| `Cmd+Q` | `Alt+Q` | Quit |
| `Cmd+S` | `Alt+S` | Save a screenshot (`copperline-screenshot-<YYYYMMDDHHmmSS>.png` in the working directory; the on-screen confirmation overlay is not part of the saved image) |
| `Cmd+R` | `Alt+R` | Start / stop a video-with-audio recording (below) |
| `Cmd+Shift+R` | `Alt+Shift+R` | Start / stop an input recording (below) |
| `Cmd+Shift+S` | `Alt+Shift+S` | Save a state (`copperline-state-<YYYYMMDDHHmmSS>.clstate` in the working directory) |
| `Cmd+Shift+L` | `Alt+Shift+L` | Load a save state from a file dialog |
| `Cmd+D` | `Alt+D` | Swap to the next disk in a drive's configured playlist |
| `Cmd+G` | `Alt+G` | Capture / release the host mouse (clicking the display also captures) |
| `Cmd+B` | `Alt+B` | Open the [debugger window](../debugger/window) |
| `Cmd+K` | `Alt+K` | Open the [debugger console](../debugger/console) |
| `Cmd+J` | `Alt+J` | Toggle joystick input mode: gamepad / keyboard (also the status-bar icon) |
| `Cmd+Shift+A` | `Alt+Shift+A` | Cycle the audio output: Default, each host device, then Disabled (also the menu's Audio Out item) |
| `Cmd+Shift++` / `Cmd+Shift+-` | `Alt+Shift++` / `Alt+Shift+-` | Raise / lower the parallel-port sampler input gain (only when a sampler is attached; also the menu's Sampler Gain item) |
| `Cmd+W` | `Alt+W` | Toggle Warp Speed (turbo) on / off |
| `Cmd+Shift+W` | `Alt+Shift+W` | Cycle the Warp Speed limit: 2x, 4x, 8x, 16x, Max |
| `Esc` | `Esc` | Close an open menu, tool window, or overlay panel; otherwise passed through to the Amiga |
| `Ctrl+Amiga+Amiga` | `Ctrl+Amiga+Amiga` | Keyboard reset (warm reboot) |

Host modifiers that are passed through to the emulated keyboard map onto
the Amiga keyboard: Alt becomes Amiga Alt, Cmd/Super becomes the left/right
Amiga keys, and left Ctrl becomes Amiga Ctrl, so `Ctrl+Amiga+Amiga` is
typed naturally. The Amiga keyboard has no right Ctrl, so host right Ctrl
also acts as the right Amiga key -- handy on PC/laptop keyboards that lack
a right Super/Win key.

All other keys are sent to the emulated machine through the real path: a
bit-timed keyboard-MCU model clocks each transition into CIA-A's serial
register over the emulated KCLK/KDAT lines, with the real handshake,
power-up stream, and recovery protocol -- so even software that talks to
the keyboard hardware directly behaves. `Ctrl+Amiga+Amiga` runs the
authentic reset protocol (reset warning, then KCLK held low), so the
reboot lands a fraction of a second after the chord, as on real hardware.

## Status bar

The status bar (44 pixels below the display) holds, left to right:

- **LED block.** PWR and FDD always; a green HDD activity LED on machines
  with a hard-disk controller (Gayle or A4000 IDE, or any SCSI adapter); a
  blue CD activity LED on CDTV/CD32 that lights while the drive reads data
  or plays CD audio (on a machine whose CD drive is a SCSI CD-ROM unit,
  the LED shows CD-DA playback; its data reads ride the HDD LED with the
  rest of the SCSI bus). A small digital counter shows the current floppy
  track.
- **Per-drive floppy controls.** Every connected drive gets a disk button
  (marked with the drive number) that opens a file dialog -- multi-select
  several images to queue a swap playlist for that drive -- plus a swap
  button that cycles to the next queued disk and an eject button. Swap and
  eject grey out when there is nothing to swap to or eject. With three or
  four drives the clusters stack two-up.
- **CD controls** on machines with a CD drive (CDTV, CD32, or a SCSI
  CD-ROM unit): a CD button that loads (or swaps) a CD image
  (`.cue`/`.iso`) with the proper media-change notification, and a CD
  eject button. These do not appear on machines without a CD drive.
- **Joystick toggle** (just left of the volume control): a gamepad or
  keyboard icon showing which source drives the joystick port. Click it to
  flip between gamepad-only and keyboard joystick emulation; see
  [](#controller-ports).
- **Volume slider**: drag, or scroll the mouse wheel over it for 5% steps.
- **Hamburger menu button**: opens the pop-up menu (below).
- **Camera button**: saves a screenshot (same as `Cmd+S` on macOS or
  `Alt+S` on Linux/Windows).
- **Pause / power / reboot buttons.** Pause freezes emulation while staying
  powered; power cold-boots (clears RAM) or powers off back to the test
  screen; reboot is a warm reset.

## Drag and drop

Disk images can be dropped anywhere on the emulator window:

- **Floppy images** (ADF/ADZ/DMS/SCP, gzip or zip packed): with one
  connected drive the disk is inserted immediately. With several, a drive
  chooser opens over the display -- click a drive, press its number
  (`1`-`4`), or press `Esc` to cancel. Dropping several floppies at once
  queues them all as the target drive's swap playlist, exactly like a
  multi-selection in the disk dialog.
- **CD images** (`.cue`/`.iso`) mount in the machine's CD drive (CDTV,
  CD32, or a SCSI CD-ROM unit), with the media-change notification.
- **Hard disk images and Kickstart ROMs** cannot be swapped at runtime; a
  notice points at the machine-configuration screen, which also refuses
  drops while it is open.

The chooser opens after the drop rather than offering per-drive drop
targets because the windowing layer reports file drops without a cursor
position. For the same reason drops are unavailable under native Wayland
(X11/XWayland works).

## Menu, tool windows, and overlay panels

```{figure} ../images/ui-preview-menu.png
:alt: The pop-up menu
:width: 75%

The pop-up menu opened from the status bar.
```

The menu opens debugger-style tool windows and smaller overlay panels. Tool
windows are separate native windows so the emulated display remains visible;
the debugger and frame analyzer can be open at the same time. Overlay panels
are drawn over the display. While either kind is open, key presses and display
clicks stay in the UI instead of reaching the Amiga; `Esc` closes the focused
tool window or overlay.

- **Machine Configuration...**: opens the configuration screen
  ([below](#machine-configuration-screen)) to reconfigure the machine and
  relaunch it. The same screen opens automatically on a no-machine start.
- **Frame Analyzer...**: pauses the machine and opens a separate diagnostic
  window showing which chip-bus owner had each Agnus colour clock across
  the captured frame, including overscan and blanking; see
  [](../debugger/window.md#frame-analyzer-pane).
- **Debugger...** (also `Cmd+B` on macOS or `Alt+B` on Linux/Windows):
  pauses the machine and opens the tabbed debugger in a tool window; see
  [](../debugger/window).
- **Console...** (also `Cmd+K` / `Alt+K`): a GDB-flavoured debugger
  command line in its own tool window; see [](../debugger/console).
- **Audio Out** (also `Cmd+Shift+A` / `Alt+Shift+A`): cycles the audio
  output through the system default, each host output device, and
  **Disabled** (sound off entirely, equivalent to `--noaudio`). The
  device switches live without a restart.
- **Calibrate Gamepad...**: the guided calibration flow, described below.
- **Joystick Input** (also `Cmd+J` / `Alt+J`, or the status-bar icon):
  toggles between gamepad-only and keyboard joystick emulation.
- **Port 1 Device / Port 2 Device**: hot-plug the controller in a game
  port, cycling mouse, joystick, CD32 pad, analogue, and empty; see
  [](#controller-ports).
- **MIDI In / MIDI Out** (shown when the serial port is in MIDI mode):
  cycle Paula's serial bridge through the host's MIDI sources and
  destinations; see the `[serial]` section of
  [Configuration](configuration.md).
- **Sampler In / Sampler Gain** (shown when a parallel-port sampler is
  attached): cycle the sampler's host capture device, and step its input
  gain (also `Cmd/Alt+Shift +/-`). Both change live. See the `[parallel]`
  section of [Configuration](configuration.md).
- **Pixel Aspect**: flips the presentation between the 4:3 CRT pixel
  aspect (the default; PAL lo-res pixels slightly wider than tall, as a
  real TV shows them) and square pixels (a 320x256 screen is an exact
  640x512, handy for pixel-exact comparison with square-pixel emulators).
  The window and its backing texture resize with the mode. The start-up
  mode comes from `[display] pixel_aspect`
  (see [Configuration](configuration.md)).
- **Warp Speed** (also `Cmd+W` / `Alt+W`): runs the emulator unpaced for
  fast-forward. Toggling back re-anchors real-time pacing cleanly.
- **Warp Limit** (also `Cmd+Shift+W` / `Alt+Shift+W`): cycles how fast warp
  runs. Because the window presents with vsync, emulating one frame per
  presented frame would cap warp at the host monitor's refresh rate (about
  1.2x for 50 Hz PAL on a 60 Hz display). The limit sets an output frame
  skip -- 2x, 4x, 8x, 16x, or **Max** -- so warp retires that many emulated
  frames per presented frame, making the effective speed roughly the limit
  times the refresh rate (host CPU permitting). `Max` runs flat out and
  still presents at vsync. The default is set by `[emulation] warp_speed`
  (see [Configuration](configuration.md)).
- **Record Video** (also `Cmd+R` / `Alt+R`): starts a video-with-audio
  recording; the same item (or shortcut again) stops it. See below.
- **Record Input** (also `Cmd+Shift+R` / `Alt+Shift+R`): records every
  input event that reaches the emulated machine; stopping writes a script
  file that `--script` replays deterministically. See below.
- **Save State** (also `Cmd+Shift+S` / `Alt+Shift+S`) and **Load State...**
  (also `Cmd+Shift+L` / `Alt+Shift+L`): snapshot the whole emulated machine
  to a file, or restore one and continue from exactly that point. See below.
- **Load Kickstart ROM...**: fit a different boot ROM. Pick a 512 KiB
  Kickstart, then optionally a second file for the extended ROM (512 KiB at
  $E00000 or 256 KiB at $F00000; Cancel to skip and remove any fitted
  extended ROM). The machine then cold-resets, as if the chip had been
  swapped and the power cycled.
- **Keyboard Shortcuts...**: the shortcut reference.
- **About...**: app version plus a summary of the emulated machine. Builds
  made from an untagged git commit append the short commit ID to the version
  shown in the window title and About panel.

```{figure} ../images/ui-preview-shortcuts.png
:alt: The keyboard shortcuts window
:width: 75%

The Keyboard Shortcuts window.
```

(machine-configuration-screen)=
## Machine configuration screen

Starting Copperline with no machine specified -- no `./copperline.toml`, no
`--config`, no ROM or override, and not a headless run -- opens the
configuration screen instead of booting. It is also available any time from the
menu's **Machine Configuration...** item, seeded with the running machine's
settings.

```{figure} ../images/ui-preview-launcher.png
:alt: The machine configuration screen
:width: 75%

The configuration screen: the machine selector across the top, category tabs
down the left, settings on the right, and the action bar along the bottom. Here
an A1200 is selected on the Memory tab; Zorro III RAM is greyed with the reason
"needs 32-bit CPU" (the A1200's 68EC020 has a 24-bit bus).
```

The layout is:

- **Machine selector** (top). Pick a machine -- A1000, A500 (OCS), A500, A500+,
  A600, A1200, A3000, A4000, CDTV, or CD32. With no profile chosen the A500 is
  highlighted,
  since that is the machine the defaults describe. Selecting a machine applies
  that profile's defaults (chipset, CPU, RAM, gate array, RTC) to every tab;
  settings that no longer apply (an IDE image on a machine with no IDE port, a
  CD image
  on a machine with no CD drive) are dropped so they cannot block a launch.
- **Category tabs** (left sidebar). *System* (chipset and Agnus/Denise
  overrides, video standard, RTC, identify board), *CPU* (model, FPU, clock,
  caches), *Memory* (chip/fast/slow/Zorro III RAM), *ROM* (Kickstart and
  extended ROM), *Floppy* (drive count and per-drive image and write-protect),
  *Hard Disk* (IDE master/slave, the SCSI controller -- A2091, A4091, or the
  A3000's onboard SCSI -- and its boot ROM and units), *Host Mounts* (host
  directories served live as AmigaDOS volumes: up to four mounts, each with a
  boot priority and a read-write/read-only **Access** field), *CD* (image,
  insert delay, CD32 NVRAM), *Zorro* (extra autoconfig boards by metadata
  file, with a config panel for a WASM plugin board's declared options),
  *Serial* (serial mode and MIDI input/output endpoints),
  *Parallel* (the parallel-port device -- None, Printer, or Sampler -- and, for
  the sampler, its host audio input and input gain),
  *Input* (the controller device in each game port and the joystick input
  source), and *A/V & Emu* (audio output device, channel mode, stereo
  separation, overscan, pixel aspect, phosphor, floppy sounds and
  volume, power-on, pacing, realtime priority, warp speed).
- **Settings rows** (right pane). `[<]`/`[>]` step through a value, On/Off
  buttons flip a toggle, and the **Browse** and **Clear** buttons set or remove
  a file path through a native file dialog. On the *Hard Disk* tab, once an IDE
  or SCSI drive has an image a small editable box appears next to **Browse**:
  click it and type to set the volume name for a directory mount (left blank, a
  directory mount inherits the host directory's name; the box has no effect on a
  raw HDF). A setting that does not apply to the chosen machine is greyed and
  shows why in place of its control -- "needs 32-bit CPU" for Zorro III RAM,
  "needs 68020+" for the FPU, "needs A600/A1200/A4000" for IDE.
- **Action bar** (bottom). **Load...** and **Save...** read and write a `.toml`
  config
  through a file dialog (Save writes a minimal file, only the settings that
  differ from the chosen profile's defaults, so it reads like the example
  configs). **Defaults** resets to the selected profile. **Run** validates the
  configuration and boots it; if anything is wrong -- an unusable RAM size, a
  missing disk image, an option the chosen machine cannot use -- the reason is
  shown on the status line and you stay on the screen to fix it.

Saved files use the same schema as a hand-written `copperline.toml`
(see [](configuration.md)), so the screen and the config file are
interchangeable: configure a machine and save it, or load an existing config to
tweak it. **Run** builds the machine in place, so the configuration screen and a
direct `--config` launch produce an identical machine.

## Recording video

`Cmd+R` on macOS or `Alt+R` on Linux/Windows (or the menu's "Record Video")
starts capturing the emulated display and sound to
`copperline-video-<YYYYMMDDHHmmSS>.avi` in the working directory; pressing it again
stops and finalizes the file. A red REC
badge sits in the display's top-right corner while a recording runs --
like the screenshot overlay, the badge, status bar, and menus are never
part of the captured video.

The file is an AVI with lossless ZMBV video (the DOSBox capture codec:
zlib-compressed keyframes plus frame deltas, which keeps typical Amiga
output to a few MB per minute) and uncompressed 16-bit stereo PCM audio
at 44.1 kHz. It plays directly in VLC, mpv, and anything else built on
ffmpeg; for other players, transcode with
`ffmpeg -i copperline-video-<ts>.avi out.mp4`.

Frames and audio are captured on the emulated timeline, not the host
clock: the recording stays in sync even when the host stutters, and a
capture made under Warp Speed plays back at normal speed. The audio
track is tapped before the status bar's volume slider, so recordings
keep full level regardless of the live output volume. Pausing (or
powering off) suspends the capture; recording resumes when emulation
continues.

## Recording input

`Cmd+Shift+R` on macOS or `Alt+Shift+R` on Linux/Windows (or the menu's
"Record Input") starts logging every input event that reaches the emulated
machine -- key presses with their hold times, mouse buttons and motion,
joystick / CD32-pad controls, analogue pot positions, and floppy inserts,
on whichever port carries each device -- each stamped
with its emulated time. Pressing it again stops the recording and writes
`copperline-input-<YYYYMMDDHHmmSS>.clscript` in the working directory: a plain
text file of scripted-input directives that
`copperline --script FILE` replays exactly, because the core is
deterministic and the events re-fire at the same emulated timestamps.

This is the direct way to turn "I can reproduce it by hand" into a
regression: play the sequence once while recording, then keep the script
(optionally together with a [save state](#save-states) to skip the
lead-in) as a deterministic, shareable reproduction. The format and the
headless `--record-input` variant are described in
[](headless.md#input-recording-and-script-files).

(save-states)=
## Save states

`Cmd+Shift+S` on macOS or `Alt+Shift+S` on Linux/Windows (or the menu's
"Save State") writes a snapshot of the whole emulated machine to
`copperline-state-<YYYYMMDDHHmmSS>.clstate` in the working directory: CPU,
chip/slow/fast RAM, ROM, the full chipset and CIA state, floppy images
(including unsaved in-memory changes), expansion boards, and CD/NVRAM
state. `Cmd+Shift+L` / `Alt+Shift+L` (or "Load State...") restores one; the
machine continues from exactly the saved point, byte-for-byte -- the core
is deterministic, so a resumed run is indistinguishable from one that was
never interrupted.

States are taken at emulated-frame boundaries and are versioned: a file
from an older, incompatible build is refused with a clear message rather
than producing a corrupt machine.

A state is self-contained: it carries its own RAM, ROM, and chipset, so
loading one always restores the machine it was taken on -- even if you
launched Copperline with a different config. When the loaded machine
differs from the running one -- a different model, chipset, video
standard, RAM size, or even a different Kickstart of the same machine
(the ROM is fingerprinted) -- the load reconfigures to match the state
and tells you so (the load message names the restored machine, e.g.
"reconfigured to A1200 / 68EC020 / AGA / PAL"); your current config is
not silently mixed in. Two caveats:

- Hard-drive images (HDF files) are referenced by path, not embedded.
  The state reopens the same file on load, so guest writes made to the
  hard drive *after* the snapshot are still visible after restoring --
  treat a state as a CPU/chipset snapshot, not a disk backup. In-memory
  volumes (directory-as-HDD) and floppy images are embedded whole.
- CD images are likewise reopened by path; keep the cue/bin where it was.

The headless flags `--save-state-after SECS PATH` and `--load-state PATH`
script the same feature for [debugging workflows](headless.md): snapshot a
long-running program just before the scene under investigation once, then
iterate from the state in seconds instead of re-emulating minutes. The
file format and what exactly is (and is not) captured are specified in
[the internals chapter](../internals/savestate.md).

## Controller ports

An Amiga has two game ports, and either accepts any controller. Copperline
models that: each port carries a device -- `mouse`, `joystick`, `cd32` pad,
`analogue` paddles, or `none` -- set with `[input] port1`/`port2` in the
config (or `--port1`/`--port2`, or the launcher's *Input* tab). The default
is the stock wiring, a mouse in port 1 and a joystick in port 2 (a CD32 pad
on the CD32 profile). The runtime menu's **Port 1 Device** / **Port 2
Device** items hot-plug a different device live, exactly like swapping the
physical plug: the old device's lines release, and the port's quadrature
counters hold.

The host mouse drives the (lowest-numbered) port with a mouse plugged in
and feeds its JOYxDAT counters. Click the display (or press `Cmd+G` on
macOS or `Alt+G` on Linux/Windows) to capture the host mouse; the same
shortcut releases it. While an overlay panel is open, host cursor motion is
not fed to the emulated mouse. Tool windows likewise keep the debugger or
analyzer interaction separate from Amiga mouse input.

A USB gamepad drives the emulated digital joystick on whichever port one is
plugged into: directions through JOYxDAT, fire through /FIRx, and a second
button through POTxY/POTGOR. A `cd32` device speaks the CD32 serial button
protocol instead, including the red/blue/green/yellow and transport
buttons, on either port. An `analogue` device presents pot resistances on
the POTxX/POTxY pins; no live host device maps to it yet -- drive it with
`--pot-after` scripting or the control protocol's `input.analogue`.

Copperline can also emulate the joystick from the host keyboard. There are
two explicit input modes, and the active one is always shown by the gamepad
/ keyboard icon in the status bar (next to the volume control), so a "my
keys aren't working" surprise can be spotted and fixed at a glance:

- **gamepad** (the default): use only a calibrated gamepad; every keyboard
  key passes through to the Amiga. With no pad connected there is no
  joystick input. This is the no-surprise mode for interactive AmigaOS
  setup.
- **keyboard**: use the keyboard-joystick mapping so the joystick port
  works without a controller.

With joysticks (or pads) in *both* ports -- a two-player setup -- the
gamepad and the cursor-key mapping drive one port each, and the mode picks
which source gets the lower-numbered port. Whenever no physical pad is
present, a second keyboard mapping on the numeric keypad stands in for it,
so two players can share one keyboard.

With mice in both ports, the host mouse drives the lower-numbered one and
the cursor-key mapping drives the second as an emulated mouse (in
`keyboard` mode; in `gamepad` mode the second mouse is undriven and the
keyboard passes through). The same applies whenever the keyboard-routed
port carries a mouse: the mapping's keys become the pointer.

Click the status-bar icon to flip between them; the menu's **Joystick Input**
item and `Cmd+J` on macOS / `Alt+J` on Linux and Windows do the same. Set the
starting mode with `[input] joystick` in the config (or `--joystick MODE`, or
the launcher's *Input* tab).

The primary keyboard mapping is FS-UAE-compatible: cursor keys for
directions, and Right Ctrl or Right Alt for fire. For CD32 pad buttons,
`C` is red/fire, `X` is blue, `D` is green, `S` is yellow, Return is
play/pause, `Z` is rewind, and `A` is forward. On a mouse port the same
keys drive the pointer: cursor keys move it, the fire keys are the left
button, `X` the right, and `D` the middle. The second (numpad) mapping is
`8`/`2`/`4`/`6` for directions, `0` for fire, `.` for the second button,
and numpad Enter for play. While a mapping owns its keys, they are not
sent to the Amiga keyboard.

## Gamepad calibration

Pads are read through raw `gilrs` events with no controller database, so
each controller is calibrated once: push each control when prompted. This
records raw axis/button codes and directions, which makes any pad work
regardless of database coverage and handles inverted or odd axis layouts
automatically.

```{figure} ../images/ui-preview-calibration.png
:alt: The gamepad calibration window
:width: 75%

The calibration window mid-flow. Skip covers pads without the CD32 extras.
```

Run it either from the menu ("Calibrate Gamepad...") -- which ends with a
live test of the finished bindings and a Save button that makes them live
immediately -- or from the terminal with `copperline --calibrate-gamepad`.
The steps are the four directions, fire (CD32 red), button 2 (CD32 blue),
and the optional CD32 green/yellow/play/rewind/forward buttons; every step
waits for the pad to return to neutral before sampling, so a held control
cannot bleed into the next binding.

Calibrations are saved per controller UUID in
`~/.config/copperline/gamepads.toml` (`$XDG_CONFIG_HOME` respected;
`%APPDATA%\copperline\` on Windows).

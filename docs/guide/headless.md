# Headless and scripted execution

Copperline runs without a window for continuous integration, regression
testing and scripted capture. A run that schedules a screenshot, screenshot
expectation, frame dump, GIF clip or `--coverage` file, or that waits for a
`--run` program's return code with `--exit-on-return`, opens no window,
needs no display server, and runs unthrottled; the emulated result is the
same as a windowed run's. Other scheduled work (`--save-state-after`,
`--audio-wav`, `--record-input`, the scripted-input flags) does not make a
run headless by itself: add a capture flag to end the run.
`--warp-boot` and `--warp-until` only affect windowed sessions, since a
capture run is already unthrottled from the start.

Physical floppy drives attached through [FluxBridge](fluxbridge.md) require
wall-clock pacing. For repeatable captures, use image-backed media, a fixed
RTC seed when a clock is fitted, and repeatable inputs. Live networking,
serial or audio input, and changing host files also affect replay; see the
[host boundary](../internals/architecture.md#determinism-and-the-host-boundary).

## Capturing screenshots

To emulate for 30 emulated seconds, save the display as a PNG, and exit:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out-30s.png
```

Repeat the flag to capture several screenshots in one run:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 10 /tmp/menu.png \
  --screenshot-after 30 /tmp/level.png \
  --screenshot-after 60 /tmp/boss.png
```

The run exits after the last scheduled capture of any kind: screenshot,
expectation, GIF clip or frame dump. The one exception is a frame dump,
which ends the run as soon as its last frame is written, even when a
screenshot or clip is scheduled later. Schedule those to finish before the
dump does, or use a separate run.

(screenshot-expectations)=
## Checking screenshots against expected images

`--expect-screenshot SECS PATH [TOLERANCE]` captures the frame at SECS
exactly as `--screenshot-after` would (the same capture path, so an image
saved by `--screenshot-after` compares pixel for pixel) and compares it with
the PNG at PATH. Repeat the flag for several checks; they schedule alongside
`--screenshot-after` and the run ends after the last capture of either kind:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --expect-screenshot 10 expected/menu.png \
  --expect-screenshot 30 expected/level.png 0.001
```

`TOLERANCE` is optional: a number with a decimal point is the largest
fraction of pixels allowed to differ (`0.001` is 0.1 percent of the frame),
a plain integer is an absolute pixel count (`250`). Without it the images
must match exactly. Only the colour channels are compared.

A failed check prints one line naming the expected image, how many pixels
differ (or the size mismatch) and the bounding box of the differences:

```text
expect-screenshot: expected/level.png: MISMATCH 1204 of 421056 pixels differ (0.286%), bounding box (96,112)-(311,201), tolerance exact; wrote expected/level.actual.png; wrote expected/level.diff.png
```

The captured frame is written next to the expected image as
`<stem>.actual.png`, and for a same-sized mismatch a red-on-black mask of
the differing pixels as `<stem>.diff.png`. A missing expected image fails the
same way but still writes the `.actual.png`, so a new expectation is blessed
by renaming that file into place. A failed check does not cut the run
short: every other scheduled screenshot, state save and input still fires,
and the process exits with status 3 once the run finishes (see
[](#exit-statuses)).

(exit-statuses)=
## Exit statuses

| Status | Meaning |
|---|---|
| 0 | The run completed and every screenshot expectation held |
| 1 | Copperline itself failed: configuration, assets, host errors |
| 3 | At least one `--expect-screenshot` check failed |
| 4 | `--exit-on-return` was given but the guest program had not returned when the run ended |
| 0-255 | The guest program's AmigaDOS return code, with `--exit-on-return` (see [](run.md#exit-on-return)) |

A non-zero guest return code takes precedence over a failed expectation; a
zero one does not hide it (the run still exits 3). A guest that stops the
emulator through the [uaelib trap's](run.md#uaelib-trap) `ExitEmu` ends the
run with status 0, or 3 if an expectation had failed.

## Dumping frame sequences

To capture consecutive frames (useful for debugging animation or beam
synchronization):

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --dump-frames /tmp/frames --dump-start 24 --dump-count 120
```

`--dump-frames DIR` needs `--dump-count COUNT`; `--dump-start SECS`
defaults to 0. Frames are saved in DIR as `frame-000000.png`,
`frame-000001.png`, and so on, and the run exits after the last one.

(capturing-gif-clips)=
## Capturing GIF clips

To write a stretch of the display as an animated GIF -- the same clip the
window's [Save Clip as GIF](ui.md#saving-a-gif-clip) produces, from a
scheduled emulated time instead of the last few seconds:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --gif-after 24 /tmp/intro.gif --gif-seconds 5
```

`--gif-after SECS PATH` starts the clip at SECS emulated seconds.
`--gif-seconds N` sets its length (up to 120 seconds) and defaults to
`[recording] clip_seconds` (10); with `clip_seconds = 0` it must be given.
Frames are thinned to `[recording] clip_fps` (25 per second on PAL, 30 on
NTSC by default), presented through the same crop and aspect as a
screenshot, and given delays from the emulated timeline, so the file plays
back at real speed however fast the run went.
The flag repeats to bracket several moments in one run; each clip is its
own file. A run ends when every scheduled capture has finished, whichever
kind comes last: a clip that completes early keeps running for a later
`--screenshot-after`, and a finished screenshot schedule waits for a clip
that is still recording. A clip has no audio track; record the run's
sound with `--audio-wav`.

Timestamps are absolute like every other scheduled flag, so the capture
composes with `--load-state`, `--script` and the scheduled-input flags:
resuming a 120 s state, `--gif-after 125 clip.gif` starts five seconds
in. Because frames are taken on the emulated timeline, the same run
produces a byte-identical GIF every time. `--gif-after` cannot be combined
with `--benchmark-until`, `--gdb` or `--control`.

(save-states-headless)=
## Save states in headless runs

Save states allow fast iteration by skipping lengthy boot and loading sequences:

```sh
# Create a snapshot at 120 emulated seconds:
./target/release/copperline --config copperline.example.toml --noaudio \
  --save-state-after 120 /tmp/snapshot-120s.clstate \
  --screenshot-after 121 /tmp/marker.png

# Resume from snapshot to capture output at 125 seconds:
./target/release/copperline --config copperline.example.toml --noaudio \
  --load-state /tmp/snapshot-120s.clstate \
  --screenshot-after 125 /tmp/scene.png
```

`--save-state-after` repeats, and the run keeps going after each save; it
needs a capture flag (here the marker screenshot) to run headless and end.
After `--load-state`, every scheduled timestamp stays absolute on the
original emulated timeline: resuming a 120 s state, `--press-after 130 ...`
fires ten seconds in, and anything scheduled before 120 s has already
passed.

A state written by `--save-state-after` carries the same metadata card as
one saved from the window: a thumbnail of the display at the save
(rendered by the display path `capture.screenshot` uses, so it matches a
screenshot of the same frame), the emulated and wall-clock save times, a
machine summary, and the media names. Read it without a session:

```sh
copperline-ctl state-info /tmp/snapshot-120s.clstate --thumbnail /tmp/at120.png
```

prints the card as JSON and writes the thumbnail PNG (see the
[control protocol reference](../debugger/control.md#state-snapshot-files)).

## Guest code coverage

`--run PROG --coverage FILE` counts every instruction the program retires
from its first instruction to its exit and writes lcov line/function
coverage to FILE. On its own it is a capture run that ends with the program;
with `--screenshot-after` it ends with the last screenshot instead:

```sh
./target/release/copperline --factory --noaudio \
  --run build/hello --coverage build/lcov.info \
  --coverage-source-map /build/src=$PWD/src
```

The program's own debug information (`-g`, or vasm `-linedebug`; a
`PROG.elf` beside it is picked up) supplies the source lines.
`--coverage-source-map FROM=TO` rewrites a source path prefix in the
output and can be repeated. See
[Guest code coverage](../debugger/profiling.md#guest-coverage).

## Scripted input events

Keyboard, mouse, controller and media events can be scheduled at emulated
timestamps. Every flag repeats:

| Flag | Description |
|---|---|
| `--press-after SECS KEY` | Press and release a key (100 ms hold) |
| `--key-after SECS KEY MS` | Hold a key for MS milliseconds |
| `--type-after SECS TEXT` | Type TEXT on the US Amiga keyboard from SECS, one key every 100 ms ([below](#typing-text)) |
| `--click-after SECS BTN MS [PORT]` | Hold mouse button `left`, `right` or `middle` for MS (default port 1) |
| `--joy-after SECS BTN MS [PORT]` | Hold a joystick/CD32-pad control for MS on port 1-4 (default port 2; 3 and 4 are the parallel-port adapter's sockets) |
| `--mouse-after SECS DX DY [PORT]` | Move the mouse by a relative delta (DX, DY) (default port 1) |
| `--mouse-to-after SECS X Y [PORT]` | Steer the sprite 0 pointer to screen pixel (X, Y) (default port 1) |
| `--pot-after SECS X Y [PORT]` | Set an analogue paddle/pot position, 0-255 per axis (default port 2) |
| `--pen-after SECS X Y [PORT]` | Hold the light pen over pixel (X, Y), the `--mouse-to-after` coordinates; a negative coordinate lifts it off (default: the port with the pen) |
| `--insert-disk-after SECS DFN PATH` | Insert a disk image into `df0`..`df3` |
| `--defer-disk-insert SECS DFN` | Start with DFN empty and insert its configured disk at SECS |
| `--insert-cd-after SECS PATH` | Swap the CD image (`.cue`, `.iso`, `.nrg`, `.chd`) in the machine's CD drive |
| `--freeze-after SECS` | Press the freezer cartridge's button (`--cartridge hrtmon`): HRTMon takes over at SECS |
| `--expect-screenshot SECS PATH [TOLERANCE]` | Compare the frame at SECS with a PNG ([above](#screenshot-expectations)) |
| `--script FILE` | Read scripted-input directives from FILE ([below](#input-recording-and-script-files)) |
| `--record-input PATH` | Record all machine-bound input for the whole run and write it as a script to PATH on exit |

KEY is a raw key code in decimal, `0x45` or `$45` form, or a name:
letters, digits, `space`, `return`, `tab`, `esc`, `del`, `backspace`,
`up`/`down`/`left`/`right`, `f1`-`f10`, `shift`/`lshift`/`rshift`, `caps`,
`ctrl`, `lalt`/`ralt` and `lami`/`rami`. Names ignore case. The
`--joy-after` controls are `up`, `down`, `left`, `right`, `red` (or `fire`),
`blue`, and the CD32 pad's `green`, `yellow`, `play`, `rwd` and `ffw`.

(typing-text)=
### Typing text

`--type-after SECS TEXT` turns a host string into the key presses a person
would make on a US Amiga keyboard: letters, digits and the punctuation on
the key caps, with Shift held for upper case and shifted symbols. `\n` is
Return, `\t` Tab, `\e` Esc, `\b` Backspace, and `\\` a literal backslash;
characters the US keymap has no key for are an error. Keys are paced in
emulated time (each held 50 ms, one key every 100 ms, Shift 20 ms ahead of
the key it qualifies), a rate the keyboard MCU's ten-event type-ahead buffer
and the guest's keyboard driver keep up with:

```sh
./target/release/copperline --config workbench.toml --noaudio \
  --type-after 40 "dir df0:\n" \
  --screenshot-after 45 /tmp/listing.png
```

The text expands into ordinary scheduled key events, so it composes with
`--press-after`, is recorded by `--record-input` as the individual keys,
keeps its absolute timestamps under `--load-state`, and is available in
[script files](#input-recording-and-script-files) as `type SECS TEXT`. The
same typing queue serves the control protocol's `input.type` and the
window's *Paste as Keystrokes* action (`Cmd+Shift+V` / `Alt+Shift+V`, see
[](ui.md)).

A light pen (`[input] port1`/`port2 = "lightpen"`) is positioned with
`--pen-after` and its tip switch / trigger is the port's `red` button in
`--joy-after` (or `left` in `--click-after`): `--pen-after 5 320 128
--joy-after 5.5 red 200 2` holds the pen over pixel (320, 128) and presses it
half a second later. The pulse only reaches Agnus from the port the board
wires to `LP` (port 2 on every Amiga after the A1000). Joysticks in the
parallel-port four-player adapter (`--parallel joystick-adapter`) are ports
`3` and `4` in `--joy-after`'s trailing PORT token.

`--freeze-after` requires an enabled cartridge (`--cartridge hrtmon` or
`[cartridge] model`, see [Configuration](configuration.md#freezer-cartridge)).
Later `--screenshot-after` captures show the monitor screen, and a
`--save-state-after` snapshot taken inside the monitor resumes inside it.
Input recordings store freeze events as `freeze-after SECS`.

(input-recording-and-script-files)=
### Input scripts and recording

An input script is a text file of directives, one per line, written like
the flags without their leading dashes:

```text
# Automated test script
joy-after 60.0 red 300
key-after 75.0 f1 200
type 80.0 "dir df1:\n"
insert-disk-after 90.0 df1 "disk2.adf"
joy-after 95.0 red 300 1
expect-screenshot 100.0 "expected/level.png" 0.001
freeze-after 120.0
```

The accepted directives are `press-after`, `key-after` (also spelled
`hold-key-after`), `type` (also `type-after`), `click-after`, `joy-after`,
`mouse-after`, `mouse-to-after`, `pot-after`, `pen-after`,
`insert-disk-after`, `defer-disk-insert`, `insert-cd-after`,
`freeze-after` and `expect-screenshot`, each taking the same arguments as
its flag. Other flags, including `--screenshot-after` and
`--save-state-after`, are not accepted in a script, and a script cannot
include another. Blank lines and lines starting with `#` are ignored, and
double quotes keep a token with spaces (such as a path) together.

Run with `--script`:

```sh
./target/release/copperline --config myconfig.toml --noaudio --cartridge hrtmon \
  --script test.clscript --screenshot-after 125 /tmp/out.png
```

Host clipboard sharing is off unless asked for, windowed or headless (see
`[clipboard]` in [Configuration](configuration.md#clipboard)). Sharing fits
a services board, so it is part of the machine. To replay a recording made
in a window that had sharing on, pass `--clipboard` so the headless machine
matches; a headless run never reads the host clipboard, so the replay stays
deterministic.

To record an interactive session to a script file:

- Press `Cmd+Shift+R` (macOS) or `Alt+Shift+R` (Linux/Windows) in the
  emulator window to start, and again to stop. The script is written as
  `copperline-input-<YYYYMMDDHHmmSS>.clscript` in the recordings folder
  (see [Recording input](ui.md#recording-input)).
- Or launch with `--record-input /tmp/session.clscript`, which records the
  whole run and writes the file on exit.

## Setting a deterministic real-time clock (RTC)

To test date- and time-dependent guest software, seed the RTC with a fixed timestamp:

```sh
# Set RTC to 2005-03-18 01:58:29 UTC (Unix timestamp 1111111109):
./target/release/copperline --config test.toml --noaudio \
  --rtc-time 1111111109 \
  --screenshot-after 45 /tmp/clock.png
```

`--rtc-time` takes Unix seconds or `"YYYY-MM-DD HH:MM[:SS]"` and fits a
battery clock if the machine has none. The clock then ticks in emulated
time, so every run boots to the same moment. Use `--rtc-frozen` to hold
the RTC at the seed without advancing. Kickstart 2.0 and later load the
system time from the battery clock at boot; Kickstart 1.3 needs
`SetClock LOAD` in the startup-sequence. A
[control protocol](../debugger/control.md) session can also read, move,
freeze and resume the clock mid-run with `rtc.get` and `rtc.set`.

## Audio capture and stem separation

- `--noaudio`: Run silently.
- `--audio-wav PATH`: Write the mixed stereo output to a 32-bit float
  44.1 kHz WAV file in emulated time, instead of playing it.
- `--audio-stems DIR --audio-stems-mode LIST`: Write separate WAV stems into
  `DIR` instead of playing the output. `--audio-stems` and `--audio-wav`
  cannot be combined. Without `--audio-stems-mode`, the list comes from
  `[audio] stem_granularity` in the configuration. `LIST` is a
  comma-separated combination of:
  - `master`: Master mix (`DIR/master.wav`).
  - `source`: Individual audio sources conditionally generated based on configured
    hardware: `DIR/paula.wav` and `DIR/drivesounds.wav` are always created, while
    `DIR/cdda.wav`, `DIR/mt32.wav`, `DIR/coppersynth.wav`, `DIR/toccata.wav`, and
    `DIR/mhi.wav` depend on the configuration and compiled features. With
    `--load-state`, additional source files are created because the restored
    machine may have different hardware; unused sources produce silent files.
  - `channel`: Individual physical hardware channels (`DIR/paula-0.wav` through `DIR/paula-3.wav`).
- `--profile-live-audio SECS`: Run a windowless Paula-to-host-audio
  profiling workload for SECS seconds and exit; combine it with
  `COPPERLINE_AUDIO_PROFILE=1` for live-audio counters (see
  [Peripherals](../internals/peripherals.md)).

## Benchmarking CPU performance

Measure host emulation throughput without a window:

```sh
./target/release/copperline --config demo.toml --benchmark-until 30
```

The emulator runs unthrottled until the absolute emulated time 30 s (after
`--load-state`, the target must lie beyond the state's own time), then
reports the emulated and host time taken, the frame count and rate, the
p50/p90/p99/max host time per frame, and every frame that took longer than
20 ms, and exits. Live audio is off unless `--audio` is given.

`--benchmark-until` cannot be combined with scheduled screenshots or
expectations, frame dumps, GIF clips, save-state writes, scheduled input,
scheduled floppy or CD inserts, input recording, `--exit-on-return`,
`--profile-live-audio`, `--gdb` or `--control`.

## Automated compatibility testing (vAmigaTS)

Copperline includes a test runner for the [vAmigaTS](https://github.com/dirkwhoffmann/vAmigaTS)
test suite:

```sh
COPPERLINE_VAMIGATS_DIR=/path/to/vAmigaTS \
COPPERLINE_VAMIGATS_KICK13=/path/to/kick13.rom \
COPPERLINE_VAMIGATS_FILTER=bbusy0 \
cargo test --release --test vamiga_ts -- --ignored --nocapture
```

Test options:

- `COPPERLINE_VAMIGATS_FILTER=TEXT`: Run only the cases whose name contains TEXT.
- `COPPERLINE_VAMIGATS_LIMIT=N`: Maximum tests to run.
- `COPPERLINE_VAMIGATS_SECONDS=SECS`: Screenshot time for every case,
  overriding each case's own wait from its shipped script (default 9 s).
- `COPPERLINE_VAMIGATS_OUT=DIR`: Directory to save test screenshots.
- `COPPERLINE_VAMIGATS_BASELINE=DIR`: Baseline directory for automated PNG comparison.
- `COPPERLINE_VAMIGATS_VAMIGA=PATH`: Path to reference `VAHeadless` binary.
- `COPPERLINE_VAMIGATS_VAMIGA_SETUP=SETUP`: Machine setup for every
  reference run, instead of the one each case's script names.

## Importing a WinUAE state

`--load-uss scene.uss` derives the CPU, chipset and memory from a WinUAE
AmigaStateFile, verifies the configured ROM, and skips one reconstructed
frame. Scheduled times start on Copperline's new timeline. See
[WinUAE state import](winuae-state.md) for commands, supported chunks and
limits on using these states for frame profiling.

## Debugging headless runs

These interfaces are documented with the debugger:

- The `COPPERLINE_DBG_*` environment variables add breakpoints,
  watchpoints, instruction traces, Copper-list dumps and per-hit
  screenshots to any run without changing its timeline
  ([Headless debugger](../debugger/headless.md)).
- `--waveform PATH` with `--wave-trigger`, `--wave-duration` and
  `--wave-signals` writes a VCD chip-signal trace for GTKWave
  ([Waveform export](../debugger/waveform.md)).
- `--control ADDR` runs the machine under the JSON-RPC
  [control protocol](../debugger/control.md), and `--gdb ADDR` under a
  [GDB remote stub](../debugger/gdb.md). Each owns the run loop, so neither
  combines with the scheduled capture and input flags; use the protocol's
  own capture, state and input methods instead.

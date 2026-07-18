# Headless and scripted runs

The emulated core is deterministic and independent of wall-clock pacing, so
the preferred way to verify behaviour -- in CI, in regression tests, or
while developing -- is a headless run: the window stays hidden, the core
runs unthrottled, and the result is reproducible.

## Screenshots

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out-30s.png
```

`--screenshot-after SECS PATH` emulates for SECS *emulated* seconds, saves
the framebuffer as a PNG, and exits.

## Frame dumps

For frame-to-frame rendering glitches, dump consecutive rendered frames:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --dump-frames /tmp/frames --dump-start 24 --dump-count 120
```

Files are written as zero-padded PNGs into the directory and the emulator
exits after the requested count. `--dump-start` defaults to 0.

(save-states-headless)=
## Save states

`--save-state-after SECS PATH` writes a [save state](ui.md#save-states) of
the whole machine at SECS emulated seconds and keeps running;
`--load-state PATH` restores one before the run starts, resuming from the
state's emulated timeline. Together they collapse long debug loops: pay
the boot/loading time once, then iterate from just before the scene under
investigation.

```sh
# Once: snapshot 2 minutes in, just before the scene under investigation.
./target/release/copperline --config copperline.example.toml --noaudio \
  --save-state-after 120 /tmp/snapshot-120s.clstate \
  --screenshot-after 121 /tmp/throwaway.png

# Then iterate: each run resumes at 120s and reaches 125s in seconds.
./target/release/copperline --config copperline.example.toml --noaudio \
  --load-state /tmp/snapshot-120s.clstate \
  --screenshot-after 125 /tmp/scene.png
```

The core is deterministic, so a resumed run is byte-identical to an
uninterrupted one -- screenshots from either path can be compared
directly. Scripted-input timestamps (below) are absolute emulated time:
after `--load-state` of a 120s state, a `--press-after 60 ...` has
already passed and fires immediately, and a `--press-after 130 ...`
fires 10 seconds in. Save states do not embed hard-drive or CD image
file contents; they are reopened from their original paths.

## Scripted input

Input can be scheduled at emulated timestamps, which composes with
screenshots and frame dumps to drive menus, trainers, and loaders
deterministically:

| Flag | Effect |
|---|---|
| `--press-after SECS KEY` | Press and release an Amiga key (default ~100 ms hold) |
| `--key-after SECS KEY MS` | Hold a key for exactly MS milliseconds (for modifier chords) |
| `--click-after SECS BUTTON MS [PORT]` | Press a mouse button (`left`/`right`/`middle`) for MS milliseconds (default port 1) |
| `--joy-after SECS BUTTON MS [PORT]` | Press a joystick / CD32-pad control (`up`/`down`/`left`/`right`/`red` (alias `fire`)/`blue`/`green`/`yellow`/`play`/`rwd`/`ffw`) for MS milliseconds (default port 2) |
| `--mouse-after SECS DX DY [PORT]` | Apply a relative mouse motion of (DX, DY) counter steps (default port 1) |
| `--pot-after SECS X Y [PORT]` | Set an analogue controller's stick/paddle position, 0-255 per axis (default port 2) |
| `--floppy-drives COUNT` | Connect `COUNT` floppy drives (`1` to `4`), so scheduled inserts can target empty external drives |
| `--insert-disk-after SECS DFN PATH` | Insert a disk image into `df0`..`df3` |
| `--defer-disk-insert SECS DFN` | Start with the configured drive empty, then insert its configured image |
| `--insert-cd-after SECS PATH` | Swap the CD image (`.cue`/`.iso`) in the machine's CD drive (CDTV, CD32, or a SCSI CD-ROM unit) |
| `--script FILE` | Run scripted-input directives from a file (below) |
| `--record-input PATH` | Record all machine-bound input for the whole run; the script is written to PATH on exit |

`KEY` is an Amiga raw key code (`0x45`, decimal also accepted) or a name:
`ctrl`, `lalt`, `lami`, `f1`, `esc`, `left`, letter and digit keys, and so
on. All the flags repeat, so several inputs can be queued:

```sh
./target/release/copperline --key-after 14.0 ctrl 500 --press-after 14.1 c
```

The controller-port flags take an optional trailing `PORT` token (`1` or
`2`) naming the game port the event lands on, so any controller wiring set
with `--port1`/`--port2` (see the configuration guide) can be driven:
omitted, each flag keeps its traditional port (mouse events port 1,
joystick/pot events port 2), so existing invocations are unchanged. The
events drive the named port's electrical lines whatever device is
configured there. (One consequence of the optional token: a positional
ROM/disk path literally named `1` or `2` cannot directly follow one of
these flags -- write it as `./1`.)

```sh
# Joystick in port 1, mouse in port 2, and inputs aimed at each.
./target/release/copperline --port1 joystick --port2 mouse \
  --joy-after 43 up 3000 1 --click-after 43 left 3000 2 \
  --mouse-after 43.5 20 10 2
```

## Input recording and script files

Long input sequences live in a script file instead of the command line:
one directive per line in the flag syntax without the leading dashes,
with `#` comments, blank lines, and double-quoted paths allowed. Only the
scripted-input directives are accepted -- a typo cannot silently change
emulator configuration.

```
# drive a loader prompt, then start
joy-after 60.0 red 300
key-after 75.0 f1 200
insert-disk-after 90.0 df1 "disk 2.adf"
# port-tagged forms work here too: fire on port 1, paddles on port 2
joy-after 95.0 red 300 1
pot-after 96.0 50 200
```

Run it with `--script FILE` (combines freely with the other flags).

Rather than writing scripts by hand, record one: in the window,
`Cmd+Shift+R` on macOS or `Alt+Shift+R` on Linux/Windows starts and stops a
live-input recording, written to
`copperline-input-<YYYYMMDDHHmmSS>.clscript` in the working directory; the
headless equivalent `--record-input PATH` records the whole run and
writes the file on exit. Every input event that reaches the emulated
machine is captured with its emulated timestamp -- key holds, mouse
buttons and motion, joystick / CD32-pad controls, analogue pot positions,
and floppy inserts -- so a manually driven session replays
deterministically:

```sh
# Play through the section by hand once...
./target/release/copperline --config copperline.example.toml \
  --record-input /tmp/session.clscript

# ...then replay it headlessly with the same emulated inputs.
./target/release/copperline --config copperline.example.toml --noaudio \
  --script /tmp/session.clscript --screenshot-after 60 /tmp/check.png
```

Recorded times are absolute emulated seconds, which makes recordings
compose with save states: `--load-state` of a snapshot plus the script
recorded from that point is a complete, shareable reproduction. Mouse
motion is captured at frame granularity (one `mouse-after` per frame of
movement); CD inserts are not recorded -- use the `[cd]` config section
for those.

The recorder is port-aware: each port is diffed according to the device
plugged into it (mouse buttons/motion, joystick/pad controls, or analogue
`pot-after` positions), and a port token is emitted only when it differs
from the directive's default -- a session on the stock mouse+joystick
wiring records byte-identically to the pre-port-aware format. Sessions on
other wirings note it in a `# ports: port1=... port2=...` header comment;
replay the script together with the matching `--port1`/`--port2` flags.
Hot-plugging a device mid-recording closes that port's open holds; the
device change itself has no directive and is not replayed.

## A deterministic guest clock

The emulated core never depends on the host clock -- with one deliberate
exception: the battery-backed RTC mirrors host time on machines that have
one, and an RTC-less machine boots to whatever date the guest OS invents.
Both are wrong for testing time-dependent guest software. `--rtc-time`
(or `[machine] rtc_time`) fits a battery clock seeded to a fixed instant --
Unix seconds or `"YYYY-MM-DD HH:MM:SS"` -- that then ticks in *emulated*
time, so every run boots to the same time and reads the same clock at the
same emulated instant, on any host. `--rtc-frozen` stops it entirely.

```sh
# Validate a TOTP generator against an RFC 6238 vector time: the guest
# boots with the clock at 2005-03-18 01:58:29 UTC (unix 1111111109),
# deterministically, on every run.
./target/release/copperline --config auth.toml --noaudio \
  --rtc-time 1111111109 \
  --screenshot-after 45 /tmp/code.png
```

Kickstart 2.0+ loads system time from the battery clock at boot on its
own; Kickstart 1.3 needs `SetClock LOAD` in the startup-sequence. A
[control-protocol](../debugger/control) session can also inspect, move,
freeze, and resume the clock mid-run with `rtc.get` / `rtc.set`.

## Audio capture

- `--noaudio` runs silent (live audio is otherwise on by default).
- `--audio-wav PATH` writes the mixed stereo output as a 32-bit float
  44.1 kHz WAV in emulated time instead of playing it -- useful for
  comparing audio behaviour across runs.
- `--profile-live-audio SECS` runs a windowless Paula-to-cpal profiling
  workload; combine with `COPPERLINE_AUDIO_PROFILE=1` for live-audio
  counters (see [](../internals/peripherals)).

## Benchmarking

`--benchmark-until SECS` runs the deterministic core frame by frame with no
window until the absolute emulated-time target SECS is reached, then reports
host-CPU counters (emulated seconds advanced, wall-clock elapsed, frame
count, frames per second) and exits:

```sh
./target/release/copperline --config sota.example.toml --benchmark-until 30
```

It is the canonical way to measure host-CPU cost while optimising the
emulator: the core is deterministic, so the emulated workload is identical
run to run and only the wall-clock time moves. Audio defaults to the null
backend (pass `--audio` to keep live audio), and the mode is mutually
exclusive with anything that needs a window or scheduled work --
`--screenshot-after`, `--dump-frames`, `--save-state-after`,
`--profile-live-audio`, `--record-input`, scripted input, and scheduled
disk inserts are all rejected. `--bench-until` is an accepted alias.

## Live control

Scripted flags fix the whole run in advance. For a tool that needs to
inspect, decide, and steer mid-session -- set a breakpoint, resume,
rewind, inject input, capture the screen -- run the
[control protocol](../debugger/control) instead: `--control ADDR` serves
a headless machine over JSON-RPC (with `--control-token` /
`--control-info` for the auth handoff), `--control-gui ADDR` attaches
the same server to a normal windowed session, and the bundled
`copperline-ctl` drives either from the shell. `--record-input` works
with `--control` too: injected input is journaled into the same
`.clscript` format, so an interactive control session replays
deterministically.

## Investigating a run

The [headless debugger](../debugger/headless) layers on top of any of
these runs through `COPPERLINE_DBG_*` environment variables: breakpoints,
watchpoints, instruction traces, Copper-list dumps, and per-hit screenshots,
all without a window.

```sh
RUST_LOG=info \
COPPERLINE_DBG_BREAK=C033C2 COPPERLINE_DBG_DUMP=C09580:4 \
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out.png
```

For chip-bus timing questions, the [waveform export](../debugger/waveform)
records a trigger-based VCD trace of the internal chipset signals during
the same kind of run (`--waveform out.vcd --wave-trigger pc=0xC033C2
--wave-duration 20000cck`, with `--wave-signals` selecting the signal
groups) for viewing in GTKWave.

## The vAmigaTS compatibility suite

An ignored integration test runs ADFs from a local
[vAmigaTS](https://github.com/dirkwhoffmann/vAmigaTS) checkout through
Copperline with Kickstart 1.3 in DF0:, captures screenshots after the
suite's default 9-second wait, and can compare them against a baseline
directory or a vAmiga reference render:

```sh
COPPERLINE_VAMIGATS_DIR=/path/to/vAmigaTS \
COPPERLINE_VAMIGATS_KICK13=/path/to/kick13.rom \
COPPERLINE_VAMIGATS_FILTER=bbusy0 \
cargo test --release --test vamiga_ts -- --ignored --nocapture
```

Optional variables: `COPPERLINE_VAMIGATS_LIMIT=N` (cap test count),
`COPPERLINE_VAMIGATS_SECONDS=SECS` (screenshot delay),
`COPPERLINE_VAMIGATS_OUT=DIR` (keep generated configs and screenshots),
`COPPERLINE_VAMIGATS_BASELINE=DIR` (require PNGs to match a baseline), and
`COPPERLINE_VAMIGATS_VAMIGA=/path/to/vAmiga` plus
`COPPERLINE_VAMIGATS_VAMIGA_SETUP=NAME` (render vAmiga references via its
RetroShell regression path).

The reference renderer is vAmiga's headless build (tested against v4.4):

```sh
git clone https://github.com/dirkwhoffmann/vAmiga
cmake -S vAmiga/Core -B vAmiga/Core/build -DCMAKE_BUILD_TYPE=Release
cmake --build vAmiga/Core/build -j8   # produces VAHeadless
```

For pixel comparison run Copperline with `COPPERLINE_SHOT_RAW=1` so the
screenshot is the native 716-wide framebuffer (the same cutout vAmiga's
regression dump uses), then rank the divergences with the offline
comparator, which normalises the two emulators' palette expansions and
capture offsets:

```sh
COPPERLINE_SHOT_RAW=1 COPPERLINE_VAMIGATS_DIR=/path/to/vAmigaTS COPPERLINE_VAMIGATS_OUT=/path/to/out COPPERLINE_VAMIGATS_VAMIGA=/path/to/VAHeadless cargo test --release --test vamiga_ts -- --ignored --nocapture

tools/vamigats-compare.py /path/to/out | less   # worst cases first
```

A full 1929-case sweep with references takes about an hour; screenshot
timing is frame-aligned in neither emulator, so tests with animating
output show phase noise - rank first, then eyeball pairs.

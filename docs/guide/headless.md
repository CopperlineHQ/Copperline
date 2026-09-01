# Headless and scripted execution

Copperline supports non-interactive, headless execution for continuous integration,
automated regression testing, and scripted media capture. Headless runs execute
unthrottled without creating a window or connecting to a display server.

The exception to unthrottled, deterministic execution is when a physical floppy drive
is attached via FluxBridge (see [Physical floppy drives](fluxbridge.md)): physical drives
require real-time wall-clock pacing to match the mechanical drive spindle, and access
external media that is not captured in emulator state.

## Capturing screenshots

To emulate for a specified number of emulated seconds, write a PNG screenshot, and exit:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 30 /tmp/out-30s.png
```

Multiple screenshots can be captured during a single execution by repeating the flag:

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --screenshot-after 10 /tmp/menu.png \
  --screenshot-after 30 /tmp/level.png \
  --screenshot-after 60 /tmp/boss.png
```

The process exits once the final requested screenshot has been saved.

## Dumping frame sequences

To capture consecutive frames (useful for debugging animation or beam synchronization):

```sh
./target/release/copperline --config copperline.example.toml --noaudio \
  --dump-frames /tmp/frames --dump-start 24 --dump-count 120
```

Frames are saved as zero-padded PNG files (`000000.png`, `000001.png`, etc.) in the
specified output directory.

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

When resuming with `--load-state`, all scheduled-input and screenshot timestamps
remain referenced to the original emulated timeline.

## Scripted input events

You can schedule keyboard, mouse, and joystick inputs at specific emulated timestamps:

| Flag | Description |
|---|---|
| `--press-after SECS KEY` | Press and release a key (~100 ms hold) |
| `--key-after SECS KEY MS` | Hold a key for specified duration in milliseconds |
| `--click-after SECS BTN MS [PORT]` | Click mouse button (`left`, `right`, `middle`) for MS (default port 1) |
| `--joy-after SECS BTN MS [PORT]` | Trigger joystick/CD32 button (`up`, `down`, `left`, `right`, `red`, `blue`, etc.) (default port 2) |
| `--mouse-after SECS DX DY [PORT]` | Move mouse by relative delta (DX, DY) (default port 1) |
| `--mouse-to-after SECS X Y [PORT]` | Steer sprite 0 pointer to pixel coordinates (X, Y) (default port 1) |
| `--pot-after SECS X Y [PORT]` | Set analogue paddle/pot position (0-255) (default port 2) |
| `--insert-disk-after SECS DFN PATH` | Insert a disk image into `df0`..`df3` |
| `--defer-disk-insert SECS DFN` | Delay insertion of configured disk until SECS |
| `--insert-cd-after SECS PATH` | Swap CD image (`.cue`, `.iso`, `.chd`) in CD drive |
| `--freeze-after SECS` | Press the freezer cartridge's button (`--cartridge hrtmon`): the HRTMon monitor takes over at SECS |
| `--script FILE` | Execute script file containing input directives |
| `--record-input PATH` | Record all inputs to script file on exit |

Key identifiers can be raw key codes (`0x45`) or standard names (`ctrl`, `lalt`,
`lami`, `f1`, `esc`, alphanumeric characters).

`--freeze-after` needs a fitted cartridge (`--cartridge hrtmon` or
`[cartridge] model`, see [Configuration](configuration.md#freezer-cartridge));
the monitor's screen is then what a later `--screenshot-after` captures,
and a `--save-state-after` taken inside the monitor resumes inside it. An
input recording writes a press as a `freeze-after SECS` script line.

(input-recording-and-script-files)=
### Input scripts and recording

Input sequences can be stored in text files (one command per line without leading dashes):

```text
# Automated test script
joy-after 60.0 red 300
key-after 75.0 f1 200
insert-disk-after 90.0 df1 "disk2.adf"
joy-after 95.0 red 300 1
freeze-after 120.0
```

Run with `--script`:

```sh
./target/release/copperline --config myconfig.toml --script test.clscript --screenshot-after 100 /tmp/out.png
```

To record an interactive session to a script file:
- Press `Cmd+Shift+R` (macOS) or `Alt+Shift+R` (Linux/Windows) in the emulator window.
- Or launch with `--record-input /tmp/session.clscript`.

## Setting a deterministic real-time clock (RTC)

To test date- and time-dependent guest software, seed the RTC with a fixed timestamp:

```sh
# Set RTC to 2005-03-18 01:58:29 UTC (Unix timestamp 1111111109):
./target/release/copperline --config test.toml --noaudio \
  --rtc-time 1111111109 \
  --screenshot-after 45 /tmp/clock.png
```

Use `--rtc-frozen` to hold the RTC at the initial seed without advancing.

## Audio capture and stem separation

- `--noaudio`: Run silently.
- `--audio-wav PATH`: Write mixed stereo output to a 32-bit float 44.1 kHz WAV file.
- `--audio-stems DIR --audio-stems-mode LIST`: Export separate WAV stems into `DIR`.
  `LIST` is a comma-separated combination of:
  - `master`: Master mix (`DIR/master.wav`).
  - `source`: Individual audio sources conditionally generated based on configured
    hardware: `DIR/paula.wav` and `DIR/drivesounds.wav` are always created, while
    `DIR/cdda.wav`, `DIR/mt32.wav`, `DIR/coppersynth.wav`, `DIR/toccata.wav`, and
    `DIR/mhi.wav` are exported only when those sound devices are fitted.
  - `channel`: Individual physical hardware channels (`DIR/paula-0.wav` through `DIR/paula-3.wav`).

## Benchmarking CPU performance

Measure host emulation throughput without rendering to a window:

```sh
./target/release/copperline --config demo.toml --benchmark-until 30
```

The emulator runs unthrottled for 30 emulated seconds, prints execution metrics
(elapsed host time, emulated time, average FPS), and exits.

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
- `COPPERLINE_VAMIGATS_LIMIT=N`: Maximum tests to run.
- `COPPERLINE_VAMIGATS_SECONDS=SECS`: Delay before screenshot (default: 9s).
- `COPPERLINE_VAMIGATS_OUT=DIR`: Directory to save test screenshots.
- `COPPERLINE_VAMIGATS_BASELINE=DIR`: Baseline directory for automated PNG comparison.
- `COPPERLINE_VAMIGATS_VAMIGA=PATH`: Path to reference `VAHeadless` binary.

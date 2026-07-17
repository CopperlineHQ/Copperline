# AGENTS.md

Guidance for AI coding agents driving Copperline, a cycle-driven Amiga emulator
(OCS/ECS/AGA) written in Rust. CLAUDE.md is a symlink to this file.

The most useful property for an agent: the emulated core is deterministic and
independent of wall-clock pacing. Headless runs are unthrottled, reproducible
byte-for-byte, and need no display or audio device, so the reliable way to
verify anything (a program you are developing, a config, a regression) is a
headless run with a scheduled screenshot, not a manual window session.

## Maintainer-local instructions

If `AGENTS.local.md` or `CLAUDE.local.md` exists in the repository root, read
it before starting work. Those files are gitignored, carry maintainer- and
machine-specific instructions, and take precedence over this file.

## Building and configuring

```sh
cargo build --release    # debug builds are far too slow for emulation use
```

Copperline bundles the open-source AROS boot ROM, so it boots with no config
file and no Kickstart at all. Configuration is a TOML file
(`copperline.example.toml` is the commented reference companion to
`docs/guide/configuration.md`), and the common machine knobs are also CLI
flags layered on top: `--model A500|A1200|CD32|...`, `--chipset OCS|ECS|AGA`,
`--cpu 68000..68060`, `--chip`/`--fast`/`--slow` memory sizes,
`--floppy-drives N`. A bare ROM or disk-image path is accepted positionally:

```sh
./target/release/copperline --model A1200 --fast 8M KICK31.ROM game.adf
```

Running with no arguments at all opens an interactive launcher window; any
flag suppresses it, so headless invocations never block on it.

## Headless verification

All `SECS` timestamps below are absolute *emulated* seconds, not wall-clock.
Full reference: `docs/guide/headless.md`.

```sh
# Emulate 30s, save the framebuffer as PNG, exit.
./target/release/copperline --config my.toml --noaudio \
  --screenshot-after 30 /tmp/out.png

# Dump 120 consecutive rendered frames starting at 24s.
./target/release/copperline --config my.toml --noaudio \
  --dump-frames /tmp/frames --dump-start 24 --dump-count 120
```

Audio: `--noaudio` runs silent; `--audio-wav PATH` captures the mixed output
as a WAV in emulated time instead of playing it.

Guest clock: `--rtc-time "2005-03-18 01:58:29"` (Unix seconds also accepted)
fits a battery clock seeded to that instant, ticking in emulated time -- the
guest boots to the same deterministic time on every run, which is how to
test time-dependent guest software (TOTP vectors, date logic).
`--rtc-frozen` pins it to the seed exactly.

## Scripted input

Input is scheduled at emulated timestamps and composes with screenshots and
frame dumps to drive menus, loaders, and games deterministically. All flags
repeat.

| Flag | Effect |
|---|---|
| `--press-after SECS KEY` | Press and release an Amiga key (~100 ms hold) |
| `--key-after SECS KEY MS` | Hold a key for exactly MS milliseconds |
| `--click-after SECS BUTTON MS [PORT]` | Mouse button (`left`/`right`/`middle`) for MS ms (default port 1) |
| `--joy-after SECS BUTTON MS [PORT]` | Joystick / CD32-pad control (`up`/`down`/`left`/`right`/`red`/`blue`/...) (default port 2) |
| `--mouse-after SECS DX DY [PORT]` | Relative mouse motion (default port 1) |
| `--pot-after SECS X Y [PORT]` | Analogue stick/paddle position, 0-255 per axis (default port 2) |
| `--insert-disk-after SECS DFN PATH` | Insert a disk image into `df0`..`df3` |
| `--script FILE` | Same directives from a file, one per line, no leading dashes |
| `--record-input PATH` | Record all machine-bound input as a replayable script |

`KEY` is a raw key code (`0x45`) or a name (`ctrl`, `f1`, `esc`, letters,
digits). A session played by hand under `--record-input` (or Cmd+Shift+R /
Alt+Shift+R in the window) replays deterministically via `--script`.

Either controller port takes any device -- `[input] port1/port2` in the
TOML, or `--port1`/`--port2` (`mouse`/`joystick`/`cd32`/`analogue`/`none`;
default mouse + joystick, CD32 pad on the CD32 profile). The scripted-input
flags' optional trailing `PORT` token (`1` or `2`) aims an event at either
port; omitted, each flag keeps its traditional port, so existing scripts
are unchanged.

## Save states

`--save-state-after SECS PATH` snapshots the whole machine;
`--load-state PATH` resumes it. A resumed run is byte-identical to an
uninterrupted one. Pay a long boot/loading sequence once, then iterate from
just before the scene of interest:

```sh
./target/release/copperline --config my.toml --noaudio \
  --save-state-after 120 /tmp/at120.clstate --screenshot-after 121 /tmp/x.png
./target/release/copperline --config my.toml --noaudio \
  --load-state /tmp/at120.clstate --screenshot-after 125 /tmp/scene.png
```

Scheduled-input timestamps stay absolute after `--load-state`: resuming a
120s state, `--press-after 130 ...` fires 10 seconds in and
`--press-after 60 ...` has already passed.

## Live control (interactive sessions)

The scripted flags fix a run in advance. To inspect, decide, and steer
mid-session -- breakpoints, resume, rewind, input injection, media swaps,
screen capture, streaming observation, diagnostic capture -- use the Copperline
Control Protocol, a JSON-RPC 2.0 interface over loopback TCP designed for
scripts and AI agents (`docs/debugger/control.md`):

```sh
./target/release/copperline --config my.toml --noaudio \
  --control :0 --control-info /tmp/ccp.json &
copperline-ctl --info /tmp/ccp.json status
copperline-ctl --info /tmp/ccp.json break.add '{"kind": "pc", "addr": "0xFC0100"}'
copperline-ctl --info /tmp/ccp.json continue    # blocks until the stop event
```

For a persistent observation session, use `--repl`; subscriptions belong to
that authenticated connection and end when it disconnects:

```text
# Shell:
copperline-ctl --info /tmp/ccp.json --repl
# Then enter in the REPL:
events.subscribe {"events":["frame","serial","interrupt","media"],"frame_interval":50}
```

The event streams are bounded, so check their drop counts when observations
must be complete. The same session can use `trace.start`/`trace.stop` and
`waveform.start`/`waveform.stop` to bracket file-backed diagnostics at runtime
once an event identifies the interesting window. See the protocol reference
for payloads, limits, and status methods.

`--control-gui ADDR` attaches the same server to a windowed session. The wire
format is newline-delimited JSON-RPC, so anything that speaks TCP can drive it
directly. A remote GDB stub (`--gdb`, `docs/debugger/gdb.md`) serves 68k-aware
debuggers.

## Diagnostics and benchmarking

- A headless debugger rides along on any run via `COPPERLINE_DBG_*`
  environment variables: breakpoints, watchpoints, instruction traces,
  Copper-list dumps, per-hit screenshots (`docs/debugger/headless.md`).
  All `COPPERLINE_*` variables are snapshotted at startup and cannot change
  at runtime.
- `--waveform out.vcd` with `--wave-trigger`/`--wave-duration` exports a VCD
  chip-signal trace for GTKWave (`docs/debugger/waveform.md`).
- `--benchmark-until SECS` measures host-CPU cost of the deterministic
  workload and exits; it is mutually exclusive with the scheduled-work flags.

## Working on Copperline itself

```sh
cargo test                        # unit suite, no external assets needed
cargo test --release -- --ignored # integration tests (need local ROMs/disks)
cargo clippy && cargo fmt --check # both expected clean
```

User and developer documentation lives in `docs/` (MyST Markdown): usage in
`docs/guide/`, debugger interfaces in `docs/debugger/`, architecture and
timing models in `docs/internals/`. Document changes in the same change that
makes them: anything user-visible (config/CLI surface, UI, debugger knobs)
updates the matching chapter in `docs/guide/` or `docs/debugger/`, and
architecture or timing-model changes update `docs/internals/`. Copperline
models Amiga hardware, not individual titles: fixes must describe
68000/Agnus/Denise/Paula/CIA behavior, never branch on program identity.
ROMs and disk images are local assets and are never committed.

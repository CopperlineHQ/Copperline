# Control protocol

The Copperline Control Protocol (CCP) is a versioned JSON-RPC 2.0
interface over loopback TCP for driving the emulator from scripts,
editors, CI, and AI agents: inspect state, set breakpoints, resume,
rewind, inject input, swap media, and capture the display -- all inside
one session, without restarting the process. It complements the
environment-driven [headless debugger](headless.md) (which is
snapshot-once at startup) and [remote GDB](gdb.md) (whose wire protocol
cannot carry chipset state, media, or input).

Two server modes share one command surface:

```sh
# Headless: the server owns the machine (like --gdb); paused at reset
# until a client resumes. Port 0 picks a free port.
./target/release/copperline --config kick13.example.toml --noaudio \
    --control :0 --control-info /tmp/ccp.json

# Windowed: attach a control server to a normal interactive session.
./target/release/copperline --config kick13.example.toml --control-gui :7710
```

On startup the server prints one machine-parseable stderr line:

```
copperline-control: listen=127.0.0.1:52114 token=1f0c... proto=1
```

`--control-info FILE` also writes it as a JSON object
(`{"listen": ..., "token": ..., "proto": 1}`, owner-readable only),
which is the preferred handoff since command lines are visible in `ps`.
`--control-token TOKEN` pins the token instead of generating one.

## The bundled client

`copperline-ctl` wraps the wire protocol for shells and scripts:

```sh
copperline-ctl --info /tmp/ccp.json status
copperline-ctl --info /tmp/ccp.json break.add '{"kind": "pc", "addr": "0xFC0100"}'
copperline-ctl --info /tmp/ccp.json continue      # blocks until the stop
copperline-ctl --info /tmp/ccp.json --repl        # METHOD [JSON] per line
```

Each response prints as one JSON line; a JSON-RPC error sets a nonzero
exit status. Anything that speaks TCP works just as well -- the protocol
is newline-delimited JSON, so `nc`, Python, or an editor extension can
drive it directly.

## Wire format and auth

One JSON-RPC 2.0 object per newline-terminated UTF-8 line. Every client
request must carry an `id`; responses echo it. The first successful
`hello {"token": ...}` (or `auth {"token": ...}`) authenticates the
connection; a wrong token gets one error and the connection is closed,
and anything except `hello`/`auth` is refused until then. `hello`
returns only version fields (`{"proto": 1, "emulator": ..., "authed":
...}`), so it is safe to answer before auth.

One client at a time (the GDB-stub convention). In headless mode a
detach leaves the machine paused and the server listening; `shutdown`
ends the emulator (windowed: closes the application).

Numbers in parameters are decimal values; strings are hex with an
optional `0x` or `$` prefix, so `"addr": "$DFF096"` and
`"addr": 14676118` are the same address.

## Execution model

Resume verbs -- `continue`, `step {n}`, `step_over`, `step_out`,
`step_copper`, `step_frame {n}`, `run_until {...}` -- do not answer
immediately: the response is the eventual **stop event**, so a script's
natural flow is "resume, block, inspect the reply". At most one resume
may be outstanding (`-32002` otherwise); inspection commands are still
serviced while the machine runs, at a frame boundary. `pause` ends a
pending resume (both requests receive the stop position). `run_until`
takes exactly one of `pc`, `vpos` (optional `hpos`), `frame`, `cck`, or
`seconds`.

Every stop event carries a consistent position on the emulated timeline:

```json
{"reason": "breakpoint", "detail": "Breakpoint at $FC0100",
 "pc": 16515328, "frame": 122, "vpos": 44, "hpos": 101,
 "cck": 8712345, "seconds": 2.456,
 "retired_instructions": 1745210}
```

Reasons: `breakpoint`, `watchpoint`, `reg_watch`, `beam_trap`,
`copper_break`, `catch`, `task_catch`, `step`, `target`, `pause`,
`user_pause`, `double_fault`, `reverse`, `budget` (a bounded step ran
out of its instruction budget), and `last_writer` (only as the
`position` field of a `last_writer` reply).

Resume verbs accept a `collect` list -- read-only requests evaluated
atomically at the stop and returned inside the stop event -- so one
round trip can resume, wait, and gather everything:

```json
{"method": "continue", "params": {"collect": [
    {"method": "regs.get"},
    {"method": "mem.read", "params": {"addr": "$20000", "len": 16}},
    {"method": "capture.digest"}
]}}
```

## Determinism and journaling

Commands land at deterministic boundaries of the emulated timeline:
instruction boundaries while paused, frame boundaries while running.
Injected input is journaled exactly like live input -- into the
reverse-debug replay log when time travel is armed, and into
`--record-input` recordings (supported headless too) -- and every
`input.*` response reports the emulated time the action landed
(`applied_at_seconds`), so an interactive control session converts into
a deterministic `.clscript` reproduction. The exceptions are documented
where they occur: `mem.write` is not part of the replay journal (its
response carries `"replay_unsafe": true` while time travel is armed),
and where a host-timed `pause` lands is inherently wall-clock, like a
GDB Ctrl-C. Every deterministic stop condition is detected by the core
per instruction regardless of how often the server polls the socket.

## Streaming observability

An authenticated connection can subscribe to low-overhead machine events:

```text
# In copperline-ctl --repl (subscriptions live for this connection):
events.subscribe {"events":["frame","serial","interrupt","media"],"frame_interval":50,"frame_digest":true}
events.list
events.unsubscribe {"events":["serial"]}
events.unsubscribe
```

`events.subscribe` adds the named families to the connection's current set.
`frame_interval` is 1--1,000,000 completed frames (default 1), and
`frame_digest` adds the same FNV-1a framebuffer result as
`capture.digest` to each `event.frame`; leave it off when the pixels are
not relevant because rendering and hashing every selected frame has a cost.
`events.unsubscribe` removes the named families, or all of them when
`events` is omitted. `events.list` returns `supported`, `active`, the frame
options, buffer limits, and the cumulative `dropped_notifications` count.

Subscriptions are per connection, so use `copperline-ctl --repl`, a client
library, or a raw long-lived socket rather than separate one-shot client
invocations. Both headless and windowed servers send JSON-RPC notifications
without an `id`:

- `event.frame`: current timeline `position`, `previous_frame`, and optional
  `digest`.
- `event.serial`: a batch of completed Paula transmissions as
  `{word, long, at_cck}`, plus `dropped_words`. The tap observes output in
  parallel with the configured serial sink; it does not replace or delay it.
- `event.interrupt`: `previous` and `current` INTENA, raw INTREQ,
  CPU-visible INTREQ, and enabled-pending values, plus raw-source `asserted`
  and `cleared` masks.
- `event.media`: floppy or CD `inserted`/`ejected`; floppy events also carry
  the drive number and inserted image name.

Every payload includes a deterministic `position` (`frame`, `cck`,
`seconds`, beam position, PC, and retired-instruction count). Interrupt
changes are coalesced between CCP sampling boundaries: instruction boundaries
for an explicit instruction step, otherwise the running driver's quantum
(normally one frame). Use register watches or a VCD waveform when every
within-frame transition matters.

Streaming is bounded. Paula retains at most 4,096 unconsumed serial words,
evicts the oldest on overflow, and reports the count in `dropped_words`.
The windowed socket path queues at most 256 stream notifications and drops
new notifications rather than blocking the emulator; the next delivered
event and `events.list` expose the cumulative drop count. If that bounded
queue cannot accept a required request reply, the server detaches the slow
client rather than blocking emulation or silently losing the reply. Headless
delivery writes events directly and detaches a client that stops draining the
socket.

The same connection can start and stop the heavier file-backed diagnostics
when an event identifies an interesting window. `trace.start {path?,
max_lines?}` writes a disassembled instruction trace (default cap 1,000,000;
maximum 10,000,000), with `trace.status` and `trace.stop`. `waveform.start
{path?, trigger?, duration?, signals?}` accepts the same trigger, duration,
and signal strings as the command-line VCD exporter, with
`waveform.status` and `waveform.stop`. Their replies report the output path,
progress, and final sample/line count; the actual trace remains in the host
file rather than being pushed through the bounded notification stream.

## Method reference

Session: `hello {token?}`, `auth {token}`, `status`, `shutdown`.

Execution: `continue`, `step {n?}`, `step_over`, `step_out`,
`step_copper`, `step_frame {n?}`,
`run_until {pc | vpos[,hpos] | frame | cck | seconds}`, `pause`
(all resume verbs accept `collect?`).

Reverse (time travel is armed automatically for control sessions;
`-32006` when history is exhausted): `reverse_step {n?}`,
`reverse_frame`, `reverse_continue`, and
`last_writer {addr}` -- find the instruction that last wrote a chip-RAM
word; on `"outcome": "found"` the machine is left parked at the writing
instruction (the reply's `position` says where).

Inspection: `regs.get`, `regs.set {reg, value}`, `mem.read {addr, len,
encoding?}` / `mem.write {addr, data, encoding?}` (hex default, base64
for bulk; 1 MiB cap; side-effect-free RAM/ROM view, device windows are
not touched), `disasm {addr?, count?}`, `custom.dump`,
`custom.read {reg}` (name or offset), `cia.get {cia: "a"|"b"}`,
`beam.get`, `display.get`, `copper.list {addr?, max?}`, `pc_history`.

Battery clock (the $DC0000 RTC; see `rtc_time` in
`docs/guide/configuration.md` for the boot-time seed): `rtc.get` reports
`{present, seeded, frozen, unix, time}`; `rtc.set {unix | time |
advance, frozen?}` moves it live -- `unix` (Unix seconds) or `time`
(`"YYYY-MM-DD HH:MM[:SS]"`) set an absolute value the clock reads from
this instant, `advance` (signed seconds) jumps relative to the current
reading, and `frozen` stops or resumes the tick (alone it
freezes/unfreezes in place). The guest only notices when it re-reads
the chip -- AmigaOS loads system time from it at boot (KS 2.0+) or via
`SetClock LOAD`, so pair `rtc.set` with a warm reset or a guest-side
re-read. `-32007` on a machine with no clock fitted.

Breakpoints (shared with the debugger window's live store, so
GUI-toggled points appear in `break.list`): `break.add {kind: "pc",
addr, cond?, ignore?}` / `{kind: "watch", addr, class?}` /
`{kind: "reg_watch", reg}` / `{kind: "beam", vpos, hpos?}` /
`{kind: "copper", addr}` / `{kind: "catch", vector}` -> `{id}`;
`break.remove {id}`; `break.list`; `break.clear`. Conditions are
`{lhs, op, rhs}` with operands `"d0"`-`"a7"`, `"pc"`, `"sr"`, a number,
or `{"mem": addr}`, and ops `eq|ne|lt|gt|le|ge|and`. A session's own
breakpoints are removed when it disconnects; GUI-set points are left
alone.

Input (applied now by default, or scheduled with `at_seconds`; `tap`
presses and schedules the release after `hold_ms`, default 80):
`input.key {rawkey, action: "press"|"release"|"tap", hold_ms?,
at_seconds?}`, `input.mouse {port?, left?, right?, middle?, dx?, dy?}`
(port defaults to 1), `input.joy {port?, up, down, left, right,
red/fire1, blue/fire2, green, yellow, play, rwd, ffw}` (held state,
replaced wholesale; port defaults to 2), `input.analogue {port?, x, y}`
(analogue stick/paddle position, 0-255 per axis, the count POTxDAT
latches; port defaults to 2). Events drive the named port's electrical
lines whatever device is configured there.

Controller ports: `input.get_ports` -> `{"port1": "mouse", "port2":
"joystick"}`; `input.set_port {port, device:
"mouse"|"joystick"|"cd32"|"analogue"|"none"}` hot-plugs a device, as if
swapping the physical plug (the old device's held lines release; the
change is applied live, mid-run included, and is not journaled for
reverse replay):

```sh
copperline-ctl --info /tmp/ccp.json input.set_port '{"port": 1, "device": "cd32"}'
copperline-ctl --info /tmp/ccp.json input.joy '{"port": 1, "red": true}'
copperline-ctl --info /tmp/ccp.json input.analogue '{"port": 2, "x": 50, "y": 200}'
```

Media: `media.floppy.insert {drive, path, write_protected?}`,
`media.floppy.eject {drive}`, `media.floppy.query`,
`media.cd.insert {path}`, `media.cd.eject`.

Streaming: `events.subscribe {events, frame_interval?, frame_digest?}`,
`events.unsubscribe {events?}`, `events.list`. Event names are `frame`,
`serial`, `interrupt`, and `media`; see
[Streaming observability](#streaming-observability).

Diagnostic captures: `trace.start {path?, max_lines?}`, `trace.status`,
`trace.stop`; `waveform.start {path?, trigger?, duration?, signals?}`,
`waveform.status`, `waveform.stop`.

State and capture: `state.save {path}`, `state.load {path}` (re-arms
the reverse-debug ring on the loaded timeline), `capture.screenshot
{path?}` (raw framebuffer PNG, 716 pixels wide), `capture.digest`
(FNV-1a hash of the rendered frame -- the cheap change-detection
primitive, identical in both server modes), `machine.reset
{kind: "warm"|"cold"}`.

Notifications have no `id`. The subscribed `event.frame`, `event.serial`,
`event.interrupt`, and `event.media` streams work in both server modes.
Additionally, windowed mode sends `event.stopped` when the machine stops
without a pending resume -- a GUI breakpoint, a user pause, or a
guru/double fault -- so an attached client can follow along.

## Errors

Standard JSON-RPC codes (`-32700` parse, `-32600` invalid request,
`-32601` unknown method, `-32602` invalid params, `-32603` internal)
plus: `-32000` auth failed (connection closed), `-32001` not
authenticated, `-32002` a resume is already pending, `-32003` invalid
state (e.g. repositioning while running, stepping a running window),
`-32004` unsupported on this machine (e.g. CD without a drive),
`-32005` host I/O failed, `-32006` reverse history exhausted, `-32007`
not found.

## Windowed-mode notes

The status bar shows a `CCP` tag while a client is attached. A remote
stop pauses the machine without opening the debugger window; local and
remote control interleave -- a user pause completes a pending resume
with reason `user_pause`, and GUI breakpoints report to the client as
`event.stopped`. Reverse operations replay snapshots synchronously and
briefly block the window. Windowed screenshots via `capture.screenshot`
use the same raw framebuffer render as headless mode (not the presented,
aspect-corrected window content), so captures are comparable across
modes.

## Security

The token guards a loopback socket against other local users and
against browser cross-protocol requests; it is not network-grade
authentication. Binding beyond loopback is a trust decision, as with
`--gdb` and `[serial] mode = "tcp"`: a control client can read and
write guest RAM, drive input, and load host files as media.

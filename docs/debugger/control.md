# Control protocol (CCP)

The Copperline Control Protocol (CCP) is a versioned JSON-RPC 2.0 interface
over TCP for programmatic control of the emulator. Scripts, developer tools,
CI runners, and automated agents use it to inspect state, set breakpoints,
step execution, inject input, change media, and capture the display.

## Starting the control server

```sh
# Headless mode (server owns execution; paused at power-on):
./target/release/copperline --config copperline.example.toml --noaudio \
    --control :0 --control-info /tmp/ccp.json

# Windowed mode (attaches control server to interactive session):
./target/release/copperline --config copperline.example.toml --control-gui :7710
```

The listen address is `HOST:PORT`, `:PORT`, or `PORT`; the port-only forms
bind `127.0.0.1`, and port 0 picks a free port. Both modes serve one client
at a time. The windowed server answers a second connection with one JSON
error line and closes it; the headless server accepts the next connection
only after the current client disconnects, keeping the machine paused in
between. `shutdown` ends the process. The headless server runs
unpaced, keeps a PC history, and arms reverse execution at start-up (the
windowed server does both when a client connects), with the budget and snapshot
interval taken from `COPPERLINE_DBG_RR_BUDGET_MB` and
`COPPERLINE_DBG_RR_INTERVAL` (see [Reverse debugging](reverse.md)).

The headless server owns the machine, so `--control` cannot be combined with
`--gdb`/`--gdb-gui`, `--benchmark-until`, the capture flags
(`--screenshot-after`, `--expect-screenshot`, `--save-state-after`,
`--dump-frames`, `--gif-after`), scheduled input, scheduled floppy or CD
inserts, or `--exit-on-return`; use the matching protocol methods instead.
`--record-input PATH` is accepted and records the input clients inject.

`--control-gui` can share the window with `--gdb-gui` (see [GDB](gdb.md)):
a machine stop answers whichever resumes are pending on both clients, a stop
the control client did not request arrives as `event.stopped`, and
`reverse_step`, `reverse_frame`, `reverse_continue`, `last_writer`,
`input.mouse_to`, and `state.load` are refused while the GDB client's
`continue` is outstanding (`pause` first). Either client's pause ends the
other's run with reason `pause`.

On start-up the server prints its endpoint and token to stderr as one line
(`copperline-control: listen=ADDR token=TOKEN proto=1`). The token is a fresh
random 128-bit value unless `--control-token TOKEN` pins one; a pinned token
is visible in the process list, so prefer `--control-info FILE`, which writes
the same details to a JSON file readable only by its owner:

```json
{"listen": "127.0.0.1:52114", "token": "1f0c...", "proto": 1}
```

With `--run PROG`, either mode arms a one-shot `loadseg` stop for the
program before the machine runs, the same break-at-entry `--gdb` has: the
first `continue` (or, windowed, the boot already under way) stops with reason
`loadseg` the moment the guest OS loads the program, before its first
instruction, and `segments.list` then reports its hunks. A windowed session
that reaches the stop before any client has attached parks there and tells
the first client with `event.stopped`. The stop fires once; a client that
wants every load arms its own `loadseg` break.

## Client usage (`copperline-ctl`)

`copperline-ctl` is a command-line client for the protocol. It connects with
`--info FILE` (the `--control-info` file) or `--connect ADDR --token TOKEN`,
authenticates, and then sends one request or runs a REPL:

```sh
# Query status
copperline-ctl --info /tmp/ccp.json status

# Add a PC breakpoint
copperline-ctl --info /tmp/ccp.json break.add '{"kind": "pc", "addr": "0xFC0100"}'

# Resume execution (blocks until a breakpoint or stop event occurs)
copperline-ctl --info /tmp/ccp.json continue

# Interactive REPL session
copperline-ctl --info /tmp/ccp.json --repl

# Describe a save state (no session needed) and write out its thumbnail
copperline-ctl state-info /tmp/at120.clstate --thumbnail /tmp/at120.png
```

Replies print to stdout as one JSON object per line, and notifications
(`event.*`) print as they arrive. A one-shot request exits with a nonzero
status when the reply is a JSON-RPC error. The REPL reads one
`METHOD [JSON-PARAMS]` per line and skips blank lines and `#` comments.

The other subcommands work without a session: `state-info` (see
[State snapshot files](#state-snapshot-files)), `diverge` (below),
`profile`, `profile-report` and `size-report` (see
[Per-frame profiling](profiling.md)), and `exe2adf` (see
[Direct executable launching](../guide/run.md)). `copperline-ctl --help`
prints the usage of every mode.

## A/B divergence finder

`copperline-ctl diverge` launches two headless sessions (two builds, or one
build under two configs) and drives them in lockstep over this protocol,
comparing the frame digest, the CPU registers and a server-side RAM digest
at frame boundaries, then narrowing the first mismatch to the instruction
and, for memory, to the byte. See [A/B divergence finder](diverge.md).

## Debug adapter

`copperline-ctl --dap` serves the [Debug Adapter Protocol](dap.md) over the
same bridge: an IDE debugs a program in the emulator with source-level
breakpoints, variables and reverse stepping, while the control protocol
underneath stays available from the Debug Console (`!status`, `!beam.get`).

(mcp-server)=
## MCP server

`copperline-ctl --mcp` exposes the control protocol over standard I/O as a
[Model Context Protocol](https://modelcontextprotocol.io) server, so AI
coding agents in clients such as Claude Code or Cursor can drive and inspect
the emulator through tool calls. Each protocol method is a tool, and a few
extra tools manage the session and its event queue.

```sh
# Unattached: the agent launches or attaches a session with session tools.
copperline-ctl --mcp

# Attached at startup to a running control server:
copperline-ctl --mcp --info /tmp/ccp.json
copperline-ctl --mcp --connect 127.0.0.1:7710 --token HEX
```

Claude Code registers it with one command:

```sh
claude mcp add copperline -- copperline-ctl --mcp
```

or via `.mcp.json` in a project:

```json
{
  "mcpServers": {
    "copperline": {
      "command": "copperline-ctl",
      "args": ["--mcp"],
      "env": {"COPPERLINE_BIN": "/path/to/copperline"}
    }
  }
}
```

`initialize` returns an `instructions` summary of the workflow, and
`tools/list` provides descriptions, JSON schemas, and parameter conventions
for all tools.

### Tool names

MCP tool names allow only `[a-zA-Z0-9_-]`, so a method's tool name replaces
its dots with underscores (`warp.get` becomes `warp_get`,
`media.floppy.insert` becomes `media_floppy_insert`, and `capture.screenshot`
becomes `capture_screenshot`). Methods without dots (such as `status` or
`run_until`) keep their names. Tool arguments are the method's parameters
(addresses accept hex strings or integers). A protocol error comes back as a
tool result with `isError: true` carrying the error code and message. The
bridge performs the handshake (`hello`, `auth`) itself; those methods are not
tools.

### Session tools

The bridge manages one active session at a time:

- `session_launch {"config", "model", "run", "whdload", "factory", "args",
  "binary", "cwd", "timeout_ms"}`: Spawns a headless emulator
  (`copperline --control :0 --control-info TMP --noaudio`, then the given
  flags), connects, and authenticates. `binary` defaults to `COPPERLINE_BIN`,
  then a `copperline` next to `copperline-ctl`, then the PATH;
  `timeout_ms` (default 30000) bounds the wait for the control endpoint.
  The emulator's output goes to a temporary log file, and it starts paused
  at power-on.
- `session_attach {"info_file"}` or `session_attach {"listen", "token"}`:
  Attaches to an already running `--control` or `--control-gui` server.
- `session_status`: Reports connection state, endpoint address, process ID and
  log path of any launched emulator, and event queue statistics.
- `session_close`: Disconnects from the server. An emulator the bridge
  launched is sent `shutdown` and killed if it has not exited within 3
  seconds; an attached one keeps running. The `shutdown` tool also ends the
  session, and end of file on standard input closes it.

### Blocking and `wait_ms`

Execution methods (`continue`, `run_until`, `step`, `step_over`, `step_out`,
`step_copper`, `step_frame`) accept an optional `wait_ms` parameter. If the
emulated machine does not halt within this time limit (in host milliseconds),
the bridge automatically pauses execution and returns the stop event with
`bridge.paused_after_ms` set. Without `wait_ms`, calls block until a breakpoint
or stop condition is reached.

### Events

The bridge queues the server's notifications (see
[Streaming observability](#streaming-observability)) as they arrive, up to
1,024; when the queue is full the oldest is dropped. `events_next
{"timeout_ms"}` returns one event, waiting up to `timeout_ms` (default 1000,
at most 600000), and `events_drain` returns everything queued. Both report
the queue depth and the number of events dropped.

### Screenshots

`capture_screenshot` returns the PNG image as an MCP image content block
(`image/png`) alongside the text result. Without `path`, the bridge uses a
temporary file and deletes it after reading. With `path`, the image is kept
there (a relative path resolves against `copperline-ctl`'s working
directory).

### Protocol subset

MCP 2025-06-18 over stdio (newline-delimited JSON-RPC 2.0): supports
`initialize`, `notifications/initialized`, `ping`, `tools/list`, and
`tools/call`. `initialize` also accepts the 2025-03-26 and 2024-11-05
revisions and echoes the client's choice. Malformed requests get the standard
JSON-RPC error codes (`-32700`, `-32600`, `-32601`, `-32602`). Standard output
carries protocol messages only; diagnostics go to stderr. The server handles
one request at a time and exits at end of file on stdin.

## Protocol overview

- **Wire format:** Newline-delimited JSON-RPC 2.0 over TCP, one UTF-8 JSON
  object per line (at most 4 MiB). Every request must carry an `id`;
  server notifications carry none.
- **Authentication:** The first `hello {"token": "..."}` or
  `auth {"token": "..."}` with the right token authenticates the connection.
  A wrong token gets one error reply and the connection is closed. `hello`
  without a token is allowed before authentication and reports only
  `proto` (the protocol version, currently 1), `emulator` (the Copperline
  version), and `authed`; any other method before authentication is refused.
- **Numbers and addresses:** Numeric parameters accept integer values or hex
  strings with a `0x` or `$` prefix (e.g., `"0xDFF096"`, `"$DFF096"`, or
  `14676118`).
- **Execution commands:** Commands such as `continue`, `step`, and `run_until`
  block until execution stops, returning a structured stop event.

Errors use the JSON-RPC `error` object. Besides the standard codes (`-32700`
parse error, `-32600` invalid request, `-32601` unknown method, `-32602`
invalid params, `-32603` internal error), the server returns:

| Code | Meaning |
|---|---|
| `-32000` | Authentication failed |
| `-32001` | Not authenticated yet |
| `-32002` | A resume is already pending |
| `-32003` | Invalid state (for example, pause before repositioning the machine) |
| `-32004` | Unsupported in this mode |
| `-32005` | Host I/O error |
| `-32006` | Reverse execution ran out of recorded history |
| `-32007` | Not found |

### Example stop event payload

```json
{
  "reason": "breakpoint",
  "detail": "Breakpoint at $FC0100",
  "pc": 16515328,
  "frame": 122,
  "vpos": 44,
  "hpos": 101,
  "cck": 8712345,
  "seconds": 2.456,
  "retired_instructions": 1745210
}
```

`reason` is one of `breakpoint`, `watchpoint`, `reg_watch`, `mmio`,
`beam_trap`, `copper_break`, `catch`, `task_catch`, `loadseg` (a trap
fired), `step` (a step verb finished), `target` (a `run_until` target was
reached), `pause`, `budget` (a bounded run gave up), or `double_fault` (the
CPU halted). `reverse_step`, `reverse_frame` and `reverse_continue` return
a stop event with reason `reverse`. `detail` is a human-readable
description.

A stop on an `mmio` break (reason `mmio`) adds `access`, the CPU access that
tripped it: `addr`, `size` in bytes, `value`, `access` (`read` or `write`), the
`pc` of the instruction that made it, and its own `position` (`frame`, `cck`,
`seconds`, `vpos`, `hpos`). The stop coordinate itself is the instruction
boundary after the access. Exception processing (the stack frame, the vector
fetch) runs no instruction of its own: its accesses carry the last instruction
retired before the exception, and a watch they trip stops before the handler's
first instruction.

```json
{
  "reason": "mmio",
  "detail": "MMIO write $B8001D.B = $48 (pc $E593B0, f521 v259 h206, cck 37076570)",
  "access": {
    "addr": 12058653, "size": 1, "value": 72, "access": "write", "pc": 15045552,
    "position": {"frame": 521, "cck": 37076570, "seconds": 10.4532, "vpos": 259, "hpos": 206}
  },
  "pc": 15045556, "frame": 521, "vpos": 259, "hpos": 208, "cck": 37076572,
  "seconds": 10.4532, "retired_instructions": 5710489
}
```

(streaming-observability)=
## Streaming observability

An authenticated client can subscribe to asynchronous event notifications:

```text
events.subscribe {"events":["frame","serial","interrupt","media","debug","bus"],"frame_interval":50,"frame_digest":true}
events.subscribe {"events":["mmio","cd"],"mmio":[{"addr":"0xB80000","len":64,"access":"write"}]}
events.list
events.unsubscribe {"events":["serial"]}
```

Subscriptions belong to the connection and add up: each `events.subscribe`
adds its families to the active set, and `events.unsubscribe` without
`events` drops them all. `events.list` reports the `active` families, the
settings, the `mmio` ranges, and the queue `limits`. The server samples
events at its deterministic command and frame boundaries. Every notification
carries `dropped_notifications`, the count the connection's bounded outbound
queue has lost.

### Event types

- **`event.frame`:** Emitted per video frame (or per `frame_interval`). Includes timeline
  position and optional FNV-1a framebuffer hash digest, plus `guest_idle_cck`: the
  colour clocks the guest declared idle during the last frame through the uaelib
  trap's idle markers (null until it uses them).
- **`event.serial`:** Paula serial output, batched as `words` (each with
  `word`, `long` and `at_cck`) plus `dropped_words`.
- **`event.interrupt`:** A change of INTREQ or INTENA: `previous` and
  `current` state plus the `asserted` and `cleared` request bits.
- **`event.media`:** A floppy disk, CD image, or PCMCIA card was inserted or
  ejected (`kind`, `action`, and `drive` or `name` where they apply).
- **`event.debug`:** Guest debug output through the
  [uaelib trap](../guide/run.md#uaelib-trap): one notification per item, with
  `kind` `log` (`text`, a `KPrintF` line, also echoed on the host console) or
  `resource` (`action` and the registered `resource`, as `debug.resources`
  reports it). `dropped_events` counts items the bounded queue lost before
  this batch.
- **`event.bus`:** A named hardware edge from the exact chip-bus timeline,
  including blitter start/final-D/finish/IRQ, Copper wake/denial/SKIP, CPU
  interrupt and STOP edges, INTREQ, and CIA IRQ pins. Each notification carries
  the raw `events` mask, decoded `event_names`, beam/timeline `position`, `ipl`,
  and queue drop count. Subscribing does not allocate a full frame trace.
- **`event.mmio`:** One notification per CPU data access (instruction fetches
  excluded) to the subscription's `mmio` ranges, which `events.subscribe` requires
  with the family: an array of `{"addr", "len", "access"}` (`len` in bytes, default
  2; `access` `read`, `write`, or `access`, the default). A later subscribe with
  new ranges replaces this connection's. Each carries `addr`, `size`, `value`,
  `access` (`read`/`write`), the instruction's `pc`, the access's `position`
  (`frame`, `cck`, `seconds`, `vpos`, `hpos`), and the queue drop count. The
  accesses are captured at the CPU's bus access, so device registers (Akiko at
  `$B80000`, the CIAs, Gayle, Zorro boards, custom registers) report exactly
  what the CPU read or wrote, which a value-comparing memory watch cannot see.
  The queue holds 4,096 accesses, so keep ranges narrow around busy polling
  loops. `events.list` reports the connection's ranges as `mmio`.
- **`event.cd`:** The CD drive's timestamped command trace (CD32 Akiko), one
  notification per step of a command's life: `phase` `executed`, `first_sector`
  (a read, play, or TOC dump delivered its first unit), or `completed`, and
  `command`, the record as `cd.trace` returns it. Silent on machines without a
  traced drive.
- **`event.warp`:** Sent without a subscription, in both modes, whenever warp
  or its holder set changes for a reason other than the client's own
  `warp.set`: `{"on", "paced", "source", "position"}` with `source` one of
  `manual`, `guest`, `gdb`, `launch`, `boot`, `power_off`. A windowed session
  adds `holders`, every programmatic hold still in force (`control`, `gdb`,
  `guest`), and also sends the event when a holder joins or leaves without
  pacing changing, so the list never goes stale. The headless server has no
  holds (it is unpaced end to end), omits `holders`, and reports the guest's
  `warpmode()` request with `paced` always false.

## Command reference summary

### Session management
- `hello {"token": "..."}`: Handshake and protocol version query.
- `auth {"token": "..."}`: Authenticate active connection.
- `status`: Returns emulation state, frame counters, host execution timing, and pacing (`paced`, `warp`).
- `shutdown`: Terminates the emulator process.

### Execution control

The resume verbs below block until the machine stops and reply with the stop
event. Each accepts `collect`, an array of `{"method", "params"}` read-only
inspection requests (`status`, `regs.get`, `mem.read`, `mem.digest`,
`disasm`, `custom.read`, `beam.get`, `capture.digest`, and the other
queries) evaluated at the stop and returned in the stop event's `collect`
array, each as `{"ok": RESULT}` or `{"err": {"code", "message"}}`, so one
round trip both runs and inspects. A second resume while one is pending is
refused.

- `continue`: Resume execution.
- `step {"n": 1}`: Single-step `n` CPU instructions (at most 1,000,000). A
  CPU parked in `STOP` retires nothing until an interrupt arrives, so a step
  there carries it to the one that wakes it and retires the handler's first
  instruction; when no interrupt can reach it (SR mask 7, or nothing enabled
  in `INTENA`) the step gives up after two video frames and the stop event
  reports the unchanged `retired_instructions`.
- `step_over`: Step over subroutine call (bounded by 5,000,000 instructions).
- `step_out`: Step out of current subroutine (bounded by 5,000,000 instructions).
- `step_copper`: Step single Copper instruction.
- `step_frame {"n": 1}`: Step `n` video frames (at most 1,000,000).
- `run_until {"pc" | "pc_outside" | "vpos" | "frame" | "cck" | "seconds" | "stable_frames"}`: Run until exactly one condition holds. `pc_outside` is `[LOW,HIGH]`, or `true` for the default Kickstart window `$F80000-$FFFFFF`. `vpos` takes an optional `hpos`. `frame`, `cck` and `seconds` are absolute emulated positions. `stable_frames` (at least 2) stops once that many consecutive frames render identically, optionally only inside the region `x`/`y`/`w`/`h`, giving up with reason `budget` after `max_frames`. A breakpoint on the way stops the run early.
- `pause`: Pause active execution.
- `machine.reset {"kind": "warm"|"cold"}`: Reset the emulated machine (default: warm).

### Speed
- `warp.get`: Report whether warp (unpaced emulation) is active, whether the machine is paced, and the source holding warp (`none`, `manual`, `control`, `gdb`, `guest`, `launch`, `boot`, `capture`, or `headless`); in a windowed session `holders` lists every programmatic hold in force (`control`, `gdb`, `guest`), `source` being the first (the headless server has no holds and omits the field).
- `warp.set {"on": true|false}`: Engage or release the client's own warp hold (unthrottled execution with audio muted). Holds are independent: releasing yours re-paces the machine only when no other holder (a GDB client, the guest) remains, and the reply's `note` says who still holds it. Disabling warp also cancels active `--run` or `--warp-boot` phases. The headless server is always unpaced and accepts `warp.set` as a no-op with a `note`.

### Reverse execution

These verbs need reverse execution armed (`tt_armed` in `status`), which both
servers do by default. A reverse step that runs past the retained history
fails with code `-32006`.

- `reverse_step {"n": 1}`: Step backward by instruction.
- `reverse_frame`: Step backward by video frame.
- `reverse_continue`: Run backward to the most recent earlier stop of any armed kind (breakpoint, watch, register watch, beam trap, Copper breakpoint, exception or task catch); PC breakpoint conditions are not evaluated on the way back. With no such stop in the recorded history it fails with `-32006`.
- `reverse_anchor`: Snapshot the machine into the reverse-debug ring at the current position, so the reverse verbs replay from here rather than from an older frame boundary. Take one at a stop you will step back from if the guest has used a host directory mount or a disk image since the last snapshot: a restore does not roll back that host-side state, so a replay from before it diverges (the DAP adapter takes one at every run stop).
- `last_writer {"addr": "..."}`: Find the instruction that last wrote the memory at `addr` by replaying the recorded history. The reply's `outcome` is `found`, `never_written`, or `beyond_history`; `record` holds the write (`addr`, `old`, `new`, `pc`, `pos`, `cck`, `frame`) and `position` where the machine was left (at the write when found).

### State inspection and modification
- `regs.get` / `regs.set {"reg": "...", "value": ...}`: Read or modify 68k registers (`d0`-`d7`, `a0`-`a7`, `sp`, `fp` for A6, `sr`, `pc`). `regs.get` includes exact raw FP0-FP7 plus FPCR/FPSR/FPIAR when an FPU is fitted.
- `mem.read {"addr": ..., "len": ..., "encoding": "hex"|"base64"}` / `mem.write {"addr": ..., "data": "...", "encoding": "hex"|"base64"}`: Read or modify memory through the CPU's address map (`len` defaults to 2; either direction moves at most 1 MiB). Memory writes are not part of the reverse-execution journal, so when reverse execution is armed the `mem.write` reply carries `replay_unsafe: true`: a replay across the write can diverge.
- `mem.digest {"region": "chip"|"all"}` or `mem.digest {"addr": ..., "len": ...}`:
  FNV-1a digest of RAM computed inside the emulator, for change detection
  and for comparing two sessions without moving the bytes. `chip` (the
  default) digests the fitted chip RAM bank, `all` every writable RAM bank
  (chip, slow, motherboard, accelerator, Zorro boards) one by one; `addr`
  and `len` digest one span through the CPU map instead (a span that lies
  inside one bank is hashed in place, anything else is peeked byte by
  byte, at most 256 MB). The reply carries `digest` over the whole and
  `regions`, each bank's `base`, `len` and `digest`. Allowed in a resume
  verb's `collect` list.
- `disasm {"addr": ..., "count": ...}`: Disassemble `count` instructions
  (default 16, at most 256) at `addr` (default: PC).
  Every line includes `cycles_min` and `cycles_max`, evaluated through the
  selected 68000-family core's generation-specific timing path. These are
  theoretical CPU cycles; precise profiles additionally measure bus contention.
- `symbols.resolve {"addr": ...}`: Resolve one address against the running
  guest's library/device jump targets and ROM resident modules. The result
  reports `found` and, when found, the symbol's start, module, name, offset,
  kind (`lvo` or `resident`), and vector/LVO metadata.
- `symbols.rom`: Snapshot the actual ROM ranges, resident tags, and named live
  LVO targets. It walks Exec and reads each `JMP abs.l`, so patched entries and
  every Kickstart/AROS revision use their current addresses rather than a
  per-ROM database.
- `custom.read {"reg": ...}` / `custom.dump`: Query custom chipset registers.
  `reg` is a name (`"DMACON"`) or an offset below `0x200`; a write-only
  register reads back the last value written. `custom.dump` returns `regs`,
  the compact name/value map, and `registers`, which adds each register's
  offset, access direction, chipset availability, summary, and the shared
  [Markdown register page](../reference/custom-registers/index.md).
- `custom.writer {"reg": ...}`: Query the last write to a custom register: its `value`, the writer (`by` and its `addr`), and the `frame`, `vpos` and `hpos` it landed at. The answer comes from the last-writer table the chipset validator keeps, so arm it with `chipset.validate` first.
- `palette.dump {"resource": ...}`: Query the whole palette as `hi` and `lo`, 256 entries each (eight banks of 32 registers; `hi` and `lo` hold the high and low colour nibbles, and OCS/ECS machines only write the first 32); with `resource`, read a guest-registered palette resource instead (`words` as 12-bit values plus `rgb24`).
- `cia.get {"cia": "a"|"b"}`: Query CIA-A or CIA-B timer, port, and interrupt states.
- `cd.trace {"since": SEQ, "max": N}`: The CD drive's recent commands with emulated
  timestamps (CD32 Akiko; `available` is false elsewhere), oldest first: those
  numbered `since` or later, the newest `max` (default 64, at most 256), plus
  `next_seq`, the number the next command gets. Each record has its `seq`, decoded
  `kind` (`noop`, `stop`, `pause`, `unpause`, `read`, `play`, `toc`, `led`, `subq`,
  `info`, `other`, `invalid`), `opcode`, the raw packet `bytes` in hex (checksum
  included), `start_lsn` and exclusive `end_lsn` of a read or play, requested
  `speed`, and the stamps `issued` (the host handed over the first byte: Akiko's TX
  DMA fetched it or the CPU wrote the PIO port), `accepted` (the drive parsed the
  packet), `executed`, `responded` (the reply reached the host), `first_sector` and
  `last_sector` (units a read, play, or TOC dump delivered), and `completed`, each
  `{"cck", "seconds"}` of emulated time or null. `sectors` counts the units
  delivered (data sectors, CD-DA sectors, or TOC packets), `status` is the reply's
  status byte, `outcome` how it ended (`ok`, `no_disc`, `checksum_error`,
  `bad_command`, `refused`, `end`, `stopped`, `superseded`, `ejected`, `reset`,
  `error`, `abandoned` when a state load replaced its timeline, or null while
  running), and `summary` is the record as one line, with each step in
  milliseconds after issue. Differences between the stamps are the drive
  latencies a driver or ROM comparison needs, identical from run to run and
  unaffected by warp. The trace is host-side: it is not saved in state files
  and survives a state load, which ends the commands in flight as
  `abandoned`. A command the restored drive was already running is not
  traced; tracing resumes with its next packet.
- `beam.get`: Query raster beam coordinates (VPOS, HPOS, colour clock).
- `frame.slots {"row": V}`: Return the bounded full records for one scanline
  (row 0 through 2047, covering the ECS 11-bit programmable vertical range)
  of the latest full Frame Analyzer/profile trace. Each entry has HPOS,
  register, address, data/size, kind/subtype, flags, IPL, raw event bits, and
  decoded event names. `data` is a fixed-width hexadecimal string so grouped
  64-bit AGA fetches remain lossless in JSON. Owner-only traces and out-of-range
  rows return errors. `instantaneous_records` contains any ordered zero-time
  floppy-turbo transfers at positions on the requested row; replay these after
  the ordinary record at the matching HPOS.
- `blit.render {"index": N, "channel": "A"|"B"|"C"|"D"|"result", "path": "..."}`:
  Reconstruct a recorded blit channel from the exact DMA word stream and write
  it as PNG. The reply includes the selected plane count and whether it came
  from a registered bitmap containing BLTDPT or the frame's BPLCON0, plus the
  safely decoded `render_planes`, interleaving status, and simplified Boolean
  minterm formula. Non-interleaved resources and the BPLCON0 fallback render
  one plane rather than guessing that consecutive blit rows are planes of one
  image. Disabled A/B/C channels use their effective constant input (including
  BLTBDAT's write-time-shifted hold latch). `result` uses captured D writes and
  returns an error when no destination stream exists rather than approximating
  the hardware shift/mask/fill pipeline. The path is optional.
- `display.get`: Query the display registers: `dmacon`, and `display`, one text line giving DIWSTRT/DIWSTOP, DDFSTRT/DDFSTOP, BPLCON0-2, the modulos and the bitplane pointers (both the registers and the pointers the display DMA is using).
- `rtc.get` / `rtc.set {"unix": ..., "time": "...", "advance": ..., "frozen": ...}`: Inspect or move the battery-backed clock. `rtc.set` takes an absolute time (`unix` seconds or `time` as `"YYYY-MM-DD HH:MM[:SS]"`) or a relative `advance` in seconds, and `frozen` stops or restarts the clock; give at least one. It fails when no clock is fitted, and like `mem.write` reports `replay_unsafe` when reverse execution is armed.
- `clipboard.get` / `clipboard.set {"text": "..."}`: The host <-> guest
  clipboard bridge (`[clipboard] share`, `--clipboard`; see
  [Configuration](../guide/configuration.md#clipboard)). `get` reports
  whether the unit is fitted and sharing, whether the guest bridge is up,
  the host and guest text generations, and the newest text the guest
  copied. `set` stages text for the guest exactly as a windowed session's
  host clipboard poll would, so a headless run (which never reads the host
  clipboard itself) can hand the guest text to paste; `replay_unsafe` is
  set when reverse execution is armed, as for `mem.write`.
- `cartridge.get`: Query freezer cartridge state (`model`, memory `base`/`size`, monitor `version`, `entered` status, `nmi_pending`, and freeze count).
- `cartridge.freeze`: Trigger the freezer cartridge NMI (level 7), transferring execution to the monitor.
- `copper.list {"addr": ..., "resource": ..., "max": ..., "trace": true}`:
  Disassemble up to `max` Copper instructions (default 32, at most 256)
  starting at `addr` or a registered `resource` (default: current Copper
  PC). With `trace`, each instruction that ran in the last full Frame
  Analyzer trace carries its exact frame, VPOS and HPOS execution slot.
- `pc_history`: Return recently executed instruction addresses (`pcs`, newest last).
- `segments.list`: The hunk segments (`current`: `{start, size}` per hunk, first hunk first) of the program the scheduled process is running, and every program an armed `loadseg` catch has seen loaded (`modules`). At a `loadseg` stop, `current` is the just-loaded program: the addresses to relocate its symbols and debug information by.

### Windowed UI

- `ui.show {"window": "debugger"|"console"|"analyzer"}`: Open or focus one of
  the inspectors in the main window's Debug layout. Only a windowed
  `--control-gui` session supports it; a headless server returns an
  unsupported error.

### Diagnostics and profiling
- `chipset.validate {"enabled": ..., "clear": ...}` / `chipset.report`: Arm or query custom register access validator.
- `smc.detect {"enabled": ..., "clear": ...}` / `smc.report`: Arm or query self-modifying code detector.
- `fault.inject {"addr": ..., "len": ..., "on": "read"|"write"|"both", "count": ...}`: Make accesses to `len` bytes at `addr` (default 2) raise a bus error in the guest: reads, writes, or both (the default), for the next `count` matching accesses (default: every access). Returns the fault's id.
- `fault.list` / `fault.clear`: List or clear active memory bus faults.
- `memory.heatmap {"enabled": ..., "base": ..., "span": ...}`: Enable (the default) or disable the memory heat map over `base` to `base + span` (default: the whole 24-bit, 16 MB space). The map is a 256 by 256 grid of cells, each remembering which engine (CPU read or write, blitter, Copper, disk, bitplane, sprite, or audio DMA) last touched it.
- `memory.heatmap.report {"path": "..."}`: Report the heat map's window and `census`, the number of recently touched cells per engine; with `path`, also write the grid as a PNG.
- `debug.resources`: List bitmaps, palettes, and copper lists registered by guest software via the [uaelib trap](../guide/run.md#uaelib-trap).
- `debug.resource.export {"address": ..., "path": "...png"}`: Export a registered bitmap or palette through the same decoder as the Resources tab.
- `debug.idle`: Query guest idle time statistics reported via uaelib idle markers.
- `trace.start {"path": "...", "max_lines": ...}` / `trace.stop` / `trace.status`: Control instruction execution trace logging. The trace stops by itself after `max_lines` (default 1,000,000, at most 10,000,000); without `path` it goes to a timestamped file in the `[paths]` traces directory.
- `waveform.start {"path": "...", "trigger": "...", "duration": "...", "signals": "..."}` / `waveform.stop` / `waveform.status`: Control a VCD chip-signal capture, with the `--waveform` flags' syntax: `trigger` is `now`, `pc=ADDR`, `beam=VPOS[:HPOS]`, `reg=OFF` or `time=SECS`, `duration` is `Ncck`, `Nf`/`Nframes`, `Nms` or `Ns`, and `signals` a comma list of `beam`, `bus`, `cpu`, `copper`, `blitter`, `regs`, `irq`, `audio`, or `all` (see [Waveform export](waveform.md)).
- `profile.start {"path": "...", "frames": ..., "slots": ..., "memory": ..., "screenshots": "none"|"every"|"last", "pc_samples": ..., "samples": ..., "registers": ..., "unwind": {"base": ADDR, "table": BASE64}, "relocation_bases": [ADDR, ...], "code_ranges": [{"base": ADDR, "size": N}, ...], "coverage": ..., "trigger": {"frame": F}|{"busy_cck_over": N}}` / `profile.stop` / `profile.status`: Export per-frame profiling data (DMA ownership, full slot/event records, frame-start custom registers and palette, blit records, CPU chip-bus wait attribution, guest idle time, retired instructions, and stack bounds) into the directory `path` (default: a timestamped directory in the `[paths]` traces directory), stopping by itself after `frames` (default 500, at most 100,000). `memory` snapshots chip and slow RAM once; because that baseline must align with the first recorded frame, it cannot be combined with a deferred `trigger`. `slots` writes a raw 24-byte-record sidecar per frame. `samples` adds a WinUAE/Bartman-compatible per-instruction binary sidecar; with it, `registers` adds D0-D7/A0-A7/SR and the optional compact `unwind` table supplies live call stacks; `relocation_bases` preserves every program hunk's runtime base for offline source mapping, and `code_ranges` identifies executable hunks outside the compact hunk-0 table. `coverage` counts every retired instruction over `code_ranges` (every address, bounded, without them) and writes the histogram as `coverage.bin` at stop for `copperline-ctl profile-report --format lcov`; it takes `relocation_bases`/`code_ranges` without `samples` and is refused while a `--coverage` run holds the counters. Data streams to `profile.jsonl` with a `profile.json` summary upon stop; see [](profiling). Arms Frame Analyzer tracing immediately and begins recording only when an optional trigger matches.

### Breakpoints and traps
- `break.add {"kind": ...}`: Add a breakpoint, watch, or trap and return its `id`. The kinds and their fields:
  - `pc`: `addr`, an optional `cond` (`{"lhs", "op", "rhs"}`, where each side is a register name, a number, or `{"mem": ADDR}` for the word there, and `op` is `eq`, `ne`, `lt`, `gt`, `le`, `ge`, or `and`), and an `ignore` count of hits to skip.
  - `watch`: a memory watch at `addr` with `access` `write` (the default), `read`, or `access`, optionally only for accesses by one `class` (`cpu`, `blitter`, `disk`, `copper`, `bpl1`..`bpl8`, `spr0`..`spr7`, `aud0`..`aud3`) and, for the CPU, only by the instruction at `pc`.
  - `reg_watch`: a write to the custom register `reg` (name or offset).
  - `mmio`: any CPU data access to the `len` bytes at `addr` (default 2), device registers included, with `access` `read`, `write`, or `access` (the default). Its stop reports the access (see the stop event above); for example `{"kind": "mmio", "addr": "0xB80000", "len": 64}` for Akiko.
  - `beam`: the beam reaching `vpos` (and optionally `hpos`).
  - `copper`: the Copper fetching the instruction at `addr`.
  - `catch`: the CPU taking exception `vector`.
  - `loadseg`: AmigaDOS loading a program, optionally only one whose `name` matches (case-insensitive).
- `break.remove {"id": ...}`: Remove breakpoint by ID.
- `break.list`: List all active breakpoints with their hit counts; points set from the debugger window appear without ids.
- `break.clear`: Remove every breakpoint, watchpoint, beam trap and Copper breakpoint, including those set in the debugger window, not only this connection's. A disconnecting client's own breakpoints are removed automatically; the window's are left alone then.

### Input injection

Injected input goes through the same path as live and scripted input, so it
is journaled for reverse execution and `--record-input`. `at_seconds` takes
an absolute emulated time; an absent or past time applies the input now.

- `input.key {"rawkey": ..., "action": "press"|"release"|"tap", "hold_ms": ..., "at_seconds": ...}`: Inject keyboard events by Amiga raw key code (0-255). `tap` (the default) presses and releases after `hold_ms` (default 80).
- `input.type {"text": ..., "at_seconds": ...}`: Type `text` on the US Amiga keyboard as raw key press/release pairs (Shift where needed, newline as Return, tab as Tab, escape as Esc), one key every 100 ms of emulated time from `at_seconds` (default now). Text with characters the US keymap cannot type is rejected. The same conversion backs `--type-after` and the window's Paste as Keystrokes.
- `input.mouse {"dx": ..., "dy": ..., "left": ..., "right": ..., "middle": ..., "port": 1|2, "at_seconds": ...}`: Inject relative mouse motion (in mouse counts) and button changes (`true` pressed, `false` released, absent unchanged) on `port` (default 1).
- `input.mouse_to {"x": ..., "y": ..., "port": 1|2, "tolerance": ..., "max_frames": ...}`: Steer the pointer to screen pixel coordinates (those of `capture.screenshot`) by feeding mouse motion until sprite 0 lands within `tolerance` pixels (default 2, at most 64). The machine runs for up to `max_frames` frames (default 60, at most 600) to get there, so the call is refused while a resume is pending, and it fails if the pointer does not converge.
- `input.joy {"up": ..., "down": ..., "left": ..., "right": ..., "red": ..., "blue": ..., "green": ..., "yellow": ..., "play": ..., "rwd": ..., "ffw": ..., "port": 1|2|3|4, "at_seconds": ...}`: Set the joystick / CD32 pad state on `port` (default 2): each control listed as `true` is held, the rest are released, and the state lasts until the next `input.joy`. Ports 3 and 4 are the parallel-port four-player adapter's sockets (driving one fits the adapter); on a light pen `red` is the tip switch / trigger.
- `input.analogue {"x": ..., "y": ..., "port": 1|2, "at_seconds": ...}`: Set analogue paddle/pot position (0-255 per axis) on `port` (default 2).
- `input.pen {"x": ..., "y": ..., "at_seconds": ...}`: Hold the light pen over presented pixel (x, y) (the `input.mouse_to` coordinates); omit or negate to lift it off the glass.
- `input.set_port {"port": 1|2|3|4, "device": "mouse"|"gamepad-mouse"|"joystick"|"cd32"|"analogue"|"lightpen"|"none"}`: Change port device (`gamepad-mouse` fits port 1 only; ports 3 and 4 take `joystick` or `none`).
- `input.get_ports`: Query active controller port device assignments (`port1`-`port4`, `parallel_adapter`, and the light pen's `port`, whether it is `wired` to Agnus LP, and its position).

### Media management
- `media.floppy.insert {"drive": 0, "path": "...", "write_protected": true}`: Insert a floppy disk image into drive 0-3 (`write_protected` defaults to false). The guest sees an ordinary disk change.
- `media.floppy.eject {"drive": 0}`: Eject floppy disk.
- `media.floppy.query`: Query the four floppy drive slots: each entry gives `drive`, `inserted`, and the inserted image's `name`.
- `media.cd.insert {"path": "..."}`: Swap the CD image in the machine's CD drive (CDTV, CD32, or SCSI CD-ROM).
- `media.cd.eject`: Eject CD image.
- `pcmcia.insert {"card": "cf"|"sram", "path": "...", "size": "2M", "read_only": false}`: Push a card into the A600/A1200 PCMCIA slot: a CompactFlash card over the hard-disk image at `path` (the default kind), or an SRAM card of `size` bytes optionally mirrored to `path`. Any card already in the slot is ejected first. Gayle latches the card-detect change, so the guest sees a real insertion (INT6 once card.resource has enabled it). Fails on a machine without the slot.
- `pcmcia.eject`: Pull the card out of the slot (latches the card-detect change the same way).
- `pcmcia.query`: Report the slot: `slot` (the machine has one), `enabled` (not disabled by the guest or shadowed by Zorro II fast RAM), `shadowed_by_fast_ram`, `inserted`, `card` (`cf`/`sram`), `description`, `path`, Gayle's sampled `pins`, and its pending `change_latches`. The `media` event stream also reports `{"kind": "pcmcia", "action": "inserted"|"ejected", "name": ...}`.
- `copperhf.attach {"unit": 0, "path": "...", "volume_name": "...", "boot_pri": 0}`: Hot-attach a `copperhf.device` unit's media (opens `path` exactly like a boot-time `[copperhf]` unit, `volume_name`/`boot_pri` optional). Bumps the unit's change counter and sets its `CHF_CHANGED_MASK` bit. Fails if no `[copperhf]` controller is configured.
- `copperhf.eject {"unit": 0}`: Hot-eject/detach a `copperhf.device` unit's media. The unit stays present (`CHF_UNIT_PRESENT`); only its media bit (`CHF_UNIT_MEDIA`) clears. Bumps the change counter and sets `CHF_CHANGED_MASK`, the same as the guest's own `TD_EJECT`.

(state-snapshot-files)=
### State snapshot files
- `state.save {"path": "..."}`: Snapshot machine state to file. The file
  carries a metadata chunk (thumbnail of the current frame from the same
  renderer as `capture.screenshot`, emulated and wall-clock save times,
  machine summary, media names) ahead of the machine.
- `state.load {"path": "..."}`: Restore machine state from file. The
  machine must be paused. Pending scheduled input is dropped, and reverse
  execution restarts from the loaded position.
- `state.info {"path": "...", "thumbnail": "..."}`: Describe a state file
  without loading it -- its container `version`, the `machine` it was
  taken on (summary, model, CPU, chipset, video standard, RAM sizes, ROM
  fingerprint), and its `meta` (`emulated_seconds`, `emulated_frames`,
  `saved_at_unix` and readable `saved_at`, `machine` summary, `media`
  with `floppies` per connected drive, `hard_disks`, and `cd`, and the
  `thumbnail` size in pixels and bytes), or `meta: null` for a state
  written before the chunk existed. With `thumbnail`, the PNG is written
  to that host path and `thumbnail_path` names it. Reads only the file's
  header; the running session is untouched, so it is allowed while the
  machine runs.

The same card is printed without a session by
`copperline-ctl state-info STATE.clstate [--thumbnail FILE.png]`, as
pretty-printed JSON with the same fields.

### Framebuffer capture
- `capture.screenshot {"path": "...", "overlays": ["blits", "overdraw", "sources"]}`:
  Write a PNG from the side-effect-free display renderer and return its
  `path`, `width` and `height`. Optional overlays outline recorded blitter
  destinations (`blits`), heat pixels by repeated D-channel and other
  chip-memory writes from a full Phase 2 trace, falling back to the captured
  D stream without one (`overdraw`), and colour the final Denise/Lisa output
  by playfield 1, playfield 2, sprite number, background, or outside-DIW
  provenance (`sources`). They work in headless sessions, and the MCP bridge
  returns the resulting PNG as its image block.
- `capture.digest`: Return FNV-1a hash digest of current frame.
- `capture.region_digest {"x": ..., "y": ..., "w": ..., "h": ...}`: Return the FNV-1a hash of the `w` by `h` rectangle at (`x`, `y`) (default 0, 0), in `capture.screenshot` coordinates; a rectangle outside the frame is an error.

### Streaming events
- `events.subscribe {"events": [...], "frame_interval": ..., "frame_digest": ..., "mmio": [...]}`: Subscribe to event families: `frame`, `serial`, `interrupt`, `media`, `debug`, `bus`, `mmio`, and `cd` (see [Streaming observability](#streaming-observability)). `frame_interval` is 1 (the default) to 1,000,000.
- `events.unsubscribe {"events": [...]}`: Unsubscribe from the listed families, or from all of them without `events`.
- `events.list`: List active event subscriptions.

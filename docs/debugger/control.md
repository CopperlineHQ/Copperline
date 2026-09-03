# Control protocol (CCP)

The Copperline Control Protocol (CCP) is a versioned JSON-RPC 2.0 interface
over TCP for programmatic control of the emulator. It allows scripts, developer
tools, CI runners, and automated agents to inspect state, set breakpoints, step
execution, inject input events, change media, and capture framebuffers.

## Starting the control server

```sh
# Headless mode (server owns execution; paused at reset):
./target/release/copperline --config kick13.example.toml --noaudio \
    --control :0 --control-info /tmp/ccp.json

# Windowed mode (attaches control server to interactive session):
./target/release/copperline --config kick13.example.toml --control-gui :7710
```

`--control-gui` can share the window with `--gdb-gui` (see [GDB](gdb.md)):
a machine stop answers whichever resumes are pending on both clients, a stop
the control client did not request arrives as `event.stopped`, and
`reverse_*`, `memory.last_writer`, `mouse.to`, and `state.load` are refused
while the GDB client's `continue` is outstanding (`pause` first). Either
client's pause ends the other's run with reason `pause`.

When `--control-info FILE` is specified, connection details are written to a
JSON file:

```json
{"listen": "127.0.0.1:52114", "token": "1f0c...", "proto": 1}
```

With `--run PROG`, either mode arms a one-shot `loadseg` stop for the
program before the machine runs, the break-at-entry `--gdb` has: the first
`continue` (or, windowed, the boot already under way) stops with reason
`loadseg` the moment the guest OS loads the program, before its first
instruction, and `segments.list` then reports its hunks. A windowed session
that reaches the stop before any client attached parks there and tells the
first client with `event.stopped`. The stop fires once; a client wanting
every load arms its own `loadseg` break.

## Client usage (`copperline-ctl`)

Copperline includes a CLI tool (`copperline-ctl`) to interact with active control sessions:

```sh
# Query status
copperline-ctl --info /tmp/ccp.json status

# Add a PC breakpoint
copperline-ctl --info /tmp/ccp.json break.add '{"kind": "pc", "addr": "0xFC0100"}'

# Resume execution (blocks until a breakpoint or stop event occurs)
copperline-ctl --info /tmp/ccp.json continue

# Interactive REPL session
copperline-ctl --info /tmp/ccp.json --repl
```

(mcp-server)=
## Debug adapter

`copperline-ctl --dap` serves the [Debug Adapter Protocol](dap.md) over the
same bridge: an IDE debugs a program in the emulator with source-level
breakpoints, variables and reverse stepping, while the control protocol
underneath stays available from the Debug Console (`!status`, `!beam.get`).

## MCP server

`copperline-ctl --mcp` exposes the control protocol over standard I/O as a
[Model Context Protocol](https://modelcontextprotocol.io) server. This allows AI
coding agents in environments like Claude Code or Cursor to drive and inspect the
emulator directly via structured tool calls. Protocol methods map directly to
MCP tools alongside session management utilities.

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

MCP tool names support `[a-zA-Z0-9_-]`, so protocol methods map to tool names
with dots replaced by underscores (e.g. `warp.get` becomes `warp_get`,
`media.floppy.insert` becomes `media_floppy_insert`, and `capture.screenshot`
becomes `capture_screenshot`). Methods without dots (such as `status` or
`run_until`) retain their exact names. Tool arguments correspond to method
parameters (addresses accept hex strings or integers). Protocol errors return
with `isError: true` containing the error code and message. Connection
handshake methods (`hello`, `auth`) are handled internally by the bridge and
not exposed as tools.

### Session tools

The bridge manages one active session at a time:

- `session_launch {"config", "model", "run", "whdload", "factory", "args",
  "binary", "cwd", "timeout_ms"}`: Spawns a headless emulator instance
  (`copperline --control :0 --control-info TMP --noaudio`) with optional
  configuration flags, connects, and authenticates. Output is redirected to a
  temporary log file. The instance starts paused at reset.
- `session_attach {"info_file"}` or `session_attach {"listen", "token"}`:
  Attaches to an already running `--control` or `--control-gui` server.
- `session_status`: Reports connection state, endpoint address, process ID and
  log path of any launched emulator, and event queue statistics.
- `session_close`: Disconnects from the server and terminates any emulator process
  launched by the bridge (sending SIGKILL after a 3-second grace period). Closing
  standard input automatically closes the session.

### Blocking and `wait_ms`

Execution methods (`continue`, `run_until`, `step`, `step_over`, `step_out`,
`step_copper`, `step_frame`) accept an optional `wait_ms` parameter. If the
emulated machine does not halt within this time limit (in host milliseconds),
the bridge automatically pauses execution and returns the stop event with
`bridge.paused_after_ms` set. Without `wait_ms`, calls block until a breakpoint
or stop condition is reached.

### Events

An internal reader thread maintains an event queue (up to 1,024 items) to
collect asynchronous events during execution. Use `events_next` to retrieve
individual events with a timeout or `events_drain` to retrieve all queued
notifications. Both report queue depth and dropped event counts.

### Screenshots

`capture_screenshot` returns the PNG image as an MCP image content block
(`image/png`) alongside text output. If `path` is omitted, a temporary file is
used and deleted after reading. When `path` is provided, the image is saved to
that path (relative paths resolve against `copperline-ctl`'s working directory).

### Protocol subset

MCP 2025-06-18 over stdio (newline-delimited JSON-RPC 2.0): supports
`initialize`, `notifications/initialized`, `ping`, `tools/list`, and
`tools/call`. Invalid requests return standard JSON-RPC error codes (`-32600`,
`-32700`, `-32601`). Standard output is reserved strictly for protocol
messages; diagnostic logs are sent to stderr. The server exits upon EOF on
stdin.

## Protocol overview

- **Wire format:** Newline-delimited JSON-RPC 2.0 over TCP.
- **Authentication:** Clients authenticate upon connection by sending `hello {"token": "..."}`
  or `auth {"token": "..."}`.
- **Numbers and addresses:** Numeric parameters accept integer values or hex strings
  (e.g., `"0xDFF096"` or `14676118`).
- **Execution commands:** Commands such as `continue`, `step`, and `run_until` block
  until execution stops, returning a structured stop event.

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

(streaming-observability)=
## Streaming observability

An authenticated client can subscribe to asynchronous event notifications:

```text
events.subscribe {"events":["frame","serial","interrupt","media","debug"],"frame_interval":50,"frame_digest":true}
events.list
events.unsubscribe {"events":["serial"]}
```

### Event types

- **`event.frame`:** Emitted per video frame (or per `frame_interval`). Includes timeline
  position and optional FNV-1a framebuffer hash digest, plus `guest_idle_cck`: the
  colour clocks the guest declared idle during the last frame through the uaelib
  trap's idle markers (null until it uses them).
- **`event.serial`:** Emitted when Paula serial transmission occurs.
- **`event.interrupt`:** Emitted when interrupt request and enable state transitions occur.
- **`event.media`:** Emitted when floppy disks or CD images are inserted or ejected.
- **`event.debug`:** Guest debug output through the
  [uaelib trap](../guide/run.md#uaelib-trap): one notification per item, with
  `kind` `log` (`text`, a `KPrintF` line, also echoed on the host console) or
  `resource` (`action` and the registered `resource`, as `debug.resources`
  reports it). `dropped_events` counts items the bounded queue lost before
  this batch.
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
- `continue`: Resume execution.
- `step {"n": 1}`: Single-step CPU instructions.
- `step_over`: Step over subroutine call.
- `step_out`: Step out of current subroutine.
- `step_copper`: Step single Copper instruction.
- `step_frame {"n": 1}`: Step video frames.
- `run_until {"pc" | "vpos" | "frame" | "cck" | "seconds" | "stable_frames"}`: Run until condition.
- `pause`: Pause active execution.
- `machine.reset {"kind": "warm"|"cold"}`: Reset the emulated machine (default: warm).

### Speed
- `warp.get`: Report whether warp (unpaced emulation) is active, whether the machine is paced, and the source holding warp (`none`, `manual`, `control`, `gdb`, `guest`, `launch`, `boot`, `capture`, or `headless`); in a windowed session `holders` lists every programmatic hold in force (`control`, `gdb`, `guest`), `source` being the first (the headless server has no holds and omits the field).
- `warp.set {"on": true|false}`: Engage or release the client's own warp hold (unthrottled execution with audio muted). Holds are independent: releasing yours re-paces the machine only when no other holder (a GDB client, the guest) remains, and the reply's `note` says who still holds it. Disabling warp also cancels active `--run` or `--warp-boot` phases.

### Reverse execution
- `reverse_step {"n": 1}`: Step backward by instruction.
- `reverse_frame`: Step backward by video frame.
- `reverse_continue`: Execute backward to previous breakpoint.
- `reverse_anchor`: Snapshot the machine into the reverse-debug ring at the current position, so the reverse verbs replay from here rather than from an older frame boundary. Take one at a stop to step back from when the guest has used a host directory mount or a disk image since the last snapshot: that host-side state is not rolled back by a restore, and a replay from before it diverges (the DAP adapter does this at every run stop).
- `last_writer {"addr": "..."}`: Find the instruction that last wrote to memory address.

### State inspection and modification
- `regs.get` / `regs.set {"reg": "...", "value": ...}`: Read or modify 68000 registers.
- `mem.read {"addr": ..., "len": ..., "encoding": "hex"|"base64"}` / `mem.write {"addr": ..., "data": "...", "encoding": "hex"|"base64"}`: Read or modify memory.
- `disasm {"addr": ..., "count": ...}`: Disassemble instructions at address (default: PC).
- `custom.read {"reg": ...}` / `custom.dump`: Query custom chipset registers.
- `custom.writer {"reg": ...}`: Query last PC and beam cycle that wrote to custom register.
- `palette.dump {"resource": ...}`: Query active 32-color or 256-color palette; with `resource`, read a guest-registered palette resource (`words` as 12-bit values plus `rgb24`).
- `cia.get {"cia": "a"|"b"}`: Query CIA-A or CIA-B timer, port, and interrupt states.
- `beam.get`: Query raster beam coordinates (VPOS, HPOS, colour clock).
- `display.get`: Query active display parameters, viewport size, and pixel format.
- `rtc.get` / `rtc.set {"unix": ..., "time": "...", "advance": ..., "frozen": ...}`: Inspect or move real-time clock.
- `cartridge.get`: Query freezer cartridge state (`model`, memory `base`/`size`, monitor `version`, `entered` status, `nmi_pending`, and freeze count).
- `cartridge.freeze`: Trigger the freezer cartridge NMI (level 7), transferring execution to the monitor.
- `copper.list {"addr": ..., "resource": ..., "max": ...}`: Disassemble Copper instructions starting at `addr` or a registered `resource` (default: current Copper PC).
- `pc_history`: Return recently executed instruction addresses.
- `segments.list`: The hunk segments (`current`: `{start, size}` per hunk, first hunk first) of the program the scheduled process is running, and every program an armed `loadseg` catch has seen loaded (`modules`). At a `loadseg` stop, `current` is the just-loaded program: the addresses to relocate its symbols and debug information by.

### Diagnostics and profiling
- `chipset.validate {"enabled": ..., "clear": ...}` / `chipset.report`: Arm or query custom register access validator.
- `smc.detect {"enabled": ..., "clear": ...}` / `smc.report`: Arm or query self-modifying code detector.
- `fault.inject {"addr": ..., "len": ..., "on": "read"|"write"|"both", "count": ...}`: Inject memory bus faults.
- `fault.list` / `fault.clear`: List or clear active memory bus faults.
- `memory.heatmap {"enabled": ..., "base": ..., "span": ...}`: Enable or configure address-space access tracking.
- `memory.heatmap.report {"path": "..."}`: Export memory access heatmap.
- `debug.resources`: List bitmaps, palettes, and copper lists registered by guest software via the [uaelib trap](../guide/run.md#uaelib-trap).
- `debug.idle`: Query guest idle time statistics reported via uaelib idle markers.
- `trace.start {"path": "...", "max_lines": ...}` / `trace.stop` / `trace.status`: Control instruction execution trace logging.
- `waveform.start {"path": "...", "trigger": "...", "duration": "...", "signals": "..."}` / `waveform.stop` / `waveform.status`: Control VCD logic analyzer waveform capture.
- `profile.start {"path": "...", "frames": ..., "slots": ..., "screenshots": "none"|"every"|"last", "pc_samples": ...}` / `profile.stop` / `profile.status`: Export per-frame profiling data (DMA ownership, blit records, guest idle time, retired instructions) streamed to `profile.jsonl` with a `profile.json` summary upon stop; see [](profiling). Arms Frame Analyzer tracing for the session.

### Breakpoints and traps
- `break.add`: Add breakpoint (`pc`, `watch`, `reg_watch`, `beam`, `copper`, `catch`, `loadseg`).
- `break.remove {"id": ...}`: Remove breakpoint by ID.
- `break.list`: List all active breakpoints.
- `break.clear`: Remove all breakpoints.

### Input injection
- `input.key {"rawkey": ..., "action": "press"|"release"|"tap", "hold_ms": ..., "at_seconds": ...}`: Inject keyboard events.
- `input.mouse {"dx": ..., "dy": ..., "left": ..., "right": ..., "middle": ..., "port": 1|2, "at_seconds": ...}`: Inject mouse motion/buttons.
- `input.mouse_to {"x": ..., "y": ..., "port": 1|2, "tolerance": ..., "max_frames": ...}`: Steer pointer to screen pixel coordinates via sprite 0.
- `input.joy {"up": ..., "down": ..., "left": ..., "right": ..., "red": ..., "blue": ..., "green": ..., "yellow": ..., "play": ..., "rwd": ..., "ffw": ..., "port": 1|2, "at_seconds": ...}`: Inject joystick / CD32 button state.
- `input.analogue {"x": ..., "y": ..., "port": 1|2, "at_seconds": ...}`: Set analogue paddle/pot position (0-255).
- `input.set_port {"port": 1|2, "device": "mouse"|"gamepad-mouse"|"joystick"|"cd32"|"analogue"|"none"}`: Change port device.
- `input.get_ports`: Query active controller port device assignments.

### Media management
- `media.floppy.insert {"drive": 0, "path": "...", "write_protected": true}`: Insert floppy disk image.
- `media.floppy.eject {"drive": 0}`: Eject floppy disk.
- `media.floppy.query`: Query connected floppy drives, mounted disk images, and write-protection status.
- `media.cd.insert {"path": "..."}`: Insert CD image.
- `media.cd.eject`: Eject CD image.
- `copperhf.attach {"unit": 0, "path": "...", "volume_name": "...", "boot_pri": 0}`: Hot-attach a `copperhf.device` unit's media (opens `path` exactly like a boot-time `[copperhf]` unit, `volume_name`/`boot_pri` optional). Bumps the unit's change counter and sets its `CHF_CHANGED_MASK` bit. Fails if no `[copperhf]` controller is configured.
- `copperhf.eject {"unit": 0}`: Hot-eject/detach a `copperhf.device` unit's media. The unit stays present (`CHF_UNIT_PRESENT`); only its media bit (`CHF_UNIT_MEDIA`) clears. Bumps the change counter and sets `CHF_CHANGED_MASK`, the same as the guest's own `TD_EJECT`.

### State snapshot files
- `state.save {"path": "..."}`: Snapshot machine state to file.
- `state.load {"path": "..."}`: Restore machine state from file.

### Framebuffer capture
- `capture.screenshot {"path": "..."}`: Write PNG screenshot of framebuffer.
- `capture.digest`: Return FNV-1a hash digest of current frame.
- `capture.region_digest {"x": ..., "y": ..., "w": ..., "h": ...}`: Return hash of screen region.

### Streaming events
- `events.subscribe {"events": [...], "frame_interval": ..., "frame_digest": ...}`: Subscribe to asynchronous event stream.
- `events.unsubscribe {"events": [...]}`: Unsubscribe from events.
- `events.list`: List active event subscriptions.

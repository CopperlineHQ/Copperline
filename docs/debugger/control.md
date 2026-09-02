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

When `--control-info FILE` is specified, connection details are written to a
JSON file:

```json
{"listen": "127.0.0.1:52114", "token": "1f0c...", "proto": 1}
```

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
## MCP server

`copperline-ctl --mcp` turns the same client into a
[Model Context Protocol](https://modelcontextprotocol.io) server over stdio,
so an agent in Claude Code, Cursor, or any other MCP client drives a live
machine through tools instead of a REPL. Every control-protocol method is a
tool, and a few bridge-owned tools manage the session.

```sh
# Unattached: the agent launches or attaches a session with the session tools.
copperline-ctl --mcp

# Attached at startup to a running control server:
copperline-ctl --mcp --info /tmp/ccp.json
copperline-ctl --mcp --connect 127.0.0.1:7710 --token HEX
```

Claude Code registers it with one command:

```sh
claude mcp add copperline -- copperline-ctl --mcp
```

or, checked into a project, `.mcp.json`:

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
`tools/list` carries a description, a JSON Schema and the parameter
conventions for every tool, so the agent needs nothing else from this
chapter.

### Tool names

MCP tool names allow only `[a-zA-Z0-9_-]`, so a method's tool is the method
with dots replaced by underscores: `warp.get` is `warp_get`,
`media.floppy.insert` is `media_floppy_insert`, `capture.screenshot` is
`capture_screenshot`; methods without dots (`status`, `run_until`) keep their
names. The arguments are the method's params, addresses included
(integers or hex strings), and the text result is the method's result. A
control-protocol error is returned as a tool result with `isError` set and
the error code and message in the text, never as a transport failure.
`hello` and `auth` are the bridge's own handshake and are not tools.

### Session tools

The bridge holds one session at a time:

- `session_launch {"config", "model", "run", "whdload", "factory", "args",
  "binary", "cwd", "timeout_ms"}` spawns a headless emulator as
  `copperline --control :0 --control-info TMP --noaudio` plus `--config`,
  `--model`, `--run`, `--whdload`, `--factory` and any further flags given
  verbatim in `args`, waits for the endpoint, connects and authenticates.
  The binary is `binary`, else `$COPPERLINE_BIN`, else the `copperline` next
  to `copperline-ctl`, else the `PATH`. The emulator's own output goes to a
  log file named in the result, which also carries the pid, the address and
  the initial `status`. The machine starts paused at power-on.
- `session_attach {"info_file"}` or `session_attach {"listen", "token"}`
  attaches to a running `--control` or `--control-gui` server.
- `session_status` reports the bridge's state: attached, address, the pid and
  log of a launched emulator, whether the connection is still open, and the
  event queue's depth and drop count.
- `session_close` disconnects (the server drops the session's breakpoints
  and subscriptions) and shuts down an emulator this server launched,
  killing it after 3 s if it does not exit. Closing the server's stdin does
  the same, so no emulator outlives its agent.

### Blocking and `wait_ms`

The resume verbs reply with the eventual stop event, and MCP serves one
request at a time, so a `continue` with no breakpoint would block the server
for good. `continue`, `run_until`, `step`, `step_over`, `step_out`,
`step_copper` and `step_frame` therefore take an extra `wait_ms`: if the
machine has not stopped within that many host milliseconds the bridge sends
`pause` and returns the resulting stop event with `bridge.paused_after_ms`
set. A stop that arrives in time is returned as it is. Without `wait_ms` the
call blocks until the machine stops on its own.

### Events

A reader thread owns the socket and queues `event.*` notifications (bounded
to 1024, drops counted) while requests are in flight, so a subscription made
with `events_subscribe` keeps collecting during a long `run_until`.
`events_next {"timeout_ms"}` blocks until the next event or the timeout
(default 1 s) and returns `{method, params}` or `timed_out`;
`events_drain` returns everything queued. Both report the queue depth and
the drop count.

### Screenshots

`capture_screenshot` returns the PNG as an MCP image content block
(`{"type": "image", "mimeType": "image/png"}`) alongside the text result, so
the model can look at the screen. With no `path` the file is temporary and
deleted after it is read; with a `path` it is kept there. A relative `path`
is resolved against `copperline-ctl`'s working directory (not the
emulator's, which can differ) and forwarded absolute, and the text result
carries that absolute path. A PNG the emulator wrote but the bridge cannot
read back is reported as a tool error, not as a result without its image.

### Protocol subset

MCP 2025-06-18 over stdio, newline-delimited JSON-RPC 2.0: `initialize`
(an earlier revision the client names is echoed; the served subset is the
same), `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
A message that is not a JSON-RPC 2.0 request (no `"jsonrpc": "2.0"`, an
`id` that is not a string or an integer, a missing method) is answered with
`-32600`, unparseable input with `-32700`, and unknown methods with
`-32601`; unknown notifications (a method and no `id`) are ignored; stdout
carries protocol messages only and diagnostics go to stderr. The server
exits on stdin EOF.

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
  changes for a reason other than the client's own `warp.set`:
  `{"on", "paced", "source", "position"}` with `source` one of `manual`,
  `guest`, `launch`, `boot`, `power_off`. The headless server reports the
  guest's `warpmode()` request with `paced` always false.

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
- `warp.get`: Report whether warp (unpaced emulation) is on, whether the machine is paced, and who holds it (`source`: `none`, `manual`, `control`, `guest`, `launch`, `boot`, `capture` for a windowed capture run, which is unpaced end to end, or `headless`).
- `warp.set {"on": true|false}`: Engage or release warp. On mutes live audio like `--warp-boot`; off also cancels a pending `--run` / `--warp-boot` phase. `Cmd+W` / `Alt+W`, the guest's `warpmode(0)`, a client disconnect, or a cold `machine.reset` release a client's warp. Accepted while a resume is pending. A bridged physical floppy drive keeps the machine paced (the reply carries a `note`), and the headless server, unpaced end to end, accepts it as a no-op (`"headless": true` plus a `note`).

### Reverse execution
- `reverse_step {"n": 1}`: Step backward by instruction.
- `reverse_frame`: Step backward by video frame.
- `reverse_continue`: Execute backward to previous breakpoint.
- `last_writer {"addr": "..."}`: Find the instruction that last wrote to memory address.

### State inspection and modification
- `regs.get` / `regs.set {"reg": "...", "value": ...}`: Read or modify 68000 registers.
- `mem.read {"addr": ..., "len": ..., "encoding": "hex"|"base64"}` / `mem.write {"addr": ..., "data": "...", "encoding": "hex"|"base64"}`: Read or modify memory.
- `disasm {"addr": ..., "count": ...}`: Disassemble instructions at address (default: PC).
- `custom.read {"reg": ...}` / `custom.dump`: Query custom chipset registers.
- `custom.writer {"reg": ...}`: Query last PC and beam cycle that wrote to custom register.
- `palette.dump {"resource": ...}`: Query the active 32-color or 256-color palette; with `resource`, read a guest-registered palette resource from memory instead (`words` as 12-bit values plus `rgb24`).
- `cia.get {"cia": "a"|"b"}`: Query CIA-A or CIA-B timer, port, and interrupt states.
- `beam.get`: Query raster beam coordinates (VPOS, HPOS, colour clock).
- `display.get`: Query active display parameters, viewport size, and pixel format.
- `rtc.get` / `rtc.set {"unix": ..., "time": "...", "advance": ..., "frozen": ...}`: Inspect or move real-time clock.
- `cartridge.get`: Describe the fitted freezer cartridge (`[cartridge] model`): `model`, `base` and `size` of its bank, the monitor's `version`, whether the monitor is `entered`, whether a press is still waiting for the CPU (`nmi_pending`), and the count of `freezes`. Not found without a cartridge.
- `cartridge.freeze`: Press the freezer cartridge's button: the level-7 vector under the current VBR is pointed at the monitor and the non-maskable interrupt raised for the next instruction boundary; the machine keeps running (resume it if stopped) and enters the monitor. Replies with the `cartridge.get` fields plus the `vector` slot written and the `entry` address it holds. Not found without a cartridge.
- `copper.list {"addr": ..., "resource": ..., "max": ...}`: Disassemble Copper instructions (default: around the live Copper PC; `resource` starts at a guest-registered copper list; `addr` and `resource` are mutually exclusive).
- `pc_history`: Return recently executed instruction addresses.

### Diagnostics and profiling
- `chipset.validate {"enabled": ..., "clear": ...}` / `chipset.report`: Arm or query custom register access validator.
- `smc.detect {"enabled": ..., "clear": ...}` / `smc.report`: Arm or query self-modifying code detector.
- `fault.inject {"addr": ..., "len": ..., "on": "read"|"write"|"both", "count": ...}`: Inject memory bus faults.
- `fault.list` / `fault.clear`: List or clear active memory bus faults.
- `memory.heatmap {"enabled": ..., "base": ..., "span": ...}`: Enable or configure address-space access tracking.
- `memory.heatmap.report {"path": "..."}`: Export memory access heatmap.
- `debug.resources`: List the bitmaps, palettes and copper lists the guest registered through the [uaelib trap](../guide/run.md#uaelib-trap) (`address`, `size`, `name`, `type`, `flags`, geometry, `registered_frame`); the Frame Analyzer's Resources tab shows the same registry.
- `debug.idle`: The guest's uaelib idle markers: current state, whether ever used, and the last completed frame's `idle_cck` / `frame_cck`.
- `trace.start {"path": "...", "max_lines": ...}` / `trace.stop` / `trace.status`: Control instruction execution trace logging.
- `waveform.start {"path": "...", "trigger": "...", "duration": "...", "signals": "..."}` / `waveform.stop` / `waveform.status`: Control VCD logic analyzer waveform capture.
- `profile.start {"path": "...", "frames": ..., "slots": ..., "screenshots": "none"|"every"|"last", "pc_samples": ...}` / `profile.stop` / `profile.status`: Per-frame profile export -- DMA ownership, blit records, guest idle time, retired instructions, optional owner grids and screenshots -- streamed to `profile.jsonl` with a `profile.json` summary at stop; see [](profiling). Arms the Frame Analyzer's trace for the session, which suspends run-ahead.

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

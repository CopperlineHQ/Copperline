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
events.subscribe {"events":["frame","serial","interrupt","media"],"frame_interval":50,"frame_digest":true}
events.list
events.unsubscribe {"events":["serial"]}
```

### Event types

- **`event.frame`:** Emitted per video frame (or per `frame_interval`). Includes timeline
  position and optional FNV-1a framebuffer hash digest.
- **`event.serial`:** Emitted when Paula serial transmission occurs.
- **`event.interrupt`:** Emitted when interrupt request and enable state transitions occur.
- **`event.media`:** Emitted when floppy disks or CD images are inserted or ejected.

## Command reference summary

### Session management
- `hello {"token": "..."}`: Handshake and protocol version query.
- `auth {"token": "..."}`: Authenticate active connection.
- `status`: Returns emulation state, frame counters, and host execution timing.
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
- `palette.dump`: Query active 32-color or 256-color palette.
- `cia.get {"cia": "a"|"b"}`: Query CIA-A or CIA-B timer, port, and interrupt states.
- `beam.get`: Query raster beam coordinates (VPOS, HPOS, colour clock).
- `display.get`: Query active display parameters, viewport size, and pixel format.
- `rtc.get` / `rtc.set {"unix": ..., "time": "...", "advance": ..., "frozen": ...}`: Inspect or move real-time clock.
- `copper.list {"addr": ..., "max": ...}`: Disassemble Copper instructions.
- `pc_history`: Return recently executed instruction addresses.

### Diagnostics and profiling
- `chipset.validate {"enabled": ..., "clear": ...}` / `chipset.report`: Arm or query custom register access validator.
- `smc.detect {"enabled": ..., "clear": ...}` / `smc.report`: Arm or query self-modifying code detector.
- `fault.inject {"addr": ..., "len": ..., "on": "read"|"write"|"both", "count": ...}`: Inject memory bus faults.
- `fault.list` / `fault.clear`: List or clear active memory bus faults.
- `memory.heatmap {"enabled": ..., "base": ..., "span": ...}`: Enable or configure address-space access tracking.
- `memory.heatmap.report {"path": "..."}`: Export memory access heatmap.
- `trace.start {"path": "...", "max_lines": ...}` / `trace.stop` / `trace.status`: Control instruction execution trace logging.
- `waveform.start {"path": "...", "trigger": "...", "duration": "...", "signals": "..."}` / `waveform.stop` / `waveform.status`: Control VCD logic analyzer waveform capture.

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

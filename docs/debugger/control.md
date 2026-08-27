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

### Reverse execution
- `reverse_step {"n": 1}`: Step backward by instruction.
- `reverse_frame`: Step backward by video frame.
- `reverse_continue`: Execute backward to previous breakpoint.
- `last_writer {"addr": "..."}`: Find the instruction that last wrote to memory address.

### State inspection and modification
- `regs.get` / `regs.set`: Read or modify 68000 registers.
- `mem.read` / `mem.write`: Read or modify memory (supports hex or base64 encoding).
- `custom.read` / `custom.dump`: Query custom chipset registers.
- `palette.dump`: Query active 32-color or 256-color palette.
- `cia.get`: Query CIA-A and CIA-B timer and port states.
- `beam.get`: Query raster beam coordinates (VPOS, HPOS, colour clock).
- `copper.list`: Disassemble Copper instructions.

### Breakpoints and traps
- `break.add`: Add breakpoint (`pc`, `watch`, `reg_watch`, `beam`, `copper`, `catch`, `loadseg`).
- `break.remove {"id": ...}`: Remove breakpoint by ID.
- `break.list`: List all active breakpoints.
- `break.clear`: Remove all breakpoints.

### Input injection
- `input.key {"rawkey": ..., "action": "press"|"release"|"tap"}`: Inject keyboard events.
- `input.mouse {"dx": ..., "dy": ..., "left": ..., "right": ...}`: Inject mouse motion/buttons.
- `input.mouse_to {"x": ..., "y": ...}`: Steer pointer to screen pixel coordinates via sprite 0.
- `input.joy {"up": ..., "down": ..., "red": ...}`: Inject joystick / CD32 button state.
- `input.set_port {"port": 1|2, "device": "mouse"|"joystick"|"cd32"|"analogue"|"none"}`: Change port device.

### Media management
- `media.floppy.insert {"drive": 0, "path": "...", "write_protected": true}`: Insert floppy disk image.
- `media.floppy.eject {"drive": 0}`: Eject floppy disk.
- `media.cd.insert {"path": "..."}`: Insert CD image.
- `media.cd.eject`: Eject CD image.

### Framebuffer capture
- `capture.screenshot {"path": "..."}`: Write PNG screenshot of framebuffer.
- `capture.digest`: Return FNV-1a hash digest of current frame.
- `capture.region_digest {"x": ..., "y": ..., "w": ..., "h": ...}`: Return hash of screen region.

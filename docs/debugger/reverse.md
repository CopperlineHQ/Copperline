# Reverse debugging

Copperline supports reverse stepping and reverse execution. Emulation is
deterministic, so Copperline keeps a ring of in-memory snapshots (see
[save states](../internals/savestate.md)) and reconstructs an earlier state by
restoring the nearest preceding snapshot and replaying forward to the exact
instruction or beam position requested.

Reverse debugging is available through:

- **Interactive UI:** `< Step`, `< Frame`, and `< Run` in the [debugger window](window).
- **Console commands:** `RSTEP`, `RFRAME`, `RRUN`, and `WRITER` in the [debugger console](console).
- **Headless analysis:** Automated "last writer" reverse watchpoints via `COPPERLINE_DBG_RWATCH`.
- **GDB remote stub:** `reverse-stepi` and `reverse-continue` (or source-level
  `reverse-step` when GDB has debug information).
- **Control protocol and DAP:** `reverse_step`, `reverse_frame`,
  `reverse_continue`, `reverse_anchor`, and `last_writer` over the
  [control protocol](control.md), and Step Back / Reverse Continue in IDEs.
- **Gameplay rewind:** The `Cmd+Z` / `Alt+Z` shortcut in normal sessions.

## Headless "last writer" reverse watchpoints

When diagnosing memory corruption or unexpected state changes, a reverse
watchpoint identifies the instruction that last wrote a memory word.

Set `COPPERLINE_DBG_RWATCH` to the target address and `COPPERLINE_DBG_UNTIL` to the
emulated time at which the check should run:

```sh
RUST_LOG=info \
COPPERLINE_RTC_FIXED_SECS=1000000000 \
COPPERLINE_DBG_RWATCH=DE488 \
COPPERLINE_DBG_UNTIL=12.5 \
./target/release/copperline --config demo.toml --noaudio \
  --screenshot-after 13 /tmp/out.png
```

Example log output:

```text
DBG RWATCH last writer of $0DE488: CAFE->0000 by pc=0x00FA37D8 pos=561401 f=40 cck=2864664
```

The line gives the writing instruction's PC, the word's value before and after
the write, the position in retired instructions (`pos`), the video frame, and
the colour clock of the write. The run then continues.

### Environment variables

- `COPPERLINE_DBG_RWATCH=ADDR`: Arms a reverse watchpoint on the word at `ADDR`
  (an `ADDR:LEN` form is accepted, but only the word at `ADDR` is checked).
- `COPPERLINE_DBG_UNTIL=SECS`: Emulated time at which to evaluate the reverse
  watchpoint; unset, it is evaluated when the run ends. The same variable also
  closes the [headless debugger's](headless.md) time window.
- `COPPERLINE_DBG_RR=1`: Enables the snapshot ring in headless runs without a watchpoint.
- `COPPERLINE_DBG_RR_BUDGET_MB=N`: Maximum host memory for the snapshot ring in MiB (default: 512).
- `COPPERLINE_DBG_RR_INTERVAL=N`: Number of emulated video frames between snapshots (default: 5).

The last two also size the ring that a [control protocol](control.md) server
arms for its reverse commands.

## Interactive reverse controls

In the [debugger window](window), the reverse controls follow the forward ones
in the transport row:

| Button | Function |
|---|---|
| **`< Frame`** | Step backward to the previous video frame |
| **`< Step`** | Step backward by one CPU instruction |
| **`< Run`** | Run backward until a breakpoint or watchpoint condition is met |

Opening the debugger arms the snapshot ring (512 MiB, one snapshot every 10
frames) if nothing has armed it already. History accrues only while the machine
advances (**Run**, **Frame**), not while it is paused.

`< Run` (and the console command `RRUN`) replays the history and stops at the
most recent event that any armed stop condition would have caught: PC
breakpoints (their conditions are not re-evaluated), memory watchpoints,
register watches, beam traps, Copper breakpoints, exception catches, and the
task catch.

## Rewind in normal sessions

Rewind also works outside the debugger:

- Set `[emulation] rewind = true` in `copperline.toml` (which records from
  power-on), or turn on **Emulation Settings -> Rewind** in the menu. The ring
  holds up to `rewind_budget_mb` MiB of snapshots (default 256).
- Press `Cmd+Z` (macOS) or `Alt+Z` (Linux/Windows) to step back one
  `rewind_interval_frames` interval (default 25 frames) at a time.

## Determinism requirements for reverse replay

For reverse replay to be exact:

1. **Guest clock:** Use `--rtc-time` or `COPPERLINE_RTC_FIXED_SECS` so clock
   reads do not drift with host wall-clock time.
2. **Deterministic input:** Input events must be repeatable (scripted input and recorded
   interactive inputs are handled automatically).
3. **Storage:** RAM contents and floppy disk states are captured directly in memory
   snapshots. However, hard drive and CD images, and host directory mounts (including
   the volumes `--run` and `--whdload` stage), are accessed live from the host and are
   not rolled back on restore, so guest disk writes or host-side changes after a
   snapshot can make replay diverge. Taking a fresh snapshot after I/O gives later
   steps a new replay starting point (`reverse_anchor` in CCP; DAP does this at
   every run stop), but it does not make earlier external I/O reversible. Physical
   devices and live network, serial, MIDI, or sampler input have the same
   limitation.

# Reverse debugging

Copperline supports reverse stepping and reverse execution. Because emulation
is deterministic, the emulator maintains a ring of in-memory snapshots
(see [save states](../internals/savestate.md)) and reconstructs earlier execution states by restoring the
nearest preceding snapshot and replaying forward to the exact requested instruction
or beam cycle.

Reverse debugging is available through:
- **Interactive UI:** `< Step`, `< Frame`, and `< Run` in the [debugger window](window).
- **Console commands:** `RSTEP`, `RFRAME`, `RRUN`, and `RWATCH` in the [debugger console](console).
- **Headless analysis:** Automated "last writer" reverse watchpoints via `COPPERLINE_DBG_RWATCH`.
- **GDB remote stub:** Standard GDB `reverse-step` and `reverse-continue` commands.
- **Gameplay rewind:** The `Cmd+Z` / `Alt+Z` shortcut during normal gameplay.

## Headless "last writer" reverse watchpoints

When diagnosing memory corruption or unexpected state transitions, a reverse
watchpoint identifies the instruction that last wrote to a target memory location.

Set `COPPERLINE_DBG_RWATCH` to the target address and `COPPERLINE_DBG_UNTIL` to the
time at which the check should run:

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

The output identifies the program counter (PC), value before and after the write,
video frame, and colour-clock cycle of the write.

### Environment variables

- `COPPERLINE_DBG_RWATCH=ADDR[:LEN]`: Arms reverse watchpoint for `ADDR`.
- `COPPERLINE_DBG_UNTIL=SECS`: Emulated timestamp at which to evaluate reverse watchpoints.
- `COPPERLINE_DBG_RR=1`: Enables the snapshot ring buffer in headless runs without a watchpoint.
- `COPPERLINE_DBG_RR_BUDGET_MB=N`: Maximum RAM allocation for snapshot ring in MiB (default: 512).
- `COPPERLINE_DBG_RR_INTERVAL=N`: Number of emulated video frames between snapshots (default: 5).

## Interactive reverse controls

In the [debugger window](window), reverse transport controls are located on the right:

| Button | Function |
|---|---|
| **`< Frame`** | Step backward to previous video frame |
| **`< Step`** | Step backward by one CPU instruction |
| **`< Run`** | Run backward until a breakpoint or watchpoint condition is met |

`< Run` (and the console command `RRUN`) evaluates all active breakpoints,
memory watchpoints, Copper breakpoints, and custom register traps in reverse order,
stopping at the most recent triggering event.

## Rewind in normal sessions

Rewind functionality can be used during gameplay:
- Set `[emulation] rewind = true` in `copperline.toml` or enable **Rewind** in the UI menu.
- Press `Cmd+Z` (macOS) or `Alt+Z` (Linux/Windows) to step backward by intervals
  defined in `rewind_interval_frames`.

## Determinism requirements for reverse replay

For reverse replay to be exact:

1. **RTC synchronization:** Use `--rtc-time` or `COPPERLINE_RTC_FIXED_SECS` so time
   reads do not drift with host wall-clock time.
2. **Deterministic input:** Input events must be repeatable (scripted input and recorded
   interactive inputs are handled automatically).
3. **Storage:** RAM contents and floppy disk states are captured directly in memory
   snapshots. However, hard drive and CD images, and host directory mounts (including
   the volumes `--run` and `--whdload` stage), are accessed live from the host and are
   not rolled back on restore; guest disk traffic or host-side modifications occurring
   after a snapshot point will cause replayed execution to diverge. A debugger avoids
   this by taking a fresh snapshot at the stop it steps back from (the control
   protocol's `reverse_anchor`; the DAP adapter does so at every run stop).

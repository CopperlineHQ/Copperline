# Per-frame profiling

The `profile.start` method on the [control protocol](control.md) captures
per-frame performance data for external profilers, analysis tools, and scripts.
Only one profile capture can run at a time; call `profile.stop` on an active
session before starting a new one.

```text
profile.start {"path": "out/profile", "frames": 500, "slots": true,
               "screenshots": "last", "pc_samples": true}
profile.stop
profile.status
```

Each committed emulated frame appends a JSON object to `profile.jsonl` in the
output directory. When the capture completes or `profile.stop` is called, a
`profile.json` summary is written beside it. Streaming to `profile.jsonl`
ensures that recorded data is preserved even if emulation stops unexpectedly.
The `frames` parameter defaults to 500 (approx. 10 seconds in PAL) and is
capped at 100,000. When the frame budget is reached, capture halts
automatically (`profile.status` reports `done`).

Running a profile capture activates Frame Analyzer bus tracing, which
temporarily suspends run-ahead input latency reduction. Tracing is shared with
the Frame Analyzer UI pane: closing the UI pane does not interrupt an active
profile capture, and stopping a profile capture leaves tracing enabled if the
UI pane remains open.

## `profile.jsonl`

One JSON object per committed frame:

| Field | Meaning |
|---|---|
| `frame`, `seconds` | Emulated timeline position. |
| `idle_cck` | Colour clocks declared idle by guest code via [uaelib trap](../guide/run.md#uaelib-trap) markers (null if unused). |
| `retired` | 68k instructions retired during the frame. |
| `pc` | Program counter sample at the frame boundary (`pc_samples: true` only). |
| `traced` | True if chip-bus tracing covered the frame. |
| `rows`, `line_cck`, `cck_length` | Raster geometry: scanline count, clocks per line, and total clocks per frame. |
| `owner_cck` | Clocks granted per chip-bus owner (`refresh`, `bitplane`, `sprite`, `disk`, `audio`, `copper`, `blitter`, `cpu`, `idle`). |
| `blitter` | `busy_cck` (clocks blitter requested bus) and `starve_cck` (breakdown of owners that stalled it). |
| `blits` | List of blits started during the frame (max 64): control words, size, pointers, and start/end beam positions. |
| `partial` | True if tracing was enabled mid-frame. |
| `slots` | When `"slots": true`: run-length encoded per-clock owner grid for each scanline (`"12R3B497."`). Codes match vAmiga DMA debugger (`R` refresh, `B` bitplane, `S` sprite, `D` disk, `A` audio, `C` copper, `L` blitter, `P` CPU, `.` idle). |
| `screenshot`, `digest` | When `"screenshots": "every"`: frame screenshot PNG filename and FNV-1a64 hash digest. |

If timeline position moves backward (via state load or reverse step), a
`{"marker": "reposition", "frame": N}` marker is emitted to rebaseline
instruction counters.

## `profile.json`

Written when the profile stops: contains `version`, machine configuration,
capture options, the list of chip-bus owner names, `started`/`ended` timeline
points, `frames_written`, and a snapshot of registered uaelib resources (matching
`debug.resources`) for address labeling.

## Storage overhead

Enabling `slots` adds roughly 1-10 KB per frame (run-length encoded). Setting
`"screenshots": "every"` produces 50 PNG images per emulated second in PAL.
Both options are disabled by default.

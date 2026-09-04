# Per-frame profiling

The `profile.start` method on the [control protocol](control.md) captures
per-frame performance data for external profilers, analysis tools, and scripts.
Only one profile capture can run at a time; call `profile.stop` on an active
session before starting a new one.

```text
profile.start {"path": "out/profile", "frames": 500, "slots": true,
               "screenshots": "last", "pc_samples": true,
               "trigger": {"busy_cck_over": 60000}}
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

An optional `trigger` keeps the profiler armed but does not write records until
an absolute emulated `frame` is reached or a completed frame's busy colour-clock
count exceeds `busy_cck_over`. Busy clocks are the frame length minus uaelib
idle markers when the guest supplies them; otherwise the traced frame length is
used. `profile.status` reports `triggered` and `triggered_at`.

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
| `cpu` | The CPU's side of the arbitration: `wait_cck` (colour clocks the CPU asked for the chip bus and was denied), `wait_by` (those clocks by denier: `refresh`, `bitplane`, `sprite`, `disk`, `audio`, `copper`, `blitter` with BLTPRI clear, `blitter_nasty` with BLTPRI set including its warm-up fence, and `port` for the 020+ chip port's own turnaround), `wait_by_kind` (by the pending access: `read`, which includes the 68000's opcode prefetches since the CPU core issues them as plain word reads; `fetch` for immediate and extension words read outside the prefetch queue; `write`; `custom` for custom-register accesses), `stall_pcs` (up to 16 `{"pc", "cck"}` entries, the instructions that waited longest, longest first), `stall_pcs_distinct` and `stall_pcs_other` (clocks pooled once 4096 distinct PCs are kept). Zero entries are omitted from the maps. |
| `partial` | True if tracing was enabled mid-frame. |
| `slots` | When `"slots": true`: run-length encoded per-clock owner grid for each scanline (`"12R3B497."`). Codes match vAmiga DMA debugger (`R` refresh, `B` bitplane, `S` sprite, `D` disk, `A` audio, `C` copper, `L` blitter, `P` CPU, `.` idle). |
| `cpu_wait` | When `"slots": true`: the CPU wait grid per scanline in the same run-length encoding. Codes are the denier's owner letter (`R`, `B`, `S`, `D`, `A`, `C`, `L` for the blitter with BLTPRI clear), `N` for the blitter with BLTPRI set, `p` for the port turnaround, and `.` where the CPU was not waiting. |
| `screenshot`, `digest` | When `"screenshots": "every"`: frame screenshot PNG filename and FNV-1a64 hash digest. |

`stall_pcs` names the instruction that was executing when each wait began.
On the precise CPU loop that is the current instruction; under `[cpu] jit`
the PC is republished once per batch, so the attribution is per batch.

If timeline position moves backward (via state load or reverse step), a
`{"marker": "reposition", "frame": N}` marker is emitted to rebaseline
instruction counters.

## `profile.json`

Written when the profile stops: contains `version`, machine configuration,
capture options, the list of chip-bus owner names (`owners`) and CPU wait
classes (`cpu_wait_classes`), `started`/`ended` timeline points,
`frames_written`, and a snapshot of registered uaelib resources (matching
`debug.resources`) for address labeling.

## Storage overhead

Enabling `slots` adds roughly 2-20 KB per frame (two run-length encoded
grids). Setting `"screenshots": "every"` produces 50 PNG images per emulated
second in PAL. Both options are disabled by default.

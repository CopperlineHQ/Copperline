# Per-frame profiling

`profile.start` over the [control protocol](control.md) brackets a
per-frame capture the way `trace.start` and `waveform.start` bracket
theirs, for external profiler views (a VS Code extension, a notebook, a
script). One capture runs at a time: `profile.start` while one is
active is refused -- `profile.stop` it first, so the running capture
gets its real summary.

```text
profile.start {"path": "out/profile", "frames": 500, "slots": true,
               "screenshots": "last", "pc_samples": true}
profile.stop
profile.status
```

Every committed emulated frame appends one JSON object to
`profile.jsonl` in the capture directory; `profile.stop` writes a
`profile.json` summary beside it. The streamed `profile.jsonl` survives a
crash without the summary. `frames` defaults to 500 (about ten seconds of
PAL) and is capped at 100000; the capture stops itself at the budget
(`profile.status` reports `done`) and `profile.stop` closes it.

While a capture runs the Frame Analyzer's chip-bus trace is armed to feed
it, which suspends run-ahead for the session (the analyzer's existing
rule). The arming is shared with the Frame Analyzer pane the way the heat
map is shared: closing the pane hands the arming to the capture rather
than wiping the recording, `profile.stop` re-arms for a pane that is
still open, and disarms otherwise if the capture armed it.

## `profile.jsonl`

One object per committed frame, in order:

| Field | Meaning |
|---|---|
| `frame`, `seconds` | Timeline position (emulated). |
| `idle_cck` | Colour clocks the guest declared idle in the frame through the [uaelib trap](../guide/run.md#uaelib-trap)'s idle markers; null until the program uses them. |
| `retired` | 68k instructions retired during the frame. |
| `pc` | One program-counter sample at the frame boundary (`pc_samples` only; no per-instruction cost). |
| `traced` | Whether the chip-bus trace covered the frame (false only in the first moments after arming). |
| `rows`, `line_cck`, `cck_length` | The traced frame's geometry: raster rows, colour clocks per line, and their product. |
| `owner_cck` | Colour clocks granted per chip-bus owner, keyed by the owner names in the summary (`refresh`, `bitplane`, `sprite`, `disk`, `audio`, `copper`, `blitter`, `cpu`, `idle`). |
| `blitter` | `busy_cck` the blitter wanted the bus, and `starve_cck` per owner that held it off. |
| `blits` | Blits started in the frame (capped at 64): control words, size, pointers, start/end beam positions. |
| `partial` | The trace did not cover the whole frame (armed mid-frame). |
| `slots` | With `"slots": true`: one string per raster row, the per-colour-clock owner grid run-length encoded as `<count><code>` runs (`"12R3B497."`). The single-character codes match vAmiga's DMA debugger (`R` refresh, `B` bitplane, `S` sprite, `D` disk, `A` audio, `C` copper, `L` blitter, `P` CPU, `.` idle). |
| `screenshot`, `digest` | With `"screenshots": "every"`: the frame's PNG (written through the same side-effect-free renderer as `capture.screenshot`, so it is mode-identical) and its FNV-1a64 digest. |

A reverse step or a state load moves the timeline backwards; the stream
marks it as `{"marker": "reposition", "frame": N}` and rebaselines the
retired-instruction delta rather than emitting a corrupt one.

## `profile.json`

Written at stop: `version`, the machine descriptor, the options the
capture ran with, the owner-name table `owner_cck` and `starve_cck` are
keyed by, `started`/`ended` positions, `frames_written`, and a snapshot
of the uaelib resources registered at stop (the same shape
`debug.resources` reports), so a profiler can label addresses.

## Volume

`slots` adds roughly 1-10 KB per frame RLE'd; `"screenshots": "every"`
writes 50 PNGs per emulated second of PAL. Both default off; the frame
cap bounds the total either way.

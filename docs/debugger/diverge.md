# A/B divergence finder

`copperline-ctl diverge` runs two headless Copperline sessions in lockstep
and reports the first point at which their emulated state differs. The two
sides are either two builds of the emulator (bisecting a regression between
an old and a new binary) or one build under two configurations (checking
that a knob such as a CPU model, a cache setting, or a timing option changes
nothing it should not).

Because the emulated core is deterministic and unpaced when headless, the
comparison is exact: a difference is a real behavioural difference between
the two sides, never host jitter, and a second run reproduces it
byte-for-byte.

```sh
# Two builds, same machine and inputs.
copperline-ctl diverge --a ./old/copperline --b ./new/copperline \
    --until 30 -- --factory --model A500 --noaudio \
    --load-state before-scene.clstate --script inputs.clscript

# One build, two configs.
copperline-ctl diverge --config-a jit-off.toml --config-b jit-on.toml \
    --frames 500 --memory all -- --factory
```

Everything after `--` is passed to both emulators unchanged, so the sides
start from the same snapshot (`--load-state`), receive the same scheduled
input (`--script`, `--press-after` and friends) and boot the same machine.
Pass `--factory` (or explicit configs) so a saved default configuration
cannot leak into either side. Each side may also take its own additions:
`--config-a`/`--config-b` name a TOML file for that side, and `--arg-a`/
`--arg-b` (repeatable) add single command-line tokens to that side, for
example `--arg-b --cpu --arg-b 68020`.

## What is compared

Both sides are launched with `--control :0 --control-info` and driven over
the [control protocol](control.md). At every frame boundary the tool
compares:

- the rendered frame (`capture.digest`);
- the CPU registers (`regs.get`: D0-D7, A0-A7, PC, SR, and the FPU when
  fitted);
- optionally RAM, through the server-side `mem.digest` method:
  `--memory chip` (the default) digests the chip RAM bank, `--memory all`
  every writable RAM bank (chip, slow, motherboard, accelerator, Zorro
  boards), `--memory none` skips it. The digest is computed inside the
  emulator, so comparing megabytes per frame costs a hash, not a transfer.
  When the two sides have different RAM layouts (different memory sizes),
  memory comparison is switched off with a note rather than reported as a
  divergence at frame 0.

## How the search runs

1. **Frame pass.** Both sides advance `--stride N` frames at a time
   (default 10), each frame ending on the vertical blank
   (`run_until {"vpos": 0}`, which stops at instruction resolution where
   `step_frame` stops on the server's host quantum), with the comparison
   data collected at each stop, until the first mismatch, `--until SECS`
   of emulated time, or `--frames N`. At
   every matching stride boundary each side snapshots itself
   (`state.save`); a side only ever reloads its own snapshot, so the two
   builds do not need to share a save-state format.
2. **Frame narrowing.** On a mismatch both sides reload the last matching
   snapshot and walk one frame at a time, snapshotting before each frame,
   until the first differing frame is found.
3. **Instruction narrowing.** Both sides reload the snapshot of the last
   matching boundary and single-step together (in blocks of
   `--step-block N` instructions, default 64, then instruction by
   instruction inside the mismatching block), comparing registers, the
   colour clock and, when enabled, the memory digest after every step. The
   first mismatch gives the emulated time, frame, beam position, PC, the
   register diff and, for memory, the first differing byte: the differing
   bank is bisected by digesting halves (`mem.digest {addr, len}`) and the
   final 16-byte span is read from both sides. `--max-steps N` (default
   1,000,000) caps the instructions stepped inside the frame.

If the display digest differs but the CPU registers (and compared memory)
stay identical through the whole frame, the divergence is reported as
**DMA-only**: the difference is in the chipset's DMA or render path
(Agnus fetch, Copper, blitter, Denise output), not in anything the CPU can
see. If the same instruction retires on both sides at different colour
clocks with identical registers, the divergence is reported as **timing**.

Snapshots are the fast path, not a requirement: when a side cannot save or
load state, the tool notes it and narrows by relaunching both sides and
replaying frames from the start instead. The container version of each
side's save-state format is reported; when they differ, a shared
`--load-state` cannot be loaded by both sides, which the launch reports.

## Output and exit status

The default output is a human-readable summary:

```text
compared 8 frame(s): frame 0 to 8 (0.000s to 0.160s), memory chip
  A: copperline 0.19.0 (state format 81): ./old/copperline --control :0 ...
  B: copperline 0.20.0 (state format 81): ./new/copperline --control :0 ...
result: DIVERGED at frame 8 (0.160s); last matching frame 7
  frame mismatch: display, registers
  display digest: A 3f1c...  B 9a02...
  kind: cpu
  first register difference after 1234 instruction(s) from the frame boundary
    A: pc $00FC0A12 frame 7 vpos 112 hpos 88 cck 3220144 (0.141s)
    B: pc $00FC0A12 frame 7 vpos 112 hpos 88 cck 3220144 (0.141s)
    d1: A $00000000  B $00000BAD
  memory: first differing byte at $00004123 (bank $00000000+$80000)
    A: 00112233445566778899aabbccddeeff
    B: 00112233445566ee8899aabbccddeeff
  screenshots: /tmp/shots/a-frame-8.png /tmp/shots/b-frame-8.png
```

`--json` prints the same report as one JSON document (`outcome`,
`frames_compared`, `sides`, `notes`, `divergence` with `frame`,
`last_matching_frame`, `frame_mismatch`, `kind`, `cpu`, `memory`,
`dma_only`, `step_cap_reached`, `screenshots`). `--screenshots DIR` saves
both sides' frames at the divergence as `a-frame-N.png` and
`b-frame-N.png`.

Exit status: 0 when the sides are identical over the compared span, 1 when
a divergence was found, 2 on error (a side failed to launch, a lockstep
break, a non-deterministic run, or an interrupt). The emulator processes
and the snapshot scratch directory are cleaned up on every exit path,
including Ctrl-C; on an error the emulator logs are kept and their paths
printed.

## Options

| Option | Effect |
|---|---|
| `--a BIN`, `--b BIN` | Emulator binaries (default: `COPPERLINE_BIN`, a `copperline` next to `copperline-ctl`, or the PATH; `--b` defaults to `--a`) |
| `--config-a FILE`, `--config-b FILE` | Per-side `--config` |
| `--arg-a ARG`, `--arg-b ARG` | Per-side extra argument token (repeatable) |
| `--until SECS` | Compare until this emulated time (absolute, like every `--*-after` timestamp) |
| `--frames N` | Compare this many frames |
| `--stride N` | Frames per step in the first pass (default 10) |
| `--memory none|chip|all` | RAM to digest at every comparison (default chip) |
| `--step-block N` | Instructions per step while narrowing inside the frame (default 64) |
| `--max-steps N` | Cap on instructions stepped inside the first differing frame (default 1000000) |
| `--screenshots DIR` | Save both sides' frames at the divergence |
| `--launch-timeout-ms MS` | How long to wait for each emulator to announce its control endpoint (default 60000) |
| `--json` | Machine-readable report |
| `-- ARGS...` | Arguments passed to both emulators |

One of `--until` or `--frames` is required.
